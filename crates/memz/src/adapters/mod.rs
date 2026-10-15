use crate::{
    Error, Result,
    auth::Scope,
    capture::{self, Capture},
    catalog::Catalog,
    daemon::{self, Binding, Limits, Server},
    inspect, lifecycle,
    retrieval::{Packet, Query},
    state::StateRoot,
    store::NamespaceStore,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
#[derive(Clone, Serialize, Deserialize)]
pub struct Client {
    pub binding: Binding,
    pub namespace: String,
}
pub fn initialize(
    state: &StateRoot,
    root: &Path,
    principal: &str,
    session: &str,
) -> Result<PathBuf> {
    let mut cat = Catalog::open(state.clone())?;
    let namespace = cat.register(root)?;
    cat.bind_session(principal, session)?;
    cat.grant(principal, &namespace, true)?;
    let client = Client {
        binding: Binding {
            token: format!("{}{}", capture::id()?, capture::id()?),
            principal: principal.into(),
            session: session.into(),
        },
        namespace,
    };
    let path = state
        .path
        .join("run")
        .join(format!("client-{}.json", capture::id()?));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)?;
    file.write_all(
        serde_json::to_string(&client)
            .map_err(|_| invalid())?
            .as_bytes(),
    )?;
    file.sync_all()?;
    Ok(path)
}
/// Fresh session identity for a client whose host did not name one.
pub fn new_session_id() -> Result<String> {
    Ok(format!("s_{}", capture::id()?))
}
pub fn load_client(path: &Path) -> Result<Client> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || meta.nlink() != 1
        || meta.len() > 8192
    {
        return Err(Error::new(
            "unsafe_state",
            "client file must be private and owner-controlled",
        ));
    }
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    serde_json::from_str(&text).map_err(|_| invalid())
}
pub struct Service {
    server: Option<Server>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    maintenance: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Service {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(worker) = self.maintenance.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
        self.server.take();
    }
}
/// Clients the daemon admits. Client files issued by `init` after startup are
/// picked up on the first request presenting their token, through the same
/// `load_client` checks; unknown tokens rescan at most once per second.
struct Registry {
    state: StateRoot,
    clients: std::sync::RwLock<Vec<Client>>,
    last_scan: std::sync::Mutex<std::time::Instant>,
}
impl Registry {
    fn find(&self, token: &str) -> Option<Client> {
        let known = |clients: &[Client]| clients.iter().find(|c| c.binding.token == token).cloned();
        if let Some(client) = known(&self.clients.read().ok()?) {
            return Some(client);
        }
        {
            let mut last = self.last_scan.lock().ok()?;
            if last.elapsed() < std::time::Duration::from_secs(1) {
                return None;
            }
            *last = std::time::Instant::now();
        }
        let fresh = scan_clients(&self.state, false).ok()?;
        let found = known(&fresh);
        *self.clients.write().ok()? = fresh;
        found
    }
    fn snapshot(&self) -> Vec<Client> {
        self.clients.read().map(|c| c.clone()).unwrap_or_default()
    }
}
/// Loads `run/client-*.json`. At startup any invalid file is fatal; on a live
/// rescan invalid files are skipped so one bad file cannot lock out the rest.
fn scan_clients(state: &StateRoot, strict: bool) -> Result<Vec<Client>> {
    let mut clients = Vec::new();
    for entry in fs::read_dir(state.path.join("run"))? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("client-") && name.ends_with(".json") {
            match load_client(&entry.path()) {
                Ok(client) => clients.push(client),
                Err(e) if strict => return Err(e),
                Err(_) => {}
            }
        }
    }
    Ok(clients)
}
pub fn serve(state: &StateRoot) -> Result<Service> {
    let extractor = match std::env::var("MEMZ_RELATION_EXTRACTOR").as_deref() {
        Err(_) | Ok("off") => None,
        Ok("literal-v1") => Some(crate::providers::relations::LiteralExtractor),
        _ => return Err(Error::new("configuration", "unknown relation extractor")),
    };
    serve_configured(
        state,
        std::env::var_os("MEMZ_MODEL_DIR").map(PathBuf::from),
        extractor,
    )
}
pub fn serve_with_model(state: &StateRoot, model: Option<PathBuf>) -> Result<Service> {
    serve_configured(state, model, None)
}
pub fn serve_configured(
    state: &StateRoot,
    model: Option<PathBuf>,
    extractor: Option<crate::providers::relations::LiteralExtractor>,
) -> Result<Service> {
    let engine = model.map(crate::providers::Engine::start).transpose()?;
    let background_engine = engine.clone();
    let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let foreground = active.clone();
    let clients = scan_clients(state, true)?;
    if clients.is_empty() {
        return Err(Error::new(
            "configuration",
            "initialize a client before starting daemon",
        ));
    }
    let registry = std::sync::Arc::new(Registry {
        state: state.clone(),
        clients: std::sync::RwLock::new(clients),
        last_scan: std::sync::Mutex::new(std::time::Instant::now()),
    });
    let lookup_registry = registry.clone();
    let handler_registry = registry.clone();
    let maintenance_registry = registry.clone();
    let owned = state.clone();
    let maintenance_state = state.clone();
    let server = Server::start_dynamic(
        state,
        move |token| lookup_registry.find(token).map(|c| c.binding),
        Limits::default(),
        move |binding, value| {
            let client = handler_registry.find(&binding.token).ok_or_else(invalid)?;
            foreground.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let result = dispatch_with_engine(
                &owned,
                &client,
                &value,
                engine.as_ref(),
                extractor.is_some(),
            );
            foreground.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            result
        },
    )?;
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopping = stop.clone();
    let maintenance = std::thread::spawn(move || {
        let mut last_maintenance = std::time::Instant::now() - std::time::Duration::from_secs(30);
        loop {
            std::thread::park_timeout(std::time::Duration::from_millis(250));
            if stopping.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            let maintain = last_maintenance.elapsed() >= std::time::Duration::from_secs(30);
            let maintenance_clients = maintenance_registry.snapshot();
            for client in &maintenance_clients {
                if stopping.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                let _ = (|| -> Result<()> {
                    let cat = Catalog::open(maintenance_state.clone())?;
                    let cap = cat.capability(&client.binding.principal, &client.binding.session)?;
                    cat.authorize(&cap, &Scope::project(&client.namespace), true)?;
                    let mut store = NamespaceStore::open(&maintenance_state, &client.namespace)?;
                    if active.load(std::sync::atomic::Ordering::SeqCst) != 0 {
                        return Ok(());
                    }
                    if maintain {
                        crate::health::maintain(&cat, &cap, &mut store, 32)?;
                    }
                    if let Some(provider) = &extractor {
                        crate::graph::extract_one(
                            &cat,
                            &cap,
                            &mut store,
                            provider,
                            active.load(std::sync::atomic::Ordering::SeqCst) != 0,
                        )?;
                    }
                    if let Some(engine) = &background_engine {
                        crate::vector::enrich_one(
                            &cat,
                            &cap,
                            &mut store,
                            engine,
                            active.load(std::sync::atomic::Ordering::SeqCst) != 0,
                        )?;
                    }
                    Ok(())
                })();
            }
            if maintain {
                last_maintenance = std::time::Instant::now();
            }
        }
    });
    Ok(Service {
        server: Some(server),
        stop,
        maintenance: Some(maintenance),
    })
}
thread_local! {static RECALL_CACHE:std::cell::RefCell<crate::cache::Cache>=std::cell::RefCell::new(crate::cache::Cache::new(64,1048576));}
fn invalid() -> Error {
    Error::new("invalid_request", "invalid request fields")
}
fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key].as_str().ok_or_else(invalid)
}
fn graph_options(v: &Value) -> Result<Option<crate::graph::GraphOptions>> {
    let enabled = match v.get("graph") {
        None => false,
        Some(value) => value.as_bool().ok_or_else(invalid)?,
    };
    if !enabled {
        if v.get("relations").is_some() {
            return Err(invalid());
        }
        return Ok(None);
    }
    let relations = v["relations"]
        .as_array()
        .ok_or_else(|| Error::new("invalid_graph", "choose a nonempty relation allowlist"))?
        .iter()
        .map(|s| s.as_str().map(str::to_owned).ok_or_else(invalid))
        .collect::<Result<Vec<_>>>()?;
    let options = crate::graph::GraphOptions::new(relations);
    options.validate()?;
    Ok(Some(options))
}
fn encoded<T: Serialize>(v: T) -> Result<Value> {
    serde_json::to_value(v).map_err(|_| invalid())
}
pub fn dispatch(state: &StateRoot, client: &Client, v: &Value) -> Result<Value> {
    dispatch_with_engine(state, client, v, None, false)
}
fn dispatch_with_engine(
    state: &StateRoot,
    client: &Client,
    v: &Value,
    engine: Option<&crate::providers::Engine>,
    extraction_enabled: bool,
) -> Result<Value> {
    for key in [
        "author",
        "principal",
        "session",
        "token",
        "grant",
        "capability",
    ] {
        if v.get(key).is_some() {
            return Err(Error::new(
                "denied",
                "identity comes from the trusted client binding",
            ));
        }
    }
    let operation = string(v, "op")?;
    if ![
        "remember",
        "recall",
        "inspect",
        "correct",
        "forget",
        "export",
        "validate_import",
        "status",
        "maintain",
        "relate",
        "extraction_policy",
    ]
    .contains(&operation)
    {
        return Err(invalid());
    }
    let cat = Catalog::open(state.clone())?;
    let cap = cat.capability(&client.binding.principal, &client.binding.session)?;
    let ns = v["namespace"].as_str().unwrap_or(&client.namespace);
    // This first adapter exposes only current-project injection; named grants remain SDK/admin capabilities.
    if ns != client.namespace {
        return Err(Error::new(
            "denied",
            "cross-project injection is not enabled in this adapter",
        ));
    }
    cat.authorize(
        &cap,
        &Scope::project(ns),
        matches!(
            operation,
            "remember" | "correct" | "forget" | "maintain" | "relate"
        ),
    )?;
    let mut store = NamespaceStore::open(state, ns)?;
    match operation {
        "relate" => {
            let req: crate::graph::Relation =
                serde_json::from_value(v["relationship"].clone()).map_err(|_| invalid())?;
            encoded(crate::graph::propose(&cat, &cap, &mut store, &req)?)
        }
        "extraction_policy" => Ok(
            json!({"enabled":extraction_enabled,"provider":"literal-v1","origins":["user"],"kinds":["decision","procedure","task","incident"],"syntax":"[[item_id@revision]] relation [[item_id@revision]]","relations":crate::graph::RELATIONS,"max_bytes":8192,"max_relations":16,"authority":"inferred","automatic_promotion":false}),
        ),
        "status" => {
            let mut health = crate::health::status(&cat, &cap, &store)?;
            health.semantic = match engine {
                Some(e) if e.identity().is_some() => "ready",
                Some(_) => "cold_or_unavailable",
                None => "unavailable",
            }
            .into();
            encoded(health)
        }
        "maintain" => encoded(crate::health::maintain(&cat, &cap, &mut store, 32)?),
        "export" => {
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let export = crate::export::snapshot(&cat, &cap, &mut store, 65536, &cancel)?;
            if let Some(name) = v["name"].as_str() {
                let paths = crate::export::publish(state, name, &export, &cancel)?;
                Ok(
                    json!({"namespace":export.namespace,"revision":export.revision,"jsonl_path":paths.0,"markdown_path":paths.1,"html_path":state.path.join("exports").join(format!("{name}.html"))}),
                )
            } else {
                encoded(export)
            }
        }
        "validate_import" => {
            crate::import::validate(&cat, &cap, &mut store, string(v, "jsonl")?, 65536)
        }
        "remember" => {
            let scope = if v["visibility"].as_str() == Some("session") {
                Scope::session(ns, cap.session())
            } else {
                Scope::project(ns)
            };
            let mut request = Capture::text(scope, string(v, "request_id")?, string(v, "text")?);
            if let Some(kind) = v["kind"].as_str() {
                request.kind = kind.into();
            }
            if let Some(origin) = v["origin"].as_str() {
                request.origin = origin.into();
            }
            request.slot = v["slot"].as_str().map(str::to_string);
            request.event_at = v["event_at"].as_i64();
            request.expires_at = v["expires_at"].as_i64();
            encoded(capture::remember(&cat, &cap, &mut store, &request)?)
        }
        "recall" => {
            let mut query = Query::new(string(v, "query")?);
            query.max_tokens = v["budget"].as_u64().unwrap_or(2000) as usize;
            query.max_bytes = query.max_tokens;
            query.min_revision = v["min_revision"].as_i64();
            query.graph = graph_options(v)?;
            if v["impact"].as_bool().unwrap_or(false) {
                return Err(Error::new("invalid_graph", "impact requires inspect"));
            }
            if engine.is_some() {
                return encoded(crate::retrieval::hybrid(
                    &cat, &cap, &mut store, &query, engine,
                )?);
            }
            encoded(
                RECALL_CACHE
                    .with(|cache| cache.borrow_mut().recall(&cat, &cap, &mut store, &query))?,
            )
        }
        "inspect" => encoded(inspect::inspect_with_options(
            &cat,
            &cap,
            &mut store,
            string(v, "id")?,
            v["history"].as_bool().unwrap_or(false),
            v["budget"].as_u64().unwrap_or(2000) as usize,
            graph_options(v)?,
            v["impact"].as_bool().unwrap_or(false),
        )?),
        "correct" => encoded(lifecycle::correct(
            &cat,
            &cap,
            &mut store,
            string(v, "id")?,
            v["expected_revision"].as_i64().ok_or_else(invalid)?,
            string(v, "text")?,
        )?),
        "forget" => Ok(
            json!({"mutation_seq":lifecycle::forget(&cat,&cap,&mut store,string(v,"id")?,v["expected_revision"].as_i64().ok_or_else(invalid)?)?}),
        ),
        _ => Err(invalid()),
    }
}
#[derive(Clone, Copy, Default)]
pub struct CapturePolicy {
    pub include_generated: bool,
}
pub fn hook_request(event: &Value, policy: CapturePolicy) -> Result<Option<Value>> {
    let role = string(event, "role")?;
    if !["user", "assistant", "tool"].contains(&role) {
        return Err(invalid());
    }
    if role != "user" && !policy.include_generated {
        return Ok(None);
    }
    Ok(Some(
        json!({"op":"remember","request_id":string(event,"event_id")?,"text":string(event,"text")?,"origin":role,"visibility":"session"}),
    ))
}
pub fn call(state: &StateRoot, client: &Client, value: Value) -> Result<Value> {
    daemon::request(
        &state.path.join("run/memz.sock"),
        &client.binding.token,
        1,
        value,
    )
}
fn tool_result(value: Value) -> Value {
    json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":value,"isError":false})
}
pub fn mcp(state: &StateRoot, client: &Client, request: &Value) -> Value {
    let id = request["id"].clone();
    let method = request["method"].as_str().unwrap_or("");
    let result = match method {
        "initialize" => Ok(
            json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"memz","version":env!("CARGO_PKG_VERSION")}}),
        ),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools":tools()})),
        "tools/call" => {
            let name = request["params"]["name"].as_str().unwrap_or("");
            if ![
                "remember",
                "recall",
                "inspect",
                "correct",
                "forget",
                "export",
                "validate_import",
                "status",
                "relate",
                "extraction_policy",
            ]
            .contains(&name)
            {
                Err(json!({"code":-32601,"message":"unknown tool"}))
            } else {
                let mut args = request["params"]["arguments"].clone();
                if args.is_null() {
                    args = json!({});
                }
                let allowed = [
                    "request_id",
                    "text",
                    "query",
                    "id",
                    "expected_revision",
                    "budget",
                    "history",
                    "visibility",
                    "kind",
                    "slot",
                    "name",
                    "jsonl",
                    "relationship",
                    "graph",
                    "relations",
                    "impact",
                ];
                if !args
                    .as_object()
                    .is_some_and(|fields| fields.keys().all(|key| allowed.contains(&key.as_str())))
                {
                    return json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"invalid tool arguments"}});
                }
                args["op"] = json!(name);
                let budget = args["budget"].as_u64().unwrap_or(2000) as usize;
                match call(state, client, args) {
                    Ok(mut value) => {
                        let mut response = tool_result(value.clone());
                        if ["recall", "inspect"].contains(&name) {
                            loop {
                                let envelope = json!({"jsonrpc":"2.0","id":id,"result":response});
                                if envelope.to_string().len() <= budget {
                                    break;
                                }
                                let parsed = serde_json::from_value::<Packet>(value.clone());
                                match parsed {
                                    Ok(mut packet) => {
                                        if !packet.trim_one() {
                                            response = json!({"content":[{"type":"text","text":"budget_too_small: envelope cannot fit"}],"isError":true});
                                            break;
                                        }
                                        value = serde_json::to_value(packet).unwrap();
                                        response = tool_result(value.clone());
                                    }
                                    _ => {
                                        response = json!({"content":[{"type":"text","text":"budget_too_small: envelope cannot fit"}],"isError":true});
                                        break;
                                    }
                                }
                            }
                        }
                        Ok(response)
                    }
                    Err(error) => Ok(
                        json!({"content":[{"type":"text","text":error.to_string()}],"structuredContent":{"error":{"code":error.code,"message":error.message}},"isError":true}),
                    ),
                }
            }
        }
        _ => Err(json!({"code":-32601,"message":"method not found"})),
    };
    match result {
        Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
        Err(error) => json!({"jsonrpc":"2.0","id":id,"error":error}),
    }
}
fn tools() -> Vec<Value> {
    [("remember",vec!["request_id","text"]),("recall",vec!["query"]),("inspect",vec!["id"]),("correct",vec!["id","expected_revision","text"]),("forget",vec!["id","expected_revision"]),("export",vec![]),("validate_import",vec!["jsonl"]),("status",vec![]),("relate",vec!["relationship"]),("extraction_policy",vec![])].into_iter().map(|(name,required)|json!({"name":name,"description":format!("{name} memory in the bound project; identity and grants are managed outside tools"),"inputSchema":{"type":"object","properties":{"graph":{"type":"boolean","description":"Opt-in bounded graph expansion; requires relations allowlist."},"relations":{"type":"array","items":{"enum":["supports","supersedes","contradicts","applies_to","requires","mitigates"]},"minItems":1,"maxItems":6},"impact":{"type":"boolean","description":"Inspect only: preview current relationships and derivatives affected by correction/forgetting."},"relationship":{"type":"object","description":"Inferred proposal only. Fields: from/to {id,revision}, relation, qualifiers object, evidence [{source,revision,start,end}] with exact UTF-8 byte spans. Never user-confirmed by this tool."},"name":{"type":"string"},"jsonl":{"type":"string"},"request_id":{"type":"string"},"text":{"type":"string"},"query":{"type":"string"},"id":{"type":"string"},"expected_revision":{"type":"integer"},"budget":{"type":"integer","minimum":256,"maximum":8000},"history":{"type":"boolean"},"visibility":{"enum":["project","session"]},"kind":{"type":"string"},"slot":{"type":"string"}},"required":required,"additionalProperties":false}})).collect()
}
