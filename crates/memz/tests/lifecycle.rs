mod common;
use common::Fixture;
use memz::{
    auth::Scope,
    capture::{Capture, remember},
    catalog::Catalog,
    inspect::inspect,
    lifecycle::{cleanup, correct, forget},
    retrieval::{Query, recall},
    state::StateRoot,
    store::NamespaceStore,
};
#[test]
fn correction_and_forgetting_invalidate_stale_content_and_retries() {
    let f = Fixture::new("lifecycle");
    let mut cat = Catalog::open(StateRoot::open(&f.root.join("state")).unwrap()).unwrap();
    std::fs::create_dir(f.root.join("project")).unwrap();
    let ns = cat.register(&f.root.join("project")).unwrap();
    cat.bind_session("user", "session").unwrap();
    cat.grant("user", &ns, true).unwrap();
    let cap = cat.capability("user", "session").unwrap();
    let mut store = NamespaceStore::open(&cat.state, &ns).unwrap();
    let request = Capture::text(Scope::project(&ns), "original", "secret old blue");
    let original = remember(&cat, &cap, &mut store, &request).unwrap();
    store.conn.execute("INSERT INTO items SELECT 'derived',revision,kind,'secret derivative',0,status,source_id,source_revision,visibility,session_id,valid_from,valid_to,slot FROM items WHERE id=?1",[&original.id]).unwrap();
    store
        .conn
        .execute(
            "INSERT INTO memory_fts VALUES('derived',1,'secret derivative')",
            [],
        )
        .unwrap();
    let changed = correct(&cat, &cap, &mut store, &original.id, 1, "current green").unwrap();
    assert_eq!(changed.revision, 2);
    assert!(changed.mutation_seq > original.mutation_seq);
    assert_eq!(
        correct(&cat, &cap, &mut store, &original.id, 1, "stale")
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(
        recall(&cat, &cap, &mut store, &Query::new("secret"))
            .unwrap()
            .status,
        "empty"
    );
    let mut q = Query::new("green");
    q.min_revision = Some(changed.mutation_seq);
    assert_eq!(
        recall(&cat, &cap, &mut store, &q).unwrap().items[0].revision,
        2
    );
    let history = inspect(&cat, &cap, &mut store, &original.id, true, 8000).unwrap();
    assert_eq!(history.items.len(), 2);
    assert_eq!(
        forget(&cat, &cap, &mut store, &original.id, 1)
            .unwrap_err()
            .code,
        "conflict"
    );
    let deleted = forget(&cat, &cap, &mut store, &original.id, 2).unwrap();
    assert!(deleted > changed.mutation_seq);
    assert_eq!(recall(&cat, &cap, &mut store, &q).unwrap().status, "empty");
    assert!(
        inspect(&cat, &cap, &mut store, &original.id, true, 8000)
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(
        remember(&cat, &cap, &mut store, &request).unwrap_err().code,
        "deleted"
    );
    for _ in 0..30 {
        if cleanup(&mut store, 1).unwrap() == 0 {
            break;
        }
    }
    for (table, col) in [
        ("sources", "text"),
        ("items", "content"),
        ("memory_fts", "content"),
    ] {
        let count: i64 = store
            .conn
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE {col}<>''"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
    assert_eq!(
        store
            .conn
            .query_row("SELECT count(*) FROM jobs", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    let tomb: (String, bool) = store
        .conn
        .query_row("SELECT payload_digest,tombstone FROM operations", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(tomb, (String::new(), true));
    assert_eq!(
        remember(&cat, &cap, &mut store, &request).unwrap_err().code,
        "deleted"
    );
}
