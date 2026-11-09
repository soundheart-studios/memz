use amx::{Draft, MemoryStore, Store, load_vectors, run_vector};
use std::path::Path;

fn vectors_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/amx/conformance")
}

#[test]
fn the_reference_store_passes_every_vector() {
    let vectors = load_vectors(&vectors_dir()).expect("vectors load");
    let levels: Vec<_> = vectors.iter().map(|v| v.level.as_str()).collect();
    for level in ["L0", "L1", "L2"] {
        assert!(
            levels.iter().filter(|l| **l == level).count() >= 2,
            "at least two {level} vectors"
        );
    }
    let mut failures = Vec::new();
    for vector in &vectors {
        let mut store = MemoryStore::new("conformance-ns");
        if let Err(failure) = run_vector(&mut store, vector) {
            failures.push(format!("{}: {failure}", vector.name));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A store that breaks one rule must fail the vectors that guard it, or the
/// suite proves nothing.
struct Resurrecting(MemoryStore);

impl Store for Resurrecting {
    fn namespace(&self) -> &str {
        self.0.namespace()
    }
    fn remember(&mut self, draft: Draft) -> amx::Result<amx::Written> {
        self.0.remember(draft)
    }
    fn recall(
        &mut self,
        query: &str,
        session: Option<&str>,
        limit: usize,
    ) -> amx::Result<Vec<amx::Record>> {
        self.0.recall(query, session, limit)
    }
    fn correct(&mut self, correction: amx::Correction) -> amx::Result<amx::Written> {
        self.0.correct(correction)
    }
    fn forget(&mut self, id: &str, reason: &str) -> amx::Result<amx::Tombstone> {
        self.0.forget(id, reason)
    }
    fn changes(&mut self, since: u64) -> amx::Result<amx::Changes> {
        self.0.changes(since)
    }
    fn export(&mut self) -> amx::Result<amx::Bundle> {
        self.0.export()
    }
    fn plan_import(&mut self, bundle: &amx::Bundle) -> amx::Result<amx::ImportPlan> {
        // Forgets its tombstones while planning: deleted memories come back.
        let mut plan = self.0.plan_import(bundle)?;
        let refused = std::mem::take(&mut plan.refused);
        for item in refused {
            let found = bundle
                .records
                .iter()
                .find(|r| r.id == item.id && r.revision == item.revision);
            if let Some(record) = found {
                plan.adds.push(amx::Item::Record(record.clone()));
            }
        }
        Ok(plan)
    }
    fn apply_import(
        &mut self,
        bundle: &amx::Bundle,
        plan: &amx::ImportPlan,
    ) -> amx::Result<amx::ImportPlan> {
        self.0.apply_import(bundle, plan)
    }
}

#[test]
fn a_store_that_resurrects_fails_the_suite() {
    let vectors = load_vectors(&vectors_dir()).unwrap();
    let failed = vectors
        .iter()
        .filter(|v| {
            let mut store = Resurrecting(MemoryStore::new("conformance-ns"));
            run_vector(&mut store, v).is_err()
        })
        .count();
    assert!(failed >= 1, "no vector caught resurrection");
}

#[test]
fn malformed_vectors_are_reported_not_skipped() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("amx-bad-vectors");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("bad.json"), "{\"name\":\"x\"}").unwrap();
    assert!(load_vectors(&dir).is_err());
}
