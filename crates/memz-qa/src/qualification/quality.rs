use super::*;
use memz::{
    auth::Scope,
    capture::{Capture, remember},
    graph::{self, Anchor, GraphOptions, Node, Relation},
    lifecycle,
    providers::{Engine, LocalModel},
    retrieval::{self, Query},
    vector,
};
use std::{collections::BTreeMap, time::Duration};
fn str_field<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().ok_or_else(|| format!("missing {k}").into())
}
fn strings(v: &Value, k: &str) -> Result<Vec<String>> {
    v[k].as_array()
        .ok_or("expected string array")?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| "expected string".into())
        })
        .collect()
}
pub(super) fn run(repo: &Path, config: &Value) -> Result<Value> {
    let path = config["model_env"]
        .as_str()
        .and_then(std::env::var_os)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_dir())
        .ok_or("explicit local embedding model is required for quality measurement")?;
    let corpus_bytes = fs::read(repo.join(str_field(config, "corpus")?))?;
    let corpus: Value = serde_json::from_slice(&corpus_bytes)?;
    if corpus["version"] != 1 {
        return Err("unsupported corpus version".into());
    }
    let methods = strings(config, "methods")?;
    if methods != ["none", "lexical", "hybrid", "graph"] {
        return Err("quality comparison requires all four methods".into());
    }
    let fingerprint = sha(format!(
        "{}{}{}",
        source_fingerprint(repo)?,
        sha(&corpus_bytes),
        config
    )
    .as_bytes());
    let answer_path = config["answer_report_env"]
        .as_str()
        .and_then(std::env::var_os)
        .map(PathBuf::from);
    if let Some(answer_path) = answer_path {
        let mut cached: Value =
            serde_json::from_slice(&fs::read(repo.join(str_field(config, "retrieval_cache")?))?)?;
        if cached["qualification_fingerprint"] != fingerprint {
            return Err("stale retrieval evidence; rerun measurement before answers".into());
        }
        let configured_identity = LocalModel::load(&path)?.identity;
        if serde_json::to_value(configured_identity)? != cached["provider"] {
            return Err("embedding assets changed since retrieval measurement".into());
        }
        let answers: Value = serde_json::from_slice(&fs::read(answer_path)?)?;
        validate_answers(&cached["answer_inputs"], &answers)?;
        let mut scored = Vec::new();
        for row in answers["rows"].as_array().ok_or("missing answers")? {
            let case = corpus["cases"]
                .as_array()
                .ok_or("missing cases")?
                .iter()
                .find(|c| c["id"] == row["case"])
                .ok_or("unknown answer case")?;
            let answer = str_field(row, "answer")?.to_lowercase();
            let expected = strings(case, "answer_contains")?;
            let correct = expected
                .iter()
                .all(|token| answer.contains(&token.to_lowercase()));
            let mut scored_row = row.clone();
            scored_row["rubric_match"] = json!(correct);
            scored_row["rubric"] = json!("case-insensitive required substrings; no LLM judge");
            scored.push(scored_row);
        }
        cached["answer_results"] = json!(scored);
        cached["answer_status"] = json!("measured");
        cached["failed"] = json!([]);
        return Ok(cached);
    }
    let fixture = Fixture::new("quality")?;
    let (mut cat, mut db) = fixture.open()?;
    cat.bind_session("qualification", "hidden")?;
    let ns = &fixture.client.namespace;
    let cap = cat.capability("qualification", "session")?;
    let hidden = cat.capability("qualification", "hidden")?;
    let mut nodes = BTreeMap::new();
    let mut labels = BTreeMap::new();
    let mut stale = Vec::new();
    let memories = corpus["memories"]
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 1000)
        .ok_or("invalid quality memory count")?;
    for memory in memories {
        let key = str_field(memory, "key")?;
        if nodes.contains_key(key) {
            return Err("duplicate memory key".into());
        }
        let private = memory["private"] == true;
        let scope = if private {
            Scope::session(ns, "hidden")
        } else {
            Scope::project(ns)
        };
        let actor = if private { &hidden } else { &cap };
        let mut req = Capture::text(scope, key, str_field(memory, "text")?);
        req.kind = str_field(memory, "kind")?.into();
        req.slot = memory["slot"].as_str().map(str::to_owned);
        let receipt = remember(&cat, actor, &mut db, &req)?;
        labels.insert(receipt.id.clone(), key.to_owned());
        let mut revision = 1;
        if let Some(text) = memory["corrected"].as_str() {
            lifecycle::correct(&cat, actor, &mut db, &receipt.id, 1, text)?;
            stale.push(format!("{key}@1"));
            revision = 2;
        }
        if memory["forget"] == true {
            lifecycle::forget(&cat, actor, &mut db, &receipt.id, revision)?;
            stale.push(format!("{key}@{revision}"));
        }
        nodes.insert(
            key.to_owned(),
            Node {
                id: receipt.id,
                revision,
            },
        );
    }
    for relation in corpus["relations"].as_array().ok_or("relations required")? {
        let from = nodes
            .get(str_field(relation, "from")?)
            .ok_or("unknown endpoint")?
            .clone();
        let to = nodes
            .get(str_field(relation, "to")?)
            .ok_or("unknown endpoint")?
            .clone();
        let proof = nodes
            .get(str_field(relation, "proof")?)
            .ok_or("unknown proof")?;
        let (source, length): (String, usize) = db.conn.query_row(
            "SELECT source_id,length(CAST(content AS BLOB)) FROM items WHERE id=?1 AND revision=?2",
            rusqlite::params![proof.id, proof.revision],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        graph::propose(
            &cat,
            &cap,
            &mut db,
            &Relation {
                from,
                to,
                relation: str_field(relation, "relation")?.into(),
                qualifiers: Default::default(),
                evidence: vec![Anchor {
                    source,
                    revision: proof.revision,
                    start: 0,
                    end: length,
                }],
            },
        )?;
    }
    let load = Instant::now();
    let model = LocalModel::load(&path)?;
    let load_ms = ms(load);
    let identity = model.identity.clone();
    let generation = vector::initialize(&mut db, &identity)?;
    let mut stmt = db
        .conn
        .prepare("SELECT id,revision,text FROM sources WHERE status='active' ORDER BY id")?;
    let sources = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);
    let enrich = Instant::now();
    for (id, revision, text) in &sources {
        let vector = model.embed(text, false)?;
        db.conn.execute("INSERT INTO memory_vectors(generation,source_id,source_revision,embedding) VALUES(?1,?2,?3,?4)",rusqlite::params![generation,id,revision,vector::bytes(&vector)?])?;
    }
    let enrichment_ms = ms(enrich);
    drop(model);
    let engine = Engine::start(path)?;
    let ready = Instant::now();
    while engine.identity().is_none() {
        if engine.active() == 0 || ready.elapsed() > Duration::from_secs(30) {
            return Err("real local query engine failed to load".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let budget = config["packet_bytes"]
        .as_u64()
        .filter(|n| *n >= 256 && *n <= 8000)
        .ok_or("invalid packet budget")? as usize;
    let max_items = config["max_items"]
        .as_u64()
        .filter(|n| *n >= 1 && *n <= 32)
        .ok_or("invalid candidate cap")? as usize;
    let answer_budget = config["answer_max_output_tokens"]
        .as_u64()
        .filter(|n| *n > 0 && *n <= 4096)
        .ok_or("invalid answer budget")?;
    let cases = corpus["cases"]
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 200)
        .ok_or("invalid cases")?;
    let mut observations = Vec::new();
    let mut requests = Vec::new();
    let mut case_ids = BTreeSet::new();
    for case in cases {
        let id = str_field(case, "id")?;
        if !case_ids.insert(id) {
            return Err("duplicate case".into());
        }
        let expected = strings(case, "expected")?;
        let embedding_start = Instant::now();
        engine.query(str_field(case, "query")?.into(), Duration::from_secs(10))?;
        let query_embedding_ms = ms(embedding_start);
        for method in &methods {
            let mut query = Query::new(str_field(case, "query")?);
            query.max_bytes = budget;
            query.max_tokens = budget;
            query.max_items = max_items;
            if method == "graph" {
                query.graph = Some(GraphOptions::new(vec![
                    "requires".into(),
                    "contradicts".into(),
                    "supports".into(),
                ]));
            }
            let begin = Instant::now();
            let packet = match method.as_str() {
                "none" => None,
                "lexical" => Some(retrieval::recall(&cat, &cap, &mut db, &query)?),
                _ => Some(retrieval::hybrid(
                    &cat,
                    &cap,
                    &mut db,
                    &query,
                    Some(&engine),
                )?),
            };
            // Let timed-out inference release its permit before the next independent case.
            let measured_ms = ms(begin);
            while engine.active() != 0 {
                if begin.elapsed() > Duration::from_secs(10) {
                    return Err("embedding worker failed to finish".into());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let mut context = Vec::new();
            let mut retrieved = Vec::new();
            if let Some(p) = &packet {
                for item in &p.items {
                    let key = labels.get(&item.id).ok_or("unknown returned item")?;
                    let label = format!("{key}@{}", item.revision);
                    retrieved.push(label.clone());
                    let via:Vec<_>=item.via.iter().map(|hop|json!({"relation":hop.edge.relation,"direction":hop.direction,"explicit":hop.edge.explicit,"evidence":hop.edge.evidence.iter().map(|e|e.quote.clone()).collect::<Vec<_>>()})).collect();
                    context.push(json!({"key":label,"text":item.content,"via":via}));
                }
            }
            let evidence = evidence_metrics(&retrieved, &expected, &stale);
            let input = format!(
                "Use only the provided memory. If it is insufficient, answer UNKNOWN. Give one brief sentence and cite memory keys when available.\nQuestion: {}\nMemory: {}",
                str_field(case, "query")?,
                json!(context)
            );
            requests.push(json!({"case":id,"method":method,"input":input,"input_sha256":sha(input.as_bytes()),"max_output_tokens":answer_budget}));
            observations.push(json!({"case":id,"category":case["category"],"method":method,"latency_ms":measured_ms,"query_embedding_ms":query_embedding_ms,"retrieved":retrieved,"expected":expected,"metrics":evidence,"packet":packet,"answer_input_sha256":sha(input.as_bytes())}));
        }
    }
    let answer_inputs = json!({"fingerprint":sha(serde_json::to_vec(&requests)?.as_slice()),"cases":case_ids,"methods":methods,"requests":requests});
    write_report(repo, str_field(config, "answer_inputs")?, &answer_inputs)?;
    let report = json!({"format":"memz-quality-v1","qualification_fingerprint":fingerprint,"corpus_sha256":sha(&corpus_bytes),"provider":identity,"model_load_ms":load_ms,"corpus_enrichment_ms":enrichment_ms,"embedded_sources":sources.len(),"packet_bytes":budget,"max_items":max_items,"labels":labels,"stale_revisions":stale,"observations":observations,"answer_inputs":answer_inputs,"answer_status":"unavailable","graph_default":"off","failed":["answer_comparison_unavailable"],"scope":"fixed synthetic held-out queries; retrieval measurements separated from answer-model behavior"});
    write_report(repo, str_field(config, "retrieval_cache")?, &report)?;
    Ok(report)
}
