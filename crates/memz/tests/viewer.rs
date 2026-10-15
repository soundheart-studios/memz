mod common;
use common::Fixture;
use memz::{
    adapters,
    auth::Scope,
    capture::{Capture, remember},
    catalog::Catalog,
    export,
    graph::{self, Anchor, Node, Relation},
    lifecycle,
    state::StateRoot,
    store::NamespaceStore,
};
use std::sync::atomic::AtomicBool;
fn setup() -> (Fixture, Catalog, NamespaceStore, String) {
    let f = Fixture::new("viewer");
    std::fs::create_dir(f.root.join("project")).unwrap();
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    let c = adapters::load_client(
        &adapters::initialize(&state, &f.root.join("project"), "user", "session").unwrap(),
    )
    .unwrap();
    let cat = Catalog::open(state.clone()).unwrap();
    let db = NamespaceStore::open(&state, &c.namespace).unwrap();
    (f, cat, db, c.namespace)
}
#[test]
fn viewer_preserves_snapshot_history_sources_and_directed_conflicts() {
    let (f, cat, mut db, ns) = setup();
    let cap = cat.capability("user", "session").unwrap();
    let a = remember(
        &cat,
        &cap,
        &mut db,
        &Capture::text(Scope::project(&ns), "a", "Deploy today"),
    )
    .unwrap();
    let b = remember(
        &cat,
        &cap,
        &mut db,
        &Capture::text(Scope::project(&ns), "b", "Wait until tomorrow"),
    )
    .unwrap();
    let history = remember(
        &cat,
        &cap,
        &mut db,
        &Capture::text(Scope::project(&ns), "old", "old timeline text"),
    )
    .unwrap();
    lifecycle::correct(&cat, &cap, &mut db, &history.id, 1, "new timeline text").unwrap();
    let source: String = db
        .conn
        .query_row("SELECT source_id FROM items WHERE id=?1", [&a.id], |r| {
            r.get(0)
        })
        .unwrap();
    let edge = graph::propose(
        &cat,
        &cap,
        &mut db,
        &Relation {
            from: Node {
                id: a.id.clone(),
                revision: 1,
            },
            to: Node {
                id: b.id.clone(),
                revision: 1,
            },
            relation: "contradicts".into(),
            qualifiers: Default::default(),
            evidence: vec![Anchor {
                source: source.clone(),
                revision: 1,
                start: 0,
                end: 12,
            }],
        },
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    let view = export::snapshot(&cat, &cap, &mut db, 1048576, &cancel).unwrap();
    if let Ok(directory) = std::env::var("MEMZ_VIEWER_PREVIEW_DIR") {
        let path = std::path::Path::new(&directory).join(format!(
            "{}.html",
            f.root.file_name().unwrap().to_string_lossy()
        ));
        std::fs::write(&path, &view.html).unwrap();
        println!("viewer preview: {}", path.display());
    }
    assert!(view.html.contains("<!doctype html>"));
    assert!(view.html.contains("old timeline text") && view.html.contains("new timeline text"));
    assert!(view.html.contains("superseded"));
    assert!(view.html.contains("Conflict"));
    assert!(view.html.contains("inferred"));
    assert!(view.html.contains(&format!("href=\"#source-{source}-1\"")));
    assert!(view.html.contains(&format!("id=\"source-{source}-1\"")));
    assert!(view.html.contains(&edge.id));
    assert!(view.jsonl.contains(&edge.id));
    assert_eq!(
        view,
        export::snapshot(&cat, &cap, &mut db, 1048576, &cancel).unwrap()
    );
    lifecycle::forget(&cat, &cap, &mut db, &a.id, 1).unwrap();
    let newer = export::snapshot(&cat, &cap, &mut db, 1048576, &cancel).unwrap();
    assert!(!newer.html.contains(&edge.id));
    assert!(view.html.contains(&edge.id));
}
#[test]
fn hostile_markup_and_private_evidence_stay_inert_and_outside_the_view() {
    let (_f, mut cat, mut db, ns) = setup();
    cat.bind_session("user", "hidden").unwrap();
    let cap = cat.capability("user", "session").unwrap();
    let hidden = cat.capability("user", "hidden").unwrap();
    remember(&cat,&cap,&mut db,&Capture::text(Scope::project(&ns),"payload","<script>alert(1)</script><img src=x onerror='bad'> & \"quoted\" [link](javascript:bad)")).unwrap();
    remember(
        &cat,
        &hidden,
        &mut db,
        &Capture::text(Scope::session(&ns, "hidden"), "secret", "DO-NOT-DISCLOSE"),
    )
    .unwrap();
    let view = export::snapshot(&cat, &cap, &mut db, 1048576, &AtomicBool::new(false)).unwrap();
    assert!(!view.html.contains("<script"));
    assert!(!view.html.contains("<img"));
    assert!(view.html.contains("&lt;script&gt;"));
    assert!(!view.html.contains("DO-NOT-DISCLOSE"));
    assert!(view.html.contains("default-src 'none'"));
    assert!(!view.html.contains("href=\"javascript:"));
}
#[test]
fn viewer_publication_is_bounded_cancellable_and_rejects_path_collisions() {
    let (f, cat, mut db, _ns) = setup();
    let cap = cat.capability("user", "session").unwrap();
    let cancel = AtomicBool::new(false);
    let view = export::snapshot(&cat, &cap, &mut db, 1048576, &cancel).unwrap();
    assert!(export::snapshot(&cat, &cap, &mut db, 100, &cancel).is_err());
    assert!(export::publish(&cat.state, "../escape", &view, &cancel).is_err());
    assert!(export::publish(&cat.state, "cancelled", &view, &AtomicBool::new(true)).is_err());
    let output = cat.state.path.join("exports");
    std::fs::write(output.join("collision.html"), "keep").unwrap();
    assert!(export::publish(&cat.state, "collision", &view, &cancel).is_err());
    assert!(!output.join("collision.jsonl").exists());
    assert!(!output.join("collision.md").exists());
    assert_eq!(
        std::fs::read_to_string(output.join("collision.html")).unwrap(),
        "keep"
    );
    export::publish(&cat.state, "viewer", &view, &cancel).unwrap();
    assert_eq!(
        std::fs::read_to_string(output.join("viewer.html")).unwrap(),
        view.html
    );
    assert!(!f.root.join("escape.html").exists());
}

#[test]
fn stale_slots_and_cross_scope_relationship_evidence_are_never_presented_as_current() {
    let (_f, mut cat, mut db, ns) = setup();
    cat.bind_session("user", "hidden").unwrap();
    let cap = cat.capability("user", "session").unwrap();
    let mut req = Capture::text(Scope::project(&ns), "slot-one", "old preference");
    req.slot = Some("preference".into());
    let first = remember(&cat, &cap, &mut db, &req).unwrap();
    req.request_id = "slot-two".into();
    req.text = "new preference".into();
    let second = remember(&cat, &cap, &mut db, &req).unwrap();
    let hidden = cat.capability("user", "hidden").unwrap();
    let private = remember(
        &cat,
        &hidden,
        &mut db,
        &Capture::text(
            Scope::session(&ns, "hidden"),
            "hidden-proof",
            "PRIVATE-EDGE-EVIDENCE",
        ),
    )
    .unwrap();
    let source: String = db
        .conn
        .query_row(
            "SELECT source_id FROM items WHERE id=?1",
            [&private.id],
            |r| r.get(0),
        )
        .unwrap();
    // Defense in depth against malformed older data: a public edge may not smuggle private evidence.
    db.conn.execute("INSERT INTO edges(id,revision,from_id,from_revision,to_id,to_revision,relation,explicit,status,visibility,session_id,valid_from) VALUES('malformed-edge',1,?1,1,?2,1,'contradicts',0,'active','project','',0)",rusqlite::params![second.id,second.id]).unwrap();
    db.conn.execute("INSERT INTO evidence(edge_id,edge_revision,source_id,source_revision,span_start,span_end,role,method,method_version) VALUES('malformed-edge',1,?1,1,0,21,'support','test','1')",[source]).unwrap();
    let view = export::snapshot(&cat, &cap, &mut db, 1048576, &AtomicBool::new(false)).unwrap();
    let rows: Vec<serde_json::Value> = view
        .jsonl
        .lines()
        .skip(1)
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(
        rows.iter().find(|v| v["id"] == first.id).unwrap()["current"],
        false
    );
    assert_eq!(
        rows.iter().find(|v| v["id"] == second.id).unwrap()["current"],
        true
    );
    assert!(view.html.contains("history · active"));
    assert!(!view.html.contains("PRIVATE-EDGE-EVIDENCE"));
    assert!(!view.html.contains("malformed-edge"));
    assert!(!view.jsonl.contains("PRIVATE-EDGE-EVIDENCE"));
}

#[test]
fn high_degree_viewer_is_explicitly_partial_and_adapter_returns_the_html_path() {
    let (_f, cat, mut db, ns) = setup();
    let cap = cat.capability("user", "session").unwrap();
    let root = remember(
        &cat,
        &cap,
        &mut db,
        &Capture::text(Scope::project(&ns), "root", "root"),
    )
    .unwrap();
    let source: String = db
        .conn
        .query_row("SELECT source_id FROM items WHERE id=?1", [&root.id], |r| {
            r.get(0)
        })
        .unwrap();
    for i in 0..130 {
        let next = remember(
            &cat,
            &cap,
            &mut db,
            &Capture::text(
                Scope::project(&ns),
                &format!("node-{i}"),
                &format!("memory {i}"),
            ),
        )
        .unwrap();
        graph::propose(
            &cat,
            &cap,
            &mut db,
            &Relation {
                from: Node {
                    id: root.id.clone(),
                    revision: 1,
                },
                to: Node {
                    id: next.id,
                    revision: 1,
                },
                relation: "supports".into(),
                qualifiers: Default::default(),
                evidence: vec![Anchor {
                    source: source.clone(),
                    revision: 1,
                    start: 0,
                    end: 4,
                }],
            },
        )
        .unwrap();
    }
    let view = export::snapshot(
        &cat,
        &cap,
        &mut db,
        8 * 1024 * 1024,
        &AtomicBool::new(false),
    )
    .unwrap();
    let header: serde_json::Value =
        serde_json::from_str(view.jsonl.lines().next().unwrap()).unwrap();
    assert_eq!(header["graph_partial"], true);
    assert_eq!(header["relationships"].as_array().unwrap().len(), 128);
    assert!(view.html.contains("Partial neighborhood"));
    // The adapter uses a lower total budget and refuses, rather than silently dropping records.
    let client = adapters::Client {
        binding: memz::daemon::Binding {
            token: "trusted-host".into(),
            principal: "user".into(),
            session: "session".into(),
        },
        namespace: ns,
    };
    assert_eq!(
        adapters::dispatch(
            &cat.state,
            &client,
            &serde_json::json!({"op":"export","name":"too-large"})
        )
        .unwrap_err()
        .code,
        "export_limit"
    );
    let (_f, small_cat, _db, small_ns) = setup();
    let small = adapters::Client {
        namespace: small_ns,
        ..client
    };
    let result = adapters::dispatch(
        &small_cat.state,
        &small,
        &serde_json::json!({"op":"export","name":"readable"}),
    )
    .unwrap();
    assert!(std::path::Path::new(result["html_path"].as_str().unwrap()).is_file());
}
