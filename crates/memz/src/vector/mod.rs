//! Versioned derived data within the namespace database, never a repository file.
use crate::{
    Error, Result,
    jobs::Lease,
    providers::{Identity, validate_vector},
    store::NamespaceStore,
};
use rusqlite::{Transaction, params};
pub fn attach(store: &NamespaceStore) -> Result<()> {
    // Register the statically linked, pinned extension for this connection only.
    unsafe {
        let init: unsafe extern "C" fn(
            *mut rusqlite::ffi::sqlite3,
            *mut *mut std::ffi::c_char,
            *const rusqlite::ffi::sqlite3_api_routines,
        ) -> i32 = std::mem::transmute(sqlite_vec::sqlite3_vec_init as *const ());
        let rc = init(store.conn.handle(), std::ptr::null_mut(), std::ptr::null());
        if rc != rusqlite::ffi::SQLITE_OK {
            return Err(Error::new(
                "vector_unavailable",
                "sqlite-vec initialization failed",
            ));
        }
    }
    let version: String = store
        .conn
        .query_row("SELECT vec_version()", [], |r| r.get(0))?;
    if version != "v0.1.9" {
        return Err(Error::new(
            "vector_unavailable",
            "unqualified sqlite-vec version",
        ));
    }
    Ok(())
}
pub fn initialize(store: &mut NamespaceStore, identity: &Identity) -> Result<String> {
    identity.validate()?;
    attach(store)?;
    let tx = store.conn.transaction()?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS embedding_generations(id TEXT PRIMARY KEY,identity TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS memory_vectors(generation TEXT NOT NULL REFERENCES embedding_generations(id),source_id TEXT NOT NULL,source_revision INTEGER NOT NULL,embedding BLOB NOT NULL CHECK(length(embedding)=1536),PRIMARY KEY(generation,source_id,source_revision),FOREIGN KEY(source_id,source_revision) REFERENCES sources(id,revision));
 CREATE TRIGGER IF NOT EXISTS vector_source_obsolete AFTER UPDATE OF status ON sources WHEN NEW.status!='active' BEGIN DELETE FROM memory_vectors WHERE source_id=NEW.id AND (source_revision=NEW.revision OR NEW.status='deleted'); END;
 CREATE TRIGGER IF NOT EXISTS vector_source_delete BEFORE DELETE ON sources BEGIN DELETE FROM memory_vectors WHERE source_id=OLD.id AND source_revision=OLD.revision; END;")?;
    let generation = identity.generation()?;
    let generations: i64 = tx.query_row(
        "SELECT count(*) FROM embedding_generations WHERE id<>?1",
        [&generation],
        |r| r.get(0),
    )?;
    if generations >= 8 {
        return Err(Error::new(
            "generation_limit",
            "at most eight retained embedding generations",
        ));
    }
    let json =
        serde_json::to_string(identity).map_err(|e| Error::new("identity", e.to_string()))?;
    tx.execute(
        "INSERT OR IGNORE INTO embedding_generations VALUES(?1,?2)",
        params![generation, json],
    )?;
    let stored: String = tx.query_row(
        "SELECT identity FROM embedding_generations WHERE id=?1",
        [&generation],
        |r| r.get(0),
    )?;
    if stored != json {
        return Err(Error::new(
            "unknown_identity",
            "stored generation identity mismatch",
        ));
    }
    tx.commit()?;
    Ok(generation)
}
pub fn bytes(v: &[f32]) -> Result<Vec<u8>> {
    validate_vector(v)?;
    Ok(v.iter().flat_map(|x| x.to_le_bytes()).collect())
}
pub fn put(tx: &Transaction<'_>, generation: &str, lease: &Lease, v: &[f32]) -> Result<()> {
    let blob = bytes(v)?;
    let stored: String = tx.query_row(
        "SELECT identity FROM embedding_generations WHERE id=?1",
        [generation],
        |r| r.get(0),
    )?;
    let identity: Identity = serde_json::from_str(&stored)
        .map_err(|_| Error::new("unknown_identity", "stored generation identity is invalid"))?;
    if identity.generation()? != generation {
        return Err(Error::new(
            "unknown_identity",
            "stored generation identity mismatch",
        ));
    }
    let fits:bool=tx.query_row("SELECT (SELECT count(*) FROM memory_vectors)<quota_records AND (SELECT coalesce(sum(length(embedding)),0) FROM memory_vectors)+1536+(SELECT coalesce(sum(length(CAST(text AS BLOB))),0) FROM sources)<=quota_bytes FROM namespace_meta",[],|r|r.get(0))?;
    if !fits {
        return Err(Error::new("quota_exceeded", "derived vector quota reached"));
    }
    tx.execute(
        "INSERT OR IGNORE INTO memory_vectors VALUES(?1,?2,?3,?4)",
        params![generation, lease.source_id, lease.source_revision, blob],
    )?;
    Ok(())
}
pub fn count(store: &NamespaceStore, generation: &str) -> Result<i64> {
    Ok(store.conn.query_row(
        "SELECT count(*) FROM memory_vectors WHERE generation=?1",
        [generation],
        |r| r.get(0),
    )?)
}

/// Bounded, idempotent generation backfill. Completed old generations never suppress new work.
pub fn enqueue(
    cat: &crate::catalog::Catalog,
    cap: &crate::auth::Capability,
    store: &mut NamespaceStore,
    generation: &str,
) -> Result<usize> {
    let _guard =
        rusqlite::Transaction::new_unchecked(&cat.conn, rusqlite::TransactionBehavior::Immediate)?;
    cat.authorize(cap, &crate::auth::Scope::project(&store.namespace), true)?;
    if cat.state.path != store.state_path {
        return Err(Error::new("denied", "worker state mismatch"));
    }
    let tx = store.conn.transaction()?;
    let queued: i64 = tx.query_row(
        "SELECT count(*) FROM jobs WHERE state IN ('pending','leased')",
        [],
        |r| r.get(0),
    )?;
    let limit = (10000 - queued).clamp(0, 32);
    let prefix = format!(
        "embedding:{generation}:{}:{}:",
        cap.principal(),
        cap.revision()
    );
    let n=tx.execute("INSERT OR IGNORE INTO jobs(id,operation_key,source_id,source_revision,job_type,version,state,next_attempt,resource_bytes,principal,capability_revision)
 SELECT ?1||s.id||':'||s.revision,?1||s.id||':'||s.revision,s.id,s.revision,'embedding',1,'pending',unixepoch(),length(CAST(s.text AS BLOB)),?2,?3 FROM sources s WHERE s.status='active' AND (s.visibility='project' OR s.session_id=?4) AND (s.expires_at IS NULL OR s.expires_at>unixepoch()) AND NOT EXISTS(SELECT 1 FROM memory_vectors v WHERE v.generation=?5 AND v.source_id=s.id AND v.source_revision=s.revision) AND NOT EXISTS(SELECT 1 FROM jobs j WHERE j.operation_key=?1||s.id||':'||s.revision OR (j.source_id=s.id AND j.source_revision=s.revision AND j.state IN ('pending','leased'))) ORDER BY s.id LIMIT ?6",params![prefix,cap.principal(),cap.revision(),cap.session(),generation,limit])?;
    tx.commit()?;
    Ok(n)
}
pub fn enrich_one(
    cat: &crate::catalog::Catalog,
    cap: &crate::auth::Capability,
    store: &mut NamespaceStore,
    engine: &crate::providers::Engine,
    busy: bool,
) -> Result<bool> {
    if busy || engine.active() != 0 {
        return Ok(false);
    }
    let Some(identity) = engine.identity() else {
        return Ok(false);
    };
    let generation = initialize(store, &identity)?;
    enqueue(cat, cap, store, &generation)?;
    let Some(lease) = crate::jobs::claim(cat, store, crate::capture::now(), 30, busy)? else {
        return Ok(false);
    };
    match engine.embed(lease.text.clone(), false, std::time::Duration::from_secs(5)) {
        Ok(v) => {
            crate::jobs::publish(cat, store, &lease, crate::capture::now(), |tx, lease| {
                put(tx, &generation, lease, &v)
            })?;
        }
        Err(e) => crate::jobs::fail(store, &lease, crate::capture::now(), e.code)?,
    }
    Ok(true)
}
