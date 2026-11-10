mod common;
use common::Fixture;
use memz::{
    adapters::{initialize, load_client, mcp, serve_with_model},
    state::StateRoot,
};
use serde_json::{Value, json};

fn call(id: u64, name: &str, args: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":args}})
}

/// The bundle dry run must work through MCP itself, the way an agent calls
/// it, not only through the internal dispatcher.
#[test]
fn validate_import_takes_a_bundle_over_mcp() {
    let f = Fixture::new("amx-mcp");
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    std::fs::create_dir(f.root.join("p")).unwrap();
    let client = load_client(&initialize(&state, &f.root.join("p"), "u", "s").unwrap()).unwrap();
    let _service = serve_with_model(&state, None).unwrap();

    let tools = mcp(
        &state,
        &client,
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
    );
    let schema = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "validate_import")
        .unwrap()["inputSchema"]
        .clone();
    assert!(schema["properties"]["bundle"].is_object());
    assert_eq!(schema["required"], json!([]));

    let saved = mcp(
        &state,
        &client,
        &call(
            2,
            "remember",
            json!({"request_id":"r1","text":"User: bundles travel between projects."}),
        ),
    );
    assert_eq!(saved["result"]["isError"], false, "{saved}");
    let exported = mcp(&state, &client, &call(3, "export", json!({"name":"mine"})));
    assert_eq!(exported["result"]["isError"], false, "{exported}");

    let report = mcp(
        &state,
        &client,
        &call(4, "validate_import", json!({"bundle":"mine"})),
    );
    assert!(report.get("error").is_none(), "{report}");
    let body = &report["result"]["structuredContent"];
    assert_eq!(body["mode"], "dry_run", "{report}");
    assert_eq!(body["clean"], true);
    assert_eq!(body["adds"], 0);
    assert!(body["noops"].as_u64().unwrap() >= 1);

    let refused = mcp(
        &state,
        &client,
        &call(5, "validate_import", json!({"bundle":"../etc"})),
    );
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert_eq!(
        refused["result"]["structuredContent"]["error"]["code"],
        "invalid_filename"
    );
}
