mod common;
use common::Fixture;
use memz::{
    auth::Scope,
    capture::{Capture, remember},
    catalog::Catalog,
    jobs,
    lifecycle::{correct, forget},
    providers::{Identity, LocalModel, validate_vector},
    state::StateRoot,
    store::NamespaceStore,
    vector,
};

fn identity() -> Identity {
    Identity::for_artifacts("a".repeat(64), "b".repeat(64), "c".repeat(64))
}
#[test]
fn configuration_and_vectors_fail_closed() {
    assert!(LocalModel::load(std::path::Path::new("relative")).is_err());
    assert!(LocalModel::load(std::path::Path::new("/does-not-exist-memz-model")).is_err());
    let mut id = identity();
    assert!(id.validate().is_ok());
    id.backend = "unknown".into();
    assert!(id.validate().is_err());
    assert!(validate_vector(&vec![0.; 384]).is_err());
    assert!(validate_vector(&[1., 0.]).is_err());
    assert!(validate_vector(&vec![f32::NAN; 384]).is_err());
}
#[test]
fn generations_are_separate_and_obsolete_vectors_are_removed() {
    let f = Fixture::new("vectors");
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    std::fs::create_dir(f.root.join("project")).unwrap();
    let client = memz::adapters::load_client(
        &memz::adapters::initialize(&state, &f.root.join("project"), "u", "s").unwrap(),
    )
    .unwrap();
    let cat = Catalog::open(state.clone()).unwrap();
    let cap = cat.capability("u", "s").unwrap();
    let mut store = NamespaceStore::open(&state, &client.namespace).unwrap();
    let id = identity();
    let g = vector::initialize(&mut store, &id).unwrap();
    let mut other = id.clone();
    other.model_sha = "d".repeat(64);
    let g2 = vector::initialize(&mut store, &other).unwrap();
    assert_ne!(g, g2);
    let r = remember(
        &cat,
        &cap,
        &mut store,
        &Capture::text(Scope::project(&client.namespace), "one", "original"),
    )
    .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let job = jobs::claim(&cat, &mut store, now, 30, false)
        .unwrap()
        .unwrap();
    let mut v = vec![0.; 384];
    v[0] = 1.;
    store
        .conn
        .execute(
            "UPDATE embedding_generations SET identity='{}' WHERE id=?1",
            [&g],
        )
        .unwrap();
    assert!(
        jobs::publish(&cat, &mut store, &job, now, |tx, job| vector::put(
            tx, &g, job, &v
        ))
        .is_err()
    );
    store
        .conn
        .execute(
            "UPDATE embedding_generations SET identity=?2 WHERE id=?1",
            rusqlite::params![g, serde_json::to_string(&id).unwrap()],
        )
        .unwrap();
    jobs::publish(&cat, &mut store, &job, now, |tx, job| {
        vector::put(tx, &g, job, &v)
    })
    .unwrap();
    assert_eq!(vector::count(&store, &g).unwrap(), 1);
    assert_eq!(vector::count(&store, &g2).unwrap(), 0);
    assert!(
        store
            .conn
            .execute(
                "INSERT INTO memory_vectors VALUES('unknown','missing',1,zeroblob(1536))",
                []
            )
            .is_err()
    );
    correct(&cat, &cap, &mut store, &r.id, 1, "replacement").unwrap();
    assert_eq!(vector::count(&store, &g).unwrap(), 0);
    let job = jobs::claim(&cat, &mut store, now, 30, false)
        .unwrap()
        .unwrap();
    jobs::publish(&cat, &mut store, &job, now, |tx, job| {
        vector::put(tx, &g2, job, &v)
    })
    .unwrap();
    forget(&cat, &cap, &mut store, &r.id, 2).unwrap();
    assert_eq!(vector::count(&store, &g2).unwrap(), 0);
}
#[test]
#[ignore = "requires explicitly provisioned MEMZ_TEST_MODEL; run with --ignored"]
fn explicit_local_model_produces_real_normalized_embeddings() {
    let path = std::env::var("MEMZ_TEST_MODEL").expect("explicit local model directory required");
    let model = LocalModel::load(std::path::Path::new(&path)).unwrap();
    let a = model
        .embed("The physician treats sick patients", false)
        .unwrap();
    let b = model.embed("Who provides medical care?", true).unwrap();
    let c = model
        .embed("Volcanic rocks contain minerals", false)
        .unwrap();
    validate_vector(&a).unwrap();
    validate_vector(&b).unwrap();
    let dot = |x: &Vec<f32>, y: &Vec<f32>| x.iter().zip(y).map(|(a, b)| a * b).sum::<f32>();
    assert!(dot(&a, &b) > dot(&c, &b));
    assert_eq!(
        a,
        model
            .embed("The physician treats sick patients", false)
            .unwrap()
    );
    let expected = model.identity.clone();
    drop(model);
    let engine = memz::providers::Engine::start(path.into()).unwrap();
    let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while engine.identity().is_none() && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(engine.identity(), Some(expected));
    while engine.active() != 0 {
        std::thread::yield_now();
    }
    let timed = engine.embed("medical care".into(), true, std::time::Duration::ZERO);
    assert!(timed.is_err());
    assert_eq!(engine.active(), 1);
    assert!(
        engine
            .embed("second request".into(), true, std::time::Duration::ZERO)
            .is_err()
    );
    while engine.active() != 0 && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        engine
            .embed(
                "The physician treats sick patients".into(),
                false,
                std::time::Duration::from_secs(10)
            )
            .unwrap(),
        a
    );
}
