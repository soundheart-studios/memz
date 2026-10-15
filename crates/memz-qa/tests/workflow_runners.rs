use memz_qa::{classify_test, run};
use serde_json::json;

#[test]
fn json_protocol_delivers_schema_violations_to_workflow_assertions() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fixture.sqlite");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch("CREATE TABLE example (value INTEGER);")
        .unwrap();
    let manifest = dir.path().join("schema.json");
    std::fs::write(
        &manifest,
        json!({"tables":[{"name":"example","columns":[{"name":"value","type":"TEXT"}]}]})
            .to_string(),
    )
    .unwrap();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_memz-qa"))
        .arg("schema-contract")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            json!({"database":db,"manifest":manifest})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "completed report must reach workflow assertions"
    );
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["violations"].as_array().unwrap().len(), 1);
}

#[test]
fn only_behavioral_test_failures_count_as_mutation_evidence() {
    assert_eq!(
        classify_test(
            false,
            "running 1 test\ntest meaningful ... FAILED\ntest result: FAILED. 0 passed; 1 failed;"
        ),
        Ok(true)
    );
    assert_eq!(
        classify_test(
            true,
            "running 1 test\ntest meaningful ... ok\ntest result: ok. 1 passed; 0 failed;"
        ),
        Ok(false)
    );
    for output in [
        "error[E0425]: cannot find value",
        "running 0 tests\ntest result: ok. 0 passed",
        "command not found",
        "error: could not compile",
    ] {
        assert!(classify_test(false, output).is_err());
    }
}

#[test]
fn schema_checks_column_type_pairs_not_independent_sets() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fixture.sqlite");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("CREATE TABLE example (name TEXT NOT NULL, count INTEGER); CREATE INDEX example_name ON example(name);").unwrap();
    let manifest = dir.path().join("schema.json");
    let good = json!({"tables":[{"name":"example","columns":[{"name":"name","type":"TEXT","notnull":true},{"name":"count","type":"INTEGER","notnull":false}],"indexes":["example_name"]}]});
    std::fs::write(&manifest, good.to_string()).unwrap();
    let result = run(
        "schema-contract",
        &json!({"database":db,"manifest":manifest}),
    )
    .unwrap();
    assert_eq!(result["violations"], json!([]));
    let mut swapped = good;
    swapped["tables"][0]["columns"][0]["type"] = json!("INTEGER");
    swapped["tables"][0]["columns"][1]["type"] = json!("TEXT");
    std::fs::write(&manifest, swapped.to_string()).unwrap();
    let result = run(
        "schema-contract",
        &json!({"database":db,"manifest":manifest}),
    )
    .unwrap();
    assert_eq!(result["violations"].as_array().unwrap().len(), 2);
}

#[test]
fn empty_schema_and_missing_database_are_errors() {
    assert!(
        run(
            "schema-contract",
            &json!({"database":"/nonexistent/memz.sqlite","manifest":"/nonexistent/manifest"})
        )
        .is_err()
    );
}

#[test]
fn release_requires_actual_checks_and_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = dir.path().join("release.json");
    std::fs::write(&manifest, r#"{"checks":[],"artifacts":[]}"#).unwrap();
    assert!(
        run(
            "release-validation",
            &json!({"repo":dir.path(),"manifest":manifest,"runId":"test-empty"})
        )
        .is_err()
    );
}

#[test]
fn mutation_rejects_missing_refs_and_invalid_repo() {
    assert!(run("verify-evidence", &json!({"repo":"/nonexistent","taskId":"1.1","territory":["src/lib.rs"],"testRefs":[],"criteria":[]})).is_err());
}

fn git(repo: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn repository() -> tempfile::TempDir {
    let run = std::env::var("PW_RUN_ID").expect("isolated run ID required");
    let dir = tempfile::Builder::new()
        .prefix(&format!("memz-qa-{run}-"))
        .tempdir()
        .unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::create_dir(root.join("tests")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"probe\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
    )
    .unwrap();
    std::fs::write(root.join(".gitignore"), "/target/\n").unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn enabled() -> bool { false }\n",
    )
    .unwrap();
    git(root, &["init", "--quiet"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "baseline"]);
    dir
}
#[test]
fn mutation_and_territory_operate_on_committed_isolated_history() {
    let dir = repository();
    let root = dir.path();
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn enabled() -> bool { true }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("tests/behavior.rs"),
        "#[test] fn feature_works(){assert!(probe::enabled());}\n",
    )
    .unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "1.1: enable feature"]);
    let tracked = std::process::Command::new("git")
        .args(["ls-files"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&tracked.stdout).contains("tests/behavior.rs"),
        "{}",
        String::from_utf8_lossy(&tracked.stdout)
    );
    let reference = "cargo test --test behavior";
    let request = json!({"repo":root,"taskId":"1.1","territory":["src/","tests/"],"testRefs":[reference],"criteria":[{"text":"feature enabled","refs":[reference]}]});
    let result = run("verify-evidence", &request).unwrap();
    assert_eq!(result["proved"], json!(["feature enabled"]));
    assert_eq!(result["failed"], json!([reference]));
    assert_eq!(
        std::fs::read_to_string(root.join("src/lib.rs")).unwrap(),
        "pub fn enabled() -> bool { true }\n"
    );
    let result = run(
        "territory-check",
        &json!({"repo":root,"taskId":"1.1","declared":["src/"]}),
    )
    .unwrap();
    assert_eq!(result["outside"], json!(["tests/behavior.rs"]));
    let result = run(
        "territory-check",
        &json!({"repo":root,"taskId":"1.1","declared":["src/","tests/"]}),
    )
    .unwrap();
    assert_eq!(result["outside"], json!([]));
}
#[test]
fn surviving_criteria_are_not_proved() {
    let dir = repository();
    let root = dir.path();
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn enabled() -> bool { true }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("tests/behavior.rs"),
        "#[test] fn unrelated(){assert_eq!(2+2,4);}\n",
    )
    .unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "1.1: unrelated test"]);
    let reference = "cargo test --test behavior";
    let result=run("verify-evidence",&json!({"repo":root,"taskId":"1.1","territory":["src/","tests/"],"testRefs":[reference],"criteria":[{"text":"feature enabled","refs":[reference]}]})).unwrap();
    assert_eq!(result["proved"], json!([]));
    assert_eq!(result["survived"], json!([reference]));
}
#[test]
fn release_reports_real_process_failure_and_missing_artifacts() {
    let dir = repository();
    let root = dir.path();
    let manifest = root.join("release.json");
    std::fs::write(root.join("receipt.txt"), "fixture evidence").unwrap();
    std::fs::write(
        &manifest,
        json!({"checks":[{"id":"negative","argv":["/usr/bin/false"]}],"artifacts":["receipt.txt"]})
            .to_string(),
    )
    .unwrap();
    let input = json!({"repo":root,"manifest":manifest,"runId":"release-test"});
    let result = run("release-validation", &input).unwrap();
    assert_eq!(result["failed"], json!(["negative"]));
    std::fs::write(&manifest,json!({"checks":[{"id":"compiler","argv":["rustc","--version"]}],"artifacts":["receipt.txt"]}).to_string()).unwrap();
    let result = run("release-validation", &input).unwrap();
    assert_eq!(result["passed"], json!(["compiler"]));
    std::fs::remove_file(root.join("receipt.txt")).unwrap();
    assert!(run("release-validation", &input).is_err());
}

#[test]
fn new_rust_api_can_be_removed_without_breaking_test_compilation() {
    let dir = repository();
    let root = dir.path();
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn enabled() -> bool { false }\npub fn new_feature() -> u32 { 42 }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("tests/behavior.rs"),
        "#[test] fn new_feature_works(){assert_eq!(probe::new_feature(),42);}\n",
    )
    .unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "1.1: add new API"]);
    let reference = "cargo test --test behavior";
    let result=run("verify-evidence",&json!({"repo":root,"taskId":"1.1","mode":"rust-function-bodies","territory":["src/","tests/"],"testRefs":[reference],"criteria":[{"text":"new API works","refs":[reference]}]})).unwrap();
    assert_eq!(result["proved"], json!(["new API works"]));
    assert_eq!(result["mutationMode"], json!("rust-function-bodies"));
}

#[test]
fn release_cli_accepts_a_manifest_and_run_id_environment() {
    let dir = repository();
    let root = dir.path();
    let manifest = root.join("release.json");
    std::fs::write(root.join("receipt.txt"), "fixture evidence").unwrap();
    std::fs::write(&manifest,json!({"checks":[{"id":"compiler","argv":["rustc","--version"]}],"artifacts":["receipt.txt"]}).to_string()).unwrap();
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_memz-qa"))
        .args(["release-validation", "--manifest"])
        .arg(&manifest)
        .args(["--run-id-env", "PW_RUN_ID"])
        .env("PW_RUN_ID", "cli-fixture")
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["passed"], json!(["compiler"]));
}
