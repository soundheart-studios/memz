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

/// Folders under the state directory where bundles may be read from.
pub const BUNDLE_FOLDERS: [&str; 2] = ["exports", "imports"];

/// Find `<name>.amx` in the state's exports or imports folder. Names are
/// plain (letters, digits, `-`, `_`), so a caller cannot reach elsewhere.
pub fn bundle_path(cat: &Catalog, name: &str) -> Result<std::path::PathBuf> {
    if name.is_empty()
        || name.len() > 80
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(Error::new(
            "invalid_filename",
            "bundle name must be letters, digits, hyphens or underscores",
        ));
    }
    BUNDLE_FOLDERS
        .iter()
        .map(|folder| cat.state.path.join(folder).join(format!("{name}.amx")))
        .find(|path| std::fs::symlink_metadata(path).is_ok())
        .ok_or_else(|| Error::new("not_found", "no bundle with that name"))
}

/// Read and verify a named bundle.
pub fn read_bundle(cat: &Catalog, name: &str) -> Result<amx::Bundle> {
    amx::Bundle::read(&bundle_path(cat, name)?)
        .map_err(|e| Error::new("invalid_bundle", e.to_string()))
}

/// Plan importing `bundle` into `store` as this capability sees it. The
/// bundle is re-homed into the store's namespace; the shared AMX planner
/// decides, then Memz limits apply. Nothing is written.
pub fn plan_bundle(
    cat: &Catalog,
    cap: &Capability,
    store: &mut NamespaceStore,
    bundle: amx::Bundle,
) -> Result<(amx::Bundle, amx::ImportPlan)> {
    let _lock =
        rusqlite::Transaction::new_unchecked(&cat.conn, rusqlite::TransactionBehavior::Immediate)?;
    cat.authorize(cap, &crate::auth::Scope::project(&store.namespace), false)?;
    if store.state_path != cat.state.path {
        return Err(Error::new("denied", "access is not authorized"));
    }
    let bundle = crate::interop::map_namespace(bundle, &store.namespace)?;
    let tx = store.conn.transaction()?;
    let now = crate::capture::now();
    let (held, unavailable) = crate::interop::holdings(&tx, &store.namespace, cap.session(), now)?;
    drop(tx);
    let support = amx::Support::new(&["project", "session"], &[]);
    let mut plan = amx::plan_import(&bundle, &held, &support);
    crate::interop::restrict(&mut plan, cap.session(), &unavailable, now);
    Ok((bundle, plan))
}

/// A plan as a JSON report: counts, and each conflict with its reason.
pub fn report(namespace: &str, plan: &amx::ImportPlan, mode: &str) -> Value {
    let conflicts: Vec<Value> = plan
        .conflicts
        .iter()
        .map(|c| json!({"kind":c.item.kind,"id":c.item.id,"revision":c.item.revision,"reason":c.reason}))
        .collect();
    json!({
        "validated": true,
        "mode": mode,
        "namespace": namespace,
        "tombstones": plan.tombstones.len(),
        "adds": plan.adds.len(),
        "supersedes": plan.supersedes.len(),
        "noops": plan.noops.len(),
        "refused": plan.refused.len(),
        "conflicts": conflicts,
        "clean": plan.is_clean(),
    })
}

/// Dry run: verify a named `.amx` bundle and report what importing it
/// would do. Writes nothing.
pub fn validate_bundle(
    cat: &Catalog,
    cap: &Capability,
    store: &mut NamespaceStore,
    name: &str,
) -> Result<Value> {
    let bundle = read_bundle(cat, name)?;
    let (_, plan) = plan_bundle(cat, cap, store, bundle)?;
    Ok(report(&store.namespace, &plan, "dry_run"))
}
