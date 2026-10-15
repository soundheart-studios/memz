use super::*;
use memz::{
    auth::Scope,
    cache::Cache,
    capture::{Capture, remember},
    retrieval::{self, Query},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
fn seed(db: &mut NamespaceStore, n: usize, seconds: u64) -> Result<()> {
    let start = Instant::now();
    let tx = db.conn.transaction()?;
    tx.execute(
        "UPDATE namespace_meta SET quota_records=?1,quota_bytes=?2,mutation_seq=?3",
        rusqlite::params![n as i64 + 10000, n as i64 * 1024 + 10485760, n as i64],
    )?;
    {
        let mut source=tx.prepare("INSERT INTO sources(id,revision,origin,author,text,content_hash,ingested_at,visibility,session_id,status,retention) VALUES(?1,1,'user','qualification',?2,?3,0,?4,?5,'active','durable')")?;
        let mut item=tx.prepare("INSERT INTO items(id,revision,kind,content,explicit,status,source_id,source_revision,visibility,session_id,valid_from) VALUES(?1,1,'fact',?2,1,'active',?1,1,?3,?4,0)")?;
        let mut evidence=tx.prepare("INSERT INTO evidence(item_id,item_revision,source_id,source_revision,span_start,span_end,role,method,method_version) VALUES(?1,1,?1,1,0,?2,'support','direct','1')")?;
        for i in 0..n {
            if i % 1000 == 0 && start.elapsed().as_secs() > seconds {
                return Err("corpus construction exceeded feasibility budget".into());
            }
            let id = format!("{i:032x}");
            let text = format!("Memory marker{i:07} records operational note number {i}.");
            let (visibility, session) = if i % 10 == 0 {
                ("project", "")
            } else {
                ("session", "hidden")
            };
            source.execute(rusqlite::params![
                id,
                text,
                sha(text.as_bytes()),
                visibility,
                session
            ])?;
            item.execute(rusqlite::params![id, text, visibility, session])?;
            evidence.execute(rusqlite::params![id, text.len()])?;
        }
    }
    tx.execute(
        "INSERT INTO memory_fts(item_id,revision,content) SELECT id,revision,content FROM items",
        [],
    )?;
    tx.commit()?;
    db.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(())
}
fn sample(label: &str, start: Instant, result: memz::Result<serde_json::Value>) -> Value {
    match result {
        Ok(value) => {
            json!({"operation":label,"ms":ms(start),"ok":true,"degraded":value["degraded"],"status":value["status"]})
        }
        Err(e) => {
            json!({"operation":label,"ms":ms(start),"ok":false,"error":e.code,"message":e.message})
        }
    }
}
fn summary(rows: &[Value], operation: &str) -> Value {
    let times: Vec<_> = rows
        .iter()
        .filter(|r| r["operation"] == operation && r["ok"] == true)
        .filter_map(|r| r["ms"].as_f64())
        .collect();
    json!({"latency":metrics(&times).ok(),"errors":rows.iter().filter(|r|r["operation"]==operation&&r["ok"]==false).count()})
}
pub(super) fn run(config: &Value) -> Result<Value> {
    let sizes = config["sizes"].as_array().ok_or("sizes required")?;
    let samples = config["samples"]
        .as_u64()
        .filter(|n| *n >= 10 && *n <= 1000)
        .ok_or("sample count must be10..1000")? as usize;
    let concurrency = config["concurrency"]
        .as_u64()
        .filter(|n| *n >= 1 && *n <= 8)
        .ok_or("concurrency must be1..8")? as usize;
    let seed_seconds = config["seed_timeout_seconds"]
        .as_u64()
        .filter(|n| *n > 0 && *n <= 600)
        .ok_or("seed timeout required")?;
    let model = config["model_env"]
        .as_str()
        .and_then(std::env::var_os)
        .map(PathBuf::from);
    let model_identity = if let Some(path) = &model {
        use std::io::Read;
        let mut hashes = Vec::new();
        for name in ["model.safetensors", "tokenizer.json", "config.json"] {
            let mut file = fs::File::open(path.join(name))?;
            let mut digest = Sha256::new();
            let mut buffer = [0; 65536];
            loop {
                let n = file.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                digest.update(&buffer[..n]);
            }
            hashes.push(format!("{:x}", digest.finalize()));
        }
        Some(memz::providers::Identity::for_artifacts(
            hashes[0].clone(),
            hashes[1].clone(),
            hashes[2].clone(),
        ))
    } else {
        None
    };
    let mut scenarios = Vec::new();
    for size in sizes {
        let n = size
            .as_u64()
            .filter(|n| *n > 0 && *n <= 1_000_000)
            .ok_or("corpus outside supported measurement bounds")? as usize;
        let fixture = Fixture::new("perf")?;
        let (mut cat, mut db) = fixture.open()?;
        cat.bind_session("qualification", "hidden")?;
        let begin = Instant::now();
        if let Err(e) = seed(&mut db, n, seed_seconds) {
            scenarios.push(json!({"items":n,"status":"unavailable","reason":e.to_string(),"seed_ms":ms(begin)}));
            continue;
        }
        let seed_ms = ms(begin);
        let cap = cat.capability("qualification", "session")?;
        let mut raw = Vec::new();
        let ns = &fixture.client.namespace;
        let cold = Instant::now();
        let packet = retrieval::recall(&cat, &cap, &mut db, &Query::new("marker0000000"))?;
        let cold_ms = ms(cold);
        if packet.items.len() != 1 {
            return Err("seeded authorized recall failed".into());
        }
        let mut cache = Cache::new(64, 1048576);
        for i in 0..samples {
            let begin = Instant::now();
            let r = remember(
                &cat,
                &cap,
                &mut db,
                &Capture::text(
                    Scope::project(ns),
                    &format!("measured-{i}"),
                    &format!("Measured durable capture {i} {}", "x".repeat(1024)),
                ),
            );
            raw.push(sample(
                "capture_sdk",
                begin,
                r.and_then(|v| {
                    serde_json::to_value(v).map_err(|e| memz::Error::new("encoding", e.to_string()))
                }),
            ));
            let q = Query::new(&format!("marker{:07}", (i % 10) * 10));
            let begin = Instant::now();
            let r = cache.recall(&cat, &cap, &mut db, &q);
            raw.push(sample(
                "lexical_cache",
                begin,
                r.and_then(|v| {
                    serde_json::to_value(v).map_err(|e| memz::Error::new("encoding", e.to_string()))
                }),
            ));
        }
        let service = adapters::serve_with_model(&fixture.state, model.clone())?;
        if model.is_some() {
            let begin = Instant::now();
            raw.push(sample(
                "cold_hybrid",
                begin,
                adapters::call(
                    &fixture.state,
                    &fixture.client,
                    json!({"op":"recall","query":"marker0000000","budget":8000}),
                ),
            ));
        }
        let stop = Arc::new(AtomicBool::new(false));
        let state = fixture.state.clone();
        let client = fixture.client.clone();
        let stopping = stop.clone();
        let exports = std::thread::spawn(move || {
            let mut results = Vec::new();
            while !stopping.load(Ordering::SeqCst) {
                let begin = Instant::now();
                results.push(sample(
                    "concurrent_export",
                    begin,
                    adapters::call(&state, &client, json!({"op":"export"})),
                ));
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            results
        });
        let mut threads = Vec::new();
        for worker in 0..concurrency {
            let state = fixture.state.clone();
            let client = fixture.client.clone();
            threads.push(std::thread::spawn(move||{let mut rows=Vec::new();for i in 0..samples/concurrency {let (label,request)=if i%4==0{("concurrent_capture",json!({"op":"remember","request_id":format!("worker-{worker}-{i}"),"text":format!("Concurrent durable sample {worker} {i}")}))}else{("concurrent_recall",json!({"op":"recall","query":format!("marker{:07}",(i%10)*10),"budget":8000}))};let begin=Instant::now();rows.push(sample(label,begin,adapters::call(&state,&client,request)));}rows}));
        }
        for thread in threads {
            raw.extend(thread.join().map_err(|_| "load worker panicked")?);
        }
        stop.store(true, Ordering::SeqCst);
        raw.extend(exports.join().map_err(|_| "export worker panicked")?);
        if model.is_some() {
            let wait = Instant::now();
            loop {
                let health =
                    adapters::call(&fixture.state, &fixture.client, json!({"op":"status"}));
                if health.as_ref().is_ok_and(|h| h["semantic"] == "ready")
                    || wait.elapsed() > std::time::Duration::from_secs(30)
                {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            for _ in 0..samples {
                let begin = Instant::now();
                raw.push(sample(
                    "warm_hybrid",
                    begin,
                    adapters::call(
                        &fixture.state,
                        &fixture.client,
                        json!({"op":"recall","query":"marker0000000","budget":8000}),
                    ),
                ));
            }
        }
        let status = adapters::call(&fixture.state, &fixture.client, json!({"op":"status"}))?;
        drop(service);
        let has_vectors: bool = db.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='memory_vectors')",
            [],
            |r| r.get(0),
        )?;
        let vector_rows: i64 = if has_vectors {
            db.conn
                .query_row("SELECT count(*) FROM memory_vectors", [], |r| r.get(0))?
        } else {
            0
        };
        let source_rows: usize = db
            .conn
            .query_row("SELECT count(*) FROM sources", [], |r| r.get(0))?;
        let acknowledged = raw
            .iter()
            .filter(|r| {
                r["ok"] == true
                    && ["capture_sdk", "concurrent_capture"]
                        .contains(&r["operation"].as_str().unwrap_or(""))
            })
            .count();
        if source_rows < n + acknowledged {
            return Err("acknowledged writes missing from workload store".into());
        }
        let disk = fs::metadata(fixture.state.namespace_db(ns)?)?.len();
        scenarios.push(json!({"items":n,"status":"measured","seed_ms":seed_ms,"cold_connection_lexical_ms":cold_ms,"fixture":"bulk-seeded authoritative SQLite rows; 90% session-hidden; seed time is not capture latency","raw_samples":raw,"capture_sdk":summary(&raw,"capture_sdk"),"lexical_cache":summary(&raw,"lexical_cache"),"concurrent_capture":summary(&raw,"concurrent_capture"),"concurrent_recall":summary(&raw,"concurrent_recall"),"concurrent_export":summary(&raw,"concurrent_export"),"warm_hybrid":summary(&raw,"warm_hybrid"),"cold_hybrid":summary(&raw,"cold_hybrid"),"cache_hits":cache.hits(),"cache_bytes":cache.bytes(),"db_bytes":disk,"health":status,"resources":resources(),"configured_model":model,"provider_identity":model_identity,"vector_rows":vector_rows,"source_rows":source_rows,"acknowledged_captures":acknowledged,"unacknowledged_committed_rows":source_rows-n-acknowledged,"provisional_targets_ms":{"capture_p95":50,"capture_p99":100,"lexical_p95":100,"hybrid_p95":500},"scope":"SDK single-writer and daemon concurrent requests including socket/queue/durable commit; simultaneous bounded exports; optional real local background enrichment"}));
    }
    Ok(json!({"format":"memz-performance-v1","config":config,"scenarios":scenarios,"failed":[]}))
}
