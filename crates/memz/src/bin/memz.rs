use memz::{
    Error, Result,
    adapters::{self, CapturePolicy},
    catalog::Catalog,
    state::StateRoot,
};
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Read, Write},
    path::{Path, PathBuf},
};
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
fn invalid() -> Error {
    Error::new(
        "usage",
        "memz backup DEST [NAMESPACE] | restore BUNDLE NEW_STATE sqlite|jsonl | relocate NEW_STATE | rebind NAMESPACE ROOT | init [ROOT [PRINCIPAL [SESSION]]] | serve | grant PRINCIPAL NAMESPACE [write] | revoke PRINCIPAL NAMESPACE | call|mcp|capture --client FILE [--include-generated]",
    )
}
/// The enclosing git work tree, so subdirectories share one namespace;
/// otherwise the current directory.
fn default_root() -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    let toplevel = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&cwd)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim_end_matches(['\n', '\r']).to_string())
        .filter(|s| !s.is_empty());
    Ok(toplevel.map(PathBuf::from).unwrap_or(cwd))
}
/// The effective OS user; same-user processes are already trusted.
fn default_principal() -> Result<String> {
    // SAFETY: getpwuid returns null or a pointer to static storage that is
    // read before any other passwd call in this single-threaded command.
    let name = unsafe {
        let entry = libc::getpwuid(libc::geteuid());
        if entry.is_null() || (*entry).pw_name.is_null() {
            None
        } else {
            std::ffi::CStr::from_ptr((*entry).pw_name)
                .to_str()
                .ok()
                .map(str::to_string)
        }
    };
    name.filter(|n| !n.is_empty()).ok_or_else(|| {
        Error::new(
            "usage",
            "cannot determine the OS user; pass PRINCIPAL explicitly",
        )
    })
}
fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let command = args.first().ok_or_else(invalid)?;
    // Restore must not create or touch the default live state.
    if command == "restore" && args.len() == 4 {
        let format = match args[3].as_str() {
            "sqlite" => memz::backup::Format::Sqlite,
            "jsonl" => memz::backup::Format::Jsonl,
            _ => return Err(invalid()),
        };
        let manifest = memz::backup::restore(Path::new(&args[1]), Path::new(&args[2]), format)?;
        println!(
            "{}",
            serde_json::to_string(&manifest).map_err(|_| invalid())?
        );
        return Ok(());
    }
    let state = StateRoot::from_env()?;
    match command.as_str() {
        "init" if args.len() <= 4 => {
            let root = match args.get(1) {
                Some(root) => PathBuf::from(root),
                None => default_root()?,
            };
            let principal = match args.get(2) {
                Some(principal) => principal.clone(),
                None => default_principal()?,
            };
            let session = match args.get(3) {
                Some(session) => session.clone(),
                None => adapters::new_session_id()?,
            };
            let path = adapters::initialize(&state, &root, &principal, &session)?;
            let client = adapters::load_client(&path)?;
            println!(
                "{}",
                json!({"client_file":path,"root":std::fs::canonicalize(&root)?,"namespace":client.namespace,"principal":principal,"session":session})
            );
        }
        "backup" if args.len() == 2 || args.len() == 3 => {
            let manifest =
                memz::backup::create(&state, Path::new(&args[1]), args.get(2).map(String::as_str))?;
            println!(
                "{}",
                serde_json::to_string(&manifest).map_err(|_| invalid())?
            );
        }
        "relocate" if args.len() == 2 => {
            let manifest = memz::backup::relocate(&state, Path::new(&args[1]))?;
            println!(
                "{}",
                json!({"manifest": manifest, "new_state":args[1], "original_retained":true, "next":"set MEMZ_HOME explicitly and reissue clients with init"})
            );
        }
        "serve" => {
            let _server = adapters::serve(&state)?;
            eprintln!("memz ready");
            loop {
                std::thread::park();
            }
        }
        "grant" if args.len() == 3 || args.len() == 4 => {
            Catalog::open(state)?.grant(
                &args[1],
                &args[2],
                args.get(3).is_some_and(|s| s == "write"),
            )?;
            println!("{{\"ok\":true}}");
        }
        "rebind" if args.len() == 3 => {
            Catalog::open(state)?.reassign(&args[1], Path::new(&args[2]))?;
            println!("{{\"ok\":true}}");
        }
        "revoke" if args.len() == 3 => {
            Catalog::open(state)?.revoke(&args[1], &args[2])?;
            println!("{{\"ok\":true}}");
        }
        "call" | "mcp" | "capture" | "assert-relation" => {
            if args.get(1).map(String::as_str) != Some("--client") {
                return Err(invalid());
            }
            let client = adapters::load_client(Path::new(args.get(2).ok_or_else(invalid)?))?;
            let mut input = io::stdin().lock();
            let mut initialized = false;
            loop {
                let mut line = Vec::new();
                let size = input.by_ref().take(131073).read_until(b'\n', &mut line)?;
                if size == 0 {
                    break;
                }
                if size > 131072 || !line.ends_with(b"\n") {
                    return Err(Error::new("too_large", "input frame too large"));
                }
                let value: Value = serde_json::from_slice(&line)
                    .map_err(|_| Error::new("invalid_request", "invalid JSON input"))?;
                let response = if command == "mcp" {
                    if value["jsonrpc"] != "2.0" || value["id"].to_string().len() > 128 {
                        json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"invalid request"}})
                    } else if value.get("id").is_none() {
                        continue;
                    } else if value["method"] == "initialize" {
                        initialized = true;
                        adapters::mcp(&state, &client, &value)
                    } else if !initialized {
                        json!({"jsonrpc":"2.0","id":value["id"],"error":{"code":-32000,"message":"initialize first"}})
                    } else {
                        adapters::mcp(&state, &client, &value)
                    }
                } else if command == "assert-relation" {
                    if !args.iter().any(|a| a == "--user-confirmed") {
                        return Err(Error::new(
                            "confirmation_required",
                            "trusted assertion requires --user-confirmed",
                        ));
                    }
                    let req: memz::graph::Relation =
                        serde_json::from_value(value).map_err(|_| {
                            Error::new("invalid_relation", "invalid relationship input")
                        })?;
                    let cat = Catalog::open(state.clone())?;
                    let cap = cat.capability(&client.binding.principal, &client.binding.session)?;
                    let mut store = memz::store::NamespaceStore::open(&state, &client.namespace)?;
                    serde_json::to_value(memz::graph::assert_explicit(
                        &cat, &cap, &mut store, &req,
                    )?)
                    .map_err(|_| Error::new("serialization", "relationship encoding failed"))?
                } else if command == "capture" {
                    match adapters::hook_request(
                        &value,
                        CapturePolicy {
                            include_generated: args.iter().any(|a| a == "--include-generated"),
                        },
                    )? {
                        Some(request) => adapters::call(&state, &client, request)?,
                        None => json!({"excluded":true}),
                    }
                } else {
                    adapters::call(&state, &client, value)?
                };
                println!("{response}");
                io::stdout().flush()?;
            }
        }
        _ => return Err(invalid()),
    }
    Ok(())
}
