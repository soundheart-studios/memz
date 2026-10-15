mod common;
use common::Fixture;
use memz::{
    adapters,
    auth::Scope,
    cache::Cache,
    capture::{Capture, remember},
    catalog::Catalog,
    export, jobs, lifecycle,
    retrieval::Query,
    state::StateRoot,
    store::NamespaceStore,
};
use std::sync::{Arc, Barrier, atomic::AtomicBool};
#[test]
fn correction_and_revocation_during_export_and_worker_publication_are_authoritative() {
    let f = Fixture::new("qualification");
    std::fs::create_dir(f.root.join("project")).unwrap();
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    let client = adapters::load_client(
        &adapters::initialize(&state, &f.root.join("project"), "u", "s").unwrap(),
    )
    .unwrap();
    let ns = client.namespace;
    let mut cat = Catalog::open(state.clone()).unwrap();
    let cap = cat.capability("u", "s").unwrap();
    let mut db = NamespaceStore::open(&state, &ns).unwrap();
    let old = remember(
        &cat,
        &cap,
        &mut db,
        &Capture::text(Scope::project(&ns), "old", "release old-secret"),
    )
    .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let lease = jobs::claim(&cat, &mut db, now, 30, false).unwrap().unwrap();
    let mut cache = Cache::new(8, 65536);
    let q = Query::new("release");
    assert_eq!(
        cache.recall(&cat, &cap, &mut db, &q).unwrap().items[0].content,
        "release old-secret"
    );
    let start = Arc::new(Barrier::new(2));
    let other = start.clone();
    let state2 = state.clone();
    let ns2 = ns.clone();
    let export_thread = std::thread::spawn(move || {
        let cat = Catalog::open(state2.clone()).unwrap();
        let cap = cat.capability("u", "s").unwrap();
        let mut db = NamespaceStore::open(&state2, &ns2).unwrap();
        other.wait();
        export::snapshot(&cat, &cap, &mut db, 1048576, &AtomicBool::new(false)).unwrap()
    });
    start.wait();
    let corrected =
        lifecycle::correct(&cat, &cap, &mut db, &old.id, 1, "release new-value").unwrap();
    let snapshot = export_thread.join().unwrap();
    assert!(snapshot.revision <= corrected.mutation_seq);
    assert_eq!(
        jobs::publish(&cat, &mut db, &lease, now, |_, _| panic!(
            "obsolete worker executed"
        ))
        .unwrap(),
        jobs::Publication::Obsolete
    );
    let fresh = cache.recall(&cat, &cap, &mut db, &q).unwrap();
    assert_eq!(fresh.items[0].content, "release new-value");
    assert!(fresh.mutation_seq >= corrected.mutation_seq);
    // The correction's job becomes claimable at its own timestamp, which can be
    // a later second than `now` on a slow runner; read the clock again.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let lease = jobs::claim(&cat, &mut db, now, 30, false).unwrap().unwrap();
    cat.revoke("u", &ns).unwrap();
    assert!(cache.recall(&cat, &cap, &mut db, &q).is_err());
    assert_eq!(
        jobs::publish(&cat, &mut db, &lease, now, |_, _| panic!(
            "revoked worker executed"
        ))
        .unwrap(),
        jobs::Publication::Obsolete
    );
    cat.grant("u", &ns, true).unwrap();
    let cap = cat.capability("u", "s").unwrap();
    lifecycle::forget(&cat, &cap, &mut db, &old.id, 2).unwrap();
    let view = export::snapshot(&cat, &cap, &mut db, 1048576, &AtomicBool::new(false)).unwrap();
    assert!(!view.jsonl.contains("old-secret") && !view.jsonl.contains("new-value"));
}
