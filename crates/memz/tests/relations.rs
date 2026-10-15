mod common;
use common::Fixture;
use memz::{
    adapters::{initialize, load_client},
    auth::Scope,
    capture::{Capture, remember},
    catalog::Catalog,
    graph::{self, Anchor, Node, Relation},
    lifecycle::{correct, forget},
    providers::relations::LiteralExtractor,
    state::StateRoot,
    store::NamespaceStore,
};
use std::collections::BTreeMap;
fn setup() -> (Fixture, Catalog, NamespaceStore, String) {
    let f = Fixture::new("relations");
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    std::fs::create_dir(f.root.join("project")).unwrap();
    let client =
        load_client(&initialize(&state, &f.root.join("project"), "user", "session").unwrap())
            .unwrap();
    let mut cat = Catalog::open(state.clone()).unwrap();
    cat.bind_session("user", "other").unwrap();
    let store = NamespaceStore::open(&state, &client.namespace).unwrap();
    (f, cat, store, client.namespace)
}
fn item(
    cat: &Catalog,
    store: &mut NamespaceStore,
    scope: Scope,
    key: &str,
    text: &str,
    origin: &str,
) -> Node {
    let session = match &scope.visibility {
        memz::auth::Visibility::Session(s) => s.as_str(),
        _ => "session",
    };
    let cap = cat.capability("user", session).unwrap();
    let mut c = Capture::text(scope.clone(), key, text);
    c.origin = origin.into();
    c.kind = "decision".into();
    let r = remember(cat, &cap, store, &c).unwrap();
    Node {
        id: r.id,
        revision: r.revision,
    }
}
fn relation(store: &NamespaceStore, from: &Node, to: &Node, evidence: &Node) -> Relation {
    let (source,revision,length):(String,i64,usize)=store.conn.query_row("SELECT s.id,s.revision,length(CAST(s.text AS BLOB)) FROM items i JOIN sources s ON s.id=i.source_id AND s.revision=i.source_revision WHERE i.id=?1 AND i.revision=?2",rusqlite::params![evidence.id,evidence.revision],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    Relation {
        from: from.clone(),
        to: to.clone(),
        relation: "requires".into(),
        qualifiers: BTreeMap::from([("when".into(), "before deployment".into())]),
        evidence: vec![Anchor {
            source,
            revision,
            start: 0,
            end: length,
        }],
    }
}
#[test]
fn explicit_and_inferred_edges_preserve_evidence_without_authority_escalation() {
    let (_f, cat, mut store, ns) = setup();
    let a = item(
        &cat,
        &mut store,
        Scope::project(&ns),
        "a",
        "Deployment",
        "user",
    );
    let b = item(
        &cat,
        &mut store,
        Scope::project(&ns),
        "b",
        "Passing checks",
        "user",
    );
    let e = item(
        &cat,
        &mut store,
        Scope::project(&ns),
        "e",
        "Deployment requires passing checks.",
        "user",
    );
    let cap = cat.capability("user", "session").unwrap();
    let req = relation(&store, &a, &b, &e);
    let proposed = graph::propose(&cat, &cap, &mut store, &req).unwrap();
    for _ in 0..5 {
        assert_eq!(
            graph::propose(&cat, &cap, &mut store, &req).unwrap(),
            proposed
        );
    }
    assert!(!proposed.explicit);
    assert_eq!(
        proposed.evidence[0].quote,
        "Deployment requires passing checks."
    );
    assert_eq!(proposed.from, a);
    assert_eq!(proposed.to, b);
    assert_eq!(proposed.qualifiers, req.qualifiers);
    let explicit = graph::assert_explicit(&cat, &cap, &mut store, &req).unwrap();
    assert!(explicit.explicit);
    assert_ne!(explicit.id, proposed.id);
    let edges = graph::neighbors(&cat, &cap, &mut store, &b).unwrap();
    assert_eq!(edges.len(), 2);
    assert!(edges.iter().all(|x| x.from == a && x.to == b));
    let generated = item(
        &cat,
        &mut store,
        Scope::project(&ns),
        "generated",
        "Repeated agent assertion",
        "assistant",
    );
    let bad = relation(&store, &a, &b, &generated);
    assert!(graph::assert_explicit(&cat, &cap, &mut store, &bad).is_err());
    assert!(
        !graph::propose(&cat, &cap, &mut store, &bad)
            .unwrap()
            .explicit
    );
}
#[test]
fn incompatible_scope_stale_sources_and_bad_spans_fail_closed() {
    let (_f, mut cat, mut store, ns) = setup();
    let a = item(
        &cat,
        &mut store,
        Scope::project(&ns),
        "a",
        "Deployment",
        "user",
    );
    let b = item(&cat, &mut store, Scope::project(&ns), "b", "Checks", "user");
    let e = item(
        &cat,
        &mut store,
        Scope::project(&ns),
        "e",
        "Requires checks",
        "user",
    );
    let private = item(
        &cat,
        &mut store,
        Scope::session(&ns, "session"),
        "private",
        "Private",
        "user",
    );
    let other = item(
        &cat,
        &mut store,
        Scope::session(&ns, "other"),
        "other",
        "Other",
        "user",
    );
    let cap = cat.capability("user", "session").unwrap();
    for to in [
        &private,
        &other,
        &Node {
            id: "missing-or-other-namespace".into(),
            revision: 1,
        },
    ] {
        let req = relation(&store, &a, to, &e);
        assert!(graph::propose(&cat, &cap, &mut store, &req).is_err());
    }
    let req = relation(&store, &a, &b, &e);
    let mut bad = req.clone();
    bad.evidence[0].end = 99999;
    assert!(graph::propose(&cat, &cap, &mut store, &bad).is_err());
    graph::propose(&cat, &cap, &mut store, &req).unwrap();
    correct(
        &cat,
        &cap,
        &mut store,
        &e.id,
        1,
        "No longer requires checks",
    )
    .unwrap();
    assert!(
        graph::neighbors(&cat, &cap, &mut store, &a)
            .unwrap()
            .is_empty()
    );
    assert!(graph::propose(&cat, &cap, &mut store, &req).is_err());
    let e2 = Node {
        id: e.id.clone(),
        revision: 2,
    };
    let req2 = relation(&store, &a, &b, &e2);
    graph::propose(&cat, &cap, &mut store, &req2).unwrap();
    forget(&cat, &cap, &mut store, &b.id, 1).unwrap();
    assert!(
        graph::neighbors(&cat, &cap, &mut store, &a)
            .unwrap()
            .is_empty()
    );
    memz::lifecycle::cleanup(&mut store, 32).unwrap();
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT count(*) FROM edges WHERE status='active'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    cat.revoke("user", &ns).unwrap();
    assert!(graph::neighbors(&cat, &cap, &mut store, &a).is_err());
}
#[test]
fn optional_literal_extraction_is_selected_bounded_and_always_inferred() {
    let (_f, cat, mut store, ns) = setup();
    let a = item(
        &cat,
        &mut store,
        Scope::project(&ns),
        "a",
        "Deployment",
        "user",
    );
    let b = item(&cat, &mut store, Scope::project(&ns), "b", "Checks", "user");
    let text = format!("[[{}@1]] requires [[{}@1]]", a.id, b.id);
    let e = item(&cat, &mut store, Scope::project(&ns), "e", &text, "user");
    let cap = cat.capability("user", "session").unwrap();
    let extractor = LiteralExtractor;
    assert!(!extractor.selection("assistant", "decision", &text).selected);
    assert!(!extractor.selection("user", "fact", &text).selected);
    assert!(
        !extractor
            .selection("user", "decision", &"x".repeat(8193))
            .selected
    );
    assert!(extractor.selection("user", "decision", &text).selected);
    assert_eq!(
        graph::enqueue_extraction(&cat, &cap, &mut store, None).unwrap(),
        0
    );
    assert_eq!(
        graph::enqueue_extraction(&cat, &cap, &mut store, Some(&extractor)).unwrap(),
        1
    );
    assert_eq!(
        graph::enqueue_extraction(&cat, &cap, &mut store, Some(&extractor)).unwrap(),
        0
    );
    assert!(graph::extract_one(&cat, &cap, &mut store, &extractor, false).unwrap());
    let edges = graph::neighbors(&cat, &cap, &mut store, &a).unwrap();
    assert_eq!(edges.len(), 1);
    assert!(!edges[0].explicit);
    assert_eq!(edges[0].evidence[0].quote, text);
    assert_eq!(edges[0].evidence[0].method, "literal-v1");
    assert_eq!(edges[0].from, a);
    assert_eq!(edges[0].to, b);
    let anchor = relation(&store, &a, &b, &e).evidence.remove(0);
    assert!(
        extractor
            .extract(&anchor, &format!("{}\n", text).repeat(17))
            .is_err()
    );
}

#[test]
fn extraction_leases_do_not_mix_with_embeddings_or_publish_changed_sources() {
    let (_f, cat, mut store, ns) = setup();
    let a = item(&cat, &mut store, Scope::project(&ns), "a", "Alpha", "user");
    let b = item(&cat, &mut store, Scope::project(&ns), "b", "Beta", "user");
    let text = format!("[[{}@1]] supports [[{}@1]]", a.id, b.id);
    let source = item(&cat, &mut store, Scope::project(&ns), "s", &text, "user");
    let cap = cat.capability("user", "session").unwrap();
    let provider = LiteralExtractor;
    graph::enqueue_extraction(&cat, &cap, &mut store, Some(&provider)).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let lease = memz::jobs::claim_relations(&cat, &mut store, now, 30, false, &cap)
        .unwrap()
        .unwrap();
    assert_eq!(lease.text, text);
    let embedding = memz::jobs::claim(&cat, &mut store, now, 30, false)
        .unwrap()
        .unwrap();
    assert_ne!(embedding.operation_key(), lease.operation_key());
    correct(
        &cat,
        &cap,
        &mut store,
        &source.id,
        1,
        "withdrawn annotation",
    )
    .unwrap();
    assert_eq!(
        memz::jobs::publish(&cat, &mut store, &lease, now, |_, _| panic!(
            "obsolete extraction published"
        ))
        .unwrap(),
        memz::jobs::Publication::Obsolete
    );
    assert!(
        graph::neighbors(&cat, &cap, &mut store, &a)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn model_surface_cannot_request_explicit_authority() {
    let (_f, cat, mut store, ns) = setup();
    let a = item(&cat, &mut store, Scope::project(&ns), "a", "A", "user");
    let b = item(&cat, &mut store, Scope::project(&ns), "b", "B", "user");
    let e = item(
        &cat,
        &mut store,
        Scope::project(&ns),
        "e",
        "A requires B",
        "user",
    );
    let req = relation(&store, &a, &b, &e);
    let client = memz::adapters::Client {
        binding: memz::daemon::Binding {
            token: "unused".into(),
            principal: "user".into(),
            session: "session".into(),
        },
        namespace: ns,
    };
    let mut forged = serde_json::to_value(&req).unwrap();
    forged["explicit"] = serde_json::json!(true);
    assert!(
        memz::adapters::dispatch(
            &cat.state,
            &client,
            &serde_json::json!({"op":"relate","relationship":forged})
        )
        .is_err()
    );
    assert!(
        memz::adapters::dispatch(
            &cat.state,
            &client,
            &serde_json::json!({"op":"assert_relation","relationship":req})
        )
        .is_err()
    );
    let p = memz::adapters::dispatch(
        &cat.state,
        &client,
        &serde_json::json!({"op":"relate","relationship":req}),
    )
    .unwrap();
    assert_eq!(p["explicit"], false);
    let tools = memz::adapters::mcp(
        &cat.state,
        &client,
        &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
    );
    assert!(
        !tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "assert_relation")
    );
}
