mod common;
use common::Fixture;
use memz::{
    adapters::{initialize, load_client},
    auth::Scope,
    capture::{Capture, remember},
    catalog::Catalog,
    export::{publish, snapshot},
    import::validate,
    lifecycle::forget,
    state::StateRoot,
    store::NamespaceStore,
};
use std::sync::atomic::AtomicBool;
#[test]
fn exports_are_deterministic_scoped_safe_and_cancellable() {
    let f = Fixture::new("exports");
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    std::fs::create_dir(f.root.join("project")).unwrap();
    let path = initialize(&state, &f.root.join("project"), "u", "s").unwrap();
    let client = load_client(&path).unwrap();
    let mut cat = Catalog::open(state.clone()).unwrap();
    let cap = cat.capability("u", "s").unwrap();
    let mut store = NamespaceStore::open(&state, &client.namespace).unwrap();
    let request = Capture::text(
        Scope::project(&client.namespace),
        "export",
        "<script>bad()</script> [link](javascript:bad)",
    );
    let receipt = remember(&cat, &cap, &mut store, &request).unwrap();
    cat.bind_session("u", "private").unwrap();
    let private = cat.capability("u", "private").unwrap();
    remember(
        &cat,
        &private,
        &mut store,
        &Capture::text(
            Scope::session(&client.namespace, "private"),
            "private",
            "hidden-secret",
        ),
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    let export = snapshot(&cat, &cap, &mut store, 1048576, &cancel).unwrap();
    assert_eq!(
        export,
        snapshot(&cat, &cap, &mut store, 1048576, &cancel).unwrap()
    );
    assert!(!export.jsonl.contains("hidden-secret"));
    assert!(!export.markdown.contains("<script>"));
    assert!(export.markdown.contains("&lt;script&gt;"));
    assert!(validate(&cat, &cap, &mut store, &export.jsonl, 1048576).is_ok());
    assert!(
        validate(
            &cat,
            &cap,
            &mut store,
            &export.jsonl.replace("bad()", "changed"),
            1048576
        )
        .is_err()
    );
    assert!(publish(&state, "../escape", &export, &cancel).is_err());
    let paths = publish(&state, "snapshot", &export, &cancel).unwrap();
    assert!(paths.0.exists() && paths.1.exists());
    assert!(publish(&state, "snapshot", &export, &cancel).is_err());
    assert!(snapshot(&cat, &cap, &mut store, 10, &cancel).is_err());
    assert!(snapshot(&cat, &cap, &mut store, 1048576, &AtomicBool::new(true)).is_err());
    forget(&cat, &cap, &mut store, &receipt.id, 1).unwrap();
    assert!(validate(&cat, &cap, &mut store, &export.jsonl, 1048576).is_err());
    assert!(
        !snapshot(&cat, &cap, &mut store, 1048576, &cancel)
            .unwrap()
            .jsonl
            .contains("bad()")
    );
}
