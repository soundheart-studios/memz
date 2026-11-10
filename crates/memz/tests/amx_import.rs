mod common;
use common::Fixture;
use memz::{
    adapters::{dispatch, initialize, load_client, mcp},
    auth::{Capability, Scope},
    capture::{Capture, remember},
    catalog::Catalog,
    export::{publish, snapshot},
    graph::{Anchor, Node, Relation, assert_explicit, propose},
    import::{apply_bundle, read_bundle},
    lifecycle::{correct, forget},
    retrieval::{Query, recall},
    state::StateRoot,
    store::NamespaceStore,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::atomic::AtomicBool};

fn export(
    state: &StateRoot,
    cat: &Catalog,
    cap: &Capability,
    store: &mut NamespaceStore,
    name: &str,
) -> amx::Bundle {
    let cancel = AtomicBool::new(false);
    let snap = snapshot(cat, cap, store, 4194304, &cancel).unwrap();
    publish(state, name, &snap, &cancel).unwrap();
    amx::Bundle::read(&state.path.join("exports").join(format!("{name}.amx"))).unwrap()
}

fn found(cat: &Catalog, cap: &Capability, store: &mut NamespaceStore, query: &str) -> Vec<String> {
    let mut contents: Vec<String> = recall(cat, cap, store, &Query::new(query))
        .unwrap()
        .items
        .into_iter()
        .map(|i| i.content)
        .collect();
    contents.sort();
    contents
}

fn counts(store: &NamespaceStore) -> (i64, i64, i64, i64) {
    store
        .conn
        .query_row(
            "SELECT (SELECT count(*) FROM items),(SELECT count(*) FROM sources),(SELECT count(*) FROM edges),(SELECT count(*) FROM evidence)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap()
}

type Shape = (String, u64, String, String, bool, String, Option<String>);

fn shape(bundle: &amx::Bundle) -> Vec<Shape> {
    let mut out: Vec<_> = bundle
        .records
        .iter()
        .map(|r| {
            (
                r.id.clone(),
                r.revision,
                r.content.clone(),
                r.status.clone(),
                r.provenance.explicit,
                r.kind.clone(),
                r.key.clone(),
            )
        })
        .collect();
    out.sort();
    out
}

fn source_of(store: &NamespaceStore, item: &str, revision: i64) -> String {
    store
        .conn
        .query_row(
            "SELECT source_id FROM items WHERE id=?1 AND revision=?2",
            rusqlite::params![item, revision],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn bundles_import_from_the_cli_only_and_never_resurrect() {
    let f = Fixture::new("amx-import");
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    for project in ["a", "b"] {
        std::fs::create_dir(f.root.join(project)).unwrap();
    }
    let client_a_path = initialize(&state, &f.root.join("a"), "u", "s").unwrap();
    let client_b_path = initialize(&state, &f.root.join("b"), "u", "s").unwrap();
    let client_a = load_client(&client_a_path).unwrap();
    let client_b = load_client(&client_b_path).unwrap();
    let cat = Catalog::open(state.clone()).unwrap();
    let cap = cat.capability("u", "s").unwrap();
    let mut a = NamespaceStore::open(&state, &client_a.namespace).unwrap();
    let mut b = NamespaceStore::open(&state, &client_b.namespace).unwrap();
    let ns = client_a.namespace.clone();

    // A: a decision (keyed, then corrected), an assistant statement, links,
    // and a memory that is forgotten after an early export.
    let mut decision = Capture::text(Scope::project(&ns), "d", "Use SQLite for storage.");
    decision.kind = "decision".into();
    decision.slot = Some("storage.engine".into());
    let d = remember(&cat, &cap, &mut a, &decision).unwrap();
    let mut said = Capture::text(Scope::project(&ns), "s", "The daemon listens on a socket.");
    said.origin = "assistant".into();
    let s = remember(&cat, &cap, &mut a, &said).unwrap();
    let doomed = remember(
        &cat,
        &cap,
        &mut a,
        &Capture::text(Scope::project(&ns), "x", "The launch code is 0000."),
    )
    .unwrap();
    correct(&cat, &cap, &mut a, &d.id, 1, "Use SQLite for all storage.").unwrap();
    let d_source = source_of(&a, &d.id, 2);
    let s_source = source_of(&a, &s.id, 1);
    assert_explicit(
        &cat,
        &cap,
        &mut a,
        &Relation {
            from: Node {
                id: d.id.clone(),
                revision: 2,
            },
            to: Node {
                id: s.id.clone(),
                revision: 1,
            },
            relation: "applies_to".into(),
            qualifiers: BTreeMap::from([("why".into(), "same daemon".into())]),
            evidence: vec![Anchor {
                source: d_source,
                revision: 2,
                start: 0,
                end: 10,
            }],
        },
    )
    .unwrap();
    propose(
        &cat,
        &cap,
        &mut a,
        &Relation {
            from: Node {
                id: s.id.clone(),
                revision: 1,
            },
            to: Node {
                id: d.id.clone(),
                revision: 2,
            },
            relation: "supports".into(),
            qualifiers: BTreeMap::new(),
            evidence: vec![Anchor {
                source: s_source,
                revision: 1,
                start: 4,
                end: 10,
            }],
        },
    )
    .unwrap();
    let before = export(&state, &cat, &cap, &mut a, "before");
    forget(&cat, &cap, &mut a, &doomed.id, 1).unwrap();
    let full = export(&state, &cat, &cap, &mut a, "full");

    // Import into the empty project B.
    let report = apply_bundle(
        &cat,
        &cap,
        &mut b,
        read_bundle(&cat, "full").unwrap(),
        false,
    )
    .unwrap();
    assert_eq!(report["mode"], "applied", "{report}");
    assert_eq!(report["clean"], true);
    for query in ["SQLite", "daemon", "socket", "launch", "storage"] {
        assert_eq!(
            found(&cat, &cap, &mut b, query),
            found(&cat, &cap, &mut a, query),
            "recall {query:?} matches the source project"
        );
    }
    let mirrored = export(&state, &cat, &cap, &mut b, "mirrored");
    assert_eq!(shape(&mirrored), shape(&full));
    let link_ids = |b: &amx::Bundle| {
        b.links
            .iter()
            .map(|l| (l.id.clone(), l.provenance.explicit))
            .collect::<Vec<_>>()
    };
    assert_eq!(link_ids(&mirrored), link_ids(&full));
    assert!(
        mirrored
            .tombstones
            .iter()
            .any(|t| t.target_type == "record" && t.target_id == doomed.id),
        "the deletion travels with the import"
    );

    // Importing again changes nothing.
    let settled = counts(&b);
    let again = apply_bundle(
        &cat,
        &cap,
        &mut b,
        read_bundle(&cat, "full").unwrap(),
        false,
    )
    .unwrap();
    assert_eq!(again["adds"], 0, "{again}");
    assert_eq!(again["supersedes"], 0);
    assert_eq!(again["tombstones"], 0);
    assert_eq!(counts(&b), settled);

    // The older bundle still holds the forgotten memory: it stays forgotten.
    assert!(before.records.iter().any(|r| r.id == doomed.id));
    let old = apply_bundle(
        &cat,
        &cap,
        &mut b,
        read_bundle(&cat, "before").unwrap(),
        false,
    )
    .unwrap();
    assert!(old["refused"].as_u64().unwrap() >= 1, "{old}");
    assert!(found(&cat, &cap, &mut b, "launch").is_empty());
    assert_eq!(counts(&b), settled);

    // A forked revision is a conflict: nothing is written unless asked to
    // skip conflicts, and then only the clean part is.
    let mut forked = mirrored.clone();
    let record = forked.records.iter_mut().find(|r| r.id == s.id).unwrap();
    record.content = "A forked statement.".into();
    let imports = state.path.join("imports");
    std::fs::create_dir_all(&imports).unwrap();
    forked.write(&imports.join("forked.amx")).unwrap();
    let error = apply_bundle(
        &cat,
        &cap,
        &mut b,
        read_bundle(&cat, "forked").unwrap(),
        false,
    )
    .unwrap_err();
    assert_eq!(error.code, "conflict");
    assert_eq!(counts(&b), settled);
    let skipped = apply_bundle(
        &cat,
        &cap,
        &mut b,
        read_bundle(&cat, "forked").unwrap(),
        true,
    )
    .unwrap();
    assert_eq!(
        skipped["conflicts"].as_array().unwrap().len(),
        1,
        "{skipped}"
    );
    assert_eq!(counts(&b), settled);
    assert!(found(&cat, &cap, &mut b, "forked").is_empty());

    // Import is a CLI command. Dry run by default; --apply writes.
    let bin = env!("CARGO_BIN_EXE_memz");
    std::fs::create_dir(f.root.join("c")).unwrap();
    let client_c_path = initialize(&state, &f.root.join("c"), "u", "s").unwrap();
    let client_c = load_client(&client_c_path).unwrap();
    // Registering a project bumps the principal's capability revision.
    let cap = cat.capability("u", "s").unwrap();
    let bundle_path = state.path.join("exports").join("full.amx");
    let run = |extra: &[&str]| -> Value {
        let out = std::process::Command::new(bin)
            .env("MEMZ_HOME", &state.path)
            .arg("import")
            .arg("--client")
            .arg(&client_c_path)
            .arg(&bundle_path)
            .args(extra)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let mut c = NamespaceStore::open(&state, &client_c.namespace).unwrap();
    let empty = counts(&c);
    let preview = run(&[]);
    assert_eq!(preview["mode"], "dry_run");
    assert_eq!(counts(&c), empty);
    let applied = run(&["--apply"]);
    assert_eq!(applied["mode"], "applied");
    assert_eq!(
        found(&cat, &cap, &mut c, "SQLite"),
        found(&cat, &cap, &mut a, "SQLite")
    );
    let bad = std::process::Command::new(bin)
        .env("MEMZ_HOME", &state.path)
        .args(["import", "--client"])
        .arg(&client_c_path)
        .arg(imports.join("forked.amx"))
        .arg("--apply")
        .output()
        .unwrap();
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("conflict"));

    // Never a model tool.
    let tools = mcp(
        &state,
        &client_b,
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
    );
    assert!(
        tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["name"] != "import" && t["name"] != "apply_import")
    );
    assert!(dispatch(&state, &client_b, &json!({"op":"import","bundle":"full"})).is_err());
}
