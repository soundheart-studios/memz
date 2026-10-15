mod common;
use common::Fixture;
use memz::{
    adapters::{CapturePolicy, dispatch, hook_request, initialize, load_client, serve},
    state::StateRoot,
};
use serde_json::{Value, json};
use std::{
    io::Write,
    process::{Command, Stdio},
};
fn cli(state: &StateRoot, client: &std::path::Path, mode: &str, input: &Value) -> Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_memz"))
        .args([mode, "--client"])
        .arg(client)
        .env("MEMZ_HOME", &state.path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    if mode == "mcp" {
        writeln!(stdin,"{}",json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}})).unwrap();
    }
    writeln!(stdin, "{input}").unwrap();
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_str(String::from_utf8_lossy(&out.stdout).lines().last().unwrap()).unwrap()
}
#[test]
fn sdk_cli_and_mcp_share_receipts_scope_errors_and_budgets() {
    let f = Fixture::new("adapters");
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    std::fs::create_dir(f.root.join("project")).unwrap();
    let path = initialize(&state, &f.root.join("project"), "user", "session").unwrap();
    let client = load_client(&path).unwrap();
    let _server = serve(&state).unwrap();
    let remember = json!({"op":"remember","request_id":"same","text":"blue door"});
    let sdk = dispatch(&state, &client, &remember).unwrap();
    let cli_result = cli(&state, &path, "call", &remember);
    assert_eq!(sdk, cli_result);
    let mcp = cli(
        &state,
        &path,
        "mcp",
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"remember","arguments":{"request_id":"same","text":"blue door"}}}),
    );
    assert_eq!(mcp["result"]["structuredContent"], sdk);
    let denied = cli(
        &state,
        &path,
        "mcp",
        &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"grant","arguments":{}}}),
    );
    assert!(denied["error"].is_object());
    assert!(
        dispatch(
            &state,
            &client,
            &json!({"op":"remember","request_id":"forged","text":"x","author":"admin"})
        )
        .is_err()
    );
    let query = json!({"op":"recall","query":"door","budget":2000});
    let sdk = dispatch(&state, &client, &query).unwrap();
    assert_eq!(sdk, cli(&state, &path, "call", &query));
    let response = cli(
        &state,
        &path,
        "mcp",
        &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"recall","arguments":{"query":"door","budget":2000}}}),
    );
    assert!(serde_json::to_vec(&response).unwrap().len() <= 2000);
    assert!(response["result"]["structuredContent"].is_object());
    let corrected = cli(
        &state,
        &path,
        "call",
        &json!({"op":"correct","id":cli_result["id"],"expected_revision":1,"text":"green door"}),
    );
    assert_eq!(corrected["revision"], 2);
    let stale = cli(
        &state,
        &path,
        "mcp",
        &json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"correct","arguments":{"id":cli_result["id"],"expected_revision":1,"text":"stale"}}}),
    );
    assert_eq!(stale["result"]["isError"], true);
    let exported = dispatch(&state, &client, &json!({"op":"export"})).unwrap();
    assert_eq!(
        exported,
        cli(&state, &path, "call", &json!({"op":"export"}))
    );
    let via_mcp = cli(
        &state,
        &path,
        "mcp",
        &json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"export","arguments":{}}}),
    );
    assert_eq!(via_mcp["result"]["structuredContent"], exported);
    assert_eq!(
        dispatch(
            &state,
            &client,
            &json!({"op":"validate_import","jsonl":exported["jsonl"]})
        )
        .unwrap()["validated"],
        true
    );
    let health = dispatch(&state, &client, &json!({"op":"status"})).unwrap();
    assert_eq!(health, cli(&state, &path, "call", &json!({"op":"status"})));
    let mcp_health = cli(
        &state,
        &path,
        "mcp",
        &json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"status","arguments":{}}}),
    );
    assert_eq!(mcp_health["result"]["structuredContent"], health);
}
#[test]
fn capture_bridge_excludes_generated_events_until_opted_in() {
    let event = json!({"role":"assistant","text":"generated","event_id":"one"});
    assert!(
        hook_request(&event, CapturePolicy::default())
            .unwrap()
            .is_none()
    );
    let request = hook_request(
        &event,
        CapturePolicy {
            include_generated: true,
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(request["origin"], "assistant");
    assert!(
        hook_request(
            &json!({"role":"tool","text":"output","event_id":"two"}),
            CapturePolicy::default()
        )
        .unwrap()
        .is_none()
    );
    assert!(
        hook_request(
            &json!({"role":"user","text":"input","event_id":"three"}),
            CapturePolicy::default()
        )
        .unwrap()
        .is_some()
    );
}
