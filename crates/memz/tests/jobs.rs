mod common;
use common::Fixture;
use memz::{
    adapters::{initialize, load_client},
    auth::Scope,
    capture::{Capture, remember},
    catalog::Catalog,
    jobs::{Publication, WorkerPool, claim, fail, publish},
    lifecycle::{correct, forget},
    retrieval::{Query, recall},
    state::StateRoot,
    store::NamespaceStore,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
fn setup() -> (Fixture, Catalog, NamespaceStore, String) {
    let f = Fixture::new("jobs");
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    std::fs::create_dir(f.root.join("project")).unwrap();
    let file = initialize(&state, &f.root.join("project"), "user", "session").unwrap();
    let client = load_client(&file).unwrap();
    let cat = Catalog::open(state.clone()).unwrap();
    let store = NamespaceStore::open(&state, &client.namespace).unwrap();
    (f, cat, store, client.namespace)
}
fn timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
#[test]
fn restart_reclaims_and_publication_is_atomic_and_idempotent() {
    let (_f, cat, mut store, ns) = setup();
    let cap = cat.capability("user", "session").unwrap();
    remember(
        &cat,
        &cap,
        &mut store,
        &Capture::text(Scope::project(&ns), "one", "recoverable memory"),
    )
    .unwrap();
    let time = timestamp();
    let old = claim(&cat, &mut store, time, 5, false).unwrap().unwrap();
    assert!(claim(&cat, &mut store, time, 5, false).unwrap().is_none());
    drop(store);
    let mut store = NamespaceStore::open(&cat.state, &ns).unwrap();
    let reclaimed = claim(&cat, &mut store, time + 6, 5, false)
        .unwrap()
        .unwrap();
    assert_eq!(old.operation_key(), reclaimed.operation_key());
    assert_eq!(
        publish(&cat, &mut store, &old, time + 6, |_, _| panic!(
            "stale publisher"
        ))
        .unwrap(),
        Publication::LostLease
    );
    store
        .conn
        .execute("CREATE TABLE publication_receipt(id TEXT PRIMARY KEY)", [])
        .unwrap();
    assert_eq!(
        publish(&cat, &mut store, &reclaimed, time + 7, |tx, job| {
            tx.execute(
                "INSERT INTO publication_receipt VALUES(?1)",
                [job.operation_key()],
            )?;
            Ok(())
        })
        .unwrap(),
        Publication::Published
    );
    assert_eq!(
        publish(&cat, &mut store, &reclaimed, time + 7, |_, _| panic!(
            "duplicate publication"
        ))
        .unwrap(),
        Publication::AlreadyPublished
    );
    assert_eq!(
        store
            .conn
            .query_row("SELECT count(*) FROM publication_receipt", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[test]
fn changed_deleted_or_revoked_inputs_cannot_publish() {
    for action in ["correct", "forget", "revoke"] {
        let (_f, mut cat, mut store, ns) = setup();
        let cap = cat.capability("user", "session").unwrap();
        let receipt = remember(
            &cat,
            &cap,
            &mut store,
            &Capture::text(Scope::project(&ns), "one", "old source"),
        )
        .unwrap();
        let time = timestamp();
        let lease = claim(&cat, &mut store, time, 30, false).unwrap().unwrap();
        match action {
            "correct" => {
                correct(&cat, &cap, &mut store, &receipt.id, 1, "new source").unwrap();
            }
            "forget" => {
                forget(&cat, &cap, &mut store, &receipt.id, 1).unwrap();
            }
            _ => cat.revoke("user", &ns).unwrap(),
        };
        assert_eq!(
            publish(&cat, &mut store, &lease, time + 1, |_, _| panic!(
                "obsolete source published"
            ))
            .unwrap(),
            Publication::Obsolete
        );
    }
}
#[test]
fn failures_back_off_preserve_lexical_content_and_pressure_yields() {
    let (_f, cat, mut store, ns) = setup();
    let cap = cat.capability("user", "session").unwrap();
    remember(
        &cat,
        &cap,
        &mut store,
        &Capture::text(Scope::project(&ns), "one", "searchable despite failure"),
    )
    .unwrap();
    let time = timestamp();
    assert!(claim(&cat, &mut store, time, 30, true).unwrap().is_none());
    for attempt in 0..3 {
        let at = time + attempt * 100;
        let lease = claim(&cat, &mut store, at, 30, false).unwrap().unwrap();
        fail(&mut store, &lease, at, "provider_unavailable").unwrap();
        assert!(claim(&cat, &mut store, at, 30, false).unwrap().is_none());
    }
    assert!(
        claim(&cat, &mut store, time + 1000, 30, false)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .conn
            .query_row("SELECT state FROM jobs", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "failed"
    );
    assert_eq!(
        recall(&cat, &cap, &mut store, &Query::new("searchable"))
            .unwrap()
            .items
            .len(),
        1
    );
}
#[test]
fn timeout_does_not_release_resource_permits_before_worker_exit() {
    let pool = WorkerPool::new(1, 1024).unwrap();
    let release = Arc::new(AtomicBool::new(false));
    let done = release.clone();
    let handle = pool
        .start(512, move || {
            while !done.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(2));
            }
            42
        })
        .unwrap();
    assert!(handle.wait(Duration::from_millis(5)).is_err());
    assert_eq!(pool.active(), 1);
    assert!(pool.start(1, || 0).is_err());
    release.store(true, Ordering::SeqCst);
    for _ in 0..100 {
        if pool.active() == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(pool.active(), 0);
    assert_eq!(
        pool.start(1, || 7)
            .unwrap()
            .wait(Duration::from_secs(1))
            .unwrap(),
        7
    );
    assert!(pool.start(2048, || 0).is_err());
}
