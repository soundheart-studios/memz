mod common;
use common::Fixture;
use memz::{
    adapters::{initialize, load_client},
    auth::Scope,
    capture::{Capture, remember},
    catalog::Catalog,
    graph::{self, Anchor, GraphOptions, Node, Relation},
    inspect::inspect_with_options,
    lifecycle::{correct, forget},
    retrieval::{Query, recall},
    state::StateRoot,
    store::NamespaceStore,
};
use std::{collections::BTreeMap, time::Duration};
fn setup() -> (Fixture, Catalog, NamespaceStore, String) {
    let f = Fixture::new("graph-recall");
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    std::fs::create_dir(f.root.join("project")).unwrap();
    let client =
        load_client(&initialize(&state, &f.root.join("project"), "u", "s").unwrap()).unwrap();
    let mut cat = Catalog::open(state.clone()).unwrap();
    cat.bind_session("u", "other").unwrap();
    let store = NamespaceStore::open(&state, &client.namespace).unwrap();
    (f, cat, store, client.namespace)
}
fn item(cat: &Catalog, store: &mut NamespaceStore, ns: &str, key: &str, text: &str) -> Node {
    let cap = cat.capability("u", "s").unwrap();
    let r = remember(
        cat,
        &cap,
        store,
        &Capture::text(Scope::project(ns), key, text),
    )
    .unwrap();
    Node {
        id: r.id,
        revision: r.revision,
    }
}
fn link(cat: &Catalog, store: &mut NamespaceStore, a: &Node, b: &Node, e: &Node) -> graph::Edge {
    let cap = cat.capability("u", "s").unwrap();
    let anchor=store.conn.query_row("SELECT s.id,s.revision,length(CAST(s.text AS BLOB)) FROM items i JOIN sources s ON s.id=i.source_id AND s.revision=i.source_revision WHERE i.id=?1 AND i.revision=?2",rusqlite::params![e.id,e.revision],|r|Ok(Anchor{source:r.get(0)?,revision:r.get(1)?,start:0,end:r.get(2)?})).unwrap();
    graph::assert_explicit(
        cat,
        &cap,
        store,
        &Relation {
            from: a.clone(),
            to: b.clone(),
            relation: "requires".into(),
            qualifiers: BTreeMap::new(),
            evidence: vec![anchor],
        },
    )
    .unwrap()
}
fn query(text: &str) -> Query {
    let mut q = Query::new(text);
    q.max_tokens = 8000;
    q.max_items = 128;
    q.graph = Some(GraphOptions::new(vec!["requires".into()]));
    q
}
#[test]
fn traversal_preserves_direction_exact_evidence_and_correction_impact() {
    let (_f, cat, mut store, ns) = setup();
    let a = item(&cat, &mut store, &ns, "a", "Launch readiness");
    let b = item(&cat, &mut store, &ns, "b", "Passing checks");
    let e = item(
        &cat,
        &mut store,
        &ns,
        "e",
        "Launching requires passing checks.",
    );
    let edge = link(&cat, &mut store, &a, &b, &e);
    let cap = cat.capability("u", "s").unwrap();
    let packet = recall(&cat, &cap, &mut store, &query("readiness")).unwrap();
    let related = packet.items.iter().find(|i| i.id == b.id).unwrap();
    assert_eq!(related.via[0].edge, edge);
    assert_eq!(related.via[0].direction, "forward");
    assert_eq!(
        related.via[0].edge.evidence[0].quote,
        "Launching requires passing checks."
    );
    assert!(related.via[0].edge.attribution_only);
    let reverse = recall(&cat, &cap, &mut store, &query("checks")).unwrap();
    let related = reverse.items.iter().find(|i| i.id == a.id).unwrap();
    assert_eq!(related.via[0].direction, "reverse");
    assert_eq!(related.via[0].edge.from, a);
    assert_eq!(related.via[0].edge.to, b);
    let preview =
        inspect_with_options(&cat, &cap, &mut store, &e.id, false, 8000, None, true).unwrap();
    assert_eq!(preview.impact.as_ref().unwrap().edges[0].id, edge.id);
    correct(&cat, &cap, &mut store, &e.id, 1, "No requirement remains").unwrap();
    assert!(
        !recall(&cat, &cap, &mut store, &query("readiness"))
            .unwrap()
            .items
            .iter()
            .any(|i| i.id == b.id)
    );
    let e2 = Node {
        id: e.id,
        revision: 2,
    };
    link(&cat, &mut store, &a, &b, &e2);
    forget(&cat, &cap, &mut store, &b.id, 1).unwrap();
    assert_eq!(
        recall(&cat, &cap, &mut store, &query("readiness"))
            .unwrap()
            .items
            .len(),
        1
    );
}
#[test]
fn cycles_high_degree_deadlines_and_packet_limits_are_bounded() {
    let (_f, cat, mut store, ns) = setup();
    let a = item(&cat, &mut store, &ns, "a", "Graphseed");
    let b = item(&cat, &mut store, &ns, "b", "B");
    let c = item(&cat, &mut store, &ns, "c", "C");
    let d = item(&cat, &mut store, &ns, "d", "D");
    let e = item(&cat, &mut store, &ns, "e", "Requirements");
    link(&cat, &mut store, &a, &b, &e);
    link(&cat, &mut store, &b, &c, &e);
    link(&cat, &mut store, &c, &d, &e);
    link(&cat, &mut store, &c, &a, &e);
    let far = item(&cat, &mut store, &ns, "far", "Beyondhoplimit");
    link(&cat, &mut store, &d, &far, &e);
    let cap = cat.capability("u", "s").unwrap();
    let p = recall(&cat, &cap, &mut store, &query("Graphseed")).unwrap();
    assert!(!p.items.iter().any(|i| i.id == far.id));
    assert!(p.items.iter().all(|i| i.via.len() <= 2));
    assert_eq!(
        p.items
            .iter()
            .map(|i| &i.id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        p.items.len()
    );
    for n in 0..70 {
        let x = item(
            &cat,
            &mut store,
            &ns,
            &format!("fan{n}"),
            &format!("Neighbor {n}"),
        );
        link(&cat, &mut store, &a, &x, &e);
    }
    let mut q = query("Graphseed");
    q.graph.as_mut().unwrap().max_added_nodes = 3;
    let p = recall(&cat, &cap, &mut store, &q).unwrap();
    assert!(p.graph.as_ref().unwrap().added_nodes <= 3);
    assert!(p.graph.as_ref().unwrap().partial);
    assert_eq!(p.status, "partial");
    assert!(serde_json::to_vec(&p).unwrap().len() <= 8000);
    q.graph.as_mut().unwrap().timeout = Duration::ZERO;
    let p = recall(&cat, &cap, &mut store, &q).unwrap();
    assert_eq!(p.items.len(), 1);
    assert!(p.graph.unwrap().limits.contains(&"deadline".into()));
    let q = query("Graphseed");
    let p = recall(&cat, &cap, &mut store, &q).unwrap();
    assert!(p.graph.as_ref().unwrap().added_nodes <= 64);
    assert!(p.graph.as_ref().unwrap().scanned_edges <= 512);
    assert!(serde_json::to_vec(&p).unwrap().len() <= 8000);
    let impact =
        inspect_with_options(&cat, &cap, &mut store, &a.id, false, 2000, None, true).unwrap();
    assert!(serde_json::to_vec(&impact).unwrap().len() <= 2000);
    assert!(impact.impact.as_ref().unwrap().partial);
    assert!(impact.impact.as_ref().unwrap().edges.len() <= 64);
}
#[test]
fn graph_options_do_not_reuse_lexical_cache_or_cross_scopes() {
    let (_f, mut cat, mut store, ns) = setup();
    let a = item(&cat, &mut store, &ns, "a", "Rootword");
    let b = item(&cat, &mut store, &ns, "b", "Neighbor");
    let e = item(&cat, &mut store, &ns, "e", "A requires B");
    link(&cat, &mut store, &a, &b, &e);
    let cap = cat.capability("u", "s").unwrap();
    let mut cache = memz::cache::Cache::new(10, 100000);
    assert_eq!(
        cache
            .recall(&cat, &cap, &mut store, &Query::new("Rootword"))
            .unwrap()
            .items
            .len(),
        1
    );
    assert!(
        cache
            .recall(&cat, &cap, &mut store, &query("Rootword"))
            .unwrap()
            .items
            .iter()
            .any(|i| i.id == b.id)
    );
    let other = cat.capability("u", "other").unwrap();
    let private = remember(
        &cat,
        &other,
        &mut store,
        &Capture::text(Scope::session(&ns, "other"), "private", "Hidden neighbor"),
    )
    .unwrap();
    // Deliberately insert a malformed cross-scope edge to verify read-time defense too.
    store.conn.execute("INSERT INTO edges SELECT 'bad',revision,from_id,from_revision,?1,1,relation,qualifiers,explicit,status,visibility,session_id,valid_from,valid_to FROM edges LIMIT 1",[&private.id]).unwrap();
    store.conn.execute("INSERT INTO evidence(edge_id,edge_revision,source_id,source_revision,span_start,span_end,role,method,method_version) SELECT 'bad',1,source_id,source_revision,span_start,span_end,role,method,method_version FROM evidence WHERE edge_id IS NOT NULL LIMIT 1",[]).unwrap();
    assert!(
        !recall(&cat, &cap, &mut store, &query("Rootword"))
            .unwrap()
            .items
            .iter()
            .any(|i| i.id == private.id)
    );
    cat.revoke("u", &ns).unwrap();
    assert!(
        cache
            .recall(&cat, &cap, &mut store, &query("Rootword"))
            .is_err()
    );
}

#[test]
fn mcp_graph_and_impact_share_the_complete_envelope_budget() {
    let (_f, cat, mut store, ns) = setup();
    let a = item(&cat, &mut store, &ns, "a", "Budgetseed");
    let b = item(&cat, &mut store, &ns, "b", "Dependency");
    let e = item(
        &cat,
        &mut store,
        &ns,
        "e",
        "Budgetseed requires Dependency.",
    );
    let edge = link(&cat, &mut store, &a, &b, &e);
    let file = std::fs::read_dir(cat.state.path.join("run"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("client-")
        })
        .unwrap();
    let client = load_client(&file).unwrap();
    let _service = memz::adapters::serve_with_model(&cat.state, None).unwrap();
    let request = |name: &str, args| serde_json::json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":name,"arguments":args}});
    let result = memz::adapters::mcp(
        &cat.state,
        &client,
        &request(
            "recall",
            serde_json::json!({"query":"Budgetseed","graph":true,"relations":["requires"],"budget":8000}),
        ),
    );
    assert!(serde_json::to_vec(&result).unwrap().len() <= 8000);
    let packet = &result["result"]["structuredContent"];
    assert!(
        packet["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["id"] == b.id && i["via"][0]["edge"]["id"] == edge.id)
    );
    let small = memz::adapters::mcp(
        &cat.state,
        &client,
        &request(
            "recall",
            serde_json::json!({"query":"Budgetseed","graph":true,"relations":["requires"],"budget":2000}),
        ),
    );
    assert!(serde_json::to_vec(&small).unwrap().len() <= 2000);
    assert_eq!(small["result"]["structuredContent"]["status"], "partial");
    let impact = memz::adapters::mcp(
        &cat.state,
        &client,
        &request(
            "inspect",
            serde_json::json!({"id":e.id,"impact":true,"budget":8000}),
        ),
    );
    assert_eq!(
        impact["result"]["structuredContent"]["impact"]["edges"][0]["id"],
        edge.id
    );
    let bad = memz::adapters::mcp(
        &cat.state,
        &client,
        &request(
            "recall",
            serde_json::json!({"query":"Budgetseed","graph":true,"relations":["invented"],"budget":8000}),
        ),
    );
    assert_eq!(bad["result"]["isError"], true);
}

#[test]
fn correction_preview_is_scoped_and_does_not_mutate_or_retarget_edges() {
    let (_f, cat, mut store, ns) = setup();
    let a = item(&cat, &mut store, &ns, "a", "Previewseed");
    let b = item(&cat, &mut store, &ns, "b", "Dependency");
    let e = item(
        &cat,
        &mut store,
        &ns,
        "e",
        "Previewseed requires Dependency.",
    );
    link(&cat, &mut store, &a, &b, &e);
    let cap = cat.capability("u", "s").unwrap();
    let preview =
        inspect_with_options(&cat, &cap, &mut store, &a.id, false, 8000, None, true).unwrap();
    assert_eq!(
        preview.impact.unwrap().edges[0].dependency,
        "endpoint_revision"
    );
    let before = recall(&cat, &cap, &mut store, &query("Previewseed")).unwrap();
    let again = recall(&cat, &cap, &mut store, &query("Previewseed")).unwrap();
    assert_eq!(before, again);
    correct(&cat, &cap, &mut store, &b.id, 1, "Updated dependency").unwrap();
    let after = recall(&cat, &cap, &mut store, &query("Previewseed")).unwrap();
    // The supporting source still matches lexically; only the obsolete graph neighbor disappears.
    assert!(!after.items.iter().any(|i| i.id == b.id));
    assert!(after.items.iter().all(|i| i.via.is_empty()));
    assert!(
        inspect_with_options(
            &cat,
            &cap,
            &mut store,
            &a.id,
            true,
            8000,
            Some(GraphOptions::new(vec!["requires".into()])),
            false
        )
        .is_err()
    );
}

#[test]
fn impact_lists_current_derivatives_without_disclosing_private_items() {
    let (_f, cat, mut store, ns) = setup();
    let source = item(&cat, &mut store, &ns, "source", "Original evidence");
    let derived = item(&cat, &mut store, &ns, "derived", "Derived output");
    let cap = cat.capability("u", "s").unwrap();
    let (sid, srev): (String, i64) = store
        .conn
        .query_row(
            "SELECT source_id,source_revision FROM items WHERE id=?1",
            [&source.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    store.conn.execute("INSERT INTO evidence(item_id,item_revision,source_id,source_revision,span_start,span_end,role,method,method_version) VALUES(?1,1,?2,?3,0,17,'supports','fixture','1')",rusqlite::params![derived.id,sid,srev]).unwrap();
    let other = cat.capability("u", "other").unwrap();
    let hidden = remember(
        &cat,
        &other,
        &mut store,
        &Capture::text(
            Scope::session(&ns, "other"),
            "hidden",
            "Private derived output",
        ),
    )
    .unwrap();
    store.conn.execute("INSERT INTO evidence(item_id,item_revision,source_id,source_revision,span_start,span_end,role,method,method_version) VALUES(?1,1,?2,?3,0,17,'supports','fixture','1')",rusqlite::params![hidden.id,sid,srev]).unwrap();
    let preview =
        inspect_with_options(&cat, &cap, &mut store, &source.id, false, 8000, None, true).unwrap();
    assert_eq!(preview.impact.as_ref().unwrap().items, vec![derived]);
    assert!(
        !serde_json::to_string(&preview)
            .unwrap()
            .contains(&hidden.id)
    );
}

#[test]
fn daemon_literal_extraction_is_opt_in_and_obsolete_after_correction() {
    let (_f, cat, mut store, ns) = setup();
    let a = item(&cat, &mut store, &ns, "a", "Automatic root");
    let b = item(&cat, &mut store, &ns, "b", "Automatic dependency");
    let cap = cat.capability("u", "s").unwrap();
    let mut request = Capture::text(
        Scope::project(&ns),
        "annotation",
        &format!("[[{}@1]] requires [[{}@1]]", a.id, b.id),
    );
    request.kind = "decision".into();
    let source = remember(&cat, &cap, &mut store, &request).unwrap();
    let file = std::fs::read_dir(cat.state.path.join("run"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("client-")
        })
        .unwrap();
    let client = load_client(&file).unwrap();
    let _service = memz::adapters::serve_configured(
        &cat.state,
        None,
        Some(memz::providers::relations::LiteralExtractor),
    )
    .unwrap();
    let until = std::time::Instant::now() + Duration::from_secs(5);
    let call = |v| memz::adapters::call(&cat.state, &client, v).unwrap();
    assert_eq!(
        call(serde_json::json!({"op":"extraction_policy"}))["enabled"],
        true
    );
    loop {
        let p = call(
            serde_json::json!({"op":"inspect","id":a.id,"graph":true,"relations":["requires"],"budget":8000}),
        );
        if let Some(related) = p["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == b.id)
        {
            assert_eq!(related["via"][0]["edge"]["explicit"], false);
            assert_eq!(
                related["via"][0]["edge"]["evidence"][0]["method"],
                "literal-v1"
            );
            break;
        }
        assert!(
            std::time::Instant::now() < until,
            "automatic extraction did not publish"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    call(
        serde_json::json!({"op":"correct","id":source.id,"expected_revision":1,"text":"Annotation withdrawn"}),
    );
    let p = call(
        serde_json::json!({"op":"inspect","id":a.id,"graph":true,"relations":["requires"],"budget":8000}),
    );
    assert_eq!(p["items"].as_array().unwrap().len(), 1);
}

#[test]
fn accepted_connections_wait_for_a_delayed_client_frame() {
    use std::io::{BufRead, BufReader, Write};
    let f = Fixture::new("delayed-frame");
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    let _server = memz::daemon::Server::start(
        &state,
        vec![memz::daemon::Binding {
            token: "test".into(),
            principal: "u".into(),
            session: "s".into(),
        }],
        memz::daemon::Limits::default(),
        |_, v| Ok(v),
    )
    .unwrap();
    let mut socket =
        std::os::unix::net::UnixStream::connect(state.path.join("run/memz.sock")).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    socket
        .write_all(b"{\"version\":1,\"token\":\"test\",\"body\":{\"delayed\":true}}\n")
        .unwrap();
    let mut line = String::new();
    BufReader::new(socket).read_line(&mut line).unwrap();
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["result"]["delayed"], true, "{value}");
}
