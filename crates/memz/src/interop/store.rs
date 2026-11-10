//! Memz as an AMX [`amx::Store`]: the operations contract over a real
//! namespace, used to run the shared conformance vectors.
//!
//! Every operation goes through the same authorized Memz functions agents
//! use (capture, recall, correct, forget, export, import), as the bound
//! principal. Sessions named by the contract are bound on demand.
//!
//! The change feed uses the namespace mutation sequence as its cursor.
//! `changes(0)` returns everything; a cursor equal to the current sequence
//! returns nothing; a cursor this adapter returned earlier returns exactly
//! what changed since. For a cursor it did not issue it returns everything
//! again, which consumers apply idempotently (tombstones first, then
//! no-ops), so nothing is ever missed.
use crate::{
    auth::{Capability, Scope as MemzScope},
    capture::{Capture, remember},
    catalog::Catalog,
    export::snapshot,
    import::{apply_bundle, plan_bundle},
    lifecycle::{correct, forget},
    retrieval::{Query, recall},
    state::StateRoot,
    store::NamespaceStore,
};
use amx::{Bundle, Changes, Correction, Draft, ImportPlan, Record, Tombstone, Written};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::AtomicBool,
};

/// Memz codes to AMX operation error codes.
fn to_amx(error: crate::Error) -> amx::Error {
    let code = match error.code {
        "conflict" | "deleted" => "conflict",
        "denied" => "denied",
        "not_found" => "not_found",
        "busy" | "unavailable" | "storage" | "io" => "unavailable",
        _ => "invalid",
    };
    amx::Error::new(code, error.to_string())
}

pub struct MemzStore {
    cat: Catalog,
    store: NamespaceStore,
    principal: String,
    session: String,
    views: BTreeMap<u64, Bundle>,
}

impl MemzStore {
    /// Bind to `namespace` as `principal` in `session`. The principal must
    /// already hold a grant for the namespace.
    pub fn open(
        state: &StateRoot,
        namespace: &str,
        principal: &str,
        session: &str,
    ) -> crate::Result<Self> {
        Ok(Self {
            cat: Catalog::open(state.clone())?,
            store: NamespaceStore::open(state, namespace)?,
            principal: principal.into(),
            session: session.into(),
            views: BTreeMap::new(),
        })
    }

    fn capability(&mut self, session: Option<&str>) -> amx::Result<Capability> {
        let session = session.unwrap_or(&self.session).to_string();
        if session != self.session {
            self.cat
                .bind_session(&self.principal, &session)
                .map_err(to_amx)?;
        }
        self.cat
            .capability(&self.principal, &session)
            .map_err(to_amx)
    }

    /// Everything the given session may see, as an AMX bundle.
    fn view(&mut self, session: Option<&str>) -> amx::Result<Bundle> {
        let cap = self.capability(session)?;
        let export = snapshot(
            &self.cat,
            &cap,
            &mut self.store,
            8388608,
            &AtomicBool::new(false),
        )
        .map_err(to_amx)?;
        Bundle::decode(&export.amx)
    }

    fn cursor(&self) -> amx::Result<u64> {
        self.store
            .conn
            .query_row("SELECT mutation_seq FROM namespace_meta", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|n| n.max(0) as u64)
            .map_err(|e| to_amx(e.into()))
    }

    fn current(&mut self, id: &str) -> amx::Result<Record> {
        self.view(None)?
            .records
            .into_iter()
            .filter(|r| r.id == id && r.status == "active")
            .max_by_key(|r| r.revision)
            .ok_or_else(|| amx::Error::new("not_found", "no such memory"))
    }
}

impl amx::Store for MemzStore {
    fn namespace(&self) -> &str {
        &self.store.namespace
    }

    fn remember(&mut self, draft: Draft) -> amx::Result<Written> {
        let private = draft.visibility == "session";
        let session = match (&draft.session, private) {
            (Some(session), true) => Some(session.clone()),
            (None, true) => {
                return Err(amx::Error::new(
                    "invalid",
                    "session visibility needs a session",
                ));
            }
            (_, false) => None,
        };
        if !matches!(draft.visibility.as_str(), "project" | "session") {
            return Err(amx::Error::new("invalid", "unsupported visibility"));
        }
        let cap = self.capability(session.as_deref())?;
        let namespace = self.store.namespace.clone();
        let scope = match &session {
            Some(session) => MemzScope::session(&namespace, session),
            None => MemzScope::project(&namespace),
        };
        let request_id = match &draft.request_id {
            Some(id) => id.clone(),
            None => crate::capture::id().map_err(to_amx)?,
        };
        let mut capture = Capture::text(scope, &request_id, &draft.content);
        capture.kind = draft.kind.clone();
        capture.slot = draft.key.clone();
        if let Some(provenance) = &draft.provenance {
            // Capture accepts Memz's own origins; an extractor is an
            // assistant-side inference.
            capture.origin = match super::origin_from_amx(&provenance.origin) {
                "extractor" => "assistant",
                origin => origin,
            }
            .into();
        }
        let receipt = remember(&self.cat, &cap, &mut self.store, &capture).map_err(to_amx)?;
        Ok(Written {
            id: receipt.id,
            revision: receipt.revision as u64,
        })
    }

    fn recall(
        &mut self,
        query: &str,
        session: Option<&str>,
        limit: usize,
    ) -> amx::Result<Vec<Record>> {
        let view = self.view(session)?;
        let active: BTreeMap<String, Record> = view
            .records
            .into_iter()
            .filter(|r| r.status == "active")
            .map(|r| (r.id.clone(), r))
            .collect();
        let mut found: Vec<Record> = if query.trim().is_empty() {
            active.into_values().collect()
        } else {
            let cap = self.capability(session)?;
            let packet =
                recall(&self.cat, &cap, &mut self.store, &Query::new(query)).map_err(to_amx)?;
            let mut seen = BTreeSet::new();
            packet
                .items
                .into_iter()
                .filter(|item| seen.insert(item.id.clone()))
                .filter_map(|item| active.get(&item.id).cloned())
                .collect()
        };
        found.truncate(limit);
        Ok(found)
    }

    fn correct(&mut self, correction: Correction) -> amx::Result<Written> {
        if correction.provenance.as_ref().is_some_and(|p| !p.explicit) {
            return Err(amx::Error::new(
                "conflict",
                "an inferred correction cannot replace a memory through this contract",
            ));
        }
        let cap = self.capability(None)?;
        let receipt = correct(
            &self.cat,
            &cap,
            &mut self.store,
            &correction.id,
            correction.expected_revision as i64,
            &correction.content,
        )
        .map_err(to_amx)?;
        Ok(Written {
            id: receipt.id,
            revision: receipt.revision as u64,
        })
    }

    fn forget(&mut self, id: &str, _reason: &str) -> amx::Result<Tombstone> {
        let current = self.current(id)?;
        let cap = self.capability(None)?;
        forget(
            &self.cat,
            &cap,
            &mut self.store,
            id,
            current.revision as i64,
        )
        .map_err(to_amx)?;
        self.view(None)?
            .tombstones
            .into_iter()
            .find(|t| t.target_type == "record" && t.target_id == id)
            .ok_or_else(|| amx::Error::new("unavailable", "deletion was not recorded"))
    }

    fn changes(&mut self, since: u64) -> amx::Result<Changes> {
        let cursor = self.cursor()?;
        if since >= cursor {
            return Ok(Changes {
                cursor,
                ..Default::default()
            });
        }
        let now = self.view(None)?;
        let before = self.views.get(&since).cloned();
        let had = |line: String, lines: &BTreeSet<String>| !lines.contains(&line);
        let lines = |b: &Option<Bundle>| -> BTreeSet<String> {
            let Some(b) = b else {
                return BTreeSet::new();
            };
            let mut out = BTreeSet::new();
            for r in &b.records {
                out.insert(amx::write_jsonl(std::slice::from_ref(r)).unwrap_or_default());
            }
            for s in &b.sources {
                out.insert(amx::write_jsonl(std::slice::from_ref(s)).unwrap_or_default());
            }
            for l in &b.links {
                out.insert(amx::write_jsonl(std::slice::from_ref(l)).unwrap_or_default());
            }
            for t in &b.tombstones {
                out.insert(amx::write_jsonl(std::slice::from_ref(t)).unwrap_or_default());
            }
            out
        };
        let old = lines(&before);
        let new = |line: String| had(line, &old);
        let changes = Changes {
            cursor,
            tombstones: now
                .tombstones
                .iter()
                .filter(|t| new(amx::write_jsonl(std::slice::from_ref(*t)).unwrap_or_default()))
                .cloned()
                .collect(),
            sources: now
                .sources
                .iter()
                .filter(|s| new(amx::write_jsonl(std::slice::from_ref(*s)).unwrap_or_default()))
                .cloned()
                .collect(),
            records: now
                .records
                .iter()
                .filter(|r| new(amx::write_jsonl(std::slice::from_ref(*r)).unwrap_or_default()))
                .cloned()
                .collect(),
            links: now
                .links
                .iter()
                .filter(|l| new(amx::write_jsonl(std::slice::from_ref(*l)).unwrap_or_default()))
                .cloned()
                .collect(),
        };
        self.views.insert(cursor, now);
        Ok(changes)
    }

    fn export(&mut self) -> amx::Result<Bundle> {
        self.view(None)
    }

    fn plan_import(&mut self, bundle: &Bundle) -> amx::Result<ImportPlan> {
        let cap = self.capability(None)?;
        plan_bundle(&self.cat, &cap, &mut self.store, bundle.clone())
            .map(|(_, plan)| plan)
            .map_err(to_amx)
    }

    fn apply_import(&mut self, bundle: &Bundle, plan: &ImportPlan) -> amx::Result<ImportPlan> {
        let fresh = self.plan_import(bundle)?;
        if fresh != *plan {
            return Err(amx::Error::new(
                "conflict",
                "the store changed since the plan was made",
            ));
        }
        let cap = self.capability(None)?;
        apply_bundle(&self.cat, &cap, &mut self.store, bundle.clone(), false).map_err(to_amx)?;
        Ok(fresh)
    }
}
