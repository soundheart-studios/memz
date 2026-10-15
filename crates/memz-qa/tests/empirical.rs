use memz_qa::qualification::{
    source_fingerprint, validate_performance_report, validate_quality_report,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}
fn read(path: &str) -> Value {
    serde_json::from_slice(
        &fs::read(repo().join(path)).expect("required empirical artifact missing"),
    )
    .expect("malformed empirical artifact")
}
fn digest(path: &str) -> String {
    format!("{:x}", Sha256::digest(fs::read(repo().join(path)).unwrap()))
}
#[test]
fn retained_performance_matches_source_and_raw_measurements() {
    let report = read("qualification/reports/performance.json");
    validate_performance_report(
        &report,
        &source_fingerprint(&repo()).unwrap(),
        &digest("qualification/performance.json"),
    )
    .unwrap();
    assert_eq!(report["config"], read("qualification/performance.json"));
    assert!(
        report["hardware"]["compiler"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    assert_eq!(report["hardware"]["debug_assertions"], false);
}
#[test]
fn retained_quality_matches_source_and_actual_answer_evidence() {
    let report = read("qualification/reports/quality.json");
    validate_quality_report(
        &report,
        &read("qualification/quality-corpus.json"),
        &source_fingerprint(&repo()).unwrap(),
        &digest("qualification/quality.json"),
    )
    .unwrap();
    assert_eq!(report["graph_default"], "off");
    assert_eq!(report["hardware"]["debug_assertions"], false);
}
