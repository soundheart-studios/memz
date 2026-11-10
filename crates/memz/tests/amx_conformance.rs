mod common;
use common::Fixture;
use memz::{adapters::initialize, adapters::load_client, interop::MemzStore, state::StateRoot};
use std::path::Path;

fn vectors() -> Vec<amx::Vector> {
    amx::load_vectors(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/amx/conformance"))
        .expect("vectors load")
}

#[test]
fn memz_passes_every_amx_conformance_vector() {
    let vectors = vectors();
    for level in ["L0", "L1", "L2"] {
        assert!(
            vectors.iter().any(|v| v.level == level),
            "the suite has {level} vectors"
        );
    }
    let mut failures = Vec::new();
    for vector in &vectors {
        assert!(amx::applies(vector, "L2"), "Memz claims L2");
        let f = Fixture::new("amx-conf");
        let state = StateRoot::open(&f.root.join("state")).unwrap();
        std::fs::create_dir(f.root.join("project")).unwrap();
        let client =
            load_client(&initialize(&state, &f.root.join("project"), "u", "base").unwrap())
                .unwrap();
        let mut store = MemzStore::open(&state, &client.namespace, "u", "base").unwrap();
        if let Err(failure) = amx::run_vector(&mut store, vector) {
            failures.push(format!("{}: {failure}", vector.name));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn what_memz_exports_it_imports_back_unchanged() {
    // Fidelity: an export imported into a second Memz project and exported
    // again describes the same records, sources, links and tombstones.
    let f = Fixture::new("amx-fidelity");
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    for name in ["one", "two"] {
        std::fs::create_dir(f.root.join(name)).unwrap();
    }
    let one = load_client(&initialize(&state, &f.root.join("one"), "u", "base").unwrap()).unwrap();
    let two = load_client(&initialize(&state, &f.root.join("two"), "u", "base").unwrap()).unwrap();
    let mut first = MemzStore::open(&state, &one.namespace, "u", "base").unwrap();
    let mut second = MemzStore::open(&state, &two.namespace, "u", "base").unwrap();
    use amx::Store;
    let draft = |content: &str| amx::Draft {
        kind: "fact".into(),
        content: content.into(),
        visibility: "project".into(),
        session: None,
        key: None,
        provenance: None,
        request_id: None,
    };
    let kept = first.remember(draft("kept fact")).unwrap();
    let gone = first.remember(draft("gone fact")).unwrap();
    let mut agent = draft("agent says hi");
    agent.provenance = Some(amx::Provenance {
        origin: "agent".into(),
        author: "bot".into(),
        explicit: false,
        method: None,
        method_version: None,
        extra: Default::default(),
    });
    first.remember(agent).unwrap();
    first
        .correct(amx::Correction {
            id: kept.id.clone(),
            expected_revision: 1,
            content: "kept fact, corrected".into(),
            provenance: None,
            request_id: None,
        })
        .unwrap();
    first.forget(&gone.id, "user_request").unwrap();
    let exported = first.export().unwrap();
    let mut mapped = exported.clone();
    mapped.manifest.namespaces[0].id = two.namespace.clone();
    for r in &mut mapped.records {
        r.scope.namespace = two.namespace.clone();
    }
    for s in &mut mapped.sources {
        s.scope.namespace = two.namespace.clone();
    }
    for t in &mut mapped.tombstones {
        t.scope.namespace = two.namespace.clone();
    }
    let plan = second.plan_import(&mapped).unwrap();
    assert!(plan.is_clean(), "{:?}", plan.conflicts);
    second.apply_import(&mapped, &plan).unwrap();
    let again = second.plan_import(&mapped).unwrap();
    assert!(
        again.adds.is_empty() && again.supersedes.is_empty(),
        "{again:?}"
    );
    assert!(again.conflicts.is_empty(), "{:?}", again.conflicts);
    assert!(again.tombstones.is_empty());
    let back = second.export().unwrap();
    let strip = |b: &amx::Bundle| {
        let mut records: Vec<_> = b
            .records
            .iter()
            .map(|r| {
                let mut r = r.clone();
                r.scope.namespace.clear();
                amx::write_jsonl(&[r]).unwrap()
            })
            .collect();
        records.sort();
        records
    };
    assert_eq!(strip(&back), strip(&exported));
}
