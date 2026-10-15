//! Attributable relationships in the existing namespace store. Citations prove provenance, not truth.
mod traversal;
use crate::{
    Error, Result,
    auth::{Capability, Scope},
    capture::{digest, now},
    catalog::Catalog,
    providers::relations::LiteralExtractor,
    store::NamespaceStore,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub use traversal::{GraphOptions, GraphReport, Hop, Impact, ImpactEdge};
pub(crate) use traversal::{expand, impact};
pub const RELATIONS: &[&str] = &[
    "supports",
    "supersedes",
    "contradicts",
    "applies_to",
    "requires",
    "mitigates",
];
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub id: String,
    pub revision: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    pub source: String,
    pub revision: i64,
    pub start: usize,
    pub end: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Relation {
    pub from: Node,
    pub to: Node,
    pub relation: String,
    #[serde(default)]
    pub qualifiers: BTreeMap<String, String>,
    pub evidence: Vec<Anchor>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Evidence {
    pub source: String,
    pub revision: i64,
    pub start: usize,
    pub end: usize,
    pub quote: String,
    pub origin: String,
    pub author: String,
    pub method: String,
    pub method_version: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Edge {
    pub id: String,
    pub revision: i64,
    pub from: Node,
    pub to: Node,
    pub relation: String,
    pub qualifiers: BTreeMap<String, String>,
    pub explicit: bool,
    pub namespace: String,
    pub visibility: String,
    pub session: String,
    pub evidence: Vec<Evidence>,
    pub attribution_only: bool,
}
fn denied() -> Error {
    Error::new(
        "denied",
        "relationship inputs are unavailable or unauthorized",
    )
}
pub(crate) fn authorize<'a>(
    cat: &'a Catalog,
    cap: &Capability,
    store: &NamespaceStore,
    write: bool,
) -> Result<Transaction<'a>> {
    let lock = Transaction::new_unchecked(&cat.conn, TransactionBehavior::Immediate)?;
    cat.authorize(cap, &Scope::project(&store.namespace), write)?;
    if cat.state.path != store.state_path {
        return Err(denied());
    }
    Ok(lock)
}
pub(crate) struct Resolved {
    pub scope: Scope,
}
pub(crate) fn item(
    tx: &Transaction<'_>,
    session: &str,
    namespace: &str,
    node: &Node,
) -> Result<Resolved> {
    let row:Option<(String,String,String,i64)>=tx.query_row("SELECT i.visibility,i.session_id,i.source_id,i.source_revision FROM items i JOIN sources s ON s.id=i.source_id AND s.revision=i.source_revision WHERE i.id=?1 AND i.revision=?2 AND i.status='active' AND s.status='active' AND i.valid_from<=?4 AND (i.valid_to IS NULL OR i.valid_to>?4) AND (s.expires_at IS NULL OR s.expires_at>?4) AND i.visibility=s.visibility AND i.session_id=s.session_id AND (i.visibility='project' OR i.session_id=?3) AND NOT EXISTS(SELECT 1 FROM sources d WHERE d.id=s.id AND d.status='deleted') AND NOT EXISTS(SELECT 1 FROM evidence e JOIN sources es ON es.id=e.source_id AND es.revision=e.source_revision WHERE e.item_id=i.id AND e.item_revision=i.revision AND (es.status<>'active' OR es.visibility<>i.visibility OR es.session_id<>i.session_id OR (es.expires_at IS NOT NULL AND es.expires_at<=?4) OR EXISTS(SELECT 1 FROM sources d WHERE d.id=es.id AND d.status='deleted'))) AND (i.slot IS NULL OR i.explicit=0 OR NOT EXISTS(SELECT 1 FROM items n JOIN sources ns ON ns.id=n.source_id AND ns.revision=n.source_revision WHERE n.slot=i.slot AND n.explicit=1 AND n.status='active' AND ns.status='active' AND n.visibility=i.visibility AND n.session_id=i.session_id AND n.rowid>i.rowid AND (ns.expires_at IS NULL OR ns.expires_at>?4)))",params![node.id,node.revision,session,now()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let (visibility, private, _source, _source_revision) = row.ok_or_else(denied)?;
    Ok(Resolved {
        scope: if visibility == "project" {
            Scope::project(namespace)
        } else {
            Scope::session(namespace, &private)
        },
    })
}
fn evidence(
    tx: &Transaction<'_>,
    namespace: &str,
    session: &str,
    a: &Anchor,
    method: &str,
    version: &str,
) -> Result<(Scope, Evidence)> {
    let row:Option<(String,String,String,String,String)>=tx.query_row("SELECT visibility,session_id,text,origin,author FROM sources s WHERE id=?1 AND revision=?2 AND status='active' AND (visibility='project' OR session_id=?3) AND (expires_at IS NULL OR expires_at>?4) AND NOT EXISTS(SELECT 1 FROM sources d WHERE d.id=s.id AND d.status='deleted')",params![a.source,a.revision,session,now()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let (visibility, private, text, origin, author) = row.ok_or_else(denied)?;
    if a.start >= a.end || a.end.saturating_sub(a.start) > 2048 {
        return Err(Error::new(
            "invalid_evidence",
            "evidence span must contain 1 to 2048 UTF-8 bytes",
        ));
    }
    let quote = text
        .get(a.start..a.end)
        .ok_or_else(|| {
            Error::new(
                "invalid_evidence",
                "evidence span is not an exact UTF-8 range",
            )
        })?
        .to_owned();
    let scope = if visibility == "project" {
        Scope::project(namespace)
    } else {
        Scope::session(namespace, &private)
    };
    Ok((
        scope,
        Evidence {
            source: a.source.clone(),
            revision: a.revision,
            start: a.start,
            end: a.end,
            quote,
            origin,
            author,
            method: method.into(),
            method_version: version.into(),
        },
    ))
}
fn validate(req: &Relation) -> Result<()> {
    if !RELATIONS.contains(&req.relation.as_str())
        || req.evidence.is_empty()
        || req.evidence.len() > 4
        || req.from == req.to
        || req.from.id.len() > 128
        || req.to.id.len() > 128
        || req.from.revision < 1
        || req.to.revision < 1
        || req.qualifiers.len() > 8
        || req
            .qualifiers
            .iter()
            .any(|(k, v)| k.is_empty() || k.len() > 64 || v.len() > 256)
        || req
            .evidence
            .iter()
            .any(|a| a.source.len() > 128 || a.revision < 1)
    {
        return Err(Error::new(
            "invalid_relation",
            "relation contract exceeds allowed bounds",
        ));
    }
    Ok(())
}
pub fn propose(
    cat: &Catalog,
    cap: &Capability,
    store: &mut NamespaceStore,
    req: &Relation,
) -> Result<Edge> {
    write(cat, cap, store, req, false)
}
/// Trusted human assertion API. Never exposed as a model-callable tool.
pub fn assert_explicit(
    cat: &Catalog,
    cap: &Capability,
    store: &mut NamespaceStore,
    req: &Relation,
) -> Result<Edge> {
    write(cat, cap, store, req, true)
}
fn write(
    cat: &Catalog,
    cap: &Capability,
    store: &mut NamespaceStore,
    req: &Relation,
    explicit: bool,
) -> Result<Edge> {
    let _lock = authorize(cat, cap, store, true)?;
    let tx = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let edge = put(
        &tx,
        cap,
        &store.namespace,
        req,
        explicit,
        if explicit {
            "user-assertion"
        } else {
            "agent-proposal"
        },
    )?;
    tx.commit()?;
    Ok(edge)
}
pub(crate) fn put(
    tx: &Transaction<'_>,
    cap: &Capability,
    namespace: &str,
    req: &Relation,
    explicit: bool,
    method: &str,
) -> Result<Edge> {
    validate(req)?;
    let from = item(tx, cap.session(), namespace, &req.from)?;
    let to = item(tx, cap.session(), namespace, &req.to)?;
    Scope::derive(&[from.scope.clone(), to.scope], &from.scope)?;
    let mut proofs = Vec::new();
    for a in &req.evidence {
        let (scope, proof) = evidence(tx, namespace, cap.session(), a, method, "1")?;
        Scope::derive(&[scope], &from.scope)?;
        if explicit && (proof.origin != "user" || proof.author != cap.principal()) {
            return Err(denied());
        }
        proofs.push(proof);
    }
    let (visibility, session) = match &from.scope.visibility {
        crate::auth::Visibility::Project => ("project", ""),
        crate::auth::Visibility::Session(s) => ("session", s.as_str()),
    };
    let id = digest(
        serde_json::json!([
            namespace,
            visibility,
            session,
            cap.principal(),
            req,
            explicit,
            method,
            "1"
        ])
        .to_string()
        .as_bytes(),
    );
    if tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM edges WHERE id=?1)",
        [&id],
        |r| r.get::<_, bool>(0),
    )? {
        return read_edge(tx, cap.session(), namespace, &id)?.ok_or_else(denied);
    }
    let fits:bool=tx.query_row("SELECT (SELECT count(*) FROM edges)<min(quota_records,100000) AND (SELECT coalesce(sum(length(qualifiers)),0) FROM edges)+?1<quota_bytes FROM namespace_meta",[serde_json::to_string(&req.qualifiers).unwrap().len() as i64],|r|r.get(0))?;
    if !fits {
        return Err(Error::new("quota", "relationship admission limit reached"));
    }
    tx.execute("INSERT INTO edges(id,revision,from_id,from_revision,to_id,to_revision,relation,qualifiers,explicit,status,visibility,session_id,valid_from,valid_to) VALUES(?1,1,?2,?3,?4,?5,?6,?7,?8,'active',?9,?10,?11,NULL)",params![id,req.from.id,req.from.revision,req.to.id,req.to.revision,req.relation,serde_json::to_string(&req.qualifiers).unwrap(),explicit,visibility,session,now()])?;
    for a in &req.evidence {
        tx.execute("INSERT INTO evidence(edge_id,edge_revision,source_id,source_revision,span_start,span_end,role,method,method_version) VALUES(?1,1,?2,?3,?4,?5,'supports',?6,'1')",params![id,a.source,a.revision,a.start as i64,a.end as i64,method])?;
    }
    tx.execute("UPDATE namespace_meta SET mutation_seq=mutation_seq+1", [])?;
    Ok(Edge {
        id,
        revision: 1,
        from: req.from.clone(),
        to: req.to.clone(),
        relation: req.relation.clone(),
        qualifiers: req.qualifiers.clone(),
        explicit,
        namespace: namespace.into(),
        visibility: visibility.into(),
        session: session.into(),
        evidence: proofs,
        attribution_only: true,
    })
}
pub(crate) fn read_edge(
    tx: &Transaction<'_>,
    session: &str,
    namespace: &str,
    id: &str,
) -> Result<Option<Edge>> {
    let row=tx.query_row("SELECT id,revision,from_id,from_revision,to_id,to_revision,relation,qualifiers,explicit,visibility,session_id FROM edges WHERE id=?1 AND status='active' AND valid_from<=?3 AND (valid_to IS NULL OR valid_to>?3) AND (visibility='project' OR session_id=?2)",params![id,session,now()],|r|Ok(Edge{id:r.get(0)?,revision:r.get(1)?,from:Node{id:r.get(2)?,revision:r.get(3)?},to:Node{id:r.get(4)?,revision:r.get(5)?},relation:r.get(6)?,qualifiers:serde_json::from_str(&r.get::<_,String>(7)?).map_err(|e|rusqlite::Error::FromSqlConversionFailure(7,rusqlite::types::Type::Text,Box::new(e)))?,explicit:r.get(8)?,namespace:namespace.into(),visibility:r.get(9)?,session:r.get(10)?,evidence:vec![],attribution_only:true})).optional()?;
    let Some(mut edge) = row else { return Ok(None) };
    let check = (|| -> Result<()> {
        let from = item(tx, session, namespace, &edge.from)?;
        let to = item(tx, session, namespace, &edge.to)?;
        let scope = if edge.visibility == "project" {
            Scope::project(namespace)
        } else {
            Scope::session(namespace, &edge.session)
        };
        Scope::derive(&[from.scope, to.scope], &scope)?;
        let mut stmt=tx.prepare("SELECT source_id,source_revision,span_start,span_end,method,method_version FROM evidence WHERE edge_id=?1 AND edge_revision=?2 ORDER BY id LIMIT 5")?;
        let rows = stmt
            .query_map(params![edge.id, edge.revision], |r| {
                Ok((
                    Anchor {
                        source: r.get(0)?,
                        revision: r.get(1)?,
                        start: r.get(2)?,
                        end: r.get(3)?,
                    },
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if rows.is_empty() || rows.len() > 4 {
            return Err(denied());
        }
        for (anchor, method, version) in rows {
            let (s, proof) = evidence(tx, namespace, session, &anchor, &method, &version)?;
            Scope::derive(&[s], &scope)?;
            edge.evidence.push(proof);
        }
        Ok(())
    })();
    match check {
        Ok(()) => Ok(Some(edge)),
        Err(e) if ["denied", "invalid_evidence"].contains(&e.code) => Ok(None),
        Err(e) => Err(e),
    }
}
pub(crate) fn adjacent(
    tx: &Transaction<'_>,
    session: &str,
    node: &Node,
    limit: usize,
) -> Result<Vec<String>> {
    let mut s=tx.prepare("SELECT id FROM edges WHERE status='active' AND (visibility='project' OR session_id=?3) AND ((from_id=?1 AND from_revision=?2) OR (to_id=?1 AND to_revision=?2)) ORDER BY explicit DESC,id LIMIT ?4")?;
    Ok(s.query_map(
        params![node.id, node.revision, session, limit as i64],
        |r| r.get(0),
    )?
    .collect::<std::result::Result<Vec<_>, _>>()?)
}
pub fn neighbors(
    cat: &Catalog,
    cap: &Capability,
    store: &mut NamespaceStore,
    node: &Node,
) -> Result<Vec<Edge>> {
    let _lock = authorize(cat, cap, store, false)?;
    let tx = store.conn.transaction()?;
    item(&tx, cap.session(), &store.namespace, node)?;
    adjacent(&tx, cap.session(), node, 128)?
        .iter()
        .filter_map(|id| read_edge(&tx, cap.session(), &store.namespace, id).transpose())
        .collect()
}

pub fn enqueue_extraction(
    cat: &Catalog,
    cap: &Capability,
    store: &mut NamespaceStore,
    provider: Option<&LiteralExtractor>,
) -> Result<usize> {
    let Some(provider) = provider else {
        return Ok(0);
    };
    let _lock = authorize(cat, cap, store, true)?;
    let tx = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let pending: i64 = tx.query_row(
        "SELECT count(*) FROM jobs WHERE state IN ('pending','leased')",
        [],
        |r| r.get(0),
    )?;
    let limit = (10000 - pending).clamp(0, 32) as usize;
    let prefix = format!("literal-v1:{}:{}:", cap.principal(), cap.revision());
    let mut stmt=tx.prepare("SELECT s.id,s.revision,s.origin,i.kind,s.text FROM sources s JOIN items i ON i.source_id=s.id AND i.source_revision=s.revision WHERE s.status='active' AND i.status='active' AND s.origin='user' AND i.kind IN ('decision','procedure','task','incident') AND (s.visibility='project' OR s.session_id=?1) AND length(CAST(s.text AS BLOB))<=8192 AND instr(s.text,'[[')>0 AND (s.expires_at IS NULL OR s.expires_at>unixepoch()) AND NOT EXISTS(SELECT 1 FROM jobs j WHERE j.operation_key=?2||s.id||':'||s.revision) ORDER BY s.id LIMIT 32")?;
    let inputs = stmt
        .query_map(params![cap.session(), prefix], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);
    let mut count = 0;
    for (id, rev, origin, kind, text) in inputs.into_iter().take(limit) {
        if !provider.selection(&origin, &kind, &text).selected {
            continue;
        }
        let key = format!("{prefix}{id}:{rev}");
        count+=tx.execute("INSERT OR IGNORE INTO jobs(id,operation_key,source_id,source_revision,job_type,version,state,next_attempt,resource_bytes,principal,capability_revision) VALUES(?1,?1,?2,?3,'relations',1,'pending',unixepoch(),?4,?5,?6)",params![key,id,rev,text.len() as i64,cap.principal(),cap.revision()])?;
    }
    tx.commit()?;
    Ok(count)
}
pub fn extract_one(
    cat: &Catalog,
    cap: &Capability,
    store: &mut NamespaceStore,
    provider: &LiteralExtractor,
    busy: bool,
) -> Result<bool> {
    if busy {
        return Ok(false);
    }
    enqueue_extraction(cat, cap, store, Some(provider))?;
    let Some(lease) = crate::jobs::claim_relations(cat, store, now(), 30, busy, cap)? else {
        return Ok(false);
    };
    let input = Anchor {
        source: lease.source_id.clone(),
        revision: lease.source_revision,
        start: 0,
        end: lease.text.len(),
    };
    let extracted = provider.extract(&input, &lease.text);
    let namespace = store.namespace.clone();
    let outcome = (|| -> Result<()> {
        let relations = extracted?;
        let session = if lease.session.is_empty() {
            cap.session()
        } else {
            &lease.session
        };
        // Only this scheduling principal's capability can publish the inferred output.
        if lease.principal != cap.principal() || lease.capability_revision != cap.revision() {
            return Err(denied());
        }
        let publisher = cat.capability(&lease.principal, session)?;
        crate::jobs::publish(cat, store, &lease, now(), |tx, _| {
            for relation in &relations {
                put(tx, &publisher, &namespace, relation, false, "literal-v1")?;
            }
            Ok(())
        })?;
        Ok(())
    })();
    if let Err(e) = outcome {
        crate::jobs::fail(store, &lease, now(), e.code)?;
    }
    Ok(true)
}
