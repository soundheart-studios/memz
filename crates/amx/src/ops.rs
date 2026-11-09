//! The AMX operations contract (SPEC section 10) and an in-memory reference
//! store that implements it.
//!
//! A [`Store`] is bound to one namespace. Every write that names an
//! `expected_revision` fails with `conflict` when the identity has moved on;
//! request ids make writes idempotent; deletions are permanent; imports go
//! through [`crate::plan_import`] so every store applies the same rules.
use crate::{
    Bundle, Error, Evidence, Generator, Holdings, ImportPlan, Item, Link, NamespaceCursor,
    Provenance, Record, Result, Scope, Source, Support, Tombstone, VERSION, format_timestamp,
    parse_timestamp, plan_import, sha256_hex,
};
use serde::Deserialize;
use std::collections::BTreeMap;

/// What a caller asks a store to remember.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Draft {
    pub kind: String,
    pub content: String,
    /// `project` (default) or `session`.
    #[serde(default = "project")]
    pub visibility: String,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    /// Who asserts it; defaults to an explicit user statement.
    #[serde(default)]
    pub provenance: Option<Provenance>,
    #[serde(default)]
    pub request_id: Option<String>,
}

fn project() -> String {
    "project".into()
}

/// A correction of an identity's current revision.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Correction {
    pub id: String,
    pub expected_revision: u64,
    pub content: String,
    #[serde(default)]
    pub provenance: Option<Provenance>,
    #[serde(default)]
    pub request_id: Option<String>,
}

/// The identity and revision a write produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Written {
    pub id: String,
    pub revision: u64,
}

/// Everything that changed after a cursor. Tombstones come first: apply
/// them before anything else in the batch.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Changes {
    pub cursor: u64,
    pub tombstones: Vec<Tombstone>,
    pub sources: Vec<Source>,
    pub records: Vec<Record>,
    pub links: Vec<Link>,
}

/// The live contract. Errors use the codes `conflict`, `not_found`,
/// `invalid`, `denied` and `unavailable`.
pub trait Store {
    fn namespace(&self) -> &str;
    fn remember(&mut self, draft: Draft) -> Result<Written>;
    /// Visible, active, unexpired records matching every word of `query`
    /// (all of them for an empty query), as seen from `session`.
    fn recall(&mut self, query: &str, session: Option<&str>, limit: usize) -> Result<Vec<Record>>;
    fn correct(&mut self, correction: Correction) -> Result<Written>;
    fn forget(&mut self, id: &str, reason: &str) -> Result<Tombstone>;
    fn changes(&mut self, since: u64) -> Result<Changes>;
    fn export(&mut self) -> Result<Bundle>;
    /// Plan an import; writes nothing.
    fn plan_import(&mut self, bundle: &Bundle) -> Result<ImportPlan>;
    /// Apply a plan atomically. Fails with `conflict` if the plan is not
    /// clean or no longer matches the store.
    fn apply_import(&mut self, bundle: &Bundle, plan: &ImportPlan) -> Result<ImportPlan>;
}

/// Whether `query` matches `content`: every whitespace-separated word of the
/// query appears in the content, ignoring case.
pub fn matches(query: &str, content: &str) -> bool {
    let content = content.to_lowercase();
    query
        .split_whitespace()
        .all(|word| content.contains(&word.to_lowercase()))
}

fn default_provenance() -> Provenance {
    Provenance {
        origin: "user".into(),
        author: "amx".into(),
        explicit: true,
        method: None,
        method_version: None,
        extra: Default::default(),
    }
}

/// An in-memory store implementing the full contract. It is the reference
/// the conformance vectors are checked against, and a model for adapters.
#[derive(Clone, Debug)]
pub struct MemoryStore {
    namespace: String,
    clock: i64,
    seq: u64,
    next_id: u64,
    records: Vec<(u64, Record)>,
    sources: Vec<(u64, Source)>,
    links: Vec<(u64, Link)>,
    tombstones: Vec<(u64, Tombstone)>,
    requests: BTreeMap<String, (String, Written)>,
}

impl MemoryStore {
    pub fn new(namespace: &str) -> Self {
        Self {
            namespace: namespace.into(),
            clock: parse_timestamp("2026-01-01T00:00:00Z").unwrap_or_default(),
            seq: 0,
            next_id: 0,
            records: Vec::new(),
            sources: Vec::new(),
            links: Vec::new(),
            tombstones: Vec::new(),
            requests: BTreeMap::new(),
        }
    }

    fn now(&mut self) -> String {
        self.clock += 1;
        format_timestamp(self.clock)
    }

    fn bump(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn fresh_id(&mut self, prefix: &str) -> String {
        self.next_id += 1;
        format!("{prefix}-{}", self.next_id)
    }

    fn scope(&self, visibility: &str, session: Option<&str>) -> Scope {
        Scope {
            namespace: self.namespace.clone(),
            visibility: visibility.into(),
            session: session.map(String::from),
            repo: None,
            branch: None,
            user: None,
            extra: Default::default(),
        }
    }

    fn support() -> Support {
        Support::new(&["project", "session"], &[])
    }

    /// Replay protection: the same request id with the same payload returns
    /// the first result; with a different payload it is a conflict.
    fn replay(&self, request_id: Option<&String>, payload: &str) -> Result<Option<Written>> {
        let Some(id) = request_id else {
            return Ok(None);
        };
        match self.requests.get(id) {
            Some((digest, written)) if *digest == sha256_hex(payload.as_bytes()) => {
                Ok(Some(written.clone()))
            }
            Some(_) => Err(Error::new(
                "conflict",
                "request id reused with a different payload",
            )),
            None => Ok(None),
        }
    }

    fn remember_request(&mut self, request_id: Option<String>, payload: &str, written: &Written) {
        if let Some(id) = request_id {
            self.requests
                .insert(id, (sha256_hex(payload.as_bytes()), written.clone()));
        }
    }

    fn current(&self, id: &str) -> Option<&Record> {
        self.records
            .iter()
            .map(|(_, r)| r)
            .filter(|r| r.id == id)
            .max_by_key(|r| r.revision)
    }

    fn holdings(&self) -> Holdings {
        let mut held = Holdings::default();
        for (_, t) in &self.tombstones {
            held.hold_tombstone(t);
        }
        for (_, s) in &self.sources {
            held.hold_source(s);
        }
        for (_, r) in &self.records {
            held.hold_record(r);
        }
        for (_, l) in &self.links {
            held.hold_link(l);
        }
        held
    }

    fn delete(&mut self, tombstone: &Tombstone) {
        let derived: Vec<&String> = tombstone.derived.iter().flatten().collect();
        let gone = |kind: &str, id: &String| {
            (tombstone.target_type == kind && *id == tombstone.target_id)
                || (kind != "source" && derived.contains(&id))
        };
        self.records.retain(|(_, r)| !gone("record", &r.id));
        self.sources.retain(|(_, s)| !gone("source", &s.id));
        self.links.retain(|(_, l)| !gone("link", &l.id));
    }

    /// Apply a deletion and record its tombstone. A deleted record takes the
    /// sources only it cited with it, each under its own tombstone: their
    /// text is the forgotten content.
    fn bury(&mut self, tombstone: &Tombstone) {
        let mut orphans: Vec<Source> = Vec::new();
        if tombstone.target_type == "record" {
            let id = tombstone.target_id.as_str();
            let cited_elsewhere = |source: &str| {
                self.records.iter().any(|(_, r)| {
                    r.id != id && r.evidence.iter().flatten().any(|e| e.source == source)
                })
            };
            for (_, record) in self.records.iter().filter(|(_, r)| r.id == id) {
                for evidence in record.evidence.iter().flatten() {
                    let found = self.sources.iter().find(|(_, s)| s.id == evidence.source);
                    if let Some((_, source)) = found
                        && !cited_elsewhere(&source.id)
                        && !orphans.iter().any(|o| o.id == source.id)
                    {
                        orphans.push(source.clone());
                    }
                }
            }
        }
        self.delete(tombstone);
        let seq = self.bump();
        self.tombstones.push((seq, tombstone.clone()));
        for source in orphans {
            let source_tombstone = Tombstone {
                id: self.fresh_id("del"),
                target_type: "source".into(),
                target_id: source.id.clone(),
                scope: source.scope.clone(),
                derived: None,
                ..tombstone.clone()
            };
            self.delete(&source_tombstone);
            self.tombstones.push((seq, source_tombstone));
        }
    }

    fn supersede_previous(&mut self, id: &str, revision: u64, link: bool) {
        if link {
            for (_, l) in self.links.iter_mut() {
                if l.id == id && l.revision < revision && l.status == "active" {
                    l.status = "superseded".into();
                }
            }
        } else {
            for (_, r) in self.records.iter_mut() {
                if r.id == id && r.revision < revision && r.status == "active" {
                    r.status = "superseded".into();
                }
            }
        }
    }
}

impl Store for MemoryStore {
    fn namespace(&self) -> &str {
        &self.namespace
    }

    fn remember(&mut self, draft: Draft) -> Result<Written> {
        let payload = format!("{draft:?}");
        if let Some(written) = self.replay(draft.request_id.as_ref(), &payload)? {
            return Ok(written);
        }
        let now = self.now();
        let provenance = draft.provenance.clone().unwrap_or_else(default_provenance);
        let source_id = self.fresh_id("src");
        let scope = self.scope(&draft.visibility, draft.session.as_deref());
        let source = Source {
            amx: VERSION.into(),
            id: source_id.clone(),
            revision: 1,
            origin: provenance.origin.clone(),
            author: provenance.author.clone(),
            external_ref: None,
            text: draft.content.clone(),
            content_hash: format!("sha256:{}", sha256_hex(draft.content.as_bytes())),
            scope: scope.clone(),
            recorded_at: now.clone(),
            event_at: None,
            expires_at: None,
            attributes: None,
            extra: Default::default(),
        };
        let id = self.fresh_id("mem");
        let record = Record {
            amx: VERSION.into(),
            id: id.clone(),
            revision: 1,
            kind: draft.kind.clone(),
            content: draft.content.clone(),
            content_type: None,
            key: draft.key.clone(),
            scope,
            provenance,
            evidence: Some(vec![Evidence {
                source: source_id,
                source_revision: 1,
                span: Some([0, draft.content.len() as u64]),
                role: "supports".into(),
                extra: Default::default(),
            }]),
            status: "active".into(),
            supersedes: None,
            valid_from: now.clone(),
            valid_to: None,
            recorded_at: now,
            event_at: None,
            expires_at: None,
            attributes: None,
            extra: Default::default(),
        };
        crate::validate_source(&source)?;
        crate::validate_record(&record)?;
        Self::support().visible(&record.scope)?;
        let seq = self.bump();
        self.sources.push((seq, source));
        self.records.push((seq, record));
        let written = Written { id, revision: 1 };
        self.remember_request(draft.request_id, &payload, &written);
        Ok(written)
    }

    fn recall(&mut self, query: &str, session: Option<&str>, limit: usize) -> Result<Vec<Record>> {
        let now = self.clock;
        let mut found: Vec<Record> = self
            .records
            .iter()
            .map(|(_, r)| r)
            .filter(|r| r.status == "active")
            .filter(|r| match r.scope.visibility.as_str() {
                "project" => true,
                "session" => session.is_some() && r.scope.session.as_deref() == session,
                _ => false,
            })
            .filter(|r| {
                r.expires_at
                    .as_deref()
                    .and_then(parse_timestamp)
                    .is_none_or(|at| at > now)
            })
            .filter(|r| matches(query, &r.content))
            .cloned()
            .collect();
        found.sort_by(|a, b| a.id.cmp(&b.id));
        found.truncate(limit);
        Ok(found)
    }

    fn correct(&mut self, correction: Correction) -> Result<Written> {
        let payload = format!("{correction:?}");
        if let Some(written) = self.replay(correction.request_id.as_ref(), &payload)? {
            return Ok(written);
        }
        let current = self
            .current(&correction.id)
            .cloned()
            .ok_or_else(|| Error::new("not_found", "no such memory"))?;
        if current.revision != correction.expected_revision {
            return Err(Error::new(
                "conflict",
                format!(
                    "expected revision {} but the memory is at revision {}",
                    correction.expected_revision, current.revision
                ),
            ));
        }
        let provenance = correction
            .provenance
            .clone()
            .unwrap_or_else(default_provenance);
        if current.provenance.explicit && !provenance.explicit {
            return Err(Error::new(
                "conflict",
                "an inferred correction cannot replace an explicit memory",
            ));
        }
        let now = self.now();
        let mut next = current.clone();
        next.revision += 1;
        next.content = correction.content.clone();
        next.provenance = provenance;
        next.status = "active".into();
        next.valid_from = now.clone();
        next.recorded_at = now;
        if !next.provenance.explicit && next.evidence.as_ref().is_none_or(Vec::is_empty) {
            return Err(Error::new("invalid", "inferred corrections need evidence"));
        }
        crate::validate_record(&next)?;
        self.supersede_previous(&next.id, next.revision, false);
        let seq = self.bump();
        let written = Written {
            id: next.id.clone(),
            revision: next.revision,
        };
        self.records.push((seq, next));
        self.remember_request(correction.request_id, &payload, &written);
        Ok(written)
    }

    fn forget(&mut self, id: &str, reason: &str) -> Result<Tombstone> {
        let current = self
            .current(id)
            .cloned()
            .ok_or_else(|| Error::new("not_found", "no such memory"))?;
        let mut derived: Vec<String> = self
            .links
            .iter()
            .filter(|(_, l)| l.from.id == id || l.to.id == id)
            .map(|(_, l)| l.id.clone())
            .collect();
        derived.sort();
        derived.dedup();
        let tombstone = Tombstone {
            amx: VERSION.into(),
            id: self.fresh_id("del"),
            target_type: "record".into(),
            target_id: id.into(),
            scope: current.scope.clone(),
            deleted_at: self.now(),
            reason: reason.into(),
            actor: "amx".into(),
            derived: (!derived.is_empty()).then_some(derived),
            attributes: None,
            extra: Default::default(),
        };
        crate::validate_tombstone(&tombstone)?;
        self.bury(&tombstone);
        Ok(tombstone)
    }

    fn changes(&mut self, since: u64) -> Result<Changes> {
        fn after<T: Clone>(items: &[(u64, T)], since: u64) -> Vec<T> {
            items
                .iter()
                .filter(|(seq, _)| *seq > since)
                .map(|(_, item)| item.clone())
                .collect()
        }
        Ok(Changes {
            cursor: self.seq,
            tombstones: after(&self.tombstones, since),
            sources: after(&self.sources, since),
            records: after(&self.records, since),
            links: after(&self.links, since),
        })
    }

    fn export(&mut self) -> Result<Bundle> {
        let created_at = self.now();
        let mut bundle = Bundle::new(
            &format!("export-{}", self.seq),
            Generator {
                name: "amx-memory-store".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            &created_at,
            vec![NamespaceCursor {
                id: self.namespace.clone(),
                cursor: self.seq,
            }],
        );
        bundle.records = self.records.iter().map(|(_, r)| r.clone()).collect();
        bundle.sources = self.sources.iter().map(|(_, s)| s.clone()).collect();
        bundle.links = self.links.iter().map(|(_, l)| l.clone()).collect();
        bundle.tombstones = self.tombstones.iter().map(|(_, t)| t.clone()).collect();
        Ok(bundle)
    }

    fn plan_import(&mut self, bundle: &Bundle) -> Result<ImportPlan> {
        if let Some(other) = bundle
            .manifest
            .namespaces
            .iter()
            .find(|n| n.id != self.namespace)
        {
            return Err(Error::new(
                "invalid",
                format!(
                    "bundle namespace {:?} must be mapped to {:?} before import",
                    other.id, self.namespace
                ),
            ));
        }
        Ok(plan_import(bundle, &self.holdings(), &Self::support()))
    }

    fn apply_import(&mut self, bundle: &Bundle, plan: &ImportPlan) -> Result<ImportPlan> {
        let fresh = self.plan_import(bundle)?;
        if fresh != *plan {
            return Err(Error::new(
                "conflict",
                "the store changed since the plan was made",
            ));
        }
        if !fresh.is_clean() {
            return Err(Error::new(
                "conflict",
                format!("{} conflicting items", fresh.conflicts.len()),
            ));
        }
        for tombstone in &fresh.tombstones {
            self.bury(tombstone);
        }
        for item in fresh.adds.iter().chain(&fresh.supersedes) {
            let seq = self.bump();
            match item {
                Item::Source(s) => self.sources.push((seq, s.clone())),
                Item::Record(r) => {
                    self.supersede_previous(&r.id, r.revision, false);
                    self.records.push((seq, r.clone()));
                }
                Item::Link(l) => {
                    self.supersede_previous(&l.id, l.revision, true);
                    self.links.push((seq, l.clone()));
                }
            }
        }
        Ok(fresh)
    }
}
