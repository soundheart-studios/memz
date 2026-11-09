//! Import planning (SPEC sections 4.3, 5, 6 and 10.1).
//!
//! [`plan_import`] compares a verified bundle with what a store holds and
//! says what applying it would do. It never writes. Every store applies the
//! same rules because every store asks the same planner:
//!
//! - tombstones come first, and a tombstoned identity never comes back;
//! - an identical revision is a no-op, a divergent one a conflict;
//! - a revision must continue its identity's chain, never skip or fork it;
//! - an inferred record never supersedes an explicit one, by revision,
//!   `supersedes` or `key`;
//! - a scope the reader cannot represent is a conflict, never widened;
//! - links need live endpoints.
use crate::{Bundle, Link, Record, Source, Support, Tombstone, sha256_hex};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Something a bundle can add: a record, source or link.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Record(Record),
    Source(Source),
    Link(Link),
}

impl Item {
    pub fn kind(&self) -> &'static str {
        match self {
            Item::Record(_) => "record",
            Item::Source(_) => "source",
            Item::Link(_) => "link",
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Item::Record(r) => &r.id,
            Item::Source(s) => &s.id,
            Item::Link(l) => &l.id,
        }
    }

    pub fn revision(&self) -> u64 {
        match self {
            Item::Record(r) => r.revision,
            Item::Source(s) => s.revision,
            Item::Link(l) => l.revision,
        }
    }

    pub fn namespace(&self) -> &str {
        match self {
            Item::Record(r) => &r.scope.namespace,
            Item::Source(s) => &s.scope.namespace,
            Item::Link(l) => &l.scope.namespace,
        }
    }

    fn explicit(&self) -> bool {
        match self {
            Item::Record(r) => r.provenance.explicit,
            Item::Source(_) => true,
            Item::Link(l) => l.provenance.explicit,
        }
    }

    fn fingerprint(&self) -> String {
        match self {
            Item::Record(r) => record_fingerprint(r),
            Item::Source(s) => source_fingerprint(s),
            Item::Link(l) => link_fingerprint(l),
        }
    }

    pub fn reference(&self) -> ItemRef {
        ItemRef {
            kind: self.kind(),
            namespace: self.namespace().into(),
            id: self.id().into(),
            revision: self.revision(),
        }
    }
}

/// Names one revision of one identity in a plan.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ItemRef {
    pub kind: &'static str,
    pub namespace: String,
    pub id: String,
    pub revision: u64,
}

/// An item the planner will not apply, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub item: ItemRef,
    pub reason: String,
}

/// What importing a bundle would do, in the order it must be applied.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportPlan {
    /// Deletions the store does not hold yet. Applied before anything else.
    pub tombstones: Vec<Tombstone>,
    /// Identities new to the store (sources, then records, then links).
    pub adds: Vec<Item>,
    /// Next revisions of identities the store holds.
    pub supersedes: Vec<Item>,
    /// Revisions the store already holds unchanged.
    pub noops: Vec<ItemRef>,
    /// Items whose identity, evidence or endpoint was deleted.
    pub refused: Vec<ItemRef>,
    /// Everything else that cannot be applied safely.
    pub conflicts: Vec<Conflict>,
}

impl ImportPlan {
    /// True when nothing conflicts. Refused items do not make a plan
    /// unclean: refusing them is the rule working.
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty()
    }
}

/// The digest of a revision's claim. Lifecycle fields that change after the
/// fact (`status`, `valid_to`) are left out, so a revision re-exported after
/// it was superseded is still the same revision.
pub fn record_fingerprint(record: &Record) -> String {
    fingerprint(record, &["status", "valid_to"])
}

pub fn source_fingerprint(source: &Source) -> String {
    fingerprint(source, &[])
}

pub fn link_fingerprint(link: &Link) -> String {
    fingerprint(link, &["status", "valid_to"])
}

fn fingerprint<T: Serialize>(value: &T, skip: &[&str]) -> String {
    let mut json = serde_json::to_value(value).unwrap_or_default();
    if let Some(object) = json.as_object_mut() {
        for field in skip {
            object.remove(*field);
        }
    }
    sha256_hex(json.to_string().as_bytes())
}

/// One identity a store holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Held {
    /// The newest revision.
    pub current: u64,
    /// Whether the newest revision was explicit.
    pub explicit: bool,
    /// Fingerprints of the revisions the store keeps.
    pub revisions: BTreeMap<u64, String>,
}

type Identity = (String, String);

/// What a store holds, as far as planning needs to know. Stores build one
/// from their own tables; [`Holdings::apply`] advances it by a plan.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Holdings {
    /// Keyed by (namespace, id).
    pub records: BTreeMap<Identity, Held>,
    pub sources: BTreeMap<Identity, Held>,
    pub links: BTreeMap<Identity, Held>,
    /// (target type, namespace, id) of every deletion.
    pub tombstones: BTreeSet<(String, String, String)>,
    /// (namespace, key) held by an explicit record.
    pub explicit_keys: BTreeSet<Identity>,
}

fn hold(map: &mut BTreeMap<Identity, Held>, item: &Item) {
    let held = map
        .entry((item.namespace().into(), item.id().into()))
        .or_default();
    held.revisions.insert(item.revision(), item.fingerprint());
    if item.revision() >= held.current {
        held.current = item.revision();
        held.explicit = item.explicit();
    }
}

impl Holdings {
    pub fn hold_record(&mut self, record: &Record) {
        hold(&mut self.records, &Item::Record(record.clone()));
        if let (true, Some(key)) = (record.provenance.explicit, &record.key) {
            self.explicit_keys
                .insert((record.scope.namespace.clone(), key.clone()));
        }
    }

    pub fn hold_source(&mut self, source: &Source) {
        hold(&mut self.sources, &Item::Source(source.clone()));
    }

    pub fn hold_link(&mut self, link: &Link) {
        hold(&mut self.links, &Item::Link(link.clone()));
    }

    /// Record a deletion: the target and everything it lists as derived
    /// are gone and may never return.
    pub fn hold_tombstone(&mut self, tombstone: &Tombstone) {
        let namespace = &tombstone.scope.namespace;
        for (kind, id) in deleted(tombstone) {
            let identity = (namespace.clone(), id.to_string());
            match kind {
                "record" => self.records.remove(&identity),
                "source" => self.sources.remove(&identity),
                _ => self.links.remove(&identity),
            };
            self.tombstones
                .insert((kind.into(), namespace.clone(), id.into()));
        }
    }

    pub fn holds_record(&self, namespace: &str, id: &str) -> bool {
        self.records
            .contains_key(&(namespace.to_string(), id.to_string()))
    }

    /// Advance these holdings as a store applying `plan` would.
    pub fn apply(&mut self, plan: &ImportPlan) {
        for tombstone in &plan.tombstones {
            self.hold_tombstone(tombstone);
        }
        for item in plan.adds.iter().chain(&plan.supersedes) {
            match item {
                Item::Record(r) => self.hold_record(r),
                Item::Source(s) => self.hold_source(s),
                Item::Link(l) => self.hold_link(l),
            }
        }
    }

    fn dead(&self, kind: &str, namespace: &str, id: &str) -> bool {
        self.tombstones
            .contains(&(kind.into(), namespace.into(), id.into()))
    }
}

/// The (type, id) pairs a tombstone deletes: its target, and each derived
/// identity as both a record and a link.
fn deleted(tombstone: &Tombstone) -> Vec<(&str, &str)> {
    let mut out = vec![(tombstone.target_type.as_str(), tombstone.target_id.as_str())];
    for id in tombstone.derived.iter().flatten() {
        out.push(("record", id));
        out.push(("link", id));
    }
    out
}

/// Plan importing `bundle` into a store that holds `held` and understands
/// the scope described by `support`.
pub fn plan_import(bundle: &Bundle, held: &Holdings, support: &Support) -> ImportPlan {
    let mut plan = ImportPlan::default();
    let mut gone = held.clone();
    let mut tombstones = bundle.tombstones.clone();
    tombstones.sort_by(|a, b| a.id.cmp(&b.id));
    for tombstone in tombstones {
        let namespace = &tombstone.scope.namespace;
        let new = deleted(&tombstone)
            .iter()
            .any(|(kind, id)| !held.dead(kind, namespace, id));
        if new {
            gone.hold_tombstone(&tombstone);
            plan.tombstones.push(tombstone);
        }
    }

    let mut sources: Vec<Item> = bundle.sources.iter().cloned().map(Item::Source).collect();
    let mut records: Vec<Item> = bundle.records.iter().cloned().map(Item::Record).collect();
    let mut links: Vec<Item> = bundle.links.iter().cloned().map(Item::Link).collect();
    for items in [&mut sources, &mut records, &mut links] {
        items.sort_by(|a, b| {
            (a.namespace(), a.id(), a.revision()).cmp(&(b.namespace(), b.id(), b.revision()))
        });
    }

    let mut planner = Planner {
        held,
        gone: &gone,
        support,
        plan,
        available: BTreeSet::new(),
    };
    planner.place_all(&sources, &held.sources);
    planner.place_all(&records, &held.records);
    planner.place_all(&links, &held.links);
    planner.plan
}

struct Planner<'a> {
    held: &'a Holdings,
    gone: &'a Holdings,
    support: &'a Support,
    plan: ImportPlan,
    /// Record revisions a link may point at: held, or placed by this plan.
    available: BTreeSet<(String, String, u64)>,
}

/// Where an identity's chain stands while the planner walks its revisions.
struct Chain<'h> {
    held: Option<&'h Held>,
    current: Option<u64>,
    explicit: bool,
}

impl<'a> Planner<'a> {
    fn place_all(&mut self, items: &[Item], held: &'a BTreeMap<Identity, Held>) {
        let mut index = 0;
        while index < items.len() {
            let identity = (items[index].namespace(), items[index].id());
            let end = items[index..]
                .iter()
                .position(|i| (i.namespace(), i.id()) != identity)
                .map_or(items.len(), |n| index + n);
            let held_identity = held.get(&(identity.0.to_string(), identity.1.to_string()));
            let mut chain = Chain {
                held: held_identity,
                current: held_identity.map(|h| h.current),
                explicit: held_identity.is_some_and(|h| h.explicit),
            };
            for item in &items[index..end] {
                self.place(item, &mut chain);
            }
            index = end;
        }
    }

    fn conflict(&mut self, item: &Item, reason: impl Into<String>) {
        self.plan.conflicts.push(Conflict {
            item: item.reference(),
            reason: reason.into(),
        });
    }

    fn place(&mut self, item: &Item, chain: &mut Chain) {
        let namespace = item.namespace();
        let scope = match item {
            Item::Record(r) => &r.scope,
            Item::Source(s) => &s.scope,
            Item::Link(l) => &l.scope,
        };
        if let Err(error) = self.support.visible(scope) {
            return self.conflict(item, format!("scope: {}", error.message));
        }
        if self.gone.dead(item.kind(), namespace, item.id()) {
            return self.plan.refused.push(item.reference());
        }
        match item {
            Item::Record(r) => {
                if let Some(reason) = self.record_rules(r) {
                    return match reason {
                        Rule::Refuse => self.plan.refused.push(item.reference()),
                        Rule::Conflict(reason) => self.conflict(item, reason),
                    };
                }
            }
            Item::Link(l) => {
                if let Some(reason) = self.link_rules(l) {
                    return match reason {
                        Rule::Refuse => self.plan.refused.push(item.reference()),
                        Rule::Conflict(reason) => self.conflict(item, reason),
                    };
                }
            }
            Item::Source(_) => {}
        }

        let revision = item.revision();
        if let Some(fingerprint) = chain.held.and_then(|h| h.revisions.get(&revision)) {
            if *fingerprint == item.fingerprint() {
                self.make_available(item);
                return self.plan.noops.push(item.reference());
            }
            return self.conflict(
                item,
                format!("revision {revision} is already held with different content"),
            );
        }
        match chain.current {
            Some(current) if revision != current + 1 => self.conflict(
                item,
                format!("revision {revision} does not follow revision {current}"),
            ),
            Some(_) if chain.explicit && !item.explicit() => self.conflict(
                item,
                "an inferred revision cannot supersede an explicit one",
            ),
            current => {
                chain.current = Some(revision);
                chain.explicit = item.explicit();
                self.make_available(item);
                if current.is_some() && chain.held.is_some() {
                    self.plan.supersedes.push(item.clone());
                } else {
                    self.plan.adds.push(item.clone());
                }
            }
        }
    }

    fn make_available(&mut self, item: &Item) {
        if let Item::Record(r) = item {
            self.available
                .insert((r.scope.namespace.clone(), r.id.clone(), r.revision));
        }
    }

    fn record_rules(&self, record: &Record) -> Option<Rule> {
        let namespace = &record.scope.namespace;
        let evidence = record.evidence.as_deref().unwrap_or_default();
        if !evidence.is_empty()
            && evidence
                .iter()
                .all(|e| self.gone.dead("source", namespace, &e.source))
        {
            return Some(Rule::Refuse);
        }
        if record.provenance.explicit {
            return None;
        }
        if let Some(target) = &record.supersedes {
            let explicit_target = self
                .held
                .records
                .get(&(namespace.clone(), target.id.clone()))
                .is_some_and(|h| h.explicit);
            if explicit_target {
                return Some(Rule::Conflict(format!(
                    "an inferred record cannot supersede explicit record {:?}",
                    target.id
                )));
            }
        }
        if let Some(key) = &record.key
            && self
                .held
                .explicit_keys
                .contains(&(namespace.clone(), key.clone()))
        {
            return Some(Rule::Conflict(format!(
                "an inferred record cannot take key {key:?} from an explicit one"
            )));
        }
        None
    }

    fn link_rules(&self, link: &Link) -> Option<Rule> {
        let namespace = &link.scope.namespace;
        for end in [&link.from, &link.to] {
            if self.gone.dead("record", namespace, &end.id) {
                return Some(Rule::Refuse);
            }
            let held = self
                .held
                .records
                .get(&(namespace.clone(), end.id.clone()))
                .is_some_and(|h| h.revisions.contains_key(&end.revision));
            let placed =
                self.available
                    .contains(&(namespace.clone(), end.id.clone(), end.revision));
            if !held && !placed {
                return Some(Rule::Conflict(format!(
                    "endpoint {:?} revision {} is neither held nor in the bundle",
                    end.id, end.revision
                )));
            }
        }
        None
    }
}

enum Rule {
    Refuse,
    Conflict(String),
}
