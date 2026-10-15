mod common;
use common::Fixture;
use memz::{
    adapters::{initialize, load_client},
    auth::Scope,
    cache::Cache,
    capture::{Capture, remember},
    catalog::Catalog,
    health::{maintain, status},
    lifecycle::correct,
    retrieval::Query,
    state::StateRoot,
    store::NamespaceStore,
};
#[test]
fn caches_invalidate_and_explicit_expiry_preserves_durable_memory() {
    let f = Fixture::new("health");
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    std::fs::create_dir(f.root.join("project")).unwrap();
    let path = initialize(&state, &f.root.join("project"), "u", "s").unwrap();
    let client = load_client(&path).unwrap();
    let mut cat = Catalog::open(state.clone()).unwrap();
    let cap = cat.capability("u", "s").unwrap();
    let mut store = NamespaceStore::open(&state, &client.namespace).unwrap();
    let durable = Capture::text(
        Scope::project(&client.namespace),
        "durable",
        "sensitive blue",
    );
    let receipt = remember(&cat, &cap, &mut store, &durable).unwrap();
    let mut cache = Cache::new(2, 4096);
    let query = Query::new("blue");
    let first = cache.recall(&cat, &cap, &mut store, &query).unwrap();
    assert_eq!(first, cache.recall(&cat, &cap, &mut store, &query).unwrap());
    assert_eq!(cache.hits(), 1);
    remember(
        &cat,
        &cap,
        &mut store,
        &Capture::text(
            Scope::session(&client.namespace, "s"),
            "private-cache",
            "session-only needle",
        ),
    )
    .unwrap();
    cat.bind_session("u", "other-session").unwrap();
    let other = cat.capability("u", "other-session").unwrap();
    assert_eq!(
        cache
            .recall(&cat, &cap, &mut store, &Query::new("needle"))
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(
        cache
            .recall(&cat, &other, &mut store, &Query::new("needle"))
            .unwrap()
            .status,
        "empty"
    );
    correct(&cat, &cap, &mut store, &receipt.id, 1, "sensitive green").unwrap();
    assert_eq!(
        cache.recall(&cat, &cap, &mut store, &query).unwrap().status,
        "empty"
    );
    cat.revoke("u", &client.namespace).unwrap();
    assert!(cache.recall(&cat, &cap, &mut store, &query).is_err());
    cat.grant("u", &client.namespace, true).unwrap();
    let cap = cat.capability("u", "s").unwrap();
    let mut temporary = Capture::text(
        Scope::session(&client.namespace, "s"),
        "temporary",
        "temporary secret",
    );
    temporary.expires_at = Some(4102444800);
    remember(&cat, &cap, &mut store, &temporary).unwrap();
    store
        .conn
        .execute(
            "UPDATE sources SET expires_at=1 WHERE retention='temporary'",
            [],
        )
        .unwrap();
    let health = status(&cat, &cap, &store).unwrap();
    let serialized = serde_json::to_string(&health).unwrap();
    assert!(!serialized.contains("sensitive"));
    assert!(!serialized.contains("secret"));
    assert!(!serialized.contains(&client.binding.token));
    assert!(health.pending_jobs > 0);
    let maintenance = maintain(&cat, &cap, &mut store, 16).unwrap();
    assert_eq!(maintenance.expired, 1);
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT count(*) FROM sources WHERE retention='durable' AND status='active'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    assert_eq!(
        remember(&cat, &cap, &mut store, &temporary)
            .unwrap_err()
            .code,
        "deleted"
    );
    for word in ["green", "one", "two", "three"] {
        let _ = cache
            .recall(&cat, &cap, &mut store, &Query::new(word))
            .unwrap();
    }
    assert!(cache.entries() <= 2);
    assert!(cache.bytes() <= 4096);
    store
        .conn
        .execute("UPDATE namespace_meta SET quota_bytes=1", [])
        .unwrap();
    assert_eq!(
        remember(
            &cat,
            &cap,
            &mut store,
            &Capture::text(
                Scope::project(&client.namespace),
                "over-quota",
                "new content"
            )
        )
        .unwrap_err()
        .code,
        "quota"
    );
    assert_eq!(
        cache
            .recall(&cat, &cap, &mut store, &Query::new("green"))
            .unwrap()
            .items
            .len(),
        1
    );
}
