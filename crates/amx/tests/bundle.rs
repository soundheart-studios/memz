use amx::{Bundle, Generator, NamespaceCursor, Record, Source, Tombstone, read_jsonl, sha256_hex};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn scratch(name: &str) -> PathBuf {
    let n = NEXT.fetch_add(1, Ordering::SeqCst);
    let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("amx-bundle-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root.join(name)
}

fn record(id: &str, revision: u64) -> Record {
    let v = json!({"amx":"0.1","id":id,"revision":revision,"kind":"fact","content":format!("content of {id}"),
        "scope":{"namespace":"ns-1","visibility":"project"},
        "provenance":{"origin":"user","author":"dev","explicit":true},
        "status":"active","valid_from":"2026-11-09T23:20:03Z","recorded_at":"2026-11-09T23:20:03Z"});
    read_jsonl(&format!("{v}\n")).unwrap().remove(0)
}

fn source(id: &str, text: &str) -> Source {
    let v = json!({"amx":"0.1","id":id,"revision":1,"origin":"user","author":"dev","text":text,
        "content_hash":format!("sha256:{}", sha256_hex(text.as_bytes())),
        "scope":{"namespace":"ns-1","visibility":"project"},"recorded_at":"2026-11-09T23:20:03Z"});
    read_jsonl(&format!("{v}\n")).unwrap().remove(0)
}

fn tombstone(id: &str, target: &str) -> Tombstone {
    let v = json!({"amx":"0.1","id":id,"target_type":"record","target_id":target,
        "scope":{"namespace":"ns-1","visibility":"project"},"deleted_at":"2026-11-09T23:30:00Z",
        "reason":"user_request","actor":"dev"});
    read_jsonl(&format!("{v}\n")).unwrap().remove(0)
}

fn sample() -> Bundle {
    let mut bundle = Bundle::new(
        "b-1",
        Generator {
            name: "amx-test".into(),
            version: "1".into(),
        },
        "2026-11-09T23:40:00Z",
        vec![NamespaceCursor {
            id: "ns-1".into(),
            cursor: 7,
        }],
    );
    bundle.records = vec![record("r-1", 1), record("r-1", 2), record("r-2", 1)];
    bundle.records[0].status = "superseded".into();
    bundle.sources = vec![source("s-1", "the original passage")];
    bundle.tombstones = vec![tombstone("t-1", "r-9")];
    bundle
}

#[test]
fn a_written_bundle_reads_back_identically() {
    let path = scratch("memory.amx");
    let bundle = sample();
    bundle.write(&path).unwrap();
    let mut names: Vec<_> = fs::read_dir(&path)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "CHECKSUMS",
            "links.jsonl",
            "manifest.json",
            "records.jsonl",
            "sources.jsonl",
            "tombstones.jsonl"
        ]
    );
    let back = Bundle::read(&path).unwrap();
    assert_eq!(back.records, bundle.records);
    assert_eq!(back.sources, bundle.sources);
    assert_eq!(back.links, bundle.links);
    assert_eq!(back.tombstones, bundle.tombstones);
    assert_eq!(back.manifest.counts.records, 3);
    assert_eq!(back.manifest.counts.sources, 1);
    assert_eq!(back.manifest.counts.tombstones, 1);
    assert_eq!(back.manifest.namespaces[0].cursor, 7);
    let checksums = fs::read_to_string(path.join("CHECKSUMS")).unwrap();
    let listed: Vec<_> = checksums
        .lines()
        .map(|l| l.split_once("  ").unwrap().1)
        .collect();
    assert_eq!(
        listed,
        [
            "links.jsonl",
            "manifest.json",
            "records.jsonl",
            "sources.jsonl",
            "tombstones.jsonl"
        ]
    );
}

#[test]
fn writing_is_deterministic_and_canonical() {
    let mut shuffled = sample();
    shuffled.records.reverse();
    let (a, b) = (scratch("a.amx"), scratch("b.amx"));
    sample().write(&a).unwrap();
    shuffled.write(&b).unwrap();
    for name in [
        "CHECKSUMS",
        "manifest.json",
        "records.jsonl",
        "tombstones.jsonl",
    ] {
        assert_eq!(
            fs::read(a.join(name)).unwrap(),
            fs::read(b.join(name)).unwrap(),
            "{name}"
        );
    }
}

#[test]
fn tampering_is_detected() {
    let path = scratch("t.amx");
    sample().write(&path).unwrap();
    let file = path.join("records.jsonl");
    let text = fs::read_to_string(&file).unwrap();
    fs::write(&file, text.replace("content of r-2", "content of r-X")).unwrap();
    assert_eq!(Bundle::read(&path).unwrap_err().code, "checksum");
}

#[test]
fn a_rewritten_checksum_does_not_hide_a_bad_record() {
    let path = scratch("c.amx");
    sample().write(&path).unwrap();
    let file = path.join("records.jsonl");
    let text = fs::read_to_string(&file).unwrap();
    let bad = text.replacen("\"author\":\"dev\"", "\"author\":\"\"", 1);
    fs::write(&file, &bad).unwrap();
    rewrite_checksums(&path);
    assert_eq!(Bundle::read(&path).unwrap_err().code, "invalid");
}

fn rewrite_checksums(path: &Path) {
    let mut out = String::new();
    for name in [
        "links.jsonl",
        "manifest.json",
        "records.jsonl",
        "sources.jsonl",
        "tombstones.jsonl",
    ] {
        let bytes = fs::read(path.join(name)).unwrap();
        out.push_str(&format!("{}  {name}\n", sha256_hex(&bytes)));
    }
    fs::write(path.join("CHECKSUMS"), out).unwrap();
}

#[test]
fn counts_and_source_hashes_are_verified() {
    let path = scratch("n.amx");
    sample().write(&path).unwrap();
    let manifest = fs::read_to_string(path.join("manifest.json")).unwrap();
    fs::write(
        path.join("manifest.json"),
        manifest.replace("\"records\":3", "\"records\":4"),
    )
    .unwrap();
    rewrite_checksums(&path);
    assert_eq!(Bundle::read(&path).unwrap_err().code, "invalid");

    let path = scratch("h.amx");
    let mut bundle = sample();
    bundle.sources[0].text = "a different passage".into();
    assert_eq!(bundle.write(&path).unwrap_err().code, "invalid");
}

#[test]
fn records_must_belong_to_listed_namespaces_and_be_unique() {
    let path = scratch("ns.amx");
    let mut bundle = sample();
    bundle.records[2].scope.namespace = "elsewhere".into();
    assert_eq!(bundle.write(&path).unwrap_err().code, "invalid");

    let path = scratch("dup.amx");
    let mut bundle = sample();
    bundle.records.push(record("r-2", 1));
    assert_eq!(bundle.write(&path).unwrap_err().code, "invalid");
}

#[test]
fn extra_files_and_symlinks_are_rejected() {
    let path = scratch("x.amx");
    sample().write(&path).unwrap();
    fs::write(path.join("notes.txt"), "hi").unwrap();
    assert_eq!(Bundle::read(&path).unwrap_err().code, "invalid");

    let path = scratch("s.amx");
    sample().write(&path).unwrap();
    let outside = path.parent().unwrap().join("outside.jsonl");
    fs::write(&outside, "").unwrap();
    fs::remove_file(path.join("links.jsonl")).unwrap();
    std::os::unix::fs::symlink(&outside, path.join("links.jsonl")).unwrap();
    assert_eq!(Bundle::read(&path).unwrap_err().code, "invalid");

    let path = scratch("real.amx");
    sample().write(&path).unwrap();
    let link = path.parent().unwrap().join("link.amx");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert_eq!(Bundle::read(&link).unwrap_err().code, "invalid");

    let path = scratch("sub.amx");
    sample().write(&path).unwrap();
    fs::create_dir(path.join("nested")).unwrap();
    assert_eq!(Bundle::read(&path).unwrap_err().code, "invalid");
}

#[test]
fn missing_files_and_bad_names_are_rejected() {
    let path = scratch("m.amx");
    sample().write(&path).unwrap();
    fs::remove_file(path.join("tombstones.jsonl")).unwrap();
    assert_eq!(Bundle::read(&path).unwrap_err().code, "invalid");

    let path = scratch("no-extension");
    assert_eq!(sample().write(&path).unwrap_err().code, "invalid");

    let path = scratch("exists.amx");
    sample().write(&path).unwrap();
    assert_eq!(sample().write(&path).unwrap_err().code, "exists");
}

#[test]
fn a_signature_file_is_carried_but_not_required() {
    let path = scratch("sig.amx");
    sample().write(&path).unwrap();
    assert_eq!(Bundle::read(&path).unwrap().signature, None);
    fs::write(path.join("manifest.sig"), "ed25519:abc:def\n").unwrap();
    assert_eq!(
        Bundle::read(&path).unwrap().signature.as_deref(),
        Some("ed25519:abc:def\n")
    );
}
