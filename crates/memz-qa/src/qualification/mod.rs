//! Empirical runners. Fixtures and observed model behavior are always distinguished.
mod performance;
mod quality;
use memz::{adapters, catalog::Catalog, state::StateRoot, store::NamespaceStore};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub fn metrics(samples: &[f64]) -> Result<Value> {
    if samples.is_empty() || samples.iter().any(|n| !n.is_finite() || *n < 0.) {
        return Err("expected nonempty finite nonnegative latency samples".into());
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let percentile = |p: f64| sorted[((p * sorted.len() as f64).ceil() as usize).saturating_sub(1)];
    Ok(
        json!({"count":samples.len(),"p50_ms":percentile(0.50),"p95_ms":percentile(0.95),"p99_ms":percentile(0.99),"min_ms":sorted[0],"max_ms":sorted[sorted.len()-1],"mean_ms":samples.iter().sum::<f64>()/samples.len() as f64}),
    )
}
pub fn evidence_metrics(retrieved: &[String], expected: &[String], stale: &[String]) -> Value {
    let r: BTreeSet<_> = retrieved.iter().collect();
    let e: BTreeSet<_> = expected.iter().collect();
    let s: BTreeSet<_> = stale.iter().collect();
    json!({"relevant":r.intersection(&e).count(),"missed":e.difference(&r).count(),"stale":r.intersection(&s).count(),"irrelevant":r.iter().filter(|id|!e.contains(*id)&&!s.contains(*id)).count(),"correct_abstention":e.is_empty()&&r.is_empty(),"should_abstain":e.is_empty(),"returned":r.len()})
}
pub fn validate_answers(inputs: &Value, report: &Value) -> Result<()> {
    if report["fingerprint"] != inputs["fingerprint"] || !inputs["fingerprint"].is_string() {
        return Err("answer evidence fingerprint mismatch".into());
    }
    let cases = inputs["cases"].as_array().ok_or("missing cases")?;
    let methods = inputs["methods"].as_array().ok_or("missing methods")?;
    let requests = inputs["requests"]
        .as_array()
        .ok_or("missing recorded answer inputs")?;
    if inputs["fingerprint"] != sha(&serde_json::to_vec(requests)?) {
        return Err("answer request set digest mismatch".into());
    }
    let rows = report["rows"]
        .as_array()
        .ok_or("answer evidence unavailable")?;
    if cases.is_empty() || methods.len() != 4 || rows.len() != cases.len() * methods.len() {
        return Err("incomplete answer comparison".into());
    }
    let mut seen = BTreeSet::new();
    let mut identity = None;
    for row in rows {
        if !cases.contains(&row["case"])
            || !methods.contains(&row["method"])
            || !seen.insert((row["case"].to_string(), row["method"].to_string()))
        {
            return Err("unknown or duplicate answer case/method".into());
        }
        let request = requests
            .iter()
            .find(|r| r["case"] == row["case"] && r["method"] == row["method"])
            .ok_or("missing case input")?;
        let input = request["input"].as_str().ok_or("missing prompt")?;
        if request["input_sha256"] != sha(input.as_bytes())
            || row["input_sha256"] != request["input_sha256"]
            || row["max_output_tokens"] != request["max_output_tokens"]
        {
            return Err("answer input or budget does not match recorded prompt".into());
        }
        let model = row["model"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("missing answer model")?;
        let budget = row["max_output_tokens"]
            .as_u64()
            .filter(|n| *n > 0 && *n <= 4096)
            .ok_or("missing answer budget")?;
        let temperature = &row["temperature"];
        if temperature != "provider-default" && !temperature.as_f64().is_some_and(f64::is_finite) {
            return Err("missing or unknown temperature setting".into());
        }
        let current = (model, budget, temperature);
        if identity.is_some_and(|i| i != current) {
            return Err("unequal answer model/configuration/budget".into());
        }
        identity = Some(current);
        if row["answer"].as_str().is_none_or(|s| s.is_empty())
            || row["output_tokens"].as_u64().is_none_or(|n| n > budget)
            || row["input_sha256"]
                .as_str()
                .is_none_or(|s| s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err("missing answer text, usage or input digest".into());
        }
    }
    Ok(())
}
pub(crate) fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub(crate) fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.
}
pub fn source_fingerprint(repo: &Path) -> Result<String> {
    fn files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        for e in fs::read_dir(dir)? {
            let p = e?.path();
            if p.is_dir() {
                files(root, &p, out)?;
            } else if p
                .extension()
                .is_some_and(|e| e == "rs" || e == "sql" || e == "css")
                || p.file_name().is_some_and(|n| n == "Cargo.toml")
            {
                out.push(p.strip_prefix(root)?.into());
            }
        }
        Ok(())
    }
    let mut paths = vec![PathBuf::from("Cargo.toml"), PathBuf::from("Cargo.lock")];
    files(repo, &repo.join("crates"), &mut paths)?;
    if repo.join("assets").exists() {
        files(repo, &repo.join("assets"), &mut paths)?;
    }
    paths.sort();
    let mut hash = Sha256::new();
    for p in paths {
        hash.update(p.to_string_lossy().as_bytes());
        hash.update(fs::read(repo.join(p))?);
    }
    Ok(format!("{:x}", hash.finalize()))
}
pub(crate) struct Fixture {
    pub _temp: tempfile::TempDir,
    pub state: StateRoot,
    pub client: adapters::Client,
}
impl Fixture {
    fn new(label: &str) -> Result<Self> {
        let run = std::env::var("PW_RUN_ID")?;
        if run.is_empty() || !run.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err("unsafe PW_RUN_ID".into());
        }
        let temp = tempfile::Builder::new()
            .prefix(&format!("mq-{}-{label}-", &sha(run.as_bytes())[..8]))
            .tempdir_in(if cfg!(target_os = "macos") {
                "/private/tmp"
            } else {
                "/tmp"
            })?;
        let project = temp.path().join("project");
        fs::create_dir(&project)?;
        let state = StateRoot::open(&temp.path().join("state"))?;
        let client = adapters::load_client(&adapters::initialize(
            &state,
            &project,
            "qualification",
            "session",
        )?)?;
        Ok(Self {
            _temp: temp,
            state,
            client,
        })
    }
    pub(crate) fn open(&self) -> Result<(Catalog, NamespaceStore)> {
        Ok((
            Catalog::open(self.state.clone())?,
            NamespaceStore::open(&self.state, &self.client.namespace)?,
        ))
    }
}
pub fn adversarial() -> Result<Value> {
    use memz::{
        auth::Scope,
        capture::{Capture, remember},
        lifecycle,
        retrieval::{Query, recall},
    };
    let fixture = Fixture::new("contracts")?;
    let (mut cat, mut db) = fixture.open()?;
    let cap = cat.capability("qualification", "session")?;
    let ns = &fixture.client.namespace;
    let request = Capture::text(Scope::project(ns), "atomic", "contract payload");
    let receipt = remember(&cat, &cap, &mut db, &request)?;
    for table in [
        "sources",
        "items",
        "evidence",
        "memory_fts",
        "operations",
        "jobs",
    ] {
        let count: i64 = db
            .conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
        if count != 1 {
            return Err(format!("atomic capture missing {table}").into());
        }
    }
    if remember(&cat, &cap, &mut db, &request)? != receipt {
        return Err("retry changed receipt".into());
    }
    cat.bind_session("qualification", "hidden")?;
    let private = cat.capability("qualification", "hidden")?;
    remember(
        &cat,
        &private,
        &mut db,
        &Capture::text(Scope::session(ns, "hidden"), "private", "private secret"),
    )?;
    if !recall(&cat, &cap, &mut db, &Query::new("private"))?
        .items
        .is_empty()
    {
        return Err("private disclosure".into());
    }
    lifecycle::forget(&cat, &cap, &mut db, &receipt.id, 1)?;
    if !recall(&cat, &cap, &mut db, &Query::new("contract"))?
        .items
        .is_empty()
        || remember(&cat, &cap, &mut db, &request).is_ok()
    {
        return Err("deletion resurrection".into());
    }
    cat.revoke("qualification", ns)?;
    if recall(&cat, &cap, &mut db, &Query::new("secret")).is_ok() {
        return Err("revocation bypass".into());
    }
    Ok(
        json!({"status":"passed","checks":["atomic_capture","idempotent_retry","private_scope","deletion_tombstone","revocation"]}),
    )
}
pub(crate) fn resources() -> Value {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    let ok = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } == 0;
    if !ok {
        return json!({"unavailable":true});
    }
    let usage = unsafe { usage.assume_init() };
    let rss = usage.ru_maxrss as u64 * if cfg!(target_os = "macos") { 1 } else { 1024 };
    json!({"peak_rss_bytes":rss,"cpu_user_seconds":usage.ru_utime.tv_sec as f64+usage.ru_utime.tv_usec as f64/1e6,"cpu_system_seconds":usage.ru_stime.tv_sec as f64+usage.ru_stime.tv_usec as f64/1e6})
}
pub(crate) fn hardware() -> Value {
    let output = std::process::Command::new("uname").arg("-a").output().ok();
    let compiler = std::process::Command::new("rustc").arg("-Vv").output().ok();
    json!({"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"logical_cpus":std::thread::available_parallelism().map(|n|n.get()).unwrap_or(1),"uname":output.map(|o|String::from_utf8_lossy(&o.stdout).trim().to_owned()),"compiler":compiler.map(|o|String::from_utf8_lossy(&o.stdout).trim().to_owned()),"debug_assertions":cfg!(debug_assertions),"fixture_filesystem_path":if cfg!(target_os="macos"){"/private/tmp"}else{"/tmp"},"sqlite":"bundled rusqlite 0.37.0","durability":"WAL synchronous FULL","cache_note":"connection-cold; operating-system file cache is not forcibly dropped"})
}
pub(crate) fn write_report(repo: &Path, path: &str, report: &Value) -> Result<()> {
    let p = Path::new(path);
    if p.is_absolute()
        || !path.starts_with("reports/")
        || p.components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err("report output must be a safe relative reports path".into());
    }
    fs::create_dir_all(repo.join(p).parent().unwrap())?;
    fs::write(repo.join(p), serde_json::to_vec_pretty(report)?)?;
    Ok(())
}
pub fn performance_workload(config: &Value) -> Result<Value> {
    performance::run(config)
}
pub fn quality_workload(repo: &Path, config: &Value) -> Result<Value> {
    quality::run(repo, config)
}
pub fn run(kind: &str, input: &Value) -> Result<Value> {
    let repo = Path::new(input["repo"].as_str().ok_or("repo required")?);
    let manifest_path = Path::new(input["manifest"].as_str().ok_or("manifest required")?);
    let bytes = fs::read(if manifest_path.is_absolute() {
        manifest_path.to_path_buf()
    } else {
        repo.join(manifest_path)
    })?;
    let manifest: Value = serde_json::from_slice(&bytes)?;
    if manifest["version"] != 1 {
        return Err("unsupported workload version".into());
    }
    let run_id = input["runId"].as_str().ok_or("run ID required")?;
    if std::env::var("PW_RUN_ID").as_deref() != Ok(run_id) {
        return Err("PW_RUN_ID mismatch".into());
    }
    let mut result = match kind {
        "qualify-performance" => performance::run(&manifest)?,
        "qualify-quality" => quality::run(repo, &manifest)?,
        _ => return Err("unknown qualification command".into()),
    };
    result["source_fingerprint"] = json!(source_fingerprint(repo)?);
    result["workload_sha256"] = json!(sha(&bytes));
    result["run_id"] = json!(run_id);
    result["created_at"] = json!(now());
    result["hardware"] = hardware();
    result["resources"] = resources();
    write_report(
        repo,
        manifest["output"].as_str().ok_or("output required")?,
        &result,
    )?;
    Ok(result)
}

pub fn validate_performance_report(report: &Value, source: &str, workload: &str) -> Result<()> {
    if report["format"] != "memz-performance-v1"
        || report["source_fingerprint"] != source
        || report["workload_sha256"] != workload
    {
        return Err("stale or wrong performance report identity".into());
    }
    let config = &report["config"];
    let sizes = config["sizes"].as_array().ok_or("missing workload sizes")?;
    let scenarios = report["scenarios"].as_array().ok_or("missing scenarios")?;
    if scenarios.len() != sizes.len() {
        return Err("incomplete measured sizes".into());
    }
    let mut measured = 0;
    for (scenario, size) in scenarios.iter().zip(sizes) {
        if scenario["items"] != *size {
            return Err("wrong corpus size".into());
        }
        if scenario["status"] == "unavailable" {
            if scenario["reason"].as_str().is_none_or(|s| s.is_empty()) {
                return Err("missing feasibility explanation".into());
            }
            continue;
        }
        if scenario["status"] != "measured" {
            return Err("unknown measurement status".into());
        }
        measured += 1;
        let rows = scenario["raw_samples"]
            .as_array()
            .filter(|r| !r.is_empty())
            .ok_or("missing raw timings")?;
        for operation in [
            "capture_sdk",
            "lexical_cache",
            "concurrent_capture",
            "concurrent_recall",
            "concurrent_export",
            "warm_hybrid",
            "cold_hybrid",
        ] {
            let selected: Vec<_> = rows
                .iter()
                .filter(|r| r["operation"] == operation)
                .collect();
            let times: Vec<_> = selected
                .iter()
                .filter(|r| r["ok"] == true)
                .map(|r| r["ms"].as_f64().ok_or("invalid raw timing"))
                .collect::<std::result::Result<_, _>>()?;
            let errors = selected.iter().filter(|r| r["ok"] == false).count();
            if errors + times.len() != selected.len() {
                return Err("unknown sample result".into());
            }
            if scenario[operation]["errors"] != errors {
                return Err("wrong error count".into());
            }
            if times.is_empty() {
                if !scenario[operation]["latency"].is_null() {
                    return Err("invented latency without successful samples".into());
                }
            } else {
                let actual = metrics(&times)?;
                for key in ["count", "p50_ms", "p95_ms", "p99_ms"] {
                    let claimed = scenario[operation]["latency"][key]
                        .as_f64()
                        .ok_or("missing statistic")?;
                    if (claimed - actual[key].as_f64().unwrap()).abs() > 0.000001 {
                        return Err("reported percentile disagrees with raw samples".into());
                    }
                }
            }
        }
        if scenario["capture_sdk"]["latency"]["count"] != config["samples"]
            || scenario["db_bytes"].as_u64().is_none_or(|n| n == 0)
        {
            return Err("missing timed captures or resource evidence".into());
        }
    }
    if measured == 0 {
        return Err("no measured workload".into());
    }
    Ok(())
}
pub fn validate_quality_report(
    report: &Value,
    corpus: &Value,
    source: &str,
    workload: &str,
) -> Result<()> {
    if report["format"] != "memz-quality-v1"
        || report["source_fingerprint"] != source
        || report["workload_sha256"] != workload
        || report["answer_status"] != "measured"
    {
        return Err("quality evidence is stale or answer comparison unavailable".into());
    }
    let identity: memz::providers::Identity = serde_json::from_value(report["provider"].clone())?;
    identity.validate()?;
    let cases = corpus["cases"].as_array().ok_or("missing corpus cases")?;
    let rows = report["observations"]
        .as_array()
        .ok_or("missing retrieval observations")?;
    if rows.len() != cases.len() * 4 {
        return Err("incomplete retrieval comparison".into());
    }
    let memories = corpus["memories"]
        .as_array()
        .ok_or("missing corpus memories")?;
    let stale: Vec<_> = memories
        .iter()
        .filter(|m| m["corrected"].is_string() || m["forget"] == true)
        .map(|m| format!("{}@1", m["key"].as_str().unwrap_or("")))
        .collect();
    let private: Vec<_> = memories
        .iter()
        .filter(|m| m["private"] == true)
        .filter_map(|m| m["key"].as_str())
        .collect();
    let mut seen = BTreeSet::new();
    for row in rows {
        let case = cases
            .iter()
            .find(|c| c["id"] == row["case"])
            .ok_or("unknown observed case")?;
        let method = row["method"].as_str().ok_or("missing method")?;
        if !["none", "lexical", "hybrid", "graph"].contains(&method)
            || !seen.insert((row["case"].to_string(), method))
        {
            return Err("duplicate or unknown retrieval method".into());
        }
        let expected: Vec<String> = serde_json::from_value(case["expected"].clone())?;
        let retrieved: Vec<String> = serde_json::from_value(row["retrieved"].clone())?;
        if row["expected"] != case["expected"]
            || row["metrics"] != evidence_metrics(&retrieved, &expected, &stale)
        {
            return Err("retrieval metrics disagree with fixed corpus".into());
        }
        if retrieved
            .iter()
            .any(|id| private.iter().any(|key| id.starts_with(&format!("{key}@"))))
            || row["metrics"]["stale"] != 0
        {
            return Err("private or stale evidence entered a packet".into());
        }
        if row["latency_ms"]
            .as_f64()
            .is_none_or(|n| !n.is_finite() || n < 0.)
        {
            return Err("invalid observed latency".into());
        }
        if method == "none" {
            if !retrieved.is_empty() || !row["packet"].is_null() {
                return Err("no-memory baseline contains memory".into());
            }
        } else {
            let items = row["packet"]["items"]
                .as_array()
                .ok_or("missing actual packet")?;
            let actual: Vec<_> = items
                .iter()
                .map(|item| {
                    let key = report["labels"][item["id"].as_str().unwrap_or("")]
                        .as_str()
                        .ok_or("unknown packet item")?;
                    Ok(format!("{key}@{}", item["revision"]))
                })
                .collect::<Result<Vec<_>>>()?;
            if actual != retrieved {
                return Err("retrieved labels disagree with packet".into());
            }
        }
    }
    let answers = json!({"fingerprint":report["answer_inputs"]["fingerprint"],"rows":report["answer_results"]});
    validate_answers(&report["answer_inputs"], &answers)?;
    for answer in report["answer_results"]
        .as_array()
        .ok_or("missing answer results")?
    {
        let case = cases
            .iter()
            .find(|c| c["id"] == answer["case"])
            .ok_or("unknown answer case")?;
        let text = answer["answer"]
            .as_str()
            .ok_or("missing answer")?
            .to_lowercase();
        let tokens: Vec<String> = serde_json::from_value(case["answer_contains"].clone())?;
        let correct = tokens.iter().all(|t| text.contains(&t.to_lowercase()));
        if answer["rubric_match"] != correct {
            return Err("answer rubric score disagrees with raw answer".into());
        }
    }
    if report["graph_default"] != "off" {
        return Err("graph default requires separately demonstrated benefit".into());
    }
    Ok(())
}
