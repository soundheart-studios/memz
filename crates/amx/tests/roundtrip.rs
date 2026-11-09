use amx::{
    Link, Manifest, Record, Source, Tombstone, format_timestamp, parse_timestamp, read_jsonl,
    write_jsonl,
};

const RECORD: &str = r#"{"amx":"0.1","id":"r-1","revision":2,"kind":"decision","content":"Use SQLite for every namespace.","content_type":"text/plain","key":"storage.engine","scope":{"namespace":"ns-1","visibility":"project"},"provenance":{"origin":"user","author":"dev","explicit":true},"status":"active","supersedes":{"id":"r-0","revision":1},"valid_from":"2026-11-09T23:20:03Z","recorded_at":"2026-11-09T23:20:03Z","attributes":{"memz.slot":"storage.engine","vcs.commit":"abc123"}}"#;

const INFERRED: &str = r#"{"amx":"0.1","id":"r-2","revision":1,"kind":"fact","content":"The daemon listens on a unix socket.","scope":{"namespace":"ns-1","visibility":"session","session":"s-9"},"provenance":{"origin":"extractor","author":"memz-extract","explicit":false,"method":"llm-extract","method_version":"3"},"evidence":[{"source":"src-1","source_revision":1,"span":[4,40],"role":"supports"}],"status":"active","valid_from":"2026-11-01T00:00:00Z","valid_to":"2026-12-01T00:00:00Z","recorded_at":"2026-11-01T00:00:05Z","event_at":"2026-10-31T23:59:00Z","expires_at":"2027-01-01T00:00:00Z"}"#;

const SOURCE: &str = r#"{"amx":"0.1","id":"src-1","revision":1,"origin":"user","author":"dev","external_ref":"msg:42","text":"The daemon listens on a unix socket under run/.","content_hash":"sha256:0000000000000000000000000000000000000000000000000000000000000000","scope":{"namespace":"ns-1","visibility":"project"},"recorded_at":"2026-11-01T00:00:00Z"}"#;

const LINK: &str = r#"{"amx":"0.1","id":"l-1","revision":1,"from":{"id":"r-2","revision":1},"to":{"id":"r-1","revision":2},"relation":"supports","qualifiers":{"weight":0.5},"scope":{"namespace":"ns-1","visibility":"project"},"provenance":{"origin":"user","author":"dev","explicit":true},"status":"active","valid_from":"2026-11-09T23:20:03Z","recorded_at":"2026-11-09T23:20:03Z"}"#;

const TOMBSTONE: &str = r#"{"amx":"0.1","id":"t-1","target_type":"record","target_id":"r-3","scope":{"namespace":"ns-1","visibility":"project"},"deleted_at":"2026-11-09T23:30:00Z","reason":"user_request","actor":"dev","derived":["r-4","l-2"]}"#;

const MANIFEST: &str = r#"{"amx":"0.1","bundle_id":"b-1","generator":{"name":"memz","version":"0.1.1"},"created_at":"2026-11-09T23:40:00Z","namespaces":[{"id":"ns-1","cursor":17}],"counts":{"records":2,"sources":1,"links":1,"tombstones":1},"capabilities":["signing"],"omitted":0}"#;

fn same_bytes<T: serde::Serialize + serde::de::DeserializeOwned>(line: &str) {
    let text = format!("{line}\n");
    let parsed: Vec<T> = read_jsonl(&text).expect("parse");
    assert_eq!(parsed.len(), 1);
    assert_eq!(write_jsonl(&parsed).expect("write"), text);
}

#[test]
fn every_record_type_round_trips_byte_for_byte() {
    same_bytes::<Record>(RECORD);
    same_bytes::<Record>(INFERRED);
    same_bytes::<Source>(SOURCE);
    same_bytes::<Link>(LINK);
    same_bytes::<Tombstone>(TOMBSTONE);
    same_bytes::<Manifest>(MANIFEST);
}

#[test]
fn fields_are_typed() {
    let records: Vec<Record> = read_jsonl(&format!("{INFERRED}\n")).unwrap();
    let r = &records[0];
    assert_eq!(r.id, "r-2");
    assert_eq!(r.revision, 1);
    assert!(!r.provenance.explicit);
    assert_eq!(r.scope.namespace, "ns-1");
    assert_eq!(r.scope.visibility, "session");
    assert_eq!(r.scope.session.as_deref(), Some("s-9"));
    let evidence = r.evidence.as_ref().unwrap();
    assert_eq!(evidence[0].span, Some([4, 40]));
    assert_eq!(r.valid_to.as_deref(), Some("2026-12-01T00:00:00Z"));
}

#[test]
fn unknown_fields_and_dimensions_survive() {
    let line = r#"{"amx":"0.1","id":"r-9","revision":1,"kind":"x.lesson","content":"c","scope":{"namespace":"ns-1","visibility":"project","x.team":"core"},"provenance":{"origin":"agent","author":"a","explicit":true,"x.model":"m-1"},"status":"active","valid_from":"2026-11-09T00:00:00Z","recorded_at":"2026-11-09T00:00:00Z","future_field":{"nested":[1,2,3]},"zz_other":true}"#;
    let records: Vec<Record> = read_jsonl(&format!("{line}\n")).unwrap();
    let r = &records[0];
    assert_eq!(
        r.scope.extra.get("x.team").and_then(|v| v.as_str()),
        Some("core")
    );
    assert!(r.extra.contains_key("future_field"));
    assert!(r.provenance.extra.contains_key("x.model"));
    assert_eq!(write_jsonl(&records).unwrap(), format!("{line}\n"));
}

#[test]
fn reordered_input_is_preserved_as_values() {
    let mut value: serde_json::Value = serde_json::from_str(RECORD).unwrap();
    let object = value.as_object_mut().unwrap();
    let content = object.remove("content").unwrap();
    object.insert("content".into(), content);
    let shuffled = serde_json::to_string(&value).unwrap();
    let records: Vec<Record> = read_jsonl(&format!("{shuffled}\n")).unwrap();
    let out = write_jsonl(&records).unwrap();
    let back: serde_json::Value = serde_json::from_str(out.trim_end()).unwrap();
    let original: serde_json::Value = serde_json::from_str(RECORD).unwrap();
    assert_eq!(back, original);
    assert_eq!(out, format!("{RECORD}\n"), "writer emits canonical order");
}

#[test]
fn malformed_lines_are_rejected_with_their_line_number() {
    let text = format!("{RECORD}\nnot json\n");
    let error = read_jsonl::<Record>(&text).unwrap_err();
    assert_eq!(error.line, Some(2));
    assert!(
        read_jsonl::<Record>(&format!("{RECORD}\n\n")).is_err(),
        "blank line"
    );
    assert!(
        read_jsonl::<Record>(RECORD).is_err(),
        "missing final newline"
    );
    let missing = r#"{"amx":"0.1","id":"r-1"}"#;
    assert!(read_jsonl::<Record>(&format!("{missing}\n")).is_err());
    assert!(read_jsonl::<Record>("").unwrap().is_empty());
}

#[test]
fn timestamps_convert_both_ways() {
    assert_eq!(format_timestamp(0), "1970-01-01T00:00:00Z");
    assert_eq!(format_timestamp(1_762_730_403), "2025-11-09T23:20:03Z");
    assert_eq!(format_timestamp(951_782_400), "2000-02-29T00:00:00Z");
    for secs in [
        0,
        59,
        951_782_400,
        1_762_730_403,
        4_102_444_800,
        253_402_300_799,
    ] {
        assert_eq!(parse_timestamp(&format_timestamp(secs)), Some(secs));
    }
    for bad in [
        "2026-11-09 23:20:03Z",
        "2026-11-09T23:20:03",
        "2026-11-09T23:20:03.5Z",
        "2026-13-01T00:00:00Z",
        "2026-02-30T00:00:00Z",
        "2025-02-29T00:00:00Z",
        "2026-11-09T24:00:00Z",
        "2026-11-09T23:60:00Z",
        "+026-11-09T23:20:03Z",
    ] {
        assert_eq!(parse_timestamp(bad), None, "{bad}");
    }
}
