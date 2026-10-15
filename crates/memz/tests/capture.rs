mod common;
use common::Fixture;
use memz::{
    auth::Scope,
    capture::{Capture, Stage, remember, remember_observed},
    catalog::Catalog,
    state::StateRoot,
    store::NamespaceStore,
};
fn setup(root: &std::path::Path) -> (Catalog, String) {
    let mut cat = Catalog::open(StateRoot::open(&root.join("state")).unwrap()).unwrap();
    std::fs::create_dir_all(root.join("project")).unwrap();
    let ns = cat.register(&root.join("project")).unwrap();
    cat.bind_session("trusted-user", "session").unwrap();
    cat.grant("trusted-user", &ns, true).unwrap();
    (cat, ns)
}
#[test]
fn capture_is_atomic_scoped_idempotent_and_bounded() {
    let f = Fixture::new("capture");
    let (cat, ns) = setup(&f.root);
    let cap = cat.capability("trusted-user", "session").unwrap();
    let mut store = NamespaceStore::open(&cat.state, &ns).unwrap();
    let mut req = Capture::text(Scope::project(&ns), "request-one", "Remember the blue door");
    let result = remember(&cat, &cap, &mut store, &req).unwrap();
    assert_eq!(remember(&cat, &cap, &mut store, &req).unwrap(), result);
    for table in [
        "sources",
        "items",
        "evidence",
        "memory_fts",
        "operations",
        "jobs",
    ] {
        let n: i64 = store
            .conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
    let author: String = store
        .conn
        .query_row("SELECT author FROM sources", [], |r| r.get(0))
        .unwrap();
    assert_eq!(author, "trusted-user");
    assert_eq!(
        store
            .conn
            .pragma_query_value(None, "synchronous", |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    req.text = "changed".into();
    assert_eq!(
        remember(&cat, &cap, &mut store, &req).unwrap_err().code,
        "conflict"
    );
    req.request_id = "large".into();
    req.text = "x".repeat(65537);
    assert_eq!(
        remember(&cat, &cap, &mut store, &req).unwrap_err().code,
        "too_large"
    );
    req.text = "small".into();
    store
        .conn
        .execute("UPDATE namespace_meta SET quota_records=1", [])
        .unwrap();
    assert_eq!(
        remember(&cat, &cap, &mut store, &req).unwrap_err().code,
        "quota"
    );
    store
        .conn
        .execute(
            "UPDATE namespace_meta SET quota_records=100000,quota_bytes=1",
            [],
        )
        .unwrap();
    assert_eq!(
        remember(&cat, &cap, &mut store, &req).unwrap_err().code,
        "quota"
    );
    store
        .conn
        .execute("UPDATE namespace_meta SET quota_bytes=67108864", [])
        .unwrap();
    store.conn.execute_batch("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<9999) INSERT INTO jobs(id,operation_key,source_id,source_revision,job_type,version,state,next_attempt,principal,capability_revision) SELECT 'fixture-'||x,'fixture-'||x,(SELECT id FROM sources LIMIT 1),1,'enrich',1,'pending',0,'trusted-user',1 FROM n;").unwrap();
    assert_eq!(
        remember(&cat, &cap, &mut store, &req).unwrap_err().code,
        "busy"
    );
    let original = Capture::text(Scope::project(&ns), "request-one", "Remember the blue door");
    assert_eq!(remember(&cat, &cap, &mut store, &original).unwrap(), result);
    req.scope = Scope::session(&ns, "forged");
    assert_eq!(
        remember(&cat, &cap, &mut store, &req).unwrap_err().code,
        "denied"
    );
    assert_eq!(
        store
            .conn
            .query_row("SELECT mutation_seq FROM namespace_meta", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn capture_dependency_tripwire_has_no_provider_or_network_path() {
    // Keep capture's compile-time dependency boundary closed as providers arrive later.
    let source = include_str!("../src/capture/mod.rs");
    for forbidden in [
        "std::net",
        "TcpStream",
        "UdpSocket",
        "reqwest",
        "ureq",
        "providers::",
        "Command::",
        "dlopen",
        "libloading",
    ] {
        assert!(
            !source.contains(forbidden),
            "capture crossed the provider/network boundary: {forbidden}"
        );
    }
}
#[test]
fn crash_child() {
    let Ok(path) = std::env::var("MEMZ_CRASH_ROOT") else {
        return;
    };
    let stage = std::env::var("MEMZ_CRASH_STAGE").unwrap();
    let (cat, ns) = setup(std::path::Path::new(&path));
    let cap = cat.capability("trusted-user", "session").unwrap();
    let mut store = NamespaceStore::open(&cat.state, &ns).unwrap();
    let request = Capture::text(Scope::project(&ns), "crash-request", "durable blue door");
    remember_observed(&cat, &cap, &mut store, &request, |point| {
        if (stage == "before" && point == Stage::BeforeCommit)
            || (stage == "after" && point == Stage::AfterCommit)
        {
            std::process::exit(73);
        }
    })
    .unwrap();
    panic!("crash point was not reached");
}
#[test]
fn abrupt_process_exit_preserves_commit_atomicity() {
    for stage in ["before", "after"] {
        let f = Fixture::new(stage);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "crash_child"])
            .env("MEMZ_CRASH_ROOT", &f.root)
            .env("MEMZ_CRASH_STAGE", stage)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(73));
        let (cat, ns) = setup(&f.root);
        let store = NamespaceStore::open(&cat.state, &ns).unwrap();
        for table in [
            "sources",
            "items",
            "evidence",
            "memory_fts",
            "jobs",
            "operations",
        ] {
            let count: i64 = store
                .conn
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, if stage == "after" { 1 } else { 0 }, "{table}");
        }
        assert_eq!(
            store
                .conn
                .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
    }
}
