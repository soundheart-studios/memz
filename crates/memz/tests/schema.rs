mod common;
use common::Fixture;
use memz::{
    catalog::Catalog,
    state::StateRoot,
    store::{NamespaceStore, migrate},
};

#[test]
fn schema_has_required_tables_indexes_and_integrity() {
    let f = Fixture::new("schema");
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    let project = f.root.join("project");
    std::fs::create_dir(&project).unwrap();
    let mut cat = Catalog::open(state.clone()).unwrap();
    let id = cat.register(&project).unwrap();
    let store = NamespaceStore::open(&state, &id).unwrap();
    for table in [
        "sources",
        "items",
        "evidence",
        "edges",
        "jobs",
        "operations",
        "namespace_meta",
        "memory_fts",
    ] {
        let count: i64 = store
            .conn
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name=?1",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "{table}");
    }
    for index in [
        "items_scope_status",
        "edges_from",
        "edges_to",
        "jobs_ready",
        "evidence_source",
    ] {
        let count: i64 = store
            .conn
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type='index' AND name=?1",
                [index],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "{index}");
    }
    assert_eq!(
        store
            .conn
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(store.conn.execute("INSERT INTO items(id,revision,kind,content,explicit,status,source_id,source_revision,visibility,session_id,valid_from) VALUES('orphan',1,'fact','x',1,'active','absent',1,'project','',0)",[]).is_err());
    let path = state.namespace_db(&id).unwrap();
    drop(store);
    drop(cat);
    // Keep this run-scoped fixture as a schema-workflow artifact, not live state.
    if std::env::var_os("MEMZ_SCHEMA_ARTIFACT").is_some() {
        println!("SCHEMA_EVIDENCE={}", path.display());
        std::mem::forget(f);
    }
}

#[test]
fn failed_migration_rolls_back_all_prior_statements() {
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE sources(existing TEXT)")
        .unwrap();
    assert!(migrate(&mut conn).is_err());
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='namespace_meta'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn future_or_damaged_schema_is_never_silently_repaired() {
    let mut future = rusqlite::Connection::open_in_memory().unwrap();
    future.execute_batch("PRAGMA user_version=999").unwrap();
    assert_eq!(migrate(&mut future).unwrap_err().code, "unsupported_schema");
    assert_eq!(
        future
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        999
    );
    let mut damaged = rusqlite::Connection::open_in_memory().unwrap();
    damaged.execute_batch("PRAGMA user_version=1").unwrap();
    assert!(migrate(&mut damaged).is_err());
    let count: i64 = damaged
        .query_row("SELECT count(*) FROM sqlite_schema", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}
