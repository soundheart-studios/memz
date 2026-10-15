use serde_json::Value;
use std::{collections::BTreeSet, fs, path::PathBuf};

#[test]
fn release_manifest_requires_all_checks_and_current_guides() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest: Value = serde_json::from_slice(
        &fs::read(root.join("qualification/release.json")).expect("release manifest missing"),
    )
    .unwrap();
    let checks = manifest["checks"].as_array().unwrap();
    let expected = [
        ("format", "cargo fmt --all -- --check"),
        (
            "lint",
            "cargo clippy --workspace --all-targets --all-features --locked -- -D warnings",
        ),
        (
            "ordinary-tests",
            "cargo test --workspace --all-features --locked",
        ),
        (
            "real-local-model",
            "cargo test --locked -p memz --test embedding --test hybrid -- --ignored",
        ),
        (
            "build",
            "cargo build --workspace --all-targets --all-features --locked",
        ),
        (
            "release-build",
            "cargo build --release --locked -p memz -p memz-qa",
        ),
        (
            "empirical-evidence",
            "cargo test --locked -p memz-qa --test empirical",
        ),
        (
            "release-contract",
            "cargo test --locked -p memz-qa --test release_contract",
        ),
    ];
    let ids: BTreeSet<_> = checks.iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(ids.len(), checks.len(), "duplicate release check");
    for (id, command) in expected {
        let check = checks
            .iter()
            .find(|c| c["id"] == id)
            .expect("required check omitted");
        let argv: Vec<_> = check["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(argv.join(" "), command, "changed or narrowed release check");
    }
    for path in [
        "qualification/reports/performance.json",
        "qualification/reports/quality.json",
        "qualification/quality-corpus.json",
        "docs/qualification.md",
        "docs/release-evidence.md",
        "docs/harnesses.md",
        "docs/providers.md",
        "docs/recovery.md",
        "docs/viewer.md",
    ] {
        assert!(
            manifest["artifacts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == path)
        );
        assert!(
            !fs::read(root.join(path))
                .expect("release artifact missing")
                .is_empty()
        );
    }
    let readme = fs::read_to_string(root.join("README.md")).expect("setup entry point missing");
    for guide in [
        "docs/harnesses.md",
        "docs/providers.md",
        "docs/recovery.md",
        "docs/viewer.md",
    ] {
        assert!(readme.contains(guide), "setup guide not discoverable");
    }
}
