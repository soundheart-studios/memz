use amx::{Change, Record, diff_tree, from_markdown, read_jsonl, render_tree, to_markdown};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn rec(v: Value) -> Record {
    read_jsonl(&format!("{v}\n")).unwrap().remove(0)
}

fn decision() -> Record {
    rec(
        json!({"amx":"0.1","id":"note-1","revision":2,"kind":"decision",
        "content":"Use SQLite for every namespace.",
        "scope":{"namespace":"ns-1","visibility":"project"},
        "provenance":{"origin":"user","author":"dev","explicit":true},
        "status":"active","valid_from":"2026-11-09T23:20:03Z","recorded_at":"2026-11-09T23:20:03Z"}),
    )
}

#[test]
fn the_profile_matches_the_spec_example() {
    let expected = "---\n\
amx: \"0.1\"\n\
id: \"note-1\"\n\
revision: 2\n\
kind: \"decision\"\n\
scope: {\"namespace\":\"ns-1\",\"visibility\":\"project\"}\n\
provenance: {\"origin\":\"user\",\"author\":\"dev\",\"explicit\":true}\n\
status: \"active\"\n\
valid_from: \"2026-11-09T23:20:03Z\"\n\
recorded_at: \"2026-11-09T23:20:03Z\"\n\
---\n\
Use SQLite for every namespace.";
    assert_eq!(to_markdown(&decision()).unwrap(), expected);
}

#[test]
fn records_round_trip_losslessly() {
    let rich = rec(
        json!({"amx":"0.1","id":"r-2","revision":1,"kind":"x.lesson",
        "content":"Line one\n---\nnot a delimiter\n\nünïcødé ✓ and \"quotes\"\n\n",
        "content_type":"text/markdown","key":"lesson.one",
        "scope":{"namespace":"ns-1","visibility":"session","session":"s-1","x.team":"core"},
        "provenance":{"origin":"extractor","author":"x","explicit":false,"method":"m","method_version":"2","x.model":"m-1"},
        "evidence":[{"source":"s-1","source_revision":3,"span":[0,9],"role":"supports"}],
        "status":"superseded","supersedes":{"id":"r-0","revision":4},
        "valid_from":"2026-11-01T00:00:00Z","valid_to":"2026-11-02T00:00:00Z",
        "recorded_at":"2026-11-01T00:00:01Z","event_at":"2026-10-31T00:00:00Z",
        "expires_at":"2027-01-01T00:00:00Z",
        "attributes":{"memz.slot":"x","vcs.commit":"abc","x.weights":[0.25,1,{"a":null}]},
        "future_field":{"nested":true}}),
    );
    for record in [decision(), rich] {
        let text = to_markdown(&record).unwrap();
        assert_eq!(from_markdown(&text).unwrap(), record);
        assert_eq!(to_markdown(&from_markdown(&text).unwrap()).unwrap(), text);
    }
    let mut empty = decision();
    empty.content = String::new();
    assert_eq!(from_markdown(&to_markdown(&empty).unwrap()).unwrap(), empty);
}

#[test]
fn malformed_files_are_rejected() {
    let good = to_markdown(&decision()).unwrap();
    let cases = [
        (
            "no opening delimiter",
            good.trim_start_matches("---\n").to_string(),
        ),
        ("no closing delimiter", good.replacen("\n---\n", "\n", 1)),
        (
            "bad JSON value",
            good.replace("revision: 2", "revision: two"),
        ),
        ("missing colon", good.replace("revision: 2", "revision 2")),
        (
            "duplicate field",
            good.replace("revision: 2\n", "revision: 2\nrevision: 3\n"),
        ),
        (
            "content in front matter",
            good.replace("revision: 2\n", "revision: 2\ncontent: \"x\"\n"),
        ),
        (
            "missing required field",
            good.replace("status: \"active\"\n", ""),
        ),
    ];
    for (why, text) in cases {
        let error = from_markdown(&text).expect_err(why);
        assert_eq!(error.code, "invalid", "{why}");
    }
}

#[test]
fn a_tree_holds_the_newest_revision_of_each_identity() {
    let mut old = decision();
    old.revision = 1;
    old.status = "superseded".into();
    old.content = "older".into();
    let mut other = decision();
    other.id = "note-2".into();
    other.revision = 1;
    let tree = render_tree(&[old, decision(), other]).unwrap();
    assert_eq!(tree.keys().collect::<Vec<_>>(), ["note-1.md", "note-2.md"]);
    assert!(tree["note-1.md"].ends_with("Use SQLite for every namespace."));
}

#[test]
fn a_changed_tree_is_a_diff_never_a_deletion_or_correction() {
    let mut other = decision();
    other.id = "note-2".into();
    other.revision = 1;
    let records = vec![decision(), other.clone()];
    let mut files: BTreeMap<String, String> = render_tree(&records).unwrap();
    assert!(diff_tree(&records, &files).is_empty(), "unchanged tree");

    files.remove("note-2.md");
    let edited = files["note-1.md"].replace("Use SQLite", "Use Postgres");
    files.insert("note-1.md".into(), edited);
    let mut added = decision();
    added.id = "note-3".into();
    files.insert("note-3.md".into(), to_markdown(&added).unwrap());
    files.insert("broken.md".into(), "not front matter".into());
    let mut renamed = other;
    renamed.revision = 1;
    files.insert("elsewhere.md".into(), to_markdown(&renamed).unwrap());
    files.insert("README.txt".into(), "ignored: not a profile file".into());

    let changes = diff_tree(&records, &files);
    let summary: Vec<_> = changes
        .iter()
        .map(|d| (d.file.as_str(), d.id.as_deref(), &d.change))
        .collect();
    assert_eq!(
        summary,
        [
            ("broken.md", None, &Change::Invalid("invalid".into())),
            ("elsewhere.md", Some("note-2"), &Change::Misnamed),
            ("note-1.md", Some("note-1"), &Change::Edited),
            ("note-2.md", Some("note-2"), &Change::Missing),
            ("note-3.md", Some("note-3"), &Change::Added),
        ]
    );
    // The records are untouched: the diff is a report, not an operation.
    assert_eq!(records.len(), 2);
    assert_eq!(records[0], decision());
}
