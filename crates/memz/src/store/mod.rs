use crate::{
    Error, Result,
    state::{StateRoot, open_db},
};
pub struct NamespaceStore {
    pub conn: rusqlite::Connection,
    pub(crate) namespace: String,
    pub(crate) state_path: std::path::PathBuf,
}
impl NamespaceStore {
    pub fn open(state: &StateRoot, id: &str) -> Result<Self> {
        let mut conn = open_db(&state.namespace_db(id)?)?;
        migrate(&mut conn)?;
        Ok(Self {
            conn,
            namespace: id.into(),
            state_path: state.path.clone(),
        })
    }
}
pub fn migrate(conn: &mut rusqlite::Connection) -> Result<()> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version > 1 {
        return Err(Error::new(
            "unsupported_schema",
            "namespace schema is newer than this binary",
        ));
    }
    if version == 0 {
        let transaction =
            conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(include_str!(
            "../../migrations/20261014T225100_namespace.sql"
        ))?;
        transaction.pragma_update(None, "user_version", 1)?;
        transaction.commit()?;
    }
    // Existing schemas are checked, never silently recreated during open.
    for query in [
        "SELECT mutation_seq,quota_bytes,quota_records FROM namespace_meta WHERE singleton=1",
        "SELECT id,revision,origin,author,text,content_hash,visibility,session_id,status,retention,expires_at FROM sources LIMIT 0",
        "SELECT id,revision,kind,content,explicit,status,source_id,source_revision,visibility,session_id,valid_from,valid_to,slot FROM items LIMIT 0",
        "SELECT id,from_id,from_revision,to_id,to_revision,relation,qualifiers,status FROM edges LIMIT 0",
        "SELECT item_id,item_revision,edge_id,edge_revision,source_id,source_revision,span_start,span_end,method FROM evidence LIMIT 0",
        "SELECT id,operation_key,source_revision,state,attempts,lease_owner,lease_expiry,next_attempt,principal,capability_revision FROM jobs LIMIT 0",
        "SELECT principal,session_id,request_id,payload_digest,result_json,item_id,tombstone,mutation_seq FROM operations LIMIT 0",
        "SELECT item_id,revision,content FROM memory_fts LIMIT 0",
    ] {
        conn.prepare(query).map_err(|_| {
            Error::new(
                "damaged_schema",
                "required namespace schema is missing or incompatible",
            )
        })?;
    }
    let singleton: i64 = conn.query_row(
        "SELECT count(*) FROM namespace_meta WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    if singleton != 1 {
        return Err(Error::new(
            "damaged_schema",
            "namespace metadata is missing",
        ));
    }
    Ok(())
}
