use amx::{
    Bundle, Generator, Holdings, ImportPlan, Item, Link, NamespaceCursor, Record, Source, Support,
    Tombstone, plan_import, read_jsonl, sha256_hex,
};
use serde_json::{Value, json};

fn parse<T: serde::de::DeserializeOwned>(v: Value) -> T {
    read_jsonl(&format!("{v}\n")).unwrap().remove(0)
}

fn record(id: &str, revision: u64, content: &str) -> Record {
    parse(
        json!({"amx":"0.1","id":id,"revision":revision,"kind":"fact","content":content,
        "scope":{"namespace":"ns","visibility":"project"},
        "provenance":{"origin":"user","author":"dev","explicit":true},
        "status":"active","valid_from":"2026-11-09T00:00:00Z","recorded_at":"2026-11-09T00:00:00Z"}),
    )
}

fn inferred(id: &str, revision: u64, content: &str) -> Record {
    let mut r = record(id, revision, content);
    r.provenance = parse(
        json!({"origin":"extractor","author":"x","explicit":false,"method":"m","method_version":"1"}),
    );
    r.evidence = Some(parse(
        json!([{"source":"s-1","source_revision":1,"role":"supports"}]),
    ));
    r
}

fn source(id: &str, text: &str) -> Source {
    parse(
        json!({"amx":"0.1","id":id,"revision":1,"origin":"user","author":"dev","text":text,
        "content_hash":format!("sha256:{}", sha256_hex(text.as_bytes())),
        "scope":{"namespace":"ns","visibility":"project"},"recorded_at":"2026-11-09T00:00:00Z"}),
    )
}

fn link(id: &str, from: (&str, u64), to: (&str, u64)) -> Link {
    parse(
        json!({"amx":"0.1","id":id,"revision":1,"from":{"id":from.0,"revision":from.1},
        "to":{"id":to.0,"revision":to.1},"relation":"supports",
        "scope":{"namespace":"ns","visibility":"project"},
        "provenance":{"origin":"user","author":"dev","explicit":true},"status":"active",
        "valid_from":"2026-11-09T00:00:00Z","recorded_at":"2026-11-09T00:00:00Z"}),
    )
}

fn tombstone(id: &str, target_type: &str, target: &str, derived: &[&str]) -> Tombstone {
    parse(
        json!({"amx":"0.1","id":id,"target_type":target_type,"target_id":target,
        "scope":{"namespace":"ns","visibility":"project"},"deleted_at":"2026-11-09T01:00:00Z",
        "reason":"user_request","actor":"dev","derived":derived}),
    )
}

fn bundle() -> Bundle {
    Bundle::new(
        "b",
        Generator {
            name: "t".into(),
            version: "1".into(),
        },
        "2026-11-09T02:00:00Z",
        vec![NamespaceCursor {
            id: "ns".into(),
            cursor: 1,
        }],
    )
}

fn memz() -> Support {
    Support::new(&["project", "session"], &[])
}

fn plan(b: &Bundle, held: &Holdings) -> ImportPlan {
    plan_import(b, held, &memz())
}

fn ids(items: &[Item]) -> Vec<(String, u64)> {
    items
        .iter()
        .map(|i| (i.id().to_string(), i.revision()))
        .collect()
}

fn refs(items: &[amx::ItemRef]) -> Vec<(String, u64)> {
    items.iter().map(|r| (r.id.clone(), r.revision)).collect()
}

#[test]
fn everything_new_is_added_parents_first() {
    let mut b = bundle();
    b.links = vec![link("l-1", ("r-2", 1), ("r-1", 1))];
    b.records = vec![record("r-2", 1, "b"), record("r-1", 1, "a")];
    b.sources = vec![source("s-1", "passage")];
    let p = plan(&b, &Holdings::default());
    assert_eq!(
        ids(&p.adds),
        [
            ("s-1".into(), 1),
            ("r-1".into(), 1),
            ("r-2".into(), 1),
            ("l-1".into(), 1)
        ]
    );
    assert!(p.conflicts.is_empty() && p.refused.is_empty() && p.supersedes.is_empty());
    assert!(p.is_clean());
}

#[test]
fn tombstones_come_first_and_refuse_resurrection_in_the_same_bundle() {
    let mut b = bundle();
    b.records = vec![
        record("r-1", 2, "back from the dead"),
        record("r-3", 1, "ok"),
    ];
    b.tombstones = vec![tombstone("t-1", "record", "r-1", &[])];
    let mut held = Holdings::default();
    held.hold_record(&record("r-1", 1, "x"));
    let p = plan(&b, &held);
    assert_eq!(p.tombstones.len(), 1);
    assert_eq!(refs(&p.refused), [("r-1".into(), 2)]);
    assert_eq!(ids(&p.adds), [("r-3".into(), 1)]);
}

#[test]
fn a_held_tombstone_refuses_every_revision_and_derived_identities() {
    let mut held = Holdings::default();
    held.hold_tombstone(&tombstone("t-1", "record", "r-1", &["r-2", "l-1"]));
    let mut b = bundle();
    b.records = vec![
        record("r-1", 1, "a"),
        record("r-1", 7, "a"),
        record("r-2", 1, "derived"),
    ];
    b.links = vec![link("l-1", ("r-9", 1), ("r-8", 1))];
    let p = plan(&b, &held);
    assert_eq!(
        refs(&p.refused),
        [
            ("r-1".into(), 1),
            ("r-1".into(), 7),
            ("r-2".into(), 1),
            ("l-1".into(), 1)
        ]
    );
    assert!(p.adds.is_empty());

    let mut again = bundle();
    again.tombstones = vec![tombstone("t-1", "record", "r-1", &["r-2", "l-1"])];
    assert!(plan(&again, &held).tombstones.is_empty(), "already held");
}

#[test]
fn deleted_sources_take_their_dependents_with_them() {
    let mut held = Holdings::default();
    held.hold_tombstone(&tombstone("t-s", "source", "s-1", &[]));
    let mut b = bundle();
    b.sources = vec![source("s-1", "passage")];
    b.records = vec![inferred("r-1", 1, "derived from s-1")];
    let p = plan(&b, &held);
    assert_eq!(refs(&p.refused), [("s-1".into(), 1), ("r-1".into(), 1)]);
}

#[test]
fn identical_revisions_are_noops_and_divergent_ones_conflict() {
    let mut held = Holdings::default();
    held.hold_record(&record("r-1", 1, "same"));
    held.hold_record(&record("r-2", 1, "original"));
    let mut b = bundle();
    let mut lifecycle_changed = record("r-1", 1, "same");
    lifecycle_changed.status = "superseded".into();
    lifecycle_changed.valid_to = Some("2026-11-10T00:00:00Z".into());
    b.records = vec![
        lifecycle_changed,
        record("r-1", 2, "next"),
        record("r-2", 1, "forked"),
    ];
    let p = plan(&b, &held);
    assert_eq!(refs(&p.noops), [("r-1".into(), 1)]);
    assert_eq!(ids(&p.supersedes), [("r-1".into(), 2)]);
    assert_eq!(
        refs(
            &p.conflicts
                .iter()
                .map(|c| c.item.clone())
                .collect::<Vec<_>>()
        ),
        [("r-2".into(), 1)]
    );
    assert!(!p.is_clean());
}

#[test]
fn revisions_must_continue_the_chain() {
    let mut held = Holdings::default();
    held.hold_record(&record("r-1", 1, "a"));
    held.hold_record(&record("r-2", 1, "a"));
    let mut b = bundle();
    b.records = vec![
        record("r-1", 2, "b"),
        record("r-1", 3, "c"),
        record("r-2", 3, "gap"),
        record("r-3", 1, "new"),
        record("r-3", 3, "gap in a new identity"),
    ];
    let p = plan(&b, &held);
    assert_eq!(ids(&p.supersedes), [("r-1".into(), 2), ("r-1".into(), 3)]);
    assert_eq!(ids(&p.adds), [("r-3".into(), 1)]);
    let conflicts: Vec<_> = p
        .conflicts
        .iter()
        .map(|c| (c.item.id.clone(), c.item.revision))
        .collect();
    assert_eq!(conflicts, [("r-2".into(), 3), ("r-3".into(), 3)]);
}

#[test]
fn inferred_never_overrides_explicit() {
    let mut held = Holdings::default();
    held.hold_record(&record("r-1", 1, "the user said this"));
    held.hold_record(&inferred("r-2", 1, "a guess"));
    let mut keyed = record("r-k", 1, "explicit keyed");
    keyed.key = Some("storage.engine".into());
    held.hold_record(&keyed);

    let mut b = bundle();
    let mut replaces = inferred("r-4", 1, "replaces r-1");
    replaces.supersedes = Some(amx::Ref {
        id: "r-1".into(),
        revision: 1,
    });
    let mut same_key = inferred("r-5", 1, "claims the key");
    same_key.key = Some("storage.engine".into());
    b.records = vec![
        inferred("r-1", 2, "overrides the user"),
        record("r-2", 2, "the user corrects the guess"),
        replaces,
        same_key,
    ];
    let p = plan(&b, &held);
    assert_eq!(ids(&p.supersedes), [("r-2".into(), 2)]);
    let conflicts: Vec<_> = p.conflicts.iter().map(|c| c.item.id.clone()).collect();
    assert_eq!(conflicts, ["r-1", "r-4", "r-5"]);
    assert!(p.conflicts.iter().all(|c| c.reason.contains("explicit")));
}

#[test]
fn unsupported_scope_is_a_conflict_not_an_import() {
    let mut b = bundle();
    let mut r = record("r-1", 1, "repo scoped");
    r.scope.repo = Some("memz".into());
    b.records = vec![r];
    let p = plan(&b, &Holdings::default());
    assert!(p.adds.is_empty());
    assert_eq!(p.conflicts.len(), 1);
    assert!(p.conflicts[0].reason.contains("scope"));
}

#[test]
fn links_need_live_endpoints() {
    let mut held = Holdings::default();
    held.hold_record(&record("r-1", 1, "a"));
    held.hold_tombstone(&tombstone("t-1", "record", "r-dead", &[]));
    let mut b = bundle();
    b.records = vec![record("r-2", 1, "b")];
    b.links = vec![
        link("l-ok", ("r-2", 1), ("r-1", 1)),
        link("l-dead", ("r-2", 1), ("r-dead", 1)),
        link("l-dangling", ("r-2", 1), ("r-404", 1)),
    ];
    let p = plan(&b, &held);
    assert_eq!(ids(&p.adds), [("r-2".into(), 1), ("l-ok".into(), 1)]);
    assert_eq!(refs(&p.refused), [("l-dead".into(), 1)]);
    assert_eq!(p.conflicts.len(), 1);
    assert_eq!(p.conflicts[0].item.id, "l-dangling");
}

#[test]
fn applying_a_plan_makes_replanning_a_noop() {
    let mut b = bundle();
    b.sources = vec![source("s-1", "passage")];
    b.records = vec![
        record("r-1", 1, "a"),
        record("r-1", 2, "b"),
        inferred("r-2", 1, "c"),
    ];
    b.links = vec![link("l-1", ("r-2", 1), ("r-1", 2))];
    b.tombstones = vec![tombstone("t-1", "record", "r-old", &[])];
    let mut held = Holdings::default();
    held.hold_record(&record("r-old", 1, "x"));
    let first = plan(&b, &held);
    assert!(first.is_clean());
    held.apply(&first);
    assert!(!held.holds_record("ns", "r-old"));
    let second = plan(&b, &held);
    assert!(second.tombstones.is_empty());
    assert!(second.adds.is_empty());
    assert!(second.supersedes.is_empty());
    assert!(second.conflicts.is_empty());
    assert!(second.refused.is_empty());
    assert_eq!(second.noops.len(), 5);
}
