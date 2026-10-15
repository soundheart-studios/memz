use crate::{
    Error, Result,
    auth::{Capability, Scope},
    capture::{MAX_PENDING, MAX_TEXT, Receipt, digest, id, now},
    catalog::Catalog,
    store::NamespaceStore,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
fn denied() -> Error {
    Error::new("denied", "access is not authorized")
}
fn prepare<'a>(
    cat: &'a Catalog,
    cap: &Capability,
    store: &NamespaceStore,
) -> Result<Transaction<'a>> {
    let lock = Transaction::new_unchecked(&cat.conn, TransactionBehavior::Immediate)?;
    cat.authorize(cap, &Scope::project(&store.namespace), true)?;
    if cat.state.path != store.state_path {
        return Err(denied());
    }
    Ok(lock)
}
fn target(
    cat: &Catalog,
    cap: &Capability,
    tx: &Transaction<'_>,
    namespace: &str,
    item: &str,
    expected: i64,
) -> Result<(String, i64)> {
    let row:Option<(String,i64,String,String,i64)>=tx.query_row("SELECT i.source_id,i.source_revision,i.visibility,i.session_id,i.revision FROM items i JOIN sources s ON s.id=i.source_id AND s.revision=i.source_revision WHERE i.id=?1 AND i.status='active' AND s.status='active'",[item],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let (source, revision, visibility, session, current) = row.ok_or_else(denied)?;
    let scope = if visibility == "project" {
        Scope::project(namespace)
    } else {
        Scope::session(namespace, &session)
    };
    cat.authorize(cap, &scope, true)?;
    if current != expected {
        return Err(Error::new("conflict", "expected revision is stale"));
    }
    Ok((source, revision))
}
pub fn correct(
    cat: &Catalog,
    cap: &Capability,
    store: &mut NamespaceStore,
    item: &str,
    expected: i64,
    text: &str,
) -> Result<Receipt> {
    let _lock = prepare(cat, cap, store)?;
    if text.is_empty() || text.len() > MAX_TEXT {
        return Err(Error::new(
            "too_large",
            "text must contain 1 to 65536 bytes",
        ));
    }
    let tx = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (source, source_revision) = target(cat, cap, &tx, &store.namespace, item, expected)?;
    let (bytes,quota):(i64,i64)=tx.query_row("SELECT (SELECT coalesce(sum(length(CAST(text AS BLOB))),0) FROM sources WHERE status<>'deleted'),quota_bytes FROM namespace_meta",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
    if bytes.saturating_add(text.len() as i64) > quota {
        return Err(Error::new("quota", "namespace admission limit reached"));
    }
    let pending: i64 = tx.query_row(
        "SELECT count(*) FROM jobs WHERE state IN ('pending','leased')",
        [],
        |r| r.get(0),
    )?;
    if pending >= MAX_PENDING {
        return Err(Error::new("busy", "enrichment backlog is full"));
    }
    let next = expected + 1;
    let source_next = source_revision + 1;
    let timestamp = now();
    tx.execute(
        "UPDATE sources SET status='superseded' WHERE id=?1 AND revision=?2",
        params![source, source_revision],
    )?;
    tx.execute(
        "UPDATE items SET status='superseded',valid_to=?3 WHERE id=?1 AND revision=?2",
        params![item, expected, timestamp],
    )?;
    tx.execute("INSERT INTO sources SELECT id,?3,'user',?4,external_ref,?5,?6,event_at,?7,visibility,session_id,'active',retention,expires_at FROM sources WHERE id=?1 AND revision=?2",params![source,source_revision,source_next,cap.principal(),text,digest(text.as_bytes()),timestamp])?;
    tx.execute("INSERT INTO items SELECT id,?3,kind,?4,1,'active',source_id,?5,visibility,session_id,?6,NULL,slot FROM items WHERE id=?1 AND revision=?2",params![item,expected,next,text,source_next,timestamp])?;
    tx.execute("INSERT INTO evidence(item_id,item_revision,source_id,source_revision,span_start,span_end,role,method,method_version) VALUES(?1,?2,?3,?4,0,?5,'supports','correction','1')",params![item,next,source,source_next,text.len() as i64])?;
    tx.execute(
        "INSERT INTO memory_fts(item_id,revision,content) VALUES(?1,?2,?3)",
        params![item, next, text],
    )?;
    tx.execute("INSERT INTO jobs(id,operation_key,source_id,source_revision,job_type,version,state,next_attempt,resource_bytes,principal,capability_revision) VALUES(?1,?1,?2,?3,'enrich',1,'pending',?4,?5,?6,?7)",params![id()?,source,source_next,timestamp,text.len() as i64,cap.principal(),cap.revision()])?;
    tx.execute("UPDATE namespace_meta SET mutation_seq=mutation_seq+1", [])?;
    let mutation_seq = tx.query_row("SELECT mutation_seq FROM namespace_meta", [], |r| r.get(0))?;
    tx.commit()?;
    Ok(Receipt {
        id: item.into(),
        revision: next,
        mutation_seq,
        searchable: "lexical".into(),
        enrichment: "pending".into(),
    })
}
pub fn forget(
    cat: &Catalog,
    cap: &Capability,
    store: &mut NamespaceStore,
    item: &str,
    expected: i64,
) -> Result<i64> {
    let _lock = prepare(cat, cap, store)?;
    let tx = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (source, revision) = target(cat, cap, &tx, &store.namespace, item, expected)?;
    // This one source revision is the durable deletion marker for its complete history.
    tx.execute(
        "UPDATE sources SET status='deleted' WHERE id=?1 AND revision=?2",
        params![source, revision],
    )?;
    tx.execute(
        "UPDATE items SET status='deleted',valid_to=?3 WHERE id=?1 AND revision=?2",
        params![item, expected, now()],
    )?;
    tx.execute(
        "UPDATE operations SET tombstone=1,payload_digest='',result_json='{}' WHERE item_id=?1",
        [item],
    )?;
    tx.execute("UPDATE namespace_meta SET mutation_seq=mutation_seq+1", [])?;
    let sequence = tx.query_row("SELECT mutation_seq FROM namespace_meta", [], |r| r.get(0))?;
    tx.commit()?;
    Ok(sequence)
}
/// Trusted maintenance operation: each call changes at most limit records in each category.
pub fn cleanup(store: &mut NamespaceStore, limit: usize) -> Result<usize> {
    if limit == 0 || limit > 256 {
        return Err(Error::new(
            "invalid_limit",
            "cleanup batch must be 1 to 256",
        ));
    }
    let tx = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut changed = 0;
    // Relationship metadata can contain removed source content. Scrub it in bounded batches.
    changed+=tx.execute("UPDATE edges SET status='deleted',qualifiers='{}',valid_to=coalesce(valid_to,unixepoch()) WHERE rowid IN (SELECT e.rowid FROM edges e WHERE e.status<>'deleted' AND (EXISTS(SELECT 1 FROM items i JOIN sources d ON d.id=i.source_id WHERE ((i.id=e.from_id AND i.revision=e.from_revision) OR (i.id=e.to_id AND i.revision=e.to_revision)) AND d.status='deleted') OR EXISTS(SELECT 1 FROM evidence v JOIN sources d ON d.id=v.source_id WHERE v.edge_id=e.id AND v.edge_revision=e.revision AND d.status='deleted')) LIMIT ?1)",[limit as i64])?;
    changed+=tx.execute("UPDATE edges SET status='invalid',valid_to=coalesce(valid_to,unixepoch()) WHERE rowid IN (SELECT e.rowid FROM edges e WHERE e.status='active' AND (EXISTS(SELECT 1 FROM items i JOIN sources s ON s.id=i.source_id AND s.revision=i.source_revision WHERE ((i.id=e.from_id AND i.revision=e.from_revision) OR (i.id=e.to_id AND i.revision=e.to_revision)) AND (i.status<>'active' OR s.status<>'active' OR (s.expires_at IS NOT NULL AND s.expires_at<=unixepoch()))) OR EXISTS(SELECT 1 FROM evidence v JOIN sources s ON s.id=v.source_id AND s.revision=v.source_revision WHERE v.edge_id=e.id AND v.edge_revision=e.revision AND (s.status<>'active' OR (s.expires_at IS NOT NULL AND s.expires_at<=unixepoch())))) LIMIT ?1)",[limit as i64])?;
    let doomed = "SELECT i.rowid FROM items i WHERE i.content<>'' AND (EXISTS(SELECT 1 FROM sources d WHERE d.id=i.source_id AND d.status='deleted') OR EXISTS(SELECT 1 FROM evidence e JOIN sources d ON d.id=e.source_id WHERE e.item_id=i.id AND e.item_revision=i.revision AND d.status='deleted')) LIMIT ?1";
    let mut stmt = tx.prepare(doomed)?;
    let rows = stmt
        .query_map([limit as i64], |r| r.get::<_, i64>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);
    for row in rows {
        changed+=tx.execute("DELETE FROM memory_fts WHERE (item_id,revision) IN (SELECT id,revision FROM items WHERE rowid=?1)",[row])?;
        changed+=tx.execute("UPDATE operations SET tombstone=1,payload_digest='',result_json='{}' WHERE item_id=(SELECT id FROM items WHERE rowid=?1)",[row])?;
        changed += tx.execute(
            "UPDATE items SET content='',status='deleted' WHERE rowid=?1",
            [row],
        )?;
    }
    changed+=tx.execute("UPDATE sources SET text='',content_hash='',external_ref=NULL,status='deleted' WHERE rowid IN (SELECT s.rowid FROM sources s WHERE s.text<>'' AND EXISTS(SELECT 1 FROM sources d WHERE d.id=s.id AND d.status='deleted') LIMIT ?1)",[limit as i64])?;
    changed+=tx.execute("DELETE FROM jobs WHERE rowid IN (SELECT j.rowid FROM jobs j WHERE EXISTS(SELECT 1 FROM sources d WHERE d.id=j.source_id AND d.status='deleted') LIMIT ?1)",[limit as i64])?;
    changed+=tx.execute("UPDATE jobs SET state='obsolete',lease_owner=NULL,lease_expiry=NULL WHERE rowid IN (SELECT j.rowid FROM jobs j JOIN sources s ON s.id=j.source_id AND s.revision=j.source_revision WHERE s.status='superseded' AND j.state IN ('pending','leased') LIMIT ?1)",[limit as i64])?;
    changed+=tx.execute("DELETE FROM memory_fts WHERE rowid IN (SELECT f.rowid FROM memory_fts f JOIN items i ON i.id=f.item_id AND i.revision=f.revision JOIN sources s ON s.id=i.source_id AND s.revision=i.source_revision WHERE i.status<>'active' OR s.status<>'active' LIMIT ?1)",[limit as i64])?;
    tx.commit()?;
    Ok(changed)
}
