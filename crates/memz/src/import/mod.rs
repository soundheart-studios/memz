//! Validation only. Fresh-instance restoration is a separate recovery milestone.
use crate::{
    Error, Result,
    auth::Capability,
    capture::digest,
    catalog::Catalog,
    export::{parse_manifest, snapshot},
    store::NamespaceStore,
};
use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;
pub fn validate(
    cat: &Catalog,
    cap: &Capability,
    store: &mut NamespaceStore,
    text: &str,
    max_bytes: usize,
) -> Result<Value> {
    if text.len() > max_bytes || max_bytes > 8388608 {
        return Err(Error::new(
            "invalid_manifest",
            "input exceeds validation limit",
        ));
    }
    let header = parse_manifest(text)?;
    let records = text
        .split_once('\n')
        .ok_or_else(|| Error::new("invalid_manifest", "missing record separator"))?
        .1;
    if header["format"] != "memz-jsonl-v1"
        || header["namespace"] != store.namespace
        || header["principal"] != cap.principal()
        || header["session"] != cap.session()
        || header["sha256"] != digest(records.as_bytes())
    {
        return Err(Error::new(
            "invalid_manifest",
            "snapshot identity or digest mismatch",
        ));
    }
    let current = snapshot(cat, cap, store, max_bytes, &AtomicBool::new(false))?;
    if current.jsonl != text {
        return Err(Error::new(
            "stale_snapshot",
            "edited, stale or deleted records cannot be replayed",
        ));
    }
    Ok(
        json!({"validated":true,"mode":"validation_only","revision":current.revision,"records":header["records"]}),
    )
}
