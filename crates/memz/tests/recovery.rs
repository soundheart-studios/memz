mod common;
use common::Fixture;
use memz::{
    adapters,
    auth::Scope,
    backup::{self, Format},
    capture::{Capture, remember},
    catalog::Catalog,
    lifecycle,
    retrieval::{self, Query},
    state::StateRoot,
    store::NamespaceStore,
};
fn setup() -> (Fixture, StateRoot, String) {
    let f = Fixture::new("recovery");
    std::fs::create_dir(f.root.join("project")).unwrap();
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    let client = adapters::load_client(
        &adapters::initialize(&state, &f.root.join("project"), "user", "session").unwrap(),
    )
    .unwrap();
    (f, state, client.namespace)
}
#[test]
fn snapshot_and_portable_restore_preserve_history_scope_and_tombstones() {
    let (f, state, ns) = setup();
    let mut cat = Catalog::open(state.clone()).unwrap();
    cat.bind_session("user", "other").unwrap();
    let cap = cat.capability("user", "session").unwrap();
    let mut db = NamespaceStore::open(&state, &ns).unwrap();
    let old = remember(
        &cat,
        &cap,
        &mut db,
        &Capture::text(Scope::project(&ns), "change", "old answer"),
    )
    .unwrap();
    lifecycle::correct(&cat, &cap, &mut db, &old.id, 1, "new answer").unwrap();
    let gone = Capture::text(Scope::project(&ns), "gone", "forgotten answer");
    let receipt = remember(&cat, &cap, &mut db, &gone).unwrap();
    lifecycle::forget(&cat, &cap, &mut db, &receipt.id, 1).unwrap();
    remember(
        &cat,
        &cap,
        &mut db,
        &Capture::text(Scope::session(&ns, "session"), "private", "private answer"),
    )
    .unwrap();
    let bundle = f.root.join("bundle");
    backup::create(&state, &bundle, None).unwrap();
    for (name, format) in [("sqlite", Format::Sqlite), ("jsonl", Format::Jsonl)] {
        let target = f.root.join(name);
        backup::restore(&bundle, &target, format).unwrap();
        let restored = StateRoot::open(&target).unwrap();
        let cat = Catalog::open(restored.clone()).unwrap();
        let cap = cat.capability("user", "session").unwrap();
        let mut db = NamespaceStore::open(&restored, &ns).unwrap();
        let result = retrieval::recall(&cat, &cap, &mut db, &Query::new("answer")).unwrap();
        assert!(result.items.iter().any(|i| i.content == "new answer"));
        assert!(
            !result
                .items
                .iter()
                .any(|i| i.content == "old answer" || i.content == "forgotten answer")
        );
        assert_eq!(
            db.conn
                .query_row("SELECT count(*) FROM items WHERE id=?1", [&old.id], |r| r
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
            2
        );
        assert!(remember(&cat, &cap, &mut db, &gone).is_err());
        let other = cat.capability("user", "other").unwrap();
        assert!(
            retrieval::recall(&cat, &other, &mut db, &Query::new("private"))
                .unwrap()
                .items
                .is_empty()
        );
        assert_eq!(std::fs::read_dir(target.join("run")).unwrap().count(), 0);
    }
}
#[test]
fn damaged_inputs_existing_destinations_and_live_relocation_fail_without_loss() {
    let (f, state, ns) = setup();
    let bundle = f.root.join("bundle");
    backup::create(&state, &bundle, Some(&ns)).unwrap();
    assert!(backup::restore(&bundle, &state.path, Format::Sqlite).is_err());
    let server = adapters::serve_with_model(&state, None).unwrap();
    assert!(backup::relocate(&state, &f.root.join("moving")).is_err());
    drop(server);
    std::fs::write(bundle.join("catalog.jsonl"), b"malformed\n").unwrap();
    assert!(backup::restore(&bundle, &f.root.join("bad"), Format::Jsonl).is_err());
    assert!(!f.root.join("bad").exists());
    assert_eq!(
        Catalog::open(state.clone())
            .unwrap()
            .resolve(&f.root.join("project"), None)
            .unwrap(),
        ns
    );
    let target = f.root.join("moved");
    backup::relocate(&state, &target).unwrap();
    assert_eq!(
        Catalog::open(StateRoot::open(&target).unwrap())
            .unwrap()
            .resolve(&f.root.join("project"), None)
            .unwrap(),
        ns
    );
    assert!(state.path.join("catalog.sqlite").is_file());
}

#[test]
fn recovery_child() {
    let Ok(root) = std::env::var("MEMZ_RECOVERY_CHILD") else {
        return;
    };
    let root = std::path::Path::new(&root);
    let state = StateRoot::open(&root.join("state")).unwrap();
    let step = std::env::var("MEMZ_RECOVERY_STEP").unwrap();
    let mut crash = |point: &str| {
        if point == step {
            std::process::exit(73)
        }
        Ok(())
    };
    if step == "namespace_restored" {
        backup::restore_with(
            &root.join("bundle"),
            &root.join("restored"),
            Format::Jsonl,
            &mut crash,
        )
        .unwrap();
    } else {
        backup::create_with(&state, &root.join("interrupted"), None, &mut crash).unwrap();
    }
    panic!("crash point was not reached");
}
#[test]
fn interruptions_and_cancellation_never_publish_partial_state_or_damage_original() {
    let (f, state, ns) = setup();
    let cat = Catalog::open(state.clone()).unwrap();
    let cap = cat.capability("user", "session").unwrap();
    let mut db = NamespaceStore::open(&state, &ns).unwrap();
    remember(
        &cat,
        &cap,
        &mut db,
        &Capture::text(Scope::project(&ns), "durable", "acknowledged memory"),
    )
    .unwrap();
    let bundle = f.root.join("bundle");
    backup::create(&state, &bundle, None).unwrap();
    for step in ["catalog_copied", "namespace_copied", "namespace_restored"] {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "recovery_child", "--nocapture"])
            .env("MEMZ_RECOVERY_CHILD", &f.root)
            .env("MEMZ_RECOVERY_STEP", step)
            .status()
            .unwrap();
        assert_eq!(result.code(), Some(73));
        assert!(!f.root.join("interrupted").exists());
        assert!(!f.root.join("restored").exists());
        assert_eq!(
            retrieval::recall(&cat, &cap, &mut db, &Query::new("acknowledged"))
                .unwrap()
                .items
                .len(),
            1
        );
    }
    backup::restore(&bundle, &f.root.join("restored"), Format::Jsonl).unwrap();
    let cancelled = f.root.join("cancelled");
    assert!(
        backup::create_with(&state, &cancelled, None, &mut |_| Err(memz::Error::new(
            "cancelled",
            "cancelled by operator"
        )))
        .is_err()
    );
    assert!(!cancelled.exists());
}
#[test]
fn relations_provenance_jobs_and_index_rebuild_survive_both_formats() {
    let (f, state, ns) = setup();
    let cat = Catalog::open(state.clone()).unwrap();
    let cap = cat.capability("user", "session").unwrap();
    let mut db = NamespaceStore::open(&state, &ns).unwrap();
    let a = remember(
        &cat,
        &cap,
        &mut db,
        &Capture::text(Scope::project(&ns), "a", "launch requires checks"),
    )
    .unwrap();
    let b = remember(
        &cat,
        &cap,
        &mut db,
        &Capture::text(Scope::project(&ns), "b", "checks"),
    )
    .unwrap();
    let source: String = db
        .conn
        .query_row("SELECT source_id FROM items WHERE id=?1", [&a.id], |r| {
            r.get(0)
        })
        .unwrap();
    let edge = memz::graph::propose(
        &cat,
        &cap,
        &mut db,
        &memz::graph::Relation {
            from: memz::graph::Node {
                id: a.id.clone(),
                revision: 1,
            },
            to: memz::graph::Node {
                id: b.id.clone(),
                revision: 1,
            },
            relation: "requires".into(),
            qualifiers: Default::default(),
            evidence: vec![memz::graph::Anchor {
                source,
                revision: 1,
                start: 0,
                end: 22,
            }],
        },
    )
    .unwrap();
    let identity =
        memz::providers::Identity::for_artifacts("a".repeat(64), "b".repeat(64), "c".repeat(64));
    memz::vector::initialize(&mut db, &identity).unwrap();
    db.conn
        .execute(
            "UPDATE embedding_generations SET identity='incompatible old provider'",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "UPDATE jobs SET state='leased',lease_owner='dead-worker',lease_expiry=9999999999",
            [],
        )
        .unwrap();
    // A stale/broken derived search index is rebuilt from authoritative rows.
    db.conn.execute("DELETE FROM memory_fts", []).unwrap();
    backup::create(&state, &f.root.join("bundle"), Some(&ns)).unwrap();
    for (name, format) in [("r-sql", Format::Sqlite), ("r-json", Format::Jsonl)] {
        backup::restore(&f.root.join("bundle"), &f.root.join(name), format).unwrap();
        let restored = StateRoot::open(&f.root.join(name)).unwrap();
        let cat = Catalog::open(restored.clone()).unwrap();
        let cap = cat.capability("user", "session").unwrap();
        let mut db = NamespaceStore::open(&restored, &ns).unwrap();
        assert_eq!(
            memz::graph::neighbors(&cat, &cap, &mut db, &edge.from).unwrap(),
            vec![edge.clone()]
        );
        assert_eq!(
            retrieval::recall(&cat, &cap, &mut db, &Query::new("launch"))
                .unwrap()
                .items
                .len(),
            1
        );
        assert_eq!(
            db.conn
                .query_row("SELECT count(*) FROM jobs WHERE state='leased'", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        assert_eq!(
            db.conn
                .query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE name='memory_vectors'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        let generation = memz::vector::initialize(&mut db, &identity).unwrap();
        assert!(memz::vector::enqueue(&cat, &cap, &mut db, &generation).is_ok());
        lifecycle::forget(&cat, &cap, &mut db, &a.id, 1).unwrap();
        assert!(
            memz::graph::neighbors(&cat, &cap, &mut db, &edge.to)
                .unwrap()
                .is_empty()
        );
    }
}
#[test]
fn manifest_tampering_traversal_symlinks_and_unknown_schemas_are_rejected() {
    use std::os::unix::fs::symlink;
    let (f, state, ns) = setup();
    let bundle = f.root.join("bundle");
    backup::create(&state, &bundle, Some(&ns)).unwrap();
    let manifest = bundle.join("manifest.json");
    let original = std::fs::read(&manifest).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&original).unwrap();
    for (field, change) in [
        ("name", serde_json::json!("../escape")),
        ("schema", serde_json::json!(999)),
        ("jsonl_sha256", serde_json::json!("bad")),
    ] {
        let mut damaged = value.clone();
        damaged["entries"][1][field] = change;
        std::fs::write(&manifest, serde_json::to_vec(&damaged).unwrap()).unwrap();
        assert!(backup::restore(&bundle, &f.root.join("bad"), Format::Jsonl).is_err());
        assert!(!f.root.join("bad").exists());
    }
    std::fs::write(&manifest, original).unwrap();
    let portable = bundle.join(format!("{ns}.jsonl"));
    std::fs::remove_file(&portable).unwrap();
    symlink(bundle.join("catalog.jsonl"), &portable).unwrap();
    assert!(backup::restore(&bundle, &f.root.join("bad"), Format::Jsonl).is_err());
    let mut damaged = value;
    damaged["entries"][1]["sqlite_sha256"] = serde_json::json!("bad");
    std::fs::write(&manifest, serde_json::to_vec(&damaged).unwrap()).unwrap();
    assert!(backup::restore(&bundle, &f.root.join("bad"), Format::Sqlite).is_err());
    assert!(Catalog::open(state).is_ok());
}
#[test]
fn trusted_cli_recovers_and_model_tools_cannot_invoke_admin_operations() {
    let (f, state, _ns) = setup();
    let bin = env!("CARGO_BIN_EXE_memz");
    let bundle = f.root.join("bundle");
    assert!(
        std::process::Command::new(bin)
            .env("MEMZ_HOME", &state.path)
            .arg("backup")
            .arg(&bundle)
            .output()
            .unwrap()
            .status
            .success()
    );
    let restored = f.root.join("restored");
    let unused = f.root.join("must-not-be-created");
    let output = std::process::Command::new(bin)
        .env("MEMZ_HOME", &unused)
        .arg("restore")
        .arg(&bundle)
        .arg(&restored)
        .arg("jsonl")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!unused.exists());
    let client_path = std::fs::read_dir(state.path.join("run"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|s| s == "json"))
        .unwrap();
    let client = adapters::load_client(&client_path).unwrap();
    let tools = adapters::mcp(
        &state,
        &client,
        &serde_json::json!({"id":1,"method":"tools/list"}),
    );
    let text = tools.to_string();
    assert!(!text.contains("\"backup\""));
    assert!(!text.contains("\"restore\""));
    assert!(!text.contains("\"relocate\""));
}

#[test]
fn selected_namespace_and_explicit_rebinding_preserve_catalog_grants() {
    let (f, state, first) = setup();
    std::fs::create_dir(f.root.join("other-project")).unwrap();
    let other = adapters::load_client(
        &adapters::initialize(&state, &f.root.join("other-project"), "user", "session").unwrap(),
    )
    .unwrap()
    .namespace;
    for (name, selected, count) in [("one", Some(first.as_str()), 2), ("all", None, 3)] {
        let bundle = f.root.join(name);
        let m = backup::create(&state, &bundle, selected).unwrap();
        assert_eq!(m.entries.len(), count);
        let target = f.root.join(format!("{name}-restored"));
        backup::restore(&bundle, &target, Format::Jsonl).unwrap();
        let cat = Catalog::open(StateRoot::open(&target).unwrap()).unwrap();
        let cap = cat.capability("user", "session").unwrap();
        assert!(cat.authorize(&cap, &Scope::project(&first), true).is_ok());
        assert_eq!(
            cat.authorize(&cap, &Scope::project(&other), true).is_ok(),
            selected.is_none()
        );
        if selected.is_some() {
            assert!(!target.join("namespaces").join(&other).exists());
        }
    }
    let new_root = f.root.join("new-project-path");
    std::fs::create_dir(&new_root).unwrap();
    assert!(
        std::process::Command::new(env!("CARGO_BIN_EXE_memz"))
            .env("MEMZ_HOME", f.root.join("one-restored"))
            .args(["rebind", &first])
            .arg(&new_root)
            .output()
            .unwrap()
            .status
            .success()
    );
    let cat = Catalog::open(StateRoot::open(&f.root.join("one-restored")).unwrap()).unwrap();
    assert_eq!(cat.resolve(&new_root, None).unwrap(), first);
}
#[test]
fn rehashed_but_invalid_portable_records_cannot_install_code_or_break_constraints() {
    use sha2::{Digest, Sha256};
    let (f, state, ns) = setup();
    let bundle = f.root.join("bundle");
    backup::create(&state, &bundle, None).unwrap();
    let path = bundle.join(format!("{ns}.jsonl"));
    let original = std::fs::read_to_string(&path).unwrap();
    let manifest_path = bundle.join("manifest.json");
    let m: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    for table in ["sources; DROP TABLE sources; --", "unknown_table"] {
        let poisoned = format!(
            "{original}{}\n",
            serde_json::json!({"table":table,"values":[]})
        );
        std::fs::write(&path, &poisoned).unwrap();
        let mut forged = m.clone();
        forged["entries"][1]["jsonl_sha256"] =
            serde_json::json!(format!("{:x}", Sha256::digest(poisoned.as_bytes())));
        forged["entries"][1]["jsonl_bytes"] = serde_json::json!(poisoned.len());
        forged["entries"][1]["rows"] =
            serde_json::json!(m["entries"][1]["rows"].as_u64().unwrap() + 1);
        std::fs::write(&manifest_path, serde_json::to_vec(&forged).unwrap()).unwrap();
        assert!(backup::restore(&bundle, &f.root.join("poisoned"), Format::Jsonl).is_err());
        assert!(!f.root.join("poisoned").exists());
    }
    assert!(NamespaceStore::open(&state, &ns).is_ok());
}
