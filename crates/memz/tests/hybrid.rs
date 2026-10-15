mod common;
use common::Fixture;
use memz::{
    adapters::{initialize, load_client},
    auth::Scope,
    capture::{Capture, remember},
    catalog::Catalog,
    lifecycle::{correct, forget},
    providers::Identity,
    retrieval::{Query, hybrid, hybrid_with_vector},
    state::StateRoot,
    store::NamespaceStore,
    vector,
};
use std::time::{Duration, Instant};
fn setup() -> (Fixture, Catalog, NamespaceStore, String, Identity) {
    let f = Fixture::new("hybrid");
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    std::fs::create_dir(f.root.join("project")).unwrap();
    let c = load_client(&initialize(&state, &f.root.join("project"), "u", "s").unwrap()).unwrap();
    let mut cat = Catalog::open(state.clone()).unwrap();
    cat.bind_session("u", "other").unwrap();
    let mut store = NamespaceStore::open(&state, &c.namespace).unwrap();
    let id = Identity::for_artifacts("a".repeat(64), "b".repeat(64), "c".repeat(64));
    vector::initialize(&mut store, &id).unwrap();
    (f, cat, store, c.namespace, id)
}
fn vec_at(n: usize) -> Vec<f32> {
    let mut v = vec![0.; 384];
    v[n] = 1.;
    v
}
#[allow(clippy::too_many_arguments)] // Explicit fixture dimensions make scope/ranking cases readable.
fn add(
    cat: &Catalog,
    store: &mut NamespaceStore,
    ns: &str,
    id: &Identity,
    session: &str,
    key: &str,
    text: &str,
    slot: Option<&str>,
    axis: usize,
) -> String {
    let cap = cat.capability("u", session).unwrap();
    let mut c = Capture::text(
        if session == "s" {
            Scope::project(ns)
        } else {
            Scope::session(ns, session)
        },
        key,
        text,
    );
    c.slot = slot.map(str::to_owned);
    let receipt = remember(cat, &cap, store, &c).unwrap();
    let (source, rev): (String, i64) = store
        .conn
        .query_row(
            "SELECT source_id,source_revision FROM items WHERE id=?1",
            [&receipt.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO memory_vectors VALUES(?1,?2,?3,?4)",
            rusqlite::params![
                id.generation().unwrap(),
                source,
                rev,
                vector::bytes(&vec_at(axis)).unwrap()
            ],
        )
        .unwrap();
    receipt.id
}
#[test]
fn scope_precedes_ranking_and_current_state_wins() {
    let (_f, mut cat, mut store, ns, id) = setup();
    for i in 0..140 {
        add(
            &cat,
            &mut store,
            &ns,
            &id,
            "other",
            &format!("private{i}"),
            "private confidential",
            None,
            0,
        );
    }
    let wanted = add(
        &cat,
        &mut store,
        &ns,
        &id,
        "s",
        "visible",
        "physician helps patients",
        None,
        0,
    );
    let old = add(
        &cat,
        &mut store,
        &ns,
        &id,
        "s",
        "old",
        "meeting Thursday",
        Some("meeting"),
        0,
    );
    let new = add(
        &cat,
        &mut store,
        &ns,
        &id,
        "s",
        "new",
        "meeting Friday",
        Some("meeting"),
        1,
    );
    let cap = cat.capability("u", "s").unwrap();
    let mut q = Query::new("medical care");
    q.max_tokens = 8000;
    let packet = hybrid_with_vector(
        &cat,
        &cap,
        &mut store,
        &q,
        &id,
        &vec_at(0),
        Instant::now() + Duration::from_secs(1),
    )
    .unwrap();
    assert!(packet.degraded.is_empty());
    assert!(packet.items.iter().any(|x| x.id == wanted));
    assert!(!packet.items.iter().any(|x| x.id == old));
    assert!(packet.items.iter().any(|x| x.id == new));
    assert!(packet.items.iter().all(|x| x.visibility == "project"));
    assert_eq!(
        packet,
        hybrid_with_vector(
            &cat,
            &cap,
            &mut store,
            &q,
            &id,
            &vec_at(0),
            Instant::now() + Duration::from_secs(1)
        )
        .unwrap()
    );
    correct(&cat, &cap, &mut store, &wanted, 1, "updated physician").unwrap();
    assert!(
        !hybrid_with_vector(
            &cat,
            &cap,
            &mut store,
            &q,
            &id,
            &vec_at(0),
            Instant::now() + Duration::from_secs(1)
        )
        .unwrap()
        .items
        .iter()
        .any(|x| x.id == wanted)
    );
    forget(&cat, &cap, &mut store, &new, 1).unwrap();
    cat.revoke("u", &ns).unwrap();
    assert!(
        hybrid_with_vector(
            &cat,
            &cap,
            &mut store,
            &q,
            &id,
            &vec_at(0),
            Instant::now() + Duration::from_secs(1)
        )
        .is_err()
    );
}
#[test]
fn fallback_deadlines_and_whole_packet_budgets() {
    let (_f, cat, mut store, ns, id) = setup();
    add(
        &cat,
        &mut store,
        &ns,
        &id,
        "s",
        "one",
        "searchable fact",
        None,
        0,
    );
    let cap = cat.capability("u", "s").unwrap();
    let q = Query::new("searchable");
    let start = Instant::now();
    let p = hybrid(&cat, &cap, &mut store, &q, None).unwrap();
    assert_eq!(p.items.len(), 1);
    assert!(!p.degraded.is_empty());
    assert!(start.elapsed() < Duration::from_millis(500));
    let p =
        hybrid_with_vector(&cat, &cap, &mut store, &q, &id, &vec_at(0), Instant::now()).unwrap();
    assert_eq!(p.items.len(), 1);
    assert!(p.degraded.iter().any(|s| s == "semantic_deadline"));
    assert!(serde_json::to_vec(&p).unwrap().len() <= q.max_tokens);
}

#[test]
fn generation_backfill_is_bounded_idempotent_and_respects_scope() {
    let (_f, cat, mut store, ns, id) = setup();
    add(
        &cat,
        &mut store,
        &ns,
        &id,
        "s",
        "one",
        "public memory",
        None,
        0,
    );
    add(
        &cat,
        &mut store,
        &ns,
        &id,
        "other",
        "two",
        "private memory",
        None,
        0,
    );
    store
        .conn
        .execute("UPDATE jobs SET state='completed'", [])
        .unwrap();
    let mut next = id.clone();
    next.model_sha = "d".repeat(64);
    let g = vector::initialize(&mut store, &next).unwrap();
    let cap = cat.capability("u", "s").unwrap();
    assert_eq!(vector::enqueue(&cat, &cap, &mut store, &g).unwrap(), 1);
    assert_eq!(vector::enqueue(&cat, &cap, &mut store, &g).unwrap(), 0);
    let lease = memz::jobs::claim(
        &cat,
        &mut store,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
        30,
        false,
    )
    .unwrap()
    .unwrap();
    assert_eq!(lease.text, "public memory");
}

#[test]
fn cold_provider_degrades_without_repeated_loads() {
    let (_f, cat, mut store, ns, id) = setup();
    add(
        &cat,
        &mut store,
        &ns,
        &id,
        "s",
        "one",
        "lexical fact",
        None,
        0,
    );
    let cap = cat.capability("u", "s").unwrap();
    let engine = memz::providers::Engine::start("/missing-explicit-model".into()).unwrap();
    for _ in 0..10 {
        let p = hybrid(
            &cat,
            &cap,
            &mut store,
            &Query::new("lexical"),
            Some(&engine),
        )
        .unwrap();
        assert_eq!(p.items.len(), 1);
        assert!(!p.degraded.is_empty());
    }
    assert!(engine.identity().is_none());
}

#[test]
fn semantic_scan_and_generation_retention_have_explicit_bounds() {
    let (_f, cat, mut store, ns, id) = setup();
    let base = add(&cat, &mut store, &ns, &id, "s", "one", "candidate", None, 0);
    store.conn.execute("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<4100) INSERT INTO items SELECT 'copy-'||n.x,i.revision,i.kind,i.content,i.explicit,i.status,i.source_id,i.source_revision,i.visibility,i.session_id,i.valid_from,i.valid_to,i.slot FROM items i,n WHERE i.id=?1",[base]).unwrap();
    let cap = cat.capability("u", "s").unwrap();
    let q = Query::new("unmatched");
    let p = hybrid_with_vector(
        &cat,
        &cap,
        &mut store,
        &q,
        &id,
        &vec_at(0),
        Instant::now() + Duration::from_secs(2),
    )
    .unwrap();
    assert!(p.candidate_limit_hit);
    assert!(p.degraded.iter().any(|s| s == "semantic_scan_limit"));
    assert!(p.items.len() <= q.max_items);
    assert!(serde_json::to_vec(&p).unwrap().len() <= q.max_tokens);
    for ch in ['d', 'e', 'f', '0', '1', '2', '3'] {
        let mut other = id.clone();
        other.model_sha = ch.to_string().repeat(64);
        vector::initialize(&mut store, &other).unwrap();
    }
    let mut excess = id.clone();
    excess.model_sha = "4".repeat(64);
    assert!(vector::initialize(&mut store, &excess).is_err());
}

#[test]
#[ignore = "requires MEMZ_TEST_MODEL and local sockets; explicit real-model integration"]
fn real_daemon_recovers_and_answers_paraphrases() {
    let path = std::env::var("MEMZ_TEST_MODEL").expect("explicit model directory");
    let f = Fixture::new("semantic-daemon");
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    std::fs::create_dir(f.root.join("project")).unwrap();
    let client =
        load_client(&initialize(&state, &f.root.join("project"), "u", "s").unwrap()).unwrap();
    let service = memz::adapters::serve_with_model(&state, Some(path.clone().into())).unwrap();
    let call = |v| memz::adapters::call(&state, &client, v).unwrap();
    let receipt = call(
        serde_json::json!({"op":"remember","request_id":"real","text":"The physician treats sick patients at the clinic."}),
    );
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        let status = call(serde_json::json!({"op":"status"}));
        if status["pending_jobs"] == 0 && status["semantic"] == "ready" {
            break;
        }
        assert!(Instant::now() < until, "enrichment failed: {status}");
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(service);
    let _service = memz::adapters::serve_with_model(&state, Some(path.into())).unwrap();
    loop {
        let p = call(
            serde_json::json!({"op":"recall","query":"Who provides medical care?","budget":8000}),
        );
        if p["degraded"].as_array().unwrap().is_empty() {
            assert_eq!(p["items"][0]["id"], receipt["id"]);
            break;
        }
        assert!(Instant::now() < until, "semantic recall failed: {p}");
        std::thread::sleep(Duration::from_millis(100));
    }
    let deleted = call(serde_json::json!({"op":"forget","id":receipt["id"],"expected_revision":1}));
    assert!(deleted["mutation_seq"].as_i64().unwrap() > 1);
    let p =
        call(serde_json::json!({"op":"recall","query":"Who provides medical care?","budget":8000}));
    assert!(p["items"].as_array().unwrap().is_empty());
}
