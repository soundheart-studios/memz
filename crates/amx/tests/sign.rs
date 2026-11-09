#![cfg(feature = "signing")]
use amx::{
    Bundle, Generator, NamespaceCursor, Tombstone, public_key, read_jsonl, read_verified,
    sha256_hex, sign_bundle, verify_files,
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
const SECRET: [u8; 32] = [7; 32];
const OTHER: [u8; 32] = [9; 32];

fn scratch(name: &str) -> PathBuf {
    let n = NEXT.fetch_add(1, Ordering::SeqCst);
    let root =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("amx-sign-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root.join(name)
}

fn sample() -> Bundle {
    let mut bundle = Bundle::new(
        "b-1",
        Generator {
            name: "t".into(),
            version: "1".into(),
        },
        "2026-11-09T23:40:00Z",
        vec![NamespaceCursor {
            id: "ns".into(),
            cursor: 1,
        }],
    );
    bundle.records = read_jsonl(&format!(
        "{}\n",
        json!({"amx":"0.1","id":"r-1","revision":1,"kind":"fact","content":"signed content",
            "scope":{"namespace":"ns","visibility":"project"},
            "provenance":{"origin":"user","author":"dev","explicit":true},
            "status":"active","valid_from":"2026-11-09T23:20:03Z","recorded_at":"2026-11-09T23:20:03Z"})
    ))
    .unwrap();
    let tombstone: Tombstone = read_jsonl(&format!(
        "{}\n",
        json!({"amx":"0.1","id":"t-1","target_type":"record","target_id":"r-9",
            "scope":{"namespace":"ns","visibility":"project"},"deleted_at":"2026-11-09T23:30:00Z",
            "reason":"user_request","actor":"dev"})
    ))
    .unwrap()
    .remove(0);
    bundle.tombstones = vec![tombstone];
    bundle
}

fn rewrite_checksums(path: &Path) {
    let mut out = String::new();
    for name in amx::CHECKSUMMED {
        let bytes = fs::read(path.join(name)).unwrap();
        out.push_str(&format!("{}  {name}\n", sha256_hex(&bytes)));
    }
    fs::write(path.join("CHECKSUMS"), out).unwrap();
}

fn signed_at(name: &str) -> PathBuf {
    let path = scratch(name);
    let mut bundle = sample();
    sign_bundle(&mut bundle, &SECRET).unwrap();
    bundle.write(&path).unwrap();
    path
}

#[test]
fn a_signed_bundle_verifies() {
    let path = signed_at("ok.amx");
    let signature = fs::read_to_string(path.join("manifest.sig")).unwrap();
    assert!(signature.starts_with("ed25519:") && signature.ends_with('\n'));
    let (bundle, key) = read_verified(&path, None).unwrap();
    assert_eq!(key, public_key(&SECRET));
    assert_eq!(bundle.records[0].content, "signed content");
    assert!(
        bundle
            .manifest
            .capabilities
            .as_ref()
            .is_some_and(|c| c.iter().any(|c| c == "signing"))
    );
    read_verified(&path, Some(&public_key(&SECRET))).unwrap();
    // Readers that do not require signatures still read it.
    Bundle::read(&path).unwrap();
}

#[test]
fn the_wrong_key_is_rejected() {
    let path = signed_at("key.amx");
    let error = read_verified(&path, Some(&public_key(&OTHER))).unwrap_err();
    assert_eq!(error.code, "signature");
}

#[test]
fn a_changed_manifest_fails_verification() {
    let path = signed_at("manifest.amx");
    let manifest = fs::read_to_string(path.join("manifest.json")).unwrap();
    fs::write(
        path.join("manifest.json"),
        manifest.replace("\"cursor\":1", "\"cursor\":2"),
    )
    .unwrap();
    rewrite_checksums(&path);
    Bundle::read(&path).unwrap();
    assert_eq!(read_verified(&path, None).unwrap_err().code, "signature");
}

#[test]
fn a_changed_tombstone_fails_verification() {
    let path = signed_at("tomb.amx");
    let tombstones = fs::read_to_string(path.join("tombstones.jsonl")).unwrap();
    fs::write(
        path.join("tombstones.jsonl"),
        tombstones.replace("\"target_id\":\"r-9\"", "\"target_id\":\"r-8\""),
    )
    .unwrap();
    rewrite_checksums(&path);
    Bundle::read(&path).unwrap();
    assert_eq!(read_verified(&path, None).unwrap_err().code, "signature");
}

#[test]
fn unsigned_or_malformed_signatures_are_rejected() {
    let path = scratch("plain.amx");
    sample().write(&path).unwrap();
    assert_eq!(read_verified(&path, None).unwrap_err().code, "unsigned");

    let path = signed_at("bad.amx");
    fs::write(path.join("manifest.sig"), "ed25519:not-base64:also-not\n").unwrap();
    assert_eq!(read_verified(&path, None).unwrap_err().code, "signature");

    let path = signed_at("rsa.amx");
    fs::write(path.join("manifest.sig"), "rsa:abc:def\n").unwrap();
    assert_eq!(read_verified(&path, None).unwrap_err().code, "signature");
}

#[test]
fn in_memory_files_verify_too() {
    let mut bundle = sample();
    sign_bundle(&mut bundle, &SECRET).unwrap();
    let files: BTreeMap<String, Vec<u8>> = bundle
        .encode()
        .unwrap()
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    let (back, key) = verify_files(&files, Some(&public_key(&SECRET))).unwrap();
    assert_eq!(key, public_key(&SECRET));
    assert_eq!(back.tombstones, bundle.tombstones);

    let mut tampered = files.clone();
    tampered.insert("CHECKSUMS".into(), b"tampered\n".to_vec());
    assert!(verify_files(&tampered, None).is_err());
}
