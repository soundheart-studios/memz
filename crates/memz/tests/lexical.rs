mod common;
use common::Fixture;
use memz::{
    auth::Scope,
    capture::{Capture, remember},
    catalog::Catalog,
    inspect::inspect,
    retrieval::{Query, recall},
    state::StateRoot,
    store::NamespaceStore,
};
#[test]
fn lexical_scope_current_slots_and_whole_packet_bounds() {
    let f = Fixture::new("lexical");
    let mut cat = Catalog::open(StateRoot::open(&f.root.join("state")).unwrap()).unwrap();
    std::fs::create_dir(f.root.join("project")).unwrap();
    let ns = cat.register(&f.root.join("project")).unwrap();
    cat.bind_session("user", "one").unwrap();
    cat.grant("user", &ns, true).unwrap();
    let cap = cat.capability("user", "one").unwrap();
    let mut store = NamespaceStore::open(&cat.state, &ns).unwrap();
    let mut request = Capture::text(Scope::project(&ns), "first", "door is blue");
    request.slot = Some("door-color".into());
    let first = remember(&cat, &cap, &mut store, &request).unwrap();
    request.request_id = "second".into();
    request.text = "door is green".into();
    let second = remember(&cat, &cap, &mut store, &request).unwrap();
    request.request_id = "private".into();
    request.text = "door hidden private".into();
    request.slot = None;
    request.scope = Scope::session(&ns, "one");
    remember(&cat, &cap, &mut store, &request).unwrap();
    let mut q = Query::new("door");
    let packet = recall(&cat, &cap, &mut store, &q).unwrap();
    assert_eq!(packet.status, "ok");
    assert_eq!(packet.items.len(), 2);
    assert!(packet.items.iter().any(|i| i.id == second.id));
    assert!(!packet.items.iter().any(|i| i.id == first.id));
    assert_eq!(packet, recall(&cat, &cap, &mut store, &q).unwrap());
    q.max_bytes = 650;
    q.max_tokens = 650;
    let bounded = recall(&cat, &cap, &mut store, &q).unwrap();
    assert!(serde_json::to_vec(&bounded).unwrap().len() <= 650);
    assert_eq!(bounded.status, "partial");
    assert!(bounded.omitted > 0);
    q = Query::new("nothingmatches");
    assert_eq!(recall(&cat, &cap, &mut store, &q).unwrap().status, "empty");
    q.min_revision = Some(999);
    assert_eq!(
        recall(&cat, &cap, &mut store, &q).unwrap_err().code,
        "revision_unavailable"
    );
    cat.bind_session("user", "two").unwrap();
    let other = cat.capability("user", "two").unwrap();
    let packet = recall(&cat, &other, &mut store, &Query::new("door")).unwrap();
    assert_eq!(packet.items.len(), 1);
    request.scope = Scope::project(&ns);
    request.origin = "assistant".into();
    request.slot = Some("unresolved".into());
    request.request_id = "conflict-a".into();
    request.text = "uncertain answer blue".into();
    remember(&cat, &cap, &mut store, &request).unwrap();
    request.request_id = "conflict-b".into();
    request.text = "uncertain answer red".into();
    remember(&cat, &cap, &mut store, &request).unwrap();
    let mut conflict_query = Query::new("uncertain");
    conflict_query.max_tokens = 8000;
    let conflict = recall(&cat, &cap, &mut store, &conflict_query).unwrap();
    assert!(conflict.conflicts);
    assert_eq!(conflict.items.len(), 2);
    let detail = inspect(&cat, &cap, &mut store, &first.id, true, 8000).unwrap();
    assert_eq!(detail.items[0].content, "door is blue");
    assert!(detail.history);
    store.conn.execute("UPDATE sources SET status='deleted' WHERE id=(SELECT source_id FROM items WHERE id=?1)",[&second.id]).unwrap();
    let packet = recall(&cat, &cap, &mut store, &Query::new("green")).unwrap();
    assert_eq!(packet.status, "empty");
    store.conn.execute("DROP TABLE memory_fts", []).unwrap();
    assert_eq!(
        recall(&cat, &cap, &mut store, &Query::new("door"))
            .unwrap_err()
            .code,
        "unavailable"
    );
}
