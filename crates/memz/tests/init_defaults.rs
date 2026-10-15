mod common;
use common::Fixture;
use serde_json::Value;
use std::{fs, path::Path, process::Command};

fn init(home: &Path, cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_memz"))
        .env("MEMZ_HOME", home)
        .current_dir(cwd)
        .arg("init")
        .args(args)
        .output()
        .unwrap()
}
fn report(out: &std::process::Output) -> Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn init_defaults_to_git_toplevel_os_user_and_fresh_session() {
    let f = Fixture::new("init-defaults");
    let home = f.root.join("state");
    let project = f.root.join("project");
    let nested = project.join("crates").join("inner");
    fs::create_dir_all(&nested).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&project)
            .status()
            .unwrap()
            .success()
    );
    let canonical = fs::canonicalize(&project).unwrap();

    // From a subdirectory, no arguments: the git work tree is the root.
    let first = report(&init(&home, &nested, &[]));
    assert_eq!(first["root"], canonical.to_str().unwrap());
    let user = first["principal"].as_str().unwrap();
    assert!(!user.is_empty());
    let session = first["session"].as_str().unwrap();
    assert!(session.starts_with("s_"));
    let client: Value =
        serde_json::from_slice(&fs::read(first["client_file"].as_str().unwrap()).unwrap()).unwrap();
    assert_eq!(client["namespace"], first["namespace"]);
    assert_eq!(client["binding"]["principal"], user);
    assert_eq!(client["binding"]["session"], session);

    // A second bare init reuses the namespace but issues a distinct session.
    let second = report(&init(&home, &project, &[]));
    assert_eq!(second["namespace"], first["namespace"]);
    assert_ne!(second["session"], first["session"]);
    assert_ne!(second["client_file"], first["client_file"]);

    // Outside any git work tree the current directory is the root.
    let plain = f.root.join("plain");
    fs::create_dir(&plain).unwrap();
    let loose = report(&init(&home, &plain, &[]));
    assert_eq!(
        loose["root"],
        fs::canonicalize(&plain).unwrap().to_str().unwrap()
    );
    assert_ne!(loose["namespace"], first["namespace"]);

    // Explicit arguments still win, including the original three-argument form.
    let explicit = report(&init(
        &home,
        &plain,
        &[project.to_str().unwrap(), "alice", "chat-1"],
    ));
    assert_eq!(explicit["namespace"], first["namespace"]);
    assert_eq!(explicit["principal"], "alice");
    assert_eq!(explicit["session"], "chat-1");
    let partial = report(&init(&home, &plain, &[project.to_str().unwrap(), "bob"]));
    assert_eq!(partial["principal"], "bob");
    assert!(partial["session"].as_str().unwrap().starts_with("s_"));

    // A session owned by another principal is still refused.
    assert!(
        !init(
            &home,
            &plain,
            &[project.to_str().unwrap(), "mallory", "chat-1"]
        )
        .status
        .success()
    );
    // Too many arguments remain a usage error.
    assert!(!init(&home, &plain, &["a", "b", "c", "d"]).status.success());
}

#[test]
fn bare_init_refuses_a_root_containing_the_state_directory() {
    let f = Fixture::new("init-home");
    let home = f.root.join("state");
    let out = init(&home, &f.root, &[]);
    assert!(!out.status.success());
    assert!(!home.join("run").exists() || fs::read_dir(home.join("run")).unwrap().count() == 0);
}
