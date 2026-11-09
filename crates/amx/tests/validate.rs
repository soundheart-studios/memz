use amx::{
    Evidence, Link, Manifest, Record, Ref, Source, Support, Tombstone, read_jsonl, validate_link,
    validate_manifest, validate_record, validate_source, validate_tombstone,
};
use serde_json::{Value, json};

fn record() -> Value {
    json!({"amx":"0.1","id":"r-1","revision":1,"kind":"decision","content":"Use SQLite.",
        "scope":{"namespace":"ns-1","visibility":"project"},
        "provenance":{"origin":"user","author":"dev","explicit":true},
        "status":"active","valid_from":"2026-11-09T23:20:03Z","recorded_at":"2026-11-09T23:20:03Z"})
}

fn inferred() -> Value {
    let mut r = record();
    r["provenance"] = json!({"origin":"extractor","author":"x","explicit":false,"method":"llm","method_version":"1"});
    r["evidence"] = json!([{"source":"s-1","source_revision":1,"span":[0,4],"role":"supports"}]);
    r
}

fn parse<T: serde::de::DeserializeOwned>(value: &Value) -> T {
    read_jsonl::<T>(&format!("{value}\n")).unwrap().remove(0)
}

fn rec(value: &Value) -> Record {
    parse(value)
}

fn rejects(value: Value, why: &str) {
    let error = validate_record(&rec(&value)).expect_err(why);
    assert_eq!(error.code, "invalid", "{why}: {error}");
}

#[test]
fn a_well_formed_record_passes() {
    validate_record(&rec(&record())).unwrap();
    validate_record(&rec(&inferred())).unwrap();
}

#[test]
fn provenance_is_required() {
    let mut r = record();
    r["provenance"]["author"] = json!("");
    rejects(r, "empty author");
    let mut r = record();
    r["provenance"]["origin"] = json!("somebody");
    rejects(r, "unknown origin");
    let mut r = record();
    r.as_object_mut().unwrap().remove("provenance");
    assert!(
        read_jsonl::<Record>(&format!("{r}\n")).is_err(),
        "missing provenance"
    );
}

#[test]
fn inferred_records_need_evidence_and_a_method() {
    let mut r = inferred();
    r.as_object_mut().unwrap().remove("evidence");
    rejects(r, "inferred without evidence");
    let mut r = inferred();
    r["evidence"] = json!([]);
    rejects(r, "inferred with empty evidence");
    let mut r = inferred();
    r["provenance"].as_object_mut().unwrap().remove("method");
    rejects(r, "inferred without method");
    let mut r = inferred();
    r["evidence"][0]["span"] = json!([5, 1]);
    rejects(r, "inverted span");
    let mut r = inferred();
    r["evidence"][0]["role"] = json!("vibes");
    rejects(r, "unknown evidence role");
}

#[test]
fn scope_must_be_complete() {
    let mut r = record();
    r["scope"]["visibility"] = json!("session");
    rejects(r, "session visibility without session");
    let mut r = record();
    r["scope"]["visibility"] = json!("everyone");
    rejects(r, "unknown visibility");
    let mut r = record();
    r["scope"]["namespace"] = json!("");
    rejects(r, "empty namespace");
    let mut r = record();
    r["scope"]["branch"] = json!("main");
    rejects(r, "branch without repo");
    let mut r = record();
    r["scope"]["team"] = json!("core");
    rejects(r, "custom dimension must be namespaced");
    let mut r = record();
    r["scope"]["x.team"] = json!(7);
    rejects(r, "dimension values are strings");
    let mut r = record();
    r["scope"]["x.team"] = json!("core");
    r["scope"]["repo"] = json!("memz");
    r["scope"]["branch"] = json!("main");
    validate_record(&rec(&r)).unwrap();
}

#[test]
fn unknown_scope_dimensions_fail_closed() {
    let memz = Support::new(&["project", "session"], &[]);
    let project = rec(&record());
    assert!(memz.visible(&project.scope).is_ok());

    let mut r = record();
    r["scope"]["visibility"] = json!("session");
    r["scope"]["session"] = json!("s-1");
    assert!(memz.visible(&rec(&r).scope).is_ok());

    let mut r = record();
    r["scope"]["repo"] = json!("memz");
    let error = memz.visible(&rec(&r).scope).unwrap_err();
    assert_eq!(error.code, "unsupported_scope");

    let mut r = record();
    r["scope"]["x.team"] = json!("core");
    assert_eq!(
        memz.visible(&rec(&r).scope).unwrap_err().code,
        "unsupported_scope"
    );

    let mut r = record();
    r["scope"]["visibility"] = json!("personal");
    assert_eq!(
        memz.visible(&rec(&r).scope).unwrap_err().code,
        "unsupported_scope"
    );

    let wide = Support::new(&["project", "personal"], &["repo", "x.team"]);
    let mut r = record();
    r["scope"]["repo"] = json!("memz");
    r["scope"]["x.team"] = json!("core");
    assert!(wide.visible(&rec(&r).scope).is_ok());
}

#[test]
fn identities_revisions_and_versions_are_checked() {
    let mut r = record();
    r["id"] = json!("has space");
    rejects(r, "bad id");
    let mut r = record();
    r["id"] = json!("x".repeat(129));
    rejects(r, "id too long");
    let mut r = record();
    r["revision"] = json!(0);
    rejects(r, "revision zero");
    let mut r = record();
    r["amx"] = json!("1.0");
    rejects(r, "unsupported major version");
    let mut r = record();
    r["amx"] = json!("0.7");
    validate_record(&rec(&r)).unwrap();
    let mut r = record();
    r["kind"] = json!("lesson");
    rejects(r, "custom kind must be namespaced");
    let mut r = record();
    r["kind"] = json!("x.lesson");
    validate_record(&rec(&r)).unwrap();
    let mut r = record();
    r["status"] = json!("deleted");
    rejects(r, "deletion is a tombstone, not a status");
    let mut r = record();
    r["content_type"] = json!("text/html");
    rejects(r, "unsupported content type");
    let mut r = record();
    r["supersedes"] = json!({"id":"r-1","revision":1});
    rejects(r, "supersedes must name a different record");
    let mut r = record();
    r["key"] = json!("");
    rejects(r, "empty key");
}

#[test]
fn timestamps_are_checked() {
    let mut r = record();
    r["valid_from"] = json!("2026-11-09 23:20:03");
    rejects(r, "bad valid_from");
    let mut r = record();
    r["valid_to"] = json!("2026-11-01T00:00:00Z");
    rejects(r, "valid_to before valid_from");
    let mut r = record();
    r["expires_at"] = json!("soon");
    rejects(r, "bad expires_at");
}

#[test]
fn secrets_are_references_only() {
    let mut r = record();
    r["kind"] = json!("secret_ref");
    r["content"] = json!("");
    r["attributes"] = json!({"secret.locator":"vault://kv/prod/db"});
    validate_record(&rec(&r)).unwrap();

    let mut bad = r.clone();
    bad["content"] = json!("hunter2");
    rejects(bad, "secret_ref with content");
    let mut bad = r.clone();
    bad["attributes"] = json!({});
    rejects(bad, "secret_ref without locator");
    let mut bad = record();
    bad["attributes"] = json!({"secret.value":"hunter2"});
    rejects(bad, "secret value attribute");
    let mut bad = record();
    bad["attributes"] = json!({"secret.value_b64":"aHVudGVyMg=="});
    rejects(bad, "secret value prefix");
    let mut bad = record();
    bad["attributes"] = json!({"slot":"x"});
    rejects(bad, "attribute keys are namespaced");
}

#[test]
fn tombstones_are_content_free() {
    let t = json!({"amx":"0.1","id":"t-1","target_type":"record","target_id":"r-1",
        "scope":{"namespace":"ns-1","visibility":"project"},"deleted_at":"2026-11-09T23:30:00Z",
        "reason":"user_request","actor":"dev","derived":["r-2"]});
    validate_tombstone(&parse::<Tombstone>(&t)).unwrap();
    for field in ["content", "text"] {
        let mut bad = t.clone();
        bad[field] = json!("the deleted words");
        let error = validate_tombstone(&parse::<Tombstone>(&bad)).unwrap_err();
        assert_eq!(error.code, "invalid", "{field}");
    }
    let mut bad = t.clone();
    bad["attributes"] = json!({"x.content":"the deleted words"});
    assert!(validate_tombstone(&parse::<Tombstone>(&bad)).is_err());
    let mut bad = t.clone();
    bad["reason"] = json!("because");
    assert!(validate_tombstone(&parse::<Tombstone>(&bad)).is_err());
    let mut bad = t.clone();
    bad["target_type"] = json!("bundle");
    assert!(validate_tombstone(&parse::<Tombstone>(&bad)).is_err());
    let mut bad = t.clone();
    bad["actor"] = json!("");
    assert!(validate_tombstone(&parse::<Tombstone>(&bad)).is_err());
    let mut bad = t;
    bad["derived"] = json!(["bad id"]);
    assert!(validate_tombstone(&parse::<Tombstone>(&bad)).is_err());
}

#[test]
fn sources_links_and_manifests_are_checked() {
    let s = json!({"amx":"0.1","id":"s-1","revision":1,"origin":"user","author":"dev","text":"hi",
        "content_hash":format!("sha256:{}", "a".repeat(64)),
        "scope":{"namespace":"ns-1","visibility":"project"},"recorded_at":"2026-11-09T23:20:03Z"});
    validate_source(&parse::<Source>(&s)).unwrap();
    let mut bad = s.clone();
    bad["content_hash"] = json!("md5:abc");
    assert!(validate_source(&parse::<Source>(&bad)).is_err());
    let mut bad = s;
    bad["origin"] = json!("");
    assert!(validate_source(&parse::<Source>(&bad)).is_err());

    let l = json!({"amx":"0.1","id":"l-1","revision":1,"from":{"id":"r-1","revision":1},
        "to":{"id":"r-2","revision":1},"relation":"supports",
        "scope":{"namespace":"ns-1","visibility":"project"},
        "provenance":{"origin":"user","author":"dev","explicit":true},"status":"active",
        "valid_from":"2026-11-09T23:20:03Z","recorded_at":"2026-11-09T23:20:03Z"});
    validate_link(&parse::<Link>(&l)).unwrap();
    let mut bad = l.clone();
    bad["relation"] = json!("likes");
    assert!(validate_link(&parse::<Link>(&bad)).is_err());
    let mut bad = l.clone();
    bad["provenance"] = json!({"origin":"extractor","author":"x","explicit":false,"method":"m","method_version":"1"});
    assert!(
        validate_link(&parse::<Link>(&bad)).is_err(),
        "inferred link without evidence"
    );
    let mut bad = l;
    bad["from"] = json!({"id":"r-1","revision":0});
    assert!(validate_link(&parse::<Link>(&bad)).is_err());

    let m = json!({"amx":"0.1","bundle_id":"b-1","generator":{"name":"memz","version":"0.1.1"},
        "created_at":"2026-11-09T23:40:00Z","namespaces":[{"id":"ns-1","cursor":3}],
        "counts":{"records":0,"sources":0,"links":0,"tombstones":0}});
    validate_manifest(&parse::<Manifest>(&m)).unwrap();
    let mut bad = m.clone();
    bad["generator"]["name"] = json!("");
    assert!(validate_manifest(&parse::<Manifest>(&bad)).is_err());
    let mut bad = m;
    bad["created_at"] = json!("yesterday");
    assert!(validate_manifest(&parse::<Manifest>(&bad)).is_err());
}

#[test]
fn constructed_values_validate_too() {
    let mut r = rec(&inferred());
    r.evidence = Some(vec![Evidence {
        source: "s-2".into(),
        source_revision: 3,
        span: None,
        role: "context".into(),
        extra: Default::default(),
    }]);
    r.supersedes = Some(Ref {
        id: "r-0".into(),
        revision: 2,
    });
    validate_record(&r).unwrap();
}
