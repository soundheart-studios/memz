mod common;
use common::Fixture;
use memz::{catalog::Catalog, state::StateRoot};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};

#[test]
fn private_state_and_sidecars_live_outside_project_roots() {
    let f = Fixture::new("layout");
    let home = f.root.join("home");
    fs::create_dir(&home).unwrap();
    assert_eq!(StateRoot::path_for(&home, None), home.join(".memz"));
    let custom = f.root.join("custom");
    assert_eq!(StateRoot::path_for(&home, Some(&custom)), custom);
    let state = StateRoot::open(&home.join(".memz")).unwrap();
    let project = f.root.join("project");
    fs::create_dir(&project).unwrap();
    let mut catalog = Catalog::open(state.clone()).unwrap();
    let id = catalog.register(&project).unwrap();
    let db = state.namespace_db(&id).unwrap();
    assert!(db.starts_with(&state.path));
    assert!(!db.starts_with(&project));
    assert_eq!(
        fs::metadata(&state.path).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&db).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE test(value TEXT);")
        .unwrap();
    for suffix in ["-wal", "-shm"] {
        let path = std::path::PathBuf::from(format!("{}{suffix}", db.display()));
        assert!(path.is_file());
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert!(state.namespace_db("../../escape").is_err());
}

#[test]
fn refuses_repository_state_symlinks_and_permissive_existing_roots() {
    let f = Fixture::new("unsafe");
    let repo = f.root.join("repo");
    fs::create_dir(&repo).unwrap();
    fs::create_dir(repo.join(".git")).unwrap();
    assert!(StateRoot::open(&repo.join(".memz")).is_err());
    let other = f.root.join("other");
    fs::create_dir(&other).unwrap();
    let link = f.root.join("link");
    symlink(&other, &link).unwrap();
    assert!(StateRoot::open(&link).is_err());
    fs::set_permissions(&other, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(StateRoot::open(&other).is_err());
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    let mut catalog = Catalog::open(state.clone()).unwrap();
    assert!(catalog.register(&f.root).is_err());
    let project = f.root.join("project");
    fs::create_dir(&project).unwrap();
    let id = catalog.register(&project).unwrap();
    let db = state.namespace_db(&id).unwrap();
    fs::remove_file(&db).unwrap();
    symlink(Path::new("/etc/passwd"), &db).unwrap();
    assert!(state.namespace_db(&id).is_err());
}

#[test]
fn root_identity_survives_git_operations_and_moves() {
    let f = Fixture::new("identity");
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    let mut cat = Catalog::open(state.clone()).unwrap();
    let project = f.root.join("project");
    fs::create_dir(&project).unwrap();
    let id = cat.register(&project).unwrap();
    assert_eq!(cat.register(&project).unwrap(), id);
    let sub = project.join("nested");
    fs::create_dir(&sub).unwrap();
    assert_eq!(cat.resolve(&sub, None).unwrap(), id);
    let nested = cat.register(&sub).unwrap();
    assert!(cat.resolve(&sub, None).is_err());
    assert_eq!(cat.resolve(&sub, Some(&nested)).unwrap(), nested);
    let sibling = f.root.join("project-other");
    fs::create_dir(&sibling).unwrap();
    assert!(cat.resolve(&sibling, None).is_err());
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&project)
        .status()
        .unwrap();
    assert!(status.success());
    fs::write(project.join("tracked.txt"), "baseline").unwrap();
    for args in [
        vec!["add", "tracked.txt"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "fixture baseline",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(&project)
                .status()
                .unwrap()
                .success()
        );
    }
    fs::write(project.join("tracked.txt"), "modified").unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["reset", "--hard", "--quiet", "HEAD"])
            .current_dir(&project)
            .status()
            .unwrap()
            .success()
    );
    assert!(state.namespace_db(&id).unwrap().is_file());
    fs::remove_dir_all(&project).unwrap();
    assert!(state.namespace_db(&id).unwrap().is_file());
    cat.reassign(&id, &sibling).unwrap();
    assert_eq!(cat.resolve(&sibling, None).unwrap(), id);
    drop(cat);
    let cat = Catalog::open(state).unwrap();
    assert_eq!(cat.resolve(&sibling, None).unwrap(), id);
}
