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

mod store;
pub use store::MemzStore;

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

/// Memz source origins to AMX provenance origins. The mapping is
/// one-to-one, so an imported origin exports unchanged.
pub fn origin_to_amx(origin: &str) -> &'static str {
    match origin {
        "user" => "user",
        "assistant" => "agent",
        "import" => "import",
        "extractor" => "extractor",
        _ => "system",
    }
}

/// AMX provenance origins to Memz source origins. `extractor` has no
/// native Memz origin and is stored as is.
pub fn origin_from_amx(origin: &str) -> &'static str {
    match origin {
        "user" => "user",
        "agent" => "assistant",
        "import" => "import",
        "extractor" => "extractor",
        _ => "tool",
    }
}

/// Evidence methods Memz records for its own bookkeeping. Explicit
/// memories do not export them as provenance methods.
const INTERNAL_METHODS: &[&str] = &[
    "direct",
    "correction",
    "import",
    "user-assertion",
    "agent-proposal",
];

/// The evidence row an import adds for a record's own synthetic passage.
/// Memz needs it; the record never claimed it, so it is not exported.
const OWN_SOURCE: &str = "amx-own-source";
/// The `external_ref` marking a synthetic own-source passage: kept for Memz,
/// never exported as a source.
const OWN_SOURCE_REF: &str = "amx:own-source";

/// Reserved link qualifiers holding the AMX provenance Memz edges have no
/// columns for. They are moved back into the link on export.
const LINK_ORIGIN: &str = "amx.origin";
const LINK_AUTHOR: &str = "amx.author";
const LINK_RECORDED_AT: &str = "amx.recorded_at";

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

/// Kinds Memz stores.
pub const KINDS: &[&str] = &[
    "fact",
    "preference",
    "decision",
    "procedure",
    "task",
    "incident",
    "entity",
];

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
        "SELECT source_id,source_revision,span_start,span_end,role,method,method_version FROM evidence WHERE {column}_id=?1 AND {column}_revision=?2 AND method<>'{OWN_SOURCE}' ORDER BY id"
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
    // Inferred memories always say how they were derived; explicit ones
    // only when the method is more than Memz's own bookkeeping.
    let first = evidence
        .first()
        .filter(|e| !explicit || !INTERNAL_METHODS.contains(&e.method.as_str()));
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
            let external_ref: Option<String> = r.get(4)?;
            source_meta.insert(
                (id.clone(), revision),
                SourceMeta(
                    origin.clone(),
                    author.clone(),
                    event_at,
                    ingested_at,
                    expires_at,
                ),
            );
            if external_ref.as_deref() == Some(OWN_SOURCE_REF) {
                // An import's bookkeeping passage: the record carries its text.
                continue;
            }
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
                external_ref,
                text: r.get(5)?,
                content_hash: format!("sha256:{}", r.get::<_, String>(6)?),
                scope: scope(namespace, &visibility, &session_id),
                recorded_at: clock.see(ingested_at),
                event_at: event_at.map(|t| clock.see(t)),
                expires_at: expires_at.map(format_timestamp),
                attributes: None,
                extra: Default::default(),
            });
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
            let mut qualifiers: Object =
                serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_default();
            let reserved = |q: &mut Object, key: &str| {
                q.remove(key).and_then(|v| v.as_str().map(String::from))
            };
            let stored_origin = reserved(&mut qualifiers, LINK_ORIGIN);
            let stored_author = reserved(&mut qualifiers, LINK_AUTHOR);
            let stored_recorded_at = reserved(&mut qualifiers, LINK_RECORDED_AT);
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
                    stored_origin
                        .as_deref()
                        .unwrap_or(if explicit { "user" } else { "agent" }),
                    stored_author.as_deref().unwrap_or(&author),
                    explicit,
                    &evidence,
                ),
                evidence: (!evidence.is_empty())
                    .then(|| evidence.into_iter().map(|e| e.evidence).collect()),
                status: r.get(9)?,
                valid_from: clock.see(valid_from),
                valid_to: valid_to.map(|t| clock.see(t)),
                recorded_at: stored_recorded_at.unwrap_or_else(|| format_timestamp(valid_from)),
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
    // Deleted sources no item cites: deletions imported for passages this
    // store never held. They stay so a later import cannot bring them back.
    {
        let mut stmt = tx.prepare(&format!("SELECT s.id,s.visibility,s.session_id,s.ingested_at FROM sources s WHERE s.status='deleted' AND {VISIBLE} AND NOT EXISTS(SELECT 1 FROM items i WHERE i.source_id=s.id) ORDER BY s.id"))?;
        let mut rows = stmt.query(params![session])?;
        while let Some(r) = rows.next()? {
            let source: String = r.get(0)?;
            let visibility: String = r.get(1)?;
            let session_id: String = r.get(2)?;
            tombstones.push(Tombstone {
                amx: VERSION.into(),
                id: format!("del-{source}"),
                target_type: "source".into(),
                target_id: source,
                scope: scope(namespace, &visibility, &session_id),
                deleted_at: clock.see(r.get::<_, i64>(3)?),
                reason: "user_request".into(),
                actor: "memz".into(),
                derived: None,
                attributes: None,
                extra: Default::default(),
            });
        }
    }

    tombstones.sort_by(|a, b| a.id.cmp(&b.id));
    tombstones.dedup_by(|a, b| a.id == b.id);

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

/// What this session's view of the store holds, for import planning, and
/// the identities present in the namespace that this view cannot use
/// (other sessions' memories, expired ones). Importing onto those would
/// collide with rows the caller may not see, so plans treat them as
/// unavailable without saying why.
pub fn holdings(
    tx: &Transaction<'_>,
    namespace: &str,
    session: &str,
    now: i64,
) -> Result<(amx::Holdings, BTreeSet<String>)> {
    let view = bundle(tx, namespace, session, 0, now)?;
    let mut held = amx::Holdings::default();
    for tombstone in &view.tombstones {
        held.hold_tombstone(tombstone);
    }
    for source in &view.sources {
        held.hold_source(source);
    }
    for record in &view.records {
        held.hold_record(record);
    }
    for link in &view.links {
        held.hold_link(link);
    }
    let mut usable: BTreeSet<String> = BTreeSet::new();
    usable.extend(view.sources.iter().map(|s| s.id.clone()));
    usable.extend(view.records.iter().map(|r| r.id.clone()));
    usable.extend(view.links.iter().map(|l| l.id.clone()));
    usable.extend(view.tombstones.iter().map(|t| t.target_id.clone()));
    let present: BTreeSet<String> = tx
        .prepare("SELECT id FROM items UNION SELECT id FROM sources UNION SELECT id FROM edges")?
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    let unavailable = present.difference(&usable).cloned().collect();
    Ok((held, unavailable))
}

/// Re-home a single-namespace bundle into `target`. Multi-namespace
/// bundles must be split by the producer first.
pub fn map_namespace(mut bundle: Bundle, target: &str) -> Result<Bundle> {
    if bundle.manifest.namespaces.len() != 1 {
        return Err(Error::new(
            "invalid_bundle",
            "only single-namespace bundles can be imported",
        ));
    }
    bundle.manifest.namespaces[0].id = target.into();
    for r in &mut bundle.records {
        r.scope.namespace = target.into();
        // Plain text is the default; Memz stores it as no content type.
        if r.content_type.as_deref() == Some("text/plain") {
            r.content_type = None;
        }
    }
    for s in &mut bundle.sources {
        s.scope.namespace = target.into();
    }
    for l in &mut bundle.links {
        l.scope.namespace = target.into();
    }
    for t in &mut bundle.tombstones {
        t.scope.namespace = target.into();
    }
    Ok(bundle)
}

fn item_scope(item: &amx::Item) -> &Scope {
    match item {
        amx::Item::Record(r) => &r.scope,
        amx::Item::Source(s) => &s.scope,
        amx::Item::Link(l) => &l.scope,
    }
}

fn item_expiry(item: &amx::Item) -> Option<&String> {
    match item {
        amx::Item::Record(r) => r.expires_at.as_ref(),
        amx::Item::Source(s) => s.expires_at.as_ref(),
        amx::Item::Link(_) => None,
    }
}

/// Memz-specific limits on a plan, applied after the shared planner:
/// another session's private memory, identities this view cannot use and
/// already-expired content become conflicts, as do links left without an
/// endpoint and tombstones aimed at memories this session cannot see.
pub fn restrict(
    plan: &mut amx::ImportPlan,
    session: &str,
    unavailable: &BTreeSet<String>,
    now: i64,
) {
    let mut blocked: BTreeSet<(String, u64)> = BTreeSet::new();
    let mut conflicts = Vec::new();
    let reason = |item: &amx::Item| -> Option<String> {
        let scope = item_scope(item);
        if scope.visibility == "session" && scope.session.as_deref() != Some(session) {
            return Some("session-private to another session".into());
        }
        if unavailable.contains(item.id()) {
            return Some("identity is not available to this session".into());
        }
        if item_expiry(item)
            .and_then(|at| amx::parse_timestamp(at))
            .is_some_and(|at| at <= now)
        {
            return Some("expired content is not imported".into());
        }
        unrepresentable(item)
    };
    for list in [&mut plan.adds, &mut plan.supersedes] {
        list.retain(|item| match reason(item) {
            Some(why) => {
                if let amx::Item::Record(r) = item {
                    blocked.insert((r.id.clone(), r.revision));
                }
                conflicts.push(amx::Conflict {
                    item: item.reference(),
                    reason: why,
                });
                false
            }
            None => true,
        });
    }
    for list in [&mut plan.adds, &mut plan.supersedes] {
        list.retain(|item| {
            let amx::Item::Link(l) = item else {
                return true;
            };
            let dangling = [&l.from, &l.to]
                .iter()
                .any(|end| blocked.contains(&(end.id.clone(), end.revision)));
            if dangling {
                conflicts.push(amx::Conflict {
                    item: item.reference(),
                    reason: "an endpoint is not importable".into(),
                });
            }
            !dangling
        });
    }
    plan.tombstones.retain(|t| {
        let foreign = (t.scope.visibility == "session"
            && t.scope.session.as_deref() != Some(session))
            || unavailable.contains(&t.target_id);
        if foreign {
            conflicts.push(amx::Conflict {
                item: amx::ItemRef {
                    kind: "tombstone",
                    namespace: t.scope.namespace.clone(),
                    id: t.target_id.clone(),
                    revision: 0,
                },
                reason: "deletion targets a memory this session cannot see".into(),
            });
        }
        !foreign
    });
    plan.conflicts.extend(conflicts);
}

fn at(text: &str) -> Result<i64> {
    amx::parse_timestamp(text)
        .ok_or_else(|| Error::new("invalid_bundle", format!("bad timestamp {text:?}")))
}

fn optional_at(text: Option<&String>) -> Result<Option<i64>> {
    text.map(|t| at(t)).transpose()
}

fn visibility_of(scope: &Scope) -> (&str, String) {
    (
        scope.visibility.as_str(),
        if scope.visibility == "session" {
            scope.session.clone().unwrap_or_default()
        } else {
            String::new()
        },
    )
}

/// A stable synthetic identifier, so re-importing the same bundle derives
/// the same rows.
fn synthetic(label: &str, id: &str) -> String {
    crate::capture::digest(format!("amx:{label}:{id}").as_bytes())[..32].to_string()
}

fn source_text(tx: &Transaction<'_>, id: &str, revision: i64) -> Result<Option<String>> {
    use rusqlite::OptionalExtension;
    Ok(tx
        .query_row(
            "SELECT text FROM sources WHERE id=?1 AND revision=?2 AND status<>'deleted'",
            params![id, revision],
            |r| r.get(0),
        )
        .optional()?)
}

fn supersede_older_sources(tx: &Transaction<'_>, id: &str) -> Result<()> {
    tx.execute(
        "UPDATE sources SET status='superseded' WHERE id=?1 AND status='active' AND revision<(SELECT max(revision) FROM sources WHERE id=?1)",
        [id],
    )?;
    Ok(())
}

/// Delete an item the way `forget` does (its current source revision is the
/// durable marker), or, when the store never held it, leave a content-free
/// placeholder so the deletion survives and later imports stay refused.
fn delete_item(tx: &Transaction<'_>, id: &str, scope: &Scope, deleted_at: i64) -> Result<()> {
    use rusqlite::OptionalExtension;
    let current: Option<(i64, String, i64)> = tx
        .query_row(
            "SELECT revision,source_id,source_revision FROM items WHERE id=?1 ORDER BY revision DESC LIMIT 1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    match current {
        Some((revision, source, source_revision)) => {
            tx.execute(
                "UPDATE sources SET status='deleted' WHERE id=?1 AND revision=?2",
                params![source, source_revision],
            )?;
            tx.execute(
                "UPDATE items SET status='deleted',valid_to=coalesce(valid_to,?3) WHERE id=?1 AND revision=?2",
                params![id, revision, deleted_at],
            )?;
            tx.execute("DELETE FROM memory_fts WHERE item_id=?1", [id])?;
            tx.execute(
                "UPDATE operations SET tombstone=1,payload_digest='',result_json='{}' WHERE item_id=?1",
                [id],
            )?;
        }
        None => {
            let (visibility, session) = visibility_of(scope);
            let source = synthetic("tombstone", id);
            tx.execute("INSERT OR IGNORE INTO sources(id,revision,origin,author,text,content_hash,event_at,ingested_at,visibility,session_id,status,retention,expires_at) VALUES(?1,1,'import','memz','','',NULL,?2,?3,?4,'deleted','durable',NULL)",params![source,deleted_at,visibility,session])?;
            tx.execute("INSERT INTO items(id,revision,kind,content,explicit,status,source_id,source_revision,visibility,session_id,valid_from,valid_to,slot) VALUES(?1,1,'fact','',0,'deleted',?2,1,?3,?4,?5,?5,NULL)",params![id,source,visibility,session,deleted_at])?;
        }
    }
    Ok(())
}

fn delete_source(tx: &Transaction<'_>, id: &str, scope: &Scope, deleted_at: i64) -> Result<()> {
    let changed = tx.execute(
        "UPDATE sources SET status='deleted' WHERE id=?1 AND revision=(SELECT max(revision) FROM sources WHERE id=?1)",
        [id],
    )?;
    if changed == 0 {
        let (visibility, session) = visibility_of(scope);
        tx.execute("INSERT INTO sources(id,revision,origin,author,text,content_hash,event_at,ingested_at,visibility,session_id,status,retention,expires_at) VALUES(?1,1,'import','memz','','',NULL,?2,?3,?4,'deleted','durable',NULL)",params![id,deleted_at,visibility,session])?;
    } else {
        tx.execute(
            "DELETE FROM memory_fts WHERE item_id IN (SELECT id FROM items WHERE source_id=?1)",
            [id],
        )?;
    }
    Ok(())
}

fn insert_source(tx: &Transaction<'_>, s: &Source) -> Result<()> {
    let (visibility, session) = visibility_of(&s.scope);
    let expires_at = optional_at(s.expires_at.as_ref())?;
    let hash = s
        .content_hash
        .strip_prefix("sha256:")
        .unwrap_or(&s.content_hash);
    tx.execute("INSERT INTO sources(id,revision,origin,author,external_ref,text,content_hash,event_at,ingested_at,visibility,session_id,status,retention,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'active',?12,?13)",params![s.id,s.revision as i64,origin_from_amx(&s.origin),s.author,s.external_ref,s.text,hash,optional_at(s.event_at.as_ref())?,at(&s.recorded_at)?,visibility,session,if expires_at.is_some(){"temporary"}else{"durable"},expires_at])?;
    Ok(())
}

fn insert_evidence(
    tx: &Transaction<'_>,
    owner: (&str, &str, i64),
    evidence: &[Evidence],
    provenance: &Provenance,
) -> Result<usize> {
    let (column, id, revision) = owner;
    let method = provenance.method.as_deref().unwrap_or("import");
    let version = provenance.method_version.as_deref().unwrap_or("1");
    let mut written = 0;
    for e in evidence {
        if source_text(tx, &e.source, e.source_revision as i64)?.is_none() {
            continue;
        }
        let (start, end) = match e.span {
            Some([s, e]) => (Some(s as i64), Some(e as i64)),
            None => (None, None),
        };
        tx.execute(&format!("INSERT INTO evidence({column}_id,{column}_revision,source_id,source_revision,span_start,span_end,role,method,method_version) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)"),params![id,revision,e.source,e.source_revision as i64,start,end,e.role,method,version])?;
        written += 1;
    }
    Ok(written)
}

fn insert_record(
    tx: &Transaction<'_>,
    r: &Record,
    principal: &str,
    capability_revision: i64,
    now: i64,
) -> Result<()> {
    let (visibility, session) = visibility_of(&r.scope);
    let revision = r.revision as i64;
    let evidence = r.evidence.as_deref().unwrap_or_default();
    // Memz keeps each memory's own passage as its source, and exports the
    // record's provenance and times from it. Use the first evidence passage
    // when it is exactly that (same text, origin, author and times), so a
    // Memz export imports back unchanged; otherwise keep the content as a
    // synthetic source of its own.
    let reusable = match evidence.first() {
        Some(e) => {
            use rusqlite::OptionalExtension;
            let row: Option<(String, SourceMeta)> = tx
                .query_row(
                    "SELECT text,origin,author,event_at,ingested_at,expires_at FROM sources WHERE id=?1 AND revision=?2 AND status<>'deleted'",
                    params![e.source, e.source_revision as i64],
                    |s| {
                        Ok((
                            s.get(0)?,
                            SourceMeta(s.get(1)?, s.get(2)?, s.get(3)?, s.get(4)?, s.get(5)?),
                        ))
                    },
                )
                .optional()?;
            row.is_some_and(
                |(text, SourceMeta(origin, author, event, ingested, expires))| {
                    text == r.content
                        && origin_to_amx(&origin) == r.provenance.origin
                        && author == r.provenance.author
                        && Some(ingested) == amx::parse_timestamp(&r.recorded_at)
                        && event == r.event_at.as_deref().and_then(amx::parse_timestamp)
                        && expires == r.expires_at.as_deref().and_then(amx::parse_timestamp)
                },
            )
        }
        None => false,
    };
    let own = match evidence.first() {
        Some(e) if reusable => (e.source.clone(), e.source_revision as i64, false),
        _ => (synthetic("source", &r.id), revision, true),
    };
    if own.2 && source_text(tx, &own.0, own.1)?.is_none() {
        let expires_at = optional_at(r.expires_at.as_ref())?;
        tx.execute("INSERT INTO sources(id,revision,origin,author,text,content_hash,event_at,ingested_at,visibility,session_id,status,retention,expires_at,external_ref) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'active',?11,?12,?13)",params![own.0,own.1,origin_from_amx(&r.provenance.origin),r.provenance.author,r.content,crate::capture::digest(r.content.as_bytes()),optional_at(r.event_at.as_ref())?,at(&r.recorded_at)?,visibility,session,if expires_at.is_some(){"temporary"}else{"durable"},expires_at,OWN_SOURCE_REF])?;
        supersede_older_sources(tx, &own.0)?;
    }
    let valid_from = at(&r.valid_from)?;
    if r.status == "active" {
        tx.execute(
            "UPDATE items SET status='superseded',valid_to=coalesce(valid_to,max(valid_from,?2)) WHERE id=?1 AND status='active'",
            params![r.id, valid_from],
        )?;
    }
    tx.execute("INSERT INTO items(id,revision,kind,content,explicit,status,source_id,source_revision,visibility,session_id,valid_from,valid_to,slot) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",params![r.id,revision,r.kind,r.content,r.provenance.explicit,r.status,own.0,own.1,visibility,session,valid_from,optional_at(r.valid_to.as_ref())?,r.key])?;
    let written = insert_evidence(tx, ("item", &r.id, revision), evidence, &r.provenance)?;
    if own.2 || written == 0 {
        // Memz links every memory to its own passage. When the record did
        // not claim it, the row is bookkeeping and stays out of exports,
        // unless it is the only evidence an inferred record would have.
        let hidden = r.provenance.explicit || written > 0;
        let (method, version) = if hidden {
            (OWN_SOURCE, "1")
        } else {
            (
                r.provenance.method.as_deref().unwrap_or("import"),
                r.provenance.method_version.as_deref().unwrap_or("1"),
            )
        };
        tx.execute("INSERT INTO evidence(item_id,item_revision,source_id,source_revision,span_start,span_end,role,method,method_version) VALUES(?1,?2,?3,?4,0,?5,'supports',?6,?7)",params![r.id,revision,own.0,own.1,r.content.len() as i64,method,version])?;
    }
    if r.status == "active" {
        tx.execute(
            "INSERT INTO memory_fts(item_id,revision,content) VALUES(?1,?2,?3)",
            params![r.id, revision, r.content],
        )?;
        let job = crate::capture::id()?;
        tx.execute("INSERT INTO jobs(id,operation_key,source_id,source_revision,job_type,version,state,next_attempt,resource_bytes,principal,capability_revision) VALUES(?1,?1,?2,?3,'enrich',1,'pending',?4,?5,?6,?7)",params![job,own.0,own.1,now,r.content.len() as i64,principal,capability_revision])?;
    }
    Ok(())
}

fn insert_link(tx: &Transaction<'_>, l: &Link) -> Result<()> {
    let (visibility, session) = visibility_of(&l.scope);
    let qualifiers: BTreeMap<String, String> = l
        .qualifiers
        .iter()
        .flatten()
        .map(|(k, v)| {
            let text = v.as_str().map_or_else(|| v.to_string(), String::from);
            (k.clone(), text)
        })
        .collect();
    let mut qualifiers = qualifiers;
    // Edges have no columns for these; keep them so the link exports as it
    // was imported.
    qualifiers.insert(LINK_ORIGIN.into(), l.provenance.origin.clone());
    qualifiers.insert(LINK_AUTHOR.into(), l.provenance.author.clone());
    qualifiers.insert(LINK_RECORDED_AT.into(), l.recorded_at.clone());
    let qualifiers = serde_json::to_string(&qualifiers)
        .map_err(|_| Error::new("serialization", "qualifiers"))?;
    let valid_from = at(&l.valid_from)?;
    if l.status == "active" {
        tx.execute(
            "UPDATE edges SET status='superseded',valid_to=coalesce(valid_to,max(valid_from,?2)) WHERE id=?1 AND status='active'",
            params![l.id, valid_from],
        )?;
    }
    let revision = l.revision as i64;
    tx.execute("INSERT INTO edges(id,revision,from_id,from_revision,to_id,to_revision,relation,qualifiers,explicit,status,visibility,session_id,valid_from,valid_to) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",params![l.id,revision,l.from.id,l.from.revision as i64,l.to.id,l.to.revision as i64,l.relation,qualifiers,l.provenance.explicit,l.status,visibility,session,valid_from,optional_at(l.valid_to.as_ref())?])?;
    insert_evidence(
        tx,
        ("edge", &l.id, revision),
        l.evidence.as_deref().unwrap_or_default(),
        &l.provenance,
    )?;
    Ok(())
}

/// Apply a plan inside the caller's write transaction: deletions first,
/// then sources, records and links. Returns whether anything was written.
pub fn apply(
    tx: &Transaction<'_>,
    plan: &amx::ImportPlan,
    principal: &str,
    capability_revision: i64,
    now: i64,
) -> Result<bool> {
    let (quota_bytes, quota_records): (i64, i64) = tx.query_row(
        "SELECT quota_bytes,quota_records FROM namespace_meta WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let (bytes, count): (i64, i64) = tx.query_row(
        "SELECT coalesce(sum(length(CAST(text AS BLOB))),0),count(*) FROM sources WHERE status<>'deleted'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let incoming: i64 = plan
        .adds
        .iter()
        .chain(&plan.supersedes)
        .map(|item| match item {
            amx::Item::Source(s) => s.text.len() as i64,
            amx::Item::Record(r) => r.content.len() as i64,
            amx::Item::Link(_) => 0,
        })
        .sum();
    let new_rows = (plan.adds.len() + plan.supersedes.len()) as i64;
    if bytes.saturating_add(incoming) > quota_bytes
        || count.saturating_add(new_rows) > quota_records
    {
        return Err(Error::new("quota", "namespace admission limit reached"));
    }

    for t in &plan.tombstones {
        let deleted_at = at(&t.deleted_at)?;
        match t.target_type.as_str() {
            "record" => delete_item(tx, &t.target_id, &t.scope, deleted_at)?,
            "source" => delete_source(tx, &t.target_id, &t.scope, deleted_at)?,
            _ => {
                tx.execute(
                    "UPDATE edges SET status='deleted',valid_to=coalesce(valid_to,?2) WHERE id=?1",
                    params![t.target_id, deleted_at],
                )?;
            }
        }
        for derived in t.derived.iter().flatten() {
            let held: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM items WHERE id=?1)",
                [derived],
                |r| r.get(0),
            )?;
            if held {
                delete_item(tx, derived, &t.scope, deleted_at)?;
            }
            tx.execute(
                "UPDATE edges SET status='deleted',valid_to=coalesce(valid_to,?2) WHERE id=?1",
                params![derived, deleted_at],
            )?;
        }
    }
    let items: Vec<&amx::Item> = plan.adds.iter().chain(&plan.supersedes).collect();
    let mut touched = BTreeSet::new();
    for item in &items {
        if let amx::Item::Source(s) = item {
            insert_source(tx, s)?;
            touched.insert(s.id.clone());
        }
    }
    for id in &touched {
        supersede_older_sources(tx, id)?;
    }
    let mut records: Vec<&Record> = items
        .iter()
        .filter_map(|i| match i {
            amx::Item::Record(r) => Some(r),
            _ => None,
        })
        .collect();
    records.sort_by(|a, b| (&a.id, a.revision).cmp(&(&b.id, b.revision)));
    for r in records {
        insert_record(tx, r, principal, capability_revision, now)?;
    }
    for item in &items {
        if let amx::Item::Link(l) = item {
            insert_link(tx, l)?;
        }
    }
    let wrote = !plan.tombstones.is_empty() || !items.is_empty();
    if wrote {
        tx.execute(
            "UPDATE namespace_meta SET mutation_seq=mutation_seq+1 WHERE singleton=1",
            [],
        )?;
    }
    Ok(wrote)
}

/// Why Memz cannot hold an item without losing part of it, if it cannot.
/// AMX readers must re-export what they import; Memz refuses what it could
/// not give back rather than dropping it silently.
fn unrepresentable(item: &amx::Item) -> Option<String> {
    let extras = |attributes: &Option<Object>, extra: &Object| {
        attributes.as_ref().is_some_and(|a| !a.is_empty()) || !extra.is_empty()
    };
    match item {
        amx::Item::Record(r) => {
            if !KINDS.contains(&r.kind.as_str()) {
                Some(format!("Memz cannot store kind {:?}", r.kind))
            } else if r.content_type.is_some() {
                Some("Memz stores plain text only".into())
            } else if r.supersedes.is_some() {
                Some("Memz cannot store supersedes on a record".into())
            } else if extras(&r.attributes, &r.extra)
                || !r.provenance.extra.is_empty()
                || !r.scope.extra.is_empty()
                || r.evidence.iter().flatten().any(|e| !e.extra.is_empty())
            {
                Some("Memz cannot preserve attributes or unknown fields".into())
            } else {
                None
            }
        }
        amx::Item::Source(s) => extras(&s.attributes, &s.extra)
            .then(|| "Memz cannot preserve attributes or unknown fields".into()),
        amx::Item::Link(l) => {
            if !crate::graph::RELATIONS.contains(&l.relation.as_str()) {
                Some(format!("Memz cannot store relation {:?}", l.relation))
            } else if l
                .qualifiers
                .iter()
                .flatten()
                .any(|(k, v)| !v.is_string() || k.starts_with("amx."))
            {
                Some("Memz stores string qualifiers only, outside the amx. prefix".into())
            } else if extras(&l.attributes, &l.extra) || !l.provenance.extra.is_empty() {
                Some("Memz cannot preserve attributes or unknown fields".into())
            } else {
                None
            }
        }
    }
}
