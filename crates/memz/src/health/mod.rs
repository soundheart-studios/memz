use crate::{
    Error, Result,
    auth::{Capability, Scope},
    capture::now,
    catalog::Catalog,
    lifecycle,
    store::NamespaceStore,
};
use serde::{Deserialize, Serialize};
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Health {
    pub mutation_seq: i64,
    pub visible_active: i64,
    pub pending_jobs: i64,
    pub failed_jobs: i64,
    pub quota_bytes: i64,
    pub quota_records: i64,
    pub wal_bytes: u64,
    pub semantic: String,
    pub checkpoint: String,
}
pub fn status(cat: &Catalog, cap: &Capability, store: &NamespaceStore) -> Result<Health> {
    let _lock =
        rusqlite::Transaction::new_unchecked(&cat.conn, rusqlite::TransactionBehavior::Immediate)?;
    cat.authorize(cap, &Scope::project(&store.namespace), false)?;
    if cat.state.path != store.state_path {
        return Err(Error::new("denied", "access is not authorized"));
    }
    let tx = store.conn.unchecked_transaction()?;
    let (mutation_seq, quota_bytes, quota_records) = tx.query_row(
        "SELECT mutation_seq,quota_bytes,quota_records FROM namespace_meta",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let visible_active=tx.query_row("SELECT count(*) FROM items i JOIN sources s ON s.id=i.source_id AND s.revision=i.source_revision WHERE i.status='active' AND s.status='active' AND (i.visibility='project' OR i.session_id=?1) AND (s.expires_at IS NULL OR s.expires_at>unixepoch())",[cap.session()],|r|r.get(0))?;
    let count = |states: &str| -> Result<i64> {
        Ok(tx.query_row(&format!("SELECT count(*) FROM jobs j JOIN sources s ON s.id=j.source_id AND s.revision=j.source_revision WHERE j.state IN ({states}) AND (s.visibility='project' OR s.session_id=?1)"),[cap.session()],|r|r.get(0))?)
    };
    let pending_jobs = count("'pending','leased'")?;
    let failed_jobs = count("'failed'")?;
    let wal = store
        .state_path
        .join("namespaces")
        .join(&store.namespace)
        .join("memory.sqlite-wal");
    let wal_bytes = std::fs::metadata(wal).map(|m| m.len()).unwrap_or(0);
    Ok(Health {
        mutation_seq,
        visible_active,
        pending_jobs,
        failed_jobs,
        quota_bytes,
        quota_records,
        wal_bytes,
        semantic: "unavailable".into(),
        checkpoint: if wal_bytes > 4194304 {
            "pressure"
        } else {
            "normal"
        }
        .into(),
    })
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Maintenance {
    pub expired: usize,
    pub cleaned: usize,
    pub checkpoint_busy: i64,
    pub wal_frames: i64,
    pub checkpointed_frames: i64,
}
/// Explicit maintenance, never invoked by foreground capture or ranking.
pub fn maintain(
    cat: &Catalog,
    cap: &Capability,
    store: &mut NamespaceStore,
    limit: usize,
) -> Result<Maintenance> {
    if limit == 0 || limit > 256 {
        return Err(Error::new(
            "invalid_limit",
            "maintenance batch must be 1 to 256",
        ));
    }
    let lock =
        rusqlite::Transaction::new_unchecked(&cat.conn, rusqlite::TransactionBehavior::Immediate)?;
    cat.authorize(cap, &Scope::project(&store.namespace), true)?;
    if cat.state.path != store.state_path {
        return Err(Error::new("denied", "access is not authorized"));
    }
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut stmt=tx.prepare("SELECT id,revision FROM sources WHERE status='active' AND retention='temporary' AND expires_at<=?1 AND (visibility='project' OR session_id=?2) LIMIT ?3")?;
    let rows = stmt
        .query_map(rusqlite::params![now(), cap.session(), limit as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);
    for (id, revision) in &rows {
        tx.execute(
            "UPDATE sources SET status='deleted' WHERE id=?1 AND revision=?2",
            rusqlite::params![id, revision],
        )?;
        tx.execute("UPDATE operations SET tombstone=1,payload_digest='',result_json='{}' WHERE item_id IN (SELECT id FROM items WHERE source_id=?1 AND source_revision=?2)",rusqlite::params![id,revision])?;
    }
    if !rows.is_empty() {
        tx.execute("UPDATE namespace_meta SET mutation_seq=mutation_seq+1", [])?;
    }
    tx.commit()?;
    drop(lock);
    let cleaned = lifecycle::cleanup(store, limit)?;
    let (checkpoint_busy, wal_frames, checkpointed_frames) =
        store
            .conn
            .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
    Ok(Maintenance {
        expired: rows.len(),
        cleaned,
        checkpoint_busy,
        wal_frames,
        checkpointed_frames,
    })
}
