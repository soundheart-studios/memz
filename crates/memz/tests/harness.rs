mod common;
use common::Fixture;

#[test]
fn harness_executes_assertions() {
    assert_eq!(2 + 2, 4);
}

#[test]
fn concurrent_fixtures_are_isolated() {
    let handles: Vec<_> = (0..4)
        .map(|_| {
            std::thread::spawn(|| {
                let fixture = Fixture::new("concurrent");
                let socket = fixture.root.join("daemon.sock");
                let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
                (fixture, listener)
            })
        })
        .collect();
    let fixtures: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let roots: std::collections::HashSet<_> = fixtures.iter().map(|(f, _)| &f.root).collect();
    assert_eq!(roots.len(), 4);
    for (fixture, _) in &fixtures {
        assert!(fixture.root.starts_with("/private/tmp"));
    }
}

#[test]
fn generated_state_is_ignored() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for path in [
        "target/artifact",
        "reports/run.json",
        "sample.sqlite",
        "sample.sqlite-wal",
        "sample.sqlite-shm",
    ] {
        let result = std::process::Command::new("git")
            .args(["check-ignore", "--quiet", path])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(result.success(), "not ignored: {path}");
    }
}
