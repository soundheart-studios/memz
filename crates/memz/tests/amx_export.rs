mod common;
use common::Fixture;
use memz::{
    adapters::{initialize, load_client},
    auth::Scope,
    capture::{Capture, remember},
    catalog::Catalog,
    export::{publish, snapshot},
    graph::{Anchor, Node, Relation, assert_explicit, propose},
    lifecycle::{cleanup, correct, forget},
    state::StateRoot,
    store::NamespaceStore,
};
use std::{collections::BTreeMap, sync::atomic::AtomicBool};

fn source_of(store: &NamespaceStore, item: &str, revision: i64) -> String {
    store
        .conn
        .query_row(
            "SELECT source_id FROM items WHERE id=?1 AND revision=?2",
            rusqlite::params![item, revision],
            |r| r.get(0),
        )
        .unwrap()
}

fn all_bytes(path: &std::path::Path) -> String {
    let mut text = String::new();
    for entry in std::fs::read_dir(path).unwrap() {
        text.push_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap());
    }
    text
}

#[test]
fn memz_exports_a_verified_amx_bundle() {
    let f = Fixture::new("amx-export");
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    std::fs::create_dir(f.root.join("project")).unwrap();
    let path = initialize(&state, &f.root.join("project"), "u", "s").unwrap();
    let client = load_client(&path).unwrap();
    let ns = client.namespace.clone();
    let mut cat = Catalog::open(state.clone()).unwrap();
    let cap = cat.capability("u", "s").unwrap();
    let mut store = NamespaceStore::open(&state, &ns).unwrap();

    let mut decision = Capture::text(
        Scope::project(&ns),
        "decision",
        "Use SQLite for every namespace.",
    );
    decision.kind = "decision".into();
    decision.slot = Some("storage.engine".into());
    let a = remember(&cat, &cap, &mut store, &decision).unwrap();
    let mut said = Capture::text(
        Scope::project(&ns),
        "assistant",
        "The daemon listens on a unix socket.",
    );
    said.origin = "assistant".into();
    let b = remember(&cat, &cap, &mut store, &said).unwrap();
    let c = remember(
        &cat,
        &cap,
        &mut store,
        &Capture::text(Scope::project(&ns), "doomed", "The launch code is 0000."),
    )
    .unwrap();
    forget(&cat, &cap, &mut store, &c.id, 1).unwrap();
    let a2 = correct(
        &cat,
        &cap,
        &mut store,
        &a.id,
        1,
        "Use SQLite for every project namespace.",
    )
    .unwrap();
    assert_eq!(a2.revision, 2);

    cat.bind_session("u", "private").unwrap();
    let private = cat.capability("u", "private").unwrap();
    remember(
        &cat,
        &private,
        &mut store,
        &Capture::text(Scope::session(&ns, "private"), "p", "hidden-secret"),
    )
    .unwrap();

    let a_source = source_of(&store, &a.id, 2);
    let b_source = source_of(&store, &b.id, 1);
    let explicit = assert_explicit(
        &cat,
        &cap,
        &mut store,
        &Relation {
            from: Node {
                id: a.id.clone(),
                revision: 2,
            },
            to: Node {
                id: b.id.clone(),
                revision: 1,
            },
            relation: "applies_to".into(),
            qualifiers: BTreeMap::from([("why".into(), "same daemon".into())]),
            evidence: vec![Anchor {
                source: a_source.clone(),
                revision: 2,
                start: 0,
                end: 10,
            }],
        },
    )
    .unwrap();
    let inferred = propose(
        &cat,
        &cap,
        &mut store,
        &Relation {
            from: Node {
                id: b.id.clone(),
                revision: 1,
            },
            to: Node {
                id: a.id.clone(),
                revision: 2,
            },
            relation: "supports".into(),
            qualifiers: BTreeMap::new(),
            evidence: vec![Anchor {
                source: b_source.clone(),
                revision: 1,
                start: 4,
                end: 10,
            }],
        },
    )
    .unwrap();

    let cancel = AtomicBool::new(false);
    let export = snapshot(&cat, &cap, &mut store, 1048576, &cancel).unwrap();
    assert_eq!(
        export,
        snapshot(&cat, &cap, &mut store, 1048576, &cancel).unwrap(),
        "deterministic, bundle included"
    );
    publish(&state, "snap", &export, &cancel).unwrap();
    let bundle_path = state.path.join("exports").join("snap.amx");
    let bundle = amx::Bundle::read(&bundle_path).expect("bundle verifies");

    // Manifest.
    assert_eq!(bundle.manifest.generator.name, "memz");
    assert_eq!(bundle.manifest.namespaces.len(), 1);
    assert_eq!(bundle.manifest.namespaces[0].id, ns);
    assert_eq!(bundle.manifest.namespaces[0].cursor, export.revision as u64);

    // Records: both revisions of the decision, the assistant's statement.
    let record = |id: &str, revision: u64| {
        bundle
            .records
            .iter()
            .find(|r| r.id == id && r.revision == revision)
            .unwrap_or_else(|| panic!("record {id}@{revision}"))
    };
    assert_eq!(bundle.records.len(), 3);
    let old = record(&a.id, 1);
    assert_eq!(old.status, "superseded");
    assert!(old.valid_to.is_some());
    let current = record(&a.id, 2);
    assert_eq!(current.status, "active");
    assert_eq!(current.kind, "decision");
    assert_eq!(current.content, "Use SQLite for every project namespace.");
    assert_eq!(current.key.as_deref(), Some("storage.engine"));
    assert_eq!(current.scope.namespace, ns);
    assert_eq!(current.scope.visibility, "project");
    assert_eq!(current.provenance.origin, "user");
    assert_eq!(current.provenance.author, "u");
    assert!(current.provenance.explicit);
    let evidence = current.evidence.as_ref().unwrap();
    assert_eq!(evidence[0].source, a_source);
    assert_eq!(evidence[0].source_revision, 2);
    assert_eq!(evidence[0].span, Some([0, 39]));
    let assistant = record(&b.id, 1);
    assert_eq!(assistant.provenance.origin, "agent");
    assert!(!assistant.provenance.explicit);
    assert_eq!(assistant.provenance.method.as_deref(), Some("direct"));
    assert_eq!(assistant.provenance.method_version.as_deref(), Some("1"));
    assert!(!assistant.evidence.as_ref().unwrap().is_empty());

    // Sources carry the passages with verified hashes (Bundle::read checked them).
    assert!(
        bundle
            .sources
            .iter()
            .any(|s| s.id == a_source && s.revision == 2)
    );
    assert!(bundle.sources.iter().any(|s| s.id == b_source));

    // Links: one stated, one inferred, both with evidence.
    assert_eq!(bundle.links.len(), 2);
    let stated = bundle.links.iter().find(|l| l.id == explicit.id).unwrap();
    assert!(stated.provenance.explicit);
    assert_eq!(stated.relation, "applies_to");
    assert_eq!(
        stated.qualifiers.as_ref().unwrap()["why"],
        serde_json::json!("same daemon")
    );
    let guessed = bundle.links.iter().find(|l| l.id == inferred.id).unwrap();
    assert!(!guessed.provenance.explicit);
    assert_eq!(guessed.provenance.method.as_deref(), Some("agent-proposal"));
    assert_eq!(guessed.evidence.as_ref().unwrap()[0].span, Some([4, 10]));

    // The forgotten memory is a tombstone for the record and its source, and
    // its words appear nowhere; the other session's memory is absent.
    let targets: Vec<_> = bundle
        .tombstones
        .iter()
        .map(|t| (t.target_type.as_str(), t.target_id.clone()))
        .collect();
    assert!(targets.contains(&("record", c.id.clone())));
    assert!(targets.iter().any(|(kind, _)| *kind == "source"));
    let text = all_bytes(&bundle_path);
    assert!(!text.contains("launch code"));
    assert!(!text.contains("hidden-secret"));

    // The other session sees its own memory, with session scope.
    let theirs = snapshot(&cat, &private, &mut store, 1048576, &cancel).unwrap();
    publish(&state, "private", &theirs, &cancel).unwrap();
    let mine = amx::Bundle::read(&state.path.join("exports").join("private.amx")).unwrap();
    let hidden = mine
        .records
        .iter()
        .find(|r| r.content == "hidden-secret")
        .unwrap();
    assert_eq!(hidden.scope.visibility, "session");
    assert_eq!(hidden.scope.session.as_deref(), Some("private"));

    // Physical cleanup keeps the deletion durable in later exports.
    cleanup(&mut store, 256).unwrap();
    let later = snapshot(&cat, &cap, &mut store, 1048576, &cancel).unwrap();
    publish(&state, "later", &later, &cancel).unwrap();
    let later_path = state.path.join("exports").join("later.amx");
    let after = amx::Bundle::read(&later_path).unwrap();
    assert!(
        after
            .tombstones
            .iter()
            .any(|t| t.target_type == "record" && t.target_id == c.id)
    );
    assert!(!all_bytes(&later_path).contains("launch code"));

    // Publishing never overwrites an existing bundle.
    assert!(publish(&state, "snap", &export, &cancel).is_err());
}
