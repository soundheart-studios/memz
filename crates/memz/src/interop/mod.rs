//! AMX (Agent Memory eXchange) mapping: Memz tables to AMX records.
//!
//! | Memz | AMX |
//! |---|---|
//! | `items` row | record (`slot` is `key`, `explicit` is `provenance.explicit`) |
//! | `sources` row | source (`ingested_at` is `recorded_at`) |
//! | `evidence` row | evidence entry (`method` fills provenance) |
//! | `edges` row | link |
//! | deleted source marker | tombstones for the record and its source |
//! | `namespace_meta.mutation_seq` | manifest cursor |
//!
//! Only what the caller's session may see is exported: project records and
//! that session's own. Deleted and expired content never leaves as text.
use crate::{Error, Result};
use amx::{
    Bundle, Evidence, Generator, Link, NamespaceCursor, Object, Provenance, Record, Ref, Scope,
    Source, Tombstone, VERSION, format_timestamp,
};
use rusqlite::{Transaction, params};
use std::collections::{BTreeMap, BTreeSet};

/// The scope AMX readers see for a Memz row.
pub fn scope(namespace: &str, visibility: &str, session: &str) -> Scope {
    Scope {
        namespace: namespace.into(),
        visibility: visibility.into(),
        session: (visibility == "session").then(|| session.to_string()),
        repo: None,
        branch: None,
        user: None,
        extra: Default::default(),
    }
}

/// Memz source origins to AMX provenance origins.
pub fn origin_to_amx(origin: &str) -> &'static str {
    match origin {
        "user" => "user",
        "assistant" => "agent",
        "import" => "import",
        _ => "system",
    }
}

/// AMX provenance origins to Memz source origins.
pub fn origin_from_amx(origin: &str) -> &'static str {
    match origin {
        "user" => "user",
        "agent" | "extractor" => "assistant",
        "import" => "import",
        _ => "tool",
    }
}

fn role(role: &str) -> String {
    match role {
        "supports" | "contradicts" | "context" => role.into(),
        _ => "context".into(),
    }
}

fn invalid(error: amx::Error) -> Error {
    Error::new("invalid_export", error.to_string())
}

struct Clock(i64);
impl Clock {
    fn see(&mut self, at: i64) -> String {
        self.0 = self.0.max(at);
        format_timestamp(at)
    }
}

const VISIBLE: &str = "(visibility='project' OR (visibility='session' AND session_id=?1))";

/// What a record needs from its own source row: origin, author, event,
/// ingestion and expiry times.
#[derive(Clone)]
struct SourceMeta(String, String, Option<i64>, i64, Option<i64>);

struct EvidenceRow {
    evidence: Evidence,
    method: String,
    method_version: String,
}

fn evidence_rows(
    tx: &Transaction<'_>,
    column: &str,
    id: &str,
    revision: i64,
) -> Result<Vec<EvidenceRow>> {
    let sql = format!(
        "SELECT source_id,source_revision,span_start,span_end,role,method,method_version FROM evidence WHERE {column}_id=?1 AND {column}_revision=?2 ORDER BY id"
    );
    let mut stmt = tx.prepare(&sql)?;
    let rows = stmt
        .query_map(params![id, revision], |r| {
            let start: Option<i64> = r.get(2)?;
            let end: Option<i64> = r.get(3)?;
            Ok(EvidenceRow {
                evidence: Evidence {
                    source: r.get(0)?,
                    source_revision: r.get::<_, i64>(1)? as u64,
                    span: start
                        .zip(end)
                        .map(|(s, e)| [s.max(0) as u64, e.max(0) as u64]),
                    role: role(&r.get::<_, String>(4)?),
                    extra: Default::default(),
                },
                method: r.get(5)?,
                method_version: r.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn provenance(origin: &str, author: &str, explicit: bool, evidence: &[EvidenceRow]) -> Provenance {
    let first = evidence.first();
    Provenance {
        origin: origin.into(),
        author: if author.is_empty() {
            "memz".into()
        } else {
            author.into()
        },
        explicit,
        method: first.map(|e| e.method.clone()),
        method_version: first.map(|e| e.method_version.clone()),
        extra: Default::default(),
    }
}

/// Build the AMX bundle a session may export, inside the caller's read
/// transaction so it matches the rest of the snapshot.
pub fn bundle(
    tx: &Transaction<'_>,
    namespace: &str,
    session: &str,
    cursor: i64,
    now: i64,
) -> Result<Bundle> {
    let mut clock = Clock(0);
    let dead: BTreeSet<String> = tx
        .prepare("SELECT DISTINCT id FROM sources WHERE status='deleted'")?
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;

    // Sources: live passages this session may see.
    let mut sources = Vec::new();
    let mut source_meta: BTreeMap<(String, i64), SourceMeta> = BTreeMap::new();
    {
        let mut stmt = tx.prepare(&format!("SELECT id,revision,origin,author,external_ref,text,content_hash,event_at,ingested_at,visibility,session_id,expires_at FROM sources WHERE status IN ('active','superseded') AND {VISIBLE} AND (expires_at IS NULL OR expires_at>?2) ORDER BY id,revision"))?;
        let mut rows = stmt.query(params![session, now])?;
        while let Some(r) = rows.next()? {
            let id: String = r.get(0)?;
            if dead.contains(&id) {
                continue;
            }
            let revision: i64 = r.get(1)?;
            let origin: String = r.get(2)?;
            let author: String = r.get(3)?;
            let event_at: Option<i64> = r.get(7)?;
            let ingested_at: i64 = r.get(8)?;
            let expires_at: Option<i64> = r.get(11)?;
            let visibility: String = r.get(9)?;
            let session_id: String = r.get(10)?;
            sources.push(Source {
                amx: VERSION.into(),
                id: id.clone(),
                revision: revision as u64,
                origin: origin_to_amx(&origin).into(),
                author: if author.is_empty() {
                    "memz".into()
                } else {
                    author.clone()
                },
                external_ref: r.get(4)?,
                text: r.get(5)?,
                content_hash: format!("sha256:{}", r.get::<_, String>(6)?),
                scope: scope(namespace, &visibility, &session_id),
                recorded_at: clock.see(ingested_at),
                event_at: event_at.map(|t| clock.see(t)),
                expires_at: expires_at.map(format_timestamp),
                attributes: None,
                extra: Default::default(),
            });
            source_meta.insert(
                (id, revision),
                SourceMeta(origin, author, event_at, ingested_at, expires_at),
            );
        }
    }
    let live_source = |id: &str| !dead.contains(id);

    // Records: every non-deleted revision whose own source and evidence live.
    let mut records = Vec::new();
    let mut exported: BTreeSet<(String, i64)> = BTreeSet::new();
    {
        let mut stmt = tx.prepare(&format!("SELECT id,revision,kind,content,explicit,status,source_id,source_revision,visibility,session_id,valid_from,valid_to,slot FROM items WHERE status IN ('active','superseded','invalid') AND {VISIBLE} ORDER BY id,revision"))?;
        let mut rows = stmt.query(params![session])?;
        while let Some(r) = rows.next()? {
            let id: String = r.get(0)?;
            let revision: i64 = r.get(1)?;
            let source_id: String = r.get(6)?;
            let source_revision: i64 = r.get(7)?;
            let Some(SourceMeta(origin, author, event_at, ingested_at, expires_at)) = source_meta
                .get(&(source_id.clone(), source_revision))
                .cloned()
            else {
                continue;
            };
            let evidence = evidence_rows(tx, "item", &id, revision)?;
            if evidence.iter().any(|e| !live_source(&e.evidence.source)) {
                continue;
            }
            let explicit: bool = r.get(4)?;
            let visibility: String = r.get(8)?;
            let session_id: String = r.get(9)?;
            let valid_from: i64 = r.get(10)?;
            let valid_to: Option<i64> = r.get(11)?;
            records.push(Record {
                amx: VERSION.into(),
                id: id.clone(),
                revision: revision as u64,
                kind: r.get(2)?,
                content: r.get(3)?,
                content_type: None,
                key: r.get(12)?,
                scope: scope(namespace, &visibility, &session_id),
                provenance: provenance(origin_to_amx(&origin), &author, explicit, &evidence),
                evidence: (!evidence.is_empty())
                    .then(|| evidence.into_iter().map(|e| e.evidence).collect()),
                status: r.get(5)?,
                supersedes: None,
                valid_from: clock.see(valid_from),
                valid_to: valid_to.map(|t| clock.see(t)),
                recorded_at: clock.see(ingested_at),
                event_at: event_at.map(format_timestamp),
                expires_at: expires_at.map(format_timestamp),
                attributes: None,
                extra: Default::default(),
            });
            exported.insert((id, revision));
        }
    }

    // Links between exported revisions.
    let mut links = Vec::new();
    {
        let mut stmt = tx.prepare(&format!("SELECT id,revision,from_id,from_revision,to_id,to_revision,relation,qualifiers,explicit,status,visibility,session_id,valid_from,valid_to FROM edges WHERE status IN ('active','superseded','invalid') AND {VISIBLE} ORDER BY id,revision"))?;
        let mut rows = stmt.query(params![session])?;
        while let Some(r) = rows.next()? {
            let id: String = r.get(0)?;
            let revision: i64 = r.get(1)?;
            let from = (r.get::<_, String>(2)?, r.get::<_, i64>(3)?);
            let to = (r.get::<_, String>(4)?, r.get::<_, i64>(5)?);
            if !exported.contains(&from) || !exported.contains(&to) {
                continue;
            }
            let evidence = evidence_rows(tx, "edge", &id, revision)?;
            if evidence.iter().any(|e| !live_source(&e.evidence.source)) {
                continue;
            }
            let explicit: bool = r.get(8)?;
            let author = evidence
                .first()
                .and_then(|e| {
                    source_meta
                        .get(&(e.evidence.source.clone(), e.evidence.source_revision as i64))
                        .map(|m| m.1.clone())
                })
                .unwrap_or_default();
            let qualifiers: Object =
                serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_default();
            let visibility: String = r.get(10)?;
            let session_id: String = r.get(11)?;
            let valid_from: i64 = r.get(12)?;
            let valid_to: Option<i64> = r.get(13)?;
            links.push(Link {
                amx: VERSION.into(),
                id,
                revision: revision as u64,
                from: Ref {
                    id: from.0,
                    revision: from.1 as u64,
                },
                to: Ref {
                    id: to.0,
                    revision: to.1 as u64,
                },
                relation: r.get(6)?,
                qualifiers: (!qualifiers.is_empty()).then_some(qualifiers),
                scope: scope(namespace, &visibility, &session_id),
                provenance: provenance(
                    if explicit { "user" } else { "agent" },
                    &author,
                    explicit,
                    &evidence,
                ),
                evidence: (!evidence.is_empty())
                    .then(|| evidence.into_iter().map(|e| e.evidence).collect()),
                status: r.get(9)?,
                valid_from: clock.see(valid_from),
                valid_to: valid_to.map(|t| clock.see(t)),
                recorded_at: format_timestamp(valid_from),
                attributes: None,
                extra: Default::default(),
            });
        }
    }

    // Tombstones: each forgotten memory and its source, content-free.
    let mut tombstones = Vec::new();
    {
        let mut stmt = tx.prepare(&format!("SELECT id,source_id,visibility,session_id,max(coalesce(valid_to,valid_from)) FROM items WHERE source_id IN (SELECT id FROM sources WHERE status='deleted') AND {VISIBLE} GROUP BY id,source_id,visibility,session_id ORDER BY id"))?;
        let mut rows = stmt.query(params![session])?;
        let mut edge_stmt =
            tx.prepare("SELECT DISTINCT id FROM edges WHERE from_id=?1 OR to_id=?1 ORDER BY id")?;
        while let Some(r) = rows.next()? {
            let item: String = r.get(0)?;
            let source: String = r.get(1)?;
            let visibility: String = r.get(2)?;
            let session_id: String = r.get(3)?;
            let deleted_at = clock.see(r.get::<_, i64>(4)?);
            let derived: Vec<String> = edge_stmt
                .query_map([&item], |e| e.get(0))?
                .collect::<std::result::Result<_, _>>()?;
            let base = Tombstone {
                amx: VERSION.into(),
                id: format!("del-{item}"),
                target_type: "record".into(),
                target_id: item,
                scope: scope(namespace, &visibility, &session_id),
                deleted_at,
                reason: "user_request".into(),
                actor: "memz".into(),
                derived: (!derived.is_empty()).then_some(derived),
                attributes: None,
                extra: Default::default(),
            };
            tombstones.push(Tombstone {
                id: format!("del-{source}"),
                target_type: "source".into(),
                target_id: source,
                derived: None,
                ..base.clone()
            });
            tombstones.push(base);
        }
    }

    let mut bundle = Bundle::new(
        &format!("memz-{cursor}"),
        Generator {
            name: "memz".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        &format_timestamp(clock.0),
        vec![NamespaceCursor {
            id: namespace.into(),
            cursor: cursor.max(0) as u64,
        }],
    );
    bundle.records = records;
    bundle.sources = sources;
    bundle.links = links;
    bundle.tombstones = tombstones;
    // Encoding validates every record; a Memz row that cannot be expressed
    // in AMX fails the export rather than shipping an invalid bundle.
    bundle.encode().map_err(invalid)?;
    Ok(bundle)
}

/// Encode a bundle into file contents keyed by file name.
pub fn encode(bundle: &Bundle) -> Result<BTreeMap<String, Vec<u8>>> {
    Ok(bundle
        .encode()
        .map_err(invalid)?
        .into_iter()
        .map(|(name, bytes)| (name.to_string(), bytes))
        .collect())
}
