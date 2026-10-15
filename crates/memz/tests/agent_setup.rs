mod common;
use common::Fixture;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

fn memz(home: &Path, cwd: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_memz"))
        .env("MEMZ_HOME", home)
        .current_dir(cwd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    if let Some(text) = stdin {
        input.write_all(text.as_bytes()).unwrap();
    }
    drop(input);
    child.wait_with_output().unwrap()
}
fn ok(out: Output) -> Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .last()
            .unwrap()
            .as_bytes(),
    )
    .unwrap()
}
fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success()
    );
}
fn project(f: &Fixture, name: &str) -> PathBuf {
    let dir = f.root.join(name);
    fs::create_dir_all(dir.join("sub")).unwrap();
    git(&dir, &["init", "--quiet"]);
    fs::canonicalize(dir).unwrap()
}
fn action(report: &Value, suffix: &str) -> String {
    report["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["path"].as_str().unwrap().ends_with(suffix))
        .unwrap_or_else(|| panic!("no change for {suffix}: {report}"))["action"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Kills a daemon that `memz call` started in the background, even on panic.
struct Daemon(PathBuf);
impl Drop for Daemon {
    fn drop(&mut self) {
        let log = fs::read_to_string(self.0.join("run/serve.log")).unwrap_or_default();
        for pid in log
            .lines()
            .filter_map(|l| l.strip_prefix("memz ready (pid "))
            .filter_map(|l| l.trim_end_matches(')').parse::<i32>().ok())
        {
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
    }
}

#[test]
fn init_connects_claude_and_codex_without_clobbering_existing_configuration() {
    let f = Fixture::new("agent-setup");
    let home = f.root.join("state");
    let root = project(&f, "proj");
    fs::write(
        root.join(".mcp.json"),
        r#"{"mcpServers":{"planwright":{"command":"pw","args":["serve"]}}}"#,
    )
    .unwrap();
    fs::create_dir_all(root.join(".codex")).unwrap();
    fs::write(
        root.join(".codex/config.toml"),
        "model = \"gpt\"\n\n[mcp_servers.other]\ncommand = \"x\"\n",
    )
    .unwrap();
    fs::set_permissions(
        root.join(".codex/config.toml"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();

    let report = ok(memz(&home, &root.join("sub"), &["init"], None));
    let client = report["client_file"].as_str().unwrap().to_string();
    let exe = fs::canonicalize(env!("CARGO_BIN_EXE_memz")).unwrap();

    // Claude Code: merged, existing servers kept, pre-approved locally.
    let mcp: Value =
        serde_json::from_str(&fs::read_to_string(root.join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(mcp["mcpServers"]["planwright"]["command"], "pw");
    assert_eq!(mcp["mcpServers"]["memz"]["command"], exe.to_str().unwrap());
    assert_eq!(
        mcp["mcpServers"]["memz"]["args"],
        json!(["mcp", "--client", client])
    );
    assert_eq!(
        mcp["mcpServers"]["memz"]["env"]["MEMZ_HOME"],
        home.to_str().unwrap()
    );
    let settings: Value = serde_json::from_str(
        &fs::read_to_string(root.join(".claude/settings.local.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(settings["enabledMcpjsonServers"], json!(["memz"]));

    // Codex: one managed block appended, other content and permissions kept.
    let codex = fs::read_to_string(root.join(".codex/config.toml")).unwrap();
    assert!(codex.starts_with("model = \"gpt\"\n\n[mcp_servers.other]\ncommand = \"x\"\n"));
    assert_eq!(codex.matches("[mcp_servers.memz]").count(), 1);
    assert!(codex.contains(&format!("args = [\"mcp\", \"--client\", \"{client}\"]")));
    assert_eq!(
        fs::metadata(root.join(".codex/config.toml"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    // Skill for both harnesses, embedded in the binary.
    for base in [".claude/skills/memz", ".agents/skills/memz"] {
        assert!(
            fs::read_to_string(root.join(base).join("SKILL.md"))
                .unwrap()
                .contains("name: memz")
        );
        assert!(root.join(base).join("triggers.md").exists());
    }

    // Machine-specific files stay out of git; skills do not.
    let exclude = fs::read_to_string(root.join(".git/info/exclude")).unwrap();
    for pattern in [
        "/.mcp.json",
        "/.claude/settings.local.json",
        "/.codex/config.toml",
    ] {
        assert!(
            exclude.lines().any(|l| l == pattern),
            "{pattern} not excluded"
        );
    }
    assert!(!exclude.contains("skills"));

    // Re-running replaces the client in place: no duplicates, skills unchanged.
    let again = ok(memz(&home, &root, &["init"], None));
    let client2 = again["client_file"].as_str().unwrap();
    assert_ne!(client2, client);
    let codex = fs::read_to_string(root.join(".codex/config.toml")).unwrap();
    assert_eq!(codex.matches("[mcp_servers.memz]").count(), 1);
    assert!(codex.contains(client2) && !codex.contains(&client));
    let mcp: Value =
        serde_json::from_str(&fs::read_to_string(root.join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(mcp["mcpServers"]["memz"]["args"][2], client2);
    assert_eq!(action(&again, ".claude/skills/memz/SKILL.md"), "unchanged");
    assert_eq!(action(&again, ".claude/settings.local.json"), "unchanged");
    let exclude2 = fs::read_to_string(root.join(".git/info/exclude")).unwrap();
    assert_eq!(exclude, exclude2);
}

#[test]
fn init_reports_tracked_files_refuses_bad_json_and_honours_no_agents() {
    let f = Fixture::new("agent-edge");
    let home = f.root.join("state");

    // A tracked .mcp.json is still configured but flagged, not excluded.
    let tracked = project(&f, "tracked");
    fs::write(tracked.join(".mcp.json"), "{}\n").unwrap();
    git(&tracked, &["add", ".mcp.json"]);
    let report = ok(memz(&home, &tracked, &["init"], None));
    assert!(
        report["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["path"].as_str().unwrap().ends_with("/.mcp.json")
                && c["action"]
                    .as_str()
                    .unwrap()
                    .starts_with("warning: tracked")),
        "{report}"
    );
    let exclude = fs::read_to_string(tracked.join(".git/info/exclude")).unwrap_or_default();
    assert!(!exclude.lines().any(|l| l == "/.mcp.json"));

    // Invalid JSON is an error and the file is left untouched.
    let broken = project(&f, "broken");
    fs::write(broken.join(".mcp.json"), "{ not json").unwrap();
    let out = memz(&home, &broken, &["init"], None);
    assert!(!out.status.success());
    assert_eq!(
        fs::read_to_string(broken.join(".mcp.json")).unwrap(),
        "{ not json"
    );

    // An unmanaged memz table in Codex is reported instead of duplicated.
    let manual = project(&f, "manual");
    fs::create_dir_all(manual.join(".codex")).unwrap();
    fs::write(
        manual.join(".codex/config.toml"),
        "[mcp_servers.memz]\ncommand = \"mine\"\n",
    )
    .unwrap();
    let report = ok(memz(&home, &manual, &["init"], None));
    assert!(action(&report, ".codex/config.toml").starts_with("skipped"));
    assert_eq!(
        fs::read_to_string(manual.join(".codex/config.toml")).unwrap(),
        "[mcp_servers.memz]\ncommand = \"mine\"\n"
    );

    // --no-agents issues a client and writes nothing into the project.
    let bare = project(&f, "bare");
    let report = ok(memz(&home, &bare, &["init", "--no-agents"], None));
    assert!(report["agents"].is_null());
    assert!(!bare.join(".mcp.json").exists() && !bare.join(".codex").exists());
    assert!(
        !memz(&home, &bare, &["init", "--bogus"], None)
            .status
            .success()
    );
}

#[test]
fn clients_work_without_starting_or_restarting_the_daemon() {
    let f = Fixture::new("agent-live");
    let home = f.root.join("state");
    let _daemon = Daemon(home.clone());
    let first = project(&f, "first");
    let a = ok(memz(&home, &first, &["init", "--no-agents"], None));

    // No daemon is running: the first client command starts one.
    let remember = r#"{"op":"remember","request_id":"r1","text":"first project fact"}"#;
    let receipt = ok(memz(
        &home,
        &first,
        &["call", "--client", a["client_file"].as_str().unwrap()],
        Some(&format!("{remember}\n")),
    ));
    assert_eq!(receipt["revision"], 1);
    let log = fs::read_to_string(home.join("run/serve.log")).unwrap();
    assert_eq!(log.matches("memz ready").count(), 1);

    // A client issued after the daemon started is admitted without a restart.
    let second = project(&f, "second");
    let b = ok(memz(&home, &second, &["init", "--no-agents"], None));
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let mcp = ok(memz(
        &home,
        &second,
        &["mcp", "--client", b["client_file"].as_str().unwrap()],
        Some(&format!(
            "{}\n{}\n",
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"remember","arguments":{"request_id":"r2","text":"second project fact"}}})
        )),
    ));
    assert_eq!(mcp["result"]["structuredContent"]["revision"], 1, "{mcp}");

    // Still one daemon, and each client stays inside its own project.
    let log = fs::read_to_string(home.join("run/serve.log")).unwrap();
    assert_eq!(log.matches("memz ready").count(), 1);
    let recall = ok(memz(
        &home,
        &first,
        &["call", "--client", a["client_file"].as_str().unwrap()],
        Some("{\"op\":\"recall\",\"query\":\"project fact\"}\n"),
    ));
    let text = recall.to_string();
    assert!(text.contains("first project fact") && !text.contains("second project fact"));

    // An unknown token is still denied after the rescan.
    let forged = f.root.join("forged.json");
    let mut client: Value =
        serde_json::from_str(&fs::read_to_string(a["client_file"].as_str().unwrap()).unwrap())
            .unwrap();
    client["binding"]["token"] = json!("0".repeat(64));
    fs::write(&forged, client.to_string()).unwrap();
    fs::set_permissions(&forged, fs::Permissions::from_mode(0o600)).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let out = memz(
        &home,
        &first,
        &["call", "--client", forged.to_str().unwrap()],
        Some("{\"op\":\"status\"}\n"),
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("denied"), "{text}");
}
