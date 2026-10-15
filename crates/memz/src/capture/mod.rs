use crate::{
    Error, Result,
    auth::{Capability, Scope, Visibility},
    catalog::Catalog,
    store::NamespaceStore,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
pub const MAX_TEXT: usize = 65536;
pub const MAX_PENDING: i64 = 10000;
#[derive(Clone, Debug)]
pub struct Capture {
    pub scope: Scope,
    pub request_id: String,
    pub text: String,
    pub kind: String,
    pub origin: String,
    pub slot: Option<String>,
    pub event_at: Option<i64>,
    pub expires_at: Option<i64>,
}
impl Capture {
    pub fn text(scope: Scope, request_id: &str, text: &str) -> Self {
        Self {
            scope,
            request_id: request_id.into(),
            text: text.into(),
            kind: "fact".into(),
            origin: "user".into(),
            slot: None,
            event_at: None,
            expires_at: None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub id: String,
    pub revision: i64,
    pub mutation_seq: i64,
    pub searchable: String,
    pub enrichment: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    BeforeCommit,
    AfterCommit,
}
pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) fn id() -> Result<String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
pub fn remember(
    cat: &Catalog,
    cap: &Capability,
    store: &mut NamespaceStore,
    req: &Capture,
) -> Result<Receipt> {
    remember_observed(cat, cap, store, req, |_| {})
}
/// Commit observation for embedding diagnostics and fault testing; no provider is invoked.
pub fn remember_observed(
    cat: &Catalog,
    cap: &Capability,
    store: &mut NamespaceStore,
    req: &Capture,
    mut observe: impl FnMut(Stage),
) -> Result<Receipt> {
    // Serialize capability changes through commit, including across processes.
    let _authorization_lock =
        rusqlite::Transaction::new_unchecked(&cat.conn, TransactionBehavior::Immediate)?;
    cat.authorize(cap, &req.scope, true)?;
    if store.namespace != req.scope.namespace || store.state_path != cat.state.path {
        return Err(Error::new("denied", "access is not authorized"));
    }
    if req.text.is_empty() || req.text.len() > MAX_TEXT {
        return Err(Error::new(
            "too_large",
            "text must contain 1 to 65536 bytes",
        ));
    }
    if req.request_id.is_empty()
        || req.request_id.len() > 256
        || req.slot.as_ref().is_some_and(|s| s.len() > 256)
        || ![
            "fact",
            "preference",
            "decision",
            "procedure",
            "task",
            "incident",
            "entity",
        ]
        .contains(&req.kind.as_str())
        || !["user", "assistant", "tool", "import"].contains(&req.origin.as_str())
        || req.expires_at.is_some_and(|t| t <= now())
    {
        return Err(Error::new("invalid_request", "invalid capture metadata"));
    }
    let (visibility, session) = match &req.scope.visibility {
        Visibility::Project => ("project", ""),
        Visibility::Session(s) => ("session", s.as_str()),
    };
    let payload = serde_json::to_vec(&(
        req.scope.namespace.as_str(),
        visibility,
        session,
        &req.text,
        &req.kind,
        &req.origin,
        &req.slot,
        req.event_at,
        req.expires_at,
    ))
    .map_err(|_| Error::new("serialization", "invalid payload"))?;
    let hash = digest(&payload);
    let tx = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let prior:Option<(String,String,bool)>=tx.query_row("SELECT payload_digest,result_json,tombstone FROM operations WHERE principal=?1 AND session_id=?2 AND request_id=?3",params![cap.principal(),cap.session(),req.request_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    if let Some((previous, result, deleted)) = prior {
        if deleted {
            return Err(Error::new("deleted", "request refers to deleted content"));
        }
        if previous != hash {
            return Err(Error::new(
                "conflict",
                "request ID already has a different payload",
            ));
        }
        return serde_json::from_str(&result)
            .map_err(|_| Error::new("storage", "invalid operation receipt"));
    }
    let (quota_bytes, quota_records): (i64, i64) = tx.query_row(
        "SELECT quota_bytes,quota_records FROM namespace_meta WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let (bytes,count):(i64,i64)=tx.query_row("SELECT coalesce(sum(length(CAST(text AS BLOB))),0),count(*) FROM sources WHERE status<>'deleted'",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
    if count >= quota_records || bytes.saturating_add(req.text.len() as i64) > quota_bytes {
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
    let item = id()?;
    let source = id()?;
    let timestamp = now();
    let text_hash = digest(req.text.as_bytes());
    tx.execute("INSERT INTO sources(id,revision,origin,author,text,content_hash,event_at,ingested_at,visibility,session_id,status,retention,expires_at) VALUES(?1,1,?2,?3,?4,?5,?6,?7,?8,?9,'active',?10,?11)",params![source,req.origin,cap.principal(),req.text,text_hash,req.event_at,timestamp,visibility,session,if req.expires_at.is_some(){"temporary"}else{"durable"},req.expires_at])?;
    tx.execute("INSERT INTO items(id,revision,kind,content,explicit,status,source_id,source_revision,visibility,session_id,valid_from,slot) VALUES(?1,1,?2,?3,?4,'active',?5,1,?6,?7,?8,?9)",params![item,req.kind,req.text,req.origin=="user",source,visibility,session,timestamp,req.slot])?;
    tx.execute("INSERT INTO evidence(item_id,item_revision,source_id,source_revision,span_start,span_end,role,method,method_version) VALUES(?1,1,?2,1,0,?3,'supports','direct','1')",params![item,source,req.text.len() as i64])?;
    tx.execute(
        "INSERT INTO memory_fts(item_id,revision,content) VALUES(?1,1,?2)",
        params![item, req.text],
    )?;
    tx.execute("INSERT INTO jobs(id,operation_key,source_id,source_revision,job_type,version,state,next_attempt,resource_bytes,principal,capability_revision) VALUES(?1,?1,?2,1,'enrich',1,'pending',?3,?4,?5,?6)",params![id()?,source,timestamp,req.text.len() as i64,cap.principal(),cap.revision()])?;
    tx.execute(
        "UPDATE namespace_meta SET mutation_seq=mutation_seq+1 WHERE singleton=1",
        [],
    )?;
    let mutation_seq = tx.query_row("SELECT mutation_seq FROM namespace_meta", [], |r| r.get(0))?;
    let result = Receipt {
        id: item.clone(),
        revision: 1,
        mutation_seq,
        searchable: "lexical".into(),
        enrichment: "pending".into(),
    };
    let receipt = serde_json::to_string(&result)
        .map_err(|_| Error::new("serialization", "invalid receipt"))?;
    tx.execute("INSERT INTO operations(principal,session_id,request_id,payload_digest,result_json,item_id,mutation_seq,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![cap.principal(),cap.session(),req.request_id,hash,receipt,item,mutation_seq,timestamp])?;
    observe(Stage::BeforeCommit);
    tx.commit()?;
    observe(Stage::AfterCommit);
    Ok(result)
}
