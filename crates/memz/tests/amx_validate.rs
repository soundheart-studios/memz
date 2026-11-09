mod common;
use common::Fixture;
use memz::{
    adapters::{dispatch, initialize, load_client},
    auth::Scope,
    capture::{Capture, remember},
    catalog::Catalog,
    export::{publish, snapshot},
    import::validate_bundle,
    lifecycle::forget,
    retrieval::Query,
    state::StateRoot,
    store::NamespaceStore,
};
use serde_json::json;
use std::sync::atomic::AtomicBool;

fn counts(store: &NamespaceStore) -> (i64, i64, i64, i64) {
    store
        .conn
        .query_row(
            "SELECT (SELECT count(*) FROM items),(SELECT count(*) FROM sources),(SELECT count(*) FROM edges),(SELECT mutation_seq FROM namespace_meta)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap()
}

fn export(
    state: &StateRoot,
    cat: &Catalog,
    cap: &memz::auth::Capability,
    store: &mut NamespaceStore,
    name: &str,
) {
    let cancel = AtomicBool::new(false);
    let snap = snapshot(cat, cap, store, 1048576, &cancel).unwrap();
    publish(state, name, &snap, &cancel).unwrap();
}

#[test]
fn validating_a_bundle_previews_the_plan_and_writes_nothing() {
    let f = Fixture::new("amx-validate");
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    for project in ["a", "b"] {
        std::fs::create_dir(f.root.join(project)).unwrap();
    }
    let client_a = load_client(&initialize(&state, &f.root.join("a"), "u", "s").unwrap()).unwrap();
    let client_b = load_client(&initialize(&state, &f.root.join("b"), "u", "s").unwrap()).unwrap();
    let mut cat = Catalog::open(state.clone()).unwrap();
    let cap = cat.capability("u", "s").unwrap();
    let mut a = NamespaceStore::open(&state, &client_a.namespace).unwrap();
    let mut b = NamespaceStore::open(&state, &client_b.namespace).unwrap();
    let ns_a = client_a.namespace.clone();

    let kept = remember(
        &cat,
        &cap,
        &mut a,
        &Capture::text(
            Scope::project(&ns_a),
            "kept",
            "SQLite holds every namespace.",
        ),
    )
    .unwrap();
    let doomed = remember(
        &cat,
        &cap,
        &mut a,
        &Capture::text(Scope::project(&ns_a), "doomed", "The launch code is 0000."),
    )
    .unwrap();
    export(&state, &cat, &cap, &mut a, "before");
    forget(&cat, &cap, &mut a, &doomed.id, 1).unwrap();
    export(&state, &cat, &cap, &mut a, "after");

    // Into an empty project: everything would be added; nothing is written.
    let before = counts(&b);
    let preview = validate_bundle(&cat, &cap, &mut b, "after").unwrap();
    assert_eq!(preview["validated"], true);
    assert_eq!(preview["mode"], "dry_run");
    assert_eq!(preview["namespace"], json!(client_b.namespace));
    assert_eq!(preview["adds"], 2, "{preview}");
    assert_eq!(preview["tombstones"], 2, "record and source tombstones");
    assert_eq!(preview["conflicts"].as_array().unwrap().len(), 0);
    assert_eq!(preview["clean"], true);
    assert_eq!(counts(&b), before, "a dry run writes nothing");

    // Back into its own project: nothing new.
    let same = validate_bundle(&cat, &cap, &mut a, "after").unwrap();
    assert_eq!(same["adds"], 0, "{same}");
    assert_eq!(same["supersedes"], 0);
    assert_eq!(same["noops"], 2);
    assert_eq!(same["tombstones"], 0);

    // An old bundle cannot bring back what was forgotten.
    let before = counts(&a);
    let old = validate_bundle(&cat, &cap, &mut a, "before").unwrap();
    assert_eq!(old["refused"], 2, "{old}");
    assert_eq!(old["adds"], 0);
    assert_eq!(counts(&a), before);
    let found = memz::retrieval::recall(&cat, &cap, &mut a, &Query::new("launch")).unwrap();
    assert!(found.items.iter().all(|i| i.id != doomed.id));
    assert!(
        found
            .items
            .iter()
            .all(|i| i.id != kept.id || i.content.contains("SQLite"))
    );

    // Names are confined to the state's exports and imports folders.
    assert_eq!(
        validate_bundle(&cat, &cap, &mut a, "../escape")
            .unwrap_err()
            .code,
        "invalid_filename"
    );
    assert_eq!(
        validate_bundle(&cat, &cap, &mut a, "missing")
            .unwrap_err()
            .code,
        "not_found"
    );
    let imports = state.path.join("imports");
    std::fs::create_dir_all(&imports).unwrap();
    std::fs::rename(
        state.path.join("exports").join("before.amx"),
        imports.join("dropped-in.amx"),
    )
    .unwrap();
    assert_eq!(
        validate_bundle(&cat, &cap, &mut b, "dropped-in").unwrap()["validated"],
        true
    );

    // Another session's private memory is not importable by this session.
    cat.bind_session("u", "other").unwrap();
    let other = cat.capability("u", "other").unwrap();
    remember(
        &cat,
        &other,
        &mut a,
        &Capture::text(Scope::session(&ns_a, "other"), "p", "their private note"),
    )
    .unwrap();
    export(&state, &cat, &other, &mut a, "theirs");
    let theirs = validate_bundle(&cat, &cap, &mut b, "theirs").unwrap();
    let conflicts = theirs["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 2, "the record and its source: {theirs}");
    assert!(
        conflicts
            .iter()
            .all(|c| c["reason"].as_str().unwrap().contains("session"))
    );
    assert_eq!(theirs["clean"], false);

    // The MCP tool previews the same plan, read-only.
    let via_tool = dispatch(
        &state,
        &client_b,
        &json!({"op":"validate_import","bundle":"after"}),
    )
    .unwrap();
    assert_eq!(
        via_tool,
        validate_bundle(&cat, &cap, &mut b, "after").unwrap()
    );
    // The JSONL validation mode still works.
    let cancel = AtomicBool::new(false);
    let jsonl = snapshot(&cat, &cap, &mut b, 1048576, &cancel)
        .unwrap()
        .jsonl;
    assert_eq!(
        dispatch(
            &state,
            &client_b,
            &json!({"op":"validate_import","jsonl":jsonl})
        )
        .unwrap()["validated"],
        true
    );
}
