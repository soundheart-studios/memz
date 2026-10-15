use memz_qa::qualification::{evidence_metrics, metrics, validate_answers};
use serde_json::json;
use sha2::Digest;
#[test]
fn percentiles_reject_invalid_samples_and_use_nearest_rank() {
    let m = metrics(&[5., 1., 4., 2., 3.]).unwrap();
    assert_eq!(m["p50_ms"], 3.);
    assert_eq!(m["p95_ms"], 5.);
    assert_eq!(m["p99_ms"], 5.);
    assert!(metrics(&[]).is_err());
    assert!(metrics(&[f64::NAN]).is_err());
    assert!(metrics(&[-1.]).is_err());
}
#[test]
fn evidence_metrics_separate_misses_irrelevance_staleness_and_abstention() {
    let m = evidence_metrics(
        &["a".into(), "stale".into(), "noise".into()],
        &["a".into(), "b".into()],
        &["stale".into()],
    );
    assert_eq!(m["missed"], 1);
    assert_eq!(m["irrelevant"], 1);
    assert_eq!(m["stale"], 1);
    assert_eq!(m["relevant"], 1);
    assert_eq!(evidence_metrics(&[], &[], &[])["correct_abstention"], true);
    assert_eq!(
        evidence_metrics(&["x".into()], &[], &[])["correct_abstention"],
        false
    );
}
#[test]
fn answer_evidence_requires_all_methods_equal_budgets_and_matching_inputs() {
    let mut inputs = json!({"fingerprint":"abc","cases":["one","two"],"methods":["none","lexical","hybrid","graph"]});
    assert!(validate_answers(&inputs, &json!({})).is_err());
    let mut rows = vec![];
    for case in ["one", "two"] {
        for method in ["none", "lexical", "hybrid", "graph"] {
            rows.push(json!({"case":case,"method":method,"model":"fixture-model","max_output_tokens":128,"temperature":0,"answer":"unknown","input_sha256":format!("{:x}",sha2::Sha256::digest(b"prompt")),"output_tokens":2}));
        }
    }
    inputs["requests"] = json!(rows.iter().map(|r|json!({"case":r["case"],"method":r["method"],"input":"prompt","input_sha256":r["input_sha256"],"max_output_tokens":128})).collect::<Vec<_>>());
    inputs["fingerprint"] = json!(format!(
        "{:x}",
        sha2::Sha256::digest(serde_json::to_vec(&inputs["requests"]).unwrap())
    ));
    let mut report = json!({"fingerprint":inputs["fingerprint"],"rows":rows});
    assert!(validate_answers(&inputs, &report).is_ok());
    for row in report["rows"].as_array_mut().unwrap() {
        row["temperature"] = json!("provider-default");
    }
    assert!(validate_answers(&inputs, &report).is_ok());
    report["rows"][0]["temperature"] = json!(0);
    assert!(validate_answers(&inputs, &report).is_err());
    report["rows"][0]["temperature"] = json!("unknown");
    assert!(validate_answers(&inputs, &report).is_err());
    report["rows"][0]["temperature"] = serde_json::Value::Null;
    assert!(validate_answers(&inputs, &report).is_err());
    report["rows"][0]["temperature"] = json!("provider-default");
    report["rows"][0]["max_output_tokens"] = json!(256);
    assert!(validate_answers(&inputs, &report).is_err());
    report["rows"][0]["max_output_tokens"] = json!(128);
    report["fingerprint"] = json!("stale");
    assert!(validate_answers(&inputs, &report).is_err());
}
#[test]
fn invariant_harness_exercises_real_capture_scope_and_deletion() {
    let report = memz_qa::qualification::adversarial().unwrap();
    assert_eq!(report["status"], "passed");
    assert!(report["checks"].as_array().unwrap().len() >= 4);
}
#[test]
fn quality_runner_requires_an_explicit_real_embedding_model() {
    let error = memz_qa::qualification::quality_workload(
        std::path::Path::new("."),
        &json!({"version":1,"model_env":"MEMZ_UNCONFIGURED_QUALIFICATION_MODEL"}),
    )
    .unwrap_err();
    assert!(error.to_string().contains("explicit local embedding model"));
}
#[test]
fn small_performance_run_measures_real_operations_and_keeps_raw_samples() {
    let report = memz_qa::qualification::performance_workload(
        &json!({"sizes":[100],"samples":12,"concurrency":2,"seed_timeout_seconds":30}),
    )
    .unwrap();
    let mut report = report;
    report["source_fingerprint"] = json!("source");
    report["workload_sha256"] = json!("workload");
    memz_qa::qualification::validate_performance_report(&report, "source", "workload").unwrap();
    assert!(
        memz_qa::qualification::validate_performance_report(&report, "changed", "workload")
            .is_err()
    );
    let mut forged = report.clone();
    forged["scenarios"][0]["capture_sdk"]["latency"]["p95_ms"] = json!(999999.);
    assert!(
        memz_qa::qualification::validate_performance_report(&forged, "source", "workload").is_err()
    );
    let scenario = &report["scenarios"][0];
    assert_eq!(scenario["status"], "measured");
    assert_eq!(scenario["capture_sdk"]["latency"]["count"], 12);
    assert!(
        scenario["raw_samples"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["operation"] == "concurrent_recall")
    );
    assert!(scenario["db_bytes"].as_u64().unwrap() > 0);
}
#[test]
fn quality_report_validation_rejects_false_scores_and_missing_answer_evidence() {
    use memz_qa::qualification::{evidence_metrics, validate_quality_report};
    let corpus = json!({"memories":[{"key":"a"}],"cases":[{"id":"q","expected":["a@1"],"answer_contains":["teal"]}]});
    let mut requests = Vec::new();
    let mut observations = Vec::new();
    let mut answers = Vec::new();
    for method in ["none", "lexical", "hybrid", "graph"] {
        let input = format!("fixture prompt {method}");
        let digest = format!("{:x}", sha2::Sha256::digest(input.as_bytes()));
        requests.push(json!({"case":"q","method":method,"input":input,"input_sha256":digest,"max_output_tokens":128}));
        let retrieved = if method == "none" {
            vec![]
        } else {
            vec!["a@1".to_owned()]
        };
        observations.push(json!({"case":"q","method":method,"expected":["a@1"],"metrics":evidence_metrics(&retrieved,&["a@1".into()],&[]),"retrieved":retrieved,"latency_ms":1.,"packet":if method=="none"{serde_json::Value::Null}else{json!({"items":[{"id":"id-a","revision":1}]})}}));
        answers.push(json!({"case":"q","method":method,"model":"test-fixture-only","max_output_tokens":128,"temperature":0,"input_sha256":digest,"output_tokens":2,"answer":"teal","rubric_match":true}));
    }
    let digest = format!(
        "{:x}",
        sha2::Sha256::digest(serde_json::to_vec(&requests).unwrap())
    );
    let identity =
        memz::providers::Identity::for_artifacts("a".repeat(64), "b".repeat(64), "c".repeat(64));
    let mut report = json!({"format":"memz-quality-v1","source_fingerprint":"source","workload_sha256":"workload","answer_status":"measured","provider":identity,"labels":{"id-a":"a"},"observations":observations,"answer_inputs":{"fingerprint":digest,"requests":requests,"cases":["q"],"methods":["none","lexical","hybrid","graph"]},"answer_results":answers,"graph_default":"off"});
    validate_quality_report(&report, &corpus, "source", "workload").unwrap();
    report["observations"][0]["metrics"]["missed"] = json!(0);
    assert!(validate_quality_report(&report, &corpus, "source", "workload").is_err());
    report["observations"][0]["metrics"]["missed"] = json!(1);
    report["answer_results"][0]["rubric_match"] = json!(false);
    assert!(validate_quality_report(&report, &corpus, "source", "workload").is_err());
    report["answer_results"][0]["rubric_match"] = json!(true);
    report["answer_status"] = json!("unavailable");
    assert!(validate_quality_report(&report, &corpus, "source", "workload").is_err());
}
#[test]
fn source_fingerprint_includes_embedded_schema_and_viewer_assets() {
    let f = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(f.path().join("crates/core")).unwrap();
    std::fs::create_dir_all(f.path().join("assets/viewer")).unwrap();
    for (path, text) in [
        ("Cargo.toml", "workspace"),
        ("Cargo.lock", "locked"),
        ("crates/core/lib.rs", "fn core() {}"),
        ("crates/core/schema.sql", "CREATE TABLE a(x);"),
        ("assets/viewer/style.css", "body{color:black}"),
    ] {
        std::fs::write(f.path().join(path), text).unwrap();
    }
    let before = memz_qa::qualification::source_fingerprint(f.path()).unwrap();
    std::fs::write(
        f.path().join("crates/core/schema.sql"),
        "CREATE TABLE a(y);",
    )
    .unwrap();
    let schema = memz_qa::qualification::source_fingerprint(f.path()).unwrap();
    assert_ne!(before, schema);
    std::fs::write(
        f.path().join("assets/viewer/style.css"),
        "body{color:white}",
    )
    .unwrap();
    assert_ne!(
        schema,
        memz_qa::qualification::source_fingerprint(f.path()).unwrap()
    );
}
