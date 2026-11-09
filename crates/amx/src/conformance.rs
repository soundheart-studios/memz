//! The conformance runner (SPEC section 11).
//!
//! A vector is a JSON file: `{"name", "level", "description", "steps"}`.
//! Each step names an `op` (`remember`, `correct`, `recall`, `forget`,
//! `changes`, `export`, `import`) and an `expect` object. When `expect` has
//! an `error`, the step must fail with that code; otherwise it must succeed
//! and every other key in `expect` must equal the step's summary.
//!
//! Strings of the form `"$name"` are variables: `as` binds the id a write
//! produced (or the cursor `changes` returned) and later steps use it.
//! Bundles in `import` steps are written compactly; the runner fills the
//! store's namespace, `amx`, default provenance, timestamps and source
//! hashes, then encodes and decodes the bundle so it is valid before use.
use crate::{
    Bundle, Correction, Draft, Error, Generator, ImportPlan, NamespaceCursor, Result, Store,
    sha256_hex,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{collections::BTreeMap, path::Path};

const OPS: &[&str] = &[
    "remember", "correct", "recall", "forget", "changes", "export", "import",
];
const TIME: &str = "2026-01-01T00:00:00Z";

/// One conformance vector.
#[derive(Clone, Debug, Deserialize)]
pub struct Vector {
    pub name: String,
    pub level: String,
    #[serde(default)]
    pub description: String,
    pub steps: Vec<Value>,
}

/// Load every `*.json` vector in `dir`, sorted by file name. A malformed
/// vector is an error, never skipped.
pub fn load_vectors(dir: &Path) -> Result<Vec<Vector>> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| Error::new("io", format!("{}: {e}", dir.display())))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    let mut vectors = Vec::new();
    for path in paths {
        let label = path.display().to_string();
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Error::new("io", format!("{label}: {e}")))?;
        let vector: Vector = serde_json::from_str(&text)
            .map_err(|e| Error::new("invalid", format!("{label}: {e}")))?;
        if !["L0", "L1", "L2"].contains(&vector.level.as_str()) {
            return Err(Error::new("invalid", format!("{label}: unknown level")));
        }
        if vector.steps.is_empty() {
            return Err(Error::new("invalid", format!("{label}: no steps")));
        }
        for (index, step) in vector.steps.iter().enumerate() {
            let op = step["op"].as_str().unwrap_or_default();
            if !OPS.contains(&op) || !step["expect"].is_object() {
                return Err(Error::new(
                    "invalid",
                    format!(
                        "{label}: step {} needs a known op and an expect object",
                        index + 1
                    ),
                ));
            }
        }
        vectors.push(vector);
    }
    if vectors.is_empty() {
        return Err(Error::new("invalid", "no vectors found"));
    }
    Ok(vectors)
}

/// Whether a store claiming `level` must pass `vector`.
pub fn applies(vector: &Vector, level: &str) -> bool {
    vector.level.as_str() <= level
}

/// Run one vector against a fresh store. The error names the failing step.
pub fn run_vector(store: &mut dyn Store, vector: &Vector) -> std::result::Result<(), String> {
    let mut vars = BTreeMap::new();
    for (index, raw) in vector.steps.iter().enumerate() {
        let label = format!("step {} ({})", index + 1, raw["op"].as_str().unwrap_or("?"));
        let step = substitute(raw, &vars).map_err(|e| format!("{label}: {e}"))?;
        let outcome = execute(store, &step).map_err(|e| format!("{label}: vector error: {e}"))?;
        let expect = step["expect"].as_object().cloned().unwrap_or_default();
        match (outcome, expect.get("error")) {
            (Err(error), Some(code)) => {
                if Some(error.code) != code.as_str() {
                    return Err(format!("{label}: expected error {code}, got {error}"));
                }
            }
            (Err(error), None) => return Err(format!("{label}: unexpected error {error}")),
            (Ok(summary), Some(code)) => {
                return Err(format!(
                    "{label}: expected error {code}, but it succeeded with {summary}"
                ));
            }
            (Ok(summary), None) => {
                for (key, want) in &expect {
                    if key == "absent" {
                        let text = summary["text"].as_str().unwrap_or_default();
                        for needle in want.as_array().into_iter().flatten() {
                            let needle = needle.as_str().unwrap_or_default();
                            if !needle.is_empty() && text.contains(needle) {
                                return Err(format!("{label}: {needle:?} must not appear"));
                            }
                        }
                        continue;
                    }
                    if let Some(base) = key.strip_suffix("_include") {
                        let have = summary[base].as_array().cloned().unwrap_or_default();
                        for item in want.as_array().into_iter().flatten() {
                            if !have.contains(item) {
                                return Err(format!(
                                    "{label}: {base} should include {item}, got {have:?}"
                                ));
                            }
                        }
                        continue;
                    }
                    let got = summary.get(key).unwrap_or(&Value::Null);
                    if got != want {
                        return Err(format!(
                            "{label}: {key} expected {want}, got {got} (summary {summary})"
                        ));
                    }
                }
                if let Some(name) = step["as"].as_str() {
                    let bound = summary
                        .get("id")
                        .or_else(|| summary.get("cursor"))
                        .cloned()
                        .ok_or_else(|| format!("{label}: nothing to bind to {name}"))?;
                    vars.insert(name.to_string(), bound);
                }
            }
        }
    }
    Ok(())
}

fn substitute(value: &Value, vars: &BTreeMap<String, Value>) -> std::result::Result<Value, String> {
    Ok(match value {
        Value::String(s) if s.starts_with('$') => vars
            .get(&s[1..])
            .cloned()
            .ok_or_else(|| format!("unbound variable {s}"))?,
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| substitute(v, vars))
                .collect::<std::result::Result<_, _>>()?,
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| Ok((k.clone(), substitute(v, vars)?)))
                .collect::<std::result::Result<_, String>>()?,
        ),
        other => other.clone(),
    })
}

fn field<T: serde::de::DeserializeOwned>(
    step: &Value,
    name: &str,
) -> std::result::Result<T, String> {
    serde_json::from_value(step[name].clone()).map_err(|e| format!("{name}: {e}"))
}

fn sorted(mut items: Vec<String>) -> Value {
    items.sort();
    json!(items)
}

type Outcome = Result<Value>;

/// Run one step. The outer error is a broken vector; the inner result is
/// what the store did.
fn execute(store: &mut dyn Store, step: &Value) -> std::result::Result<Outcome, String> {
    let op = step["op"].as_str().unwrap_or_default();
    Ok(match op {
        "remember" => {
            let draft: Draft = field(step, "draft")?;
            store
                .remember(draft)
                .map(|w| json!({"id": w.id, "revision": w.revision}))
        }
        "correct" => {
            let mut body = step.clone();
            body.as_object_mut().map(|o| o.remove("expect"));
            let correction: Correction =
                serde_json::from_value(body).map_err(|e| format!("correction: {e}"))?;
            store
                .correct(correction)
                .map(|w| json!({"id": w.id, "revision": w.revision}))
        }
        "recall" => {
            let query = step["query"].as_str().unwrap_or_default().to_string();
            let session = step["session"].as_str().map(String::from);
            let limit = step["limit"].as_u64().unwrap_or(100) as usize;
            store.recall(&query, session.as_deref(), limit).map(|found| {
                json!({"contents": sorted(found.into_iter().map(|r| r.content).collect())})
            })
        }
        "forget" => {
            let id = step["id"].as_str().ok_or("forget needs an id")?;
            let reason = step["reason"].as_str().unwrap_or("user_request");
            store
                .forget(id, reason)
                .map(|t| json!({"target_id": t.target_id}))
        }
        "changes" => {
            let since = step["since"].as_u64().unwrap_or(0);
            store.changes(since).map(|c| {
                json!({
                    "cursor": c.cursor,
                    "records": c.records.len(),
                    "tombstones": c.tombstones.len(),
                    "tombstones_for": sorted(c.tombstones.iter().map(|t| t.target_id.clone()).collect()),
                    "contents": sorted(c.records.iter().map(|r| r.content.clone()).collect()),
                    "text": changes_text(&c),
                })
            })
        }
        "export" => store.export().and_then(|bundle| {
            let files = bundle.encode()?;
            let text: String = files.values().map(|v| String::from_utf8_lossy(v).into_owned()).collect();
            let decoded = Bundle::decode(
                &files
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect(),
            )?;
            Ok(json!({
                "records": decoded.records.len(),
                "sources": decoded.sources.len(),
                "links": decoded.links.len(),
                "tombstones": decoded.tombstones.len(),
                "active_contents": sorted(decoded.records.iter().filter(|r| r.status == "active").map(|r| r.content.clone()).collect()),
                "tombstones_for": sorted(decoded.tombstones.iter().map(|t| t.target_id.clone()).collect()),
                "text": text,
            }))
        }),
        "import" => {
            let bundle = build_bundle(store.namespace(), &step["bundle"])?;
            let apply = step["apply"].as_bool().unwrap_or(false);
            store.plan_import(&bundle).and_then(|plan| {
                let summary = |plan: &ImportPlan, applied: bool| {
                    json!({
                        "tombstones": plan.tombstones.len(),
                        "adds": plan.adds.len(),
                        "supersedes": plan.supersedes.len(),
                        "noops": plan.noops.len(),
                        "refused": plan.refused.len(),
                        "conflicts": plan.conflicts.len(),
                        "applied": applied,
                    })
                };
                if apply {
                    let applied = store.apply_import(&bundle, &plan)?;
                    Ok(summary(&applied, true))
                } else {
                    Ok(summary(&plan, false))
                }
            })
        }
        other => return Err(format!("unknown op {other}")),
    })
}

fn fill(object: &mut Map<String, Value>, defaults: Value) {
    if let Value::Object(defaults) = defaults {
        for (key, value) in defaults {
            object.entry(key).or_insert(value);
        }
    }
}

fn build_bundle(namespace: &str, spec: &Value) -> std::result::Result<Bundle, String> {
    let provenance = json!({"origin": "user", "author": "vector", "explicit": true});
    let items = |name: &str, defaults: &dyn Fn(&mut Map<String, Value>)| {
        spec[name]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|mut item| {
                let object = item
                    .as_object_mut()
                    .ok_or_else(|| format!("{name} entries are objects"))?;
                object.entry("amx").or_insert(json!("0.1"));
                let scope = object
                    .entry("scope")
                    .or_insert(json!({"visibility": "project"}));
                scope["namespace"] = json!(namespace);
                defaults(object);
                Ok(item)
            })
            .collect::<std::result::Result<Vec<Value>, String>>()
    };
    let records = items("records", &|o| {
        fill(
            o,
            json!({"revision": 1, "kind": "fact", "provenance": provenance, "status": "active",
                "valid_from": TIME, "recorded_at": TIME}),
        )
    })?;
    let sources = items("sources", &|o| {
        let text = o.get("text").and_then(Value::as_str).unwrap_or_default();
        let hash = format!("sha256:{}", sha256_hex(text.as_bytes()));
        fill(
            o,
            json!({"revision": 1, "origin": "user", "author": "vector", "content_hash": hash,
                "recorded_at": TIME}),
        )
    })?;
    let links = items("links", &|o| {
        fill(
            o,
            json!({"revision": 1, "relation": "supports", "provenance": provenance,
                "status": "active", "valid_from": TIME, "recorded_at": TIME}),
        )
    })?;
    let tombstones = items("tombstones", &|o| {
        let target = o.get("target_id").and_then(Value::as_str).unwrap_or("x");
        let id = format!("t-{target}");
        fill(
            o,
            json!({"id": id, "target_type": "record", "deleted_at": TIME,
                "reason": "user_request", "actor": "vector"}),
        )
    })?;
    let mut bundle = Bundle::new(
        "conformance",
        Generator {
            name: "amx-conformance".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        TIME,
        vec![NamespaceCursor {
            id: namespace.into(),
            cursor: 0,
        }],
    );
    bundle.records = parse(records)?;
    bundle.sources = parse(sources)?;
    bundle.links = parse(links)?;
    bundle.tombstones = parse(tombstones)?;
    let files = bundle.encode().map_err(|e| format!("bundle: {e}"))?;
    Bundle::decode(&files.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
        .map_err(|e| format!("bundle: {e}"))
}

fn changes_text(changes: &crate::Changes) -> String {
    [
        crate::write_jsonl(&changes.tombstones),
        crate::write_jsonl(&changes.sources),
        crate::write_jsonl(&changes.records),
        crate::write_jsonl(&changes.links),
    ]
    .into_iter()
    .map(|r| r.unwrap_or_default())
    .collect()
}

fn parse<T: serde::de::DeserializeOwned>(
    values: Vec<Value>,
) -> std::result::Result<Vec<T>, String> {
    values
        .into_iter()
        .map(|v| serde_json::from_value(v).map_err(|e| e.to_string()))
        .collect()
}
