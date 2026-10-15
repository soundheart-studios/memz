use std::{
    fs,
    hash::{Hash, Hasher},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

/// Short, private-capable temporary base; socket paths must stay under SUN_LEN.
pub const TMP: &str = if cfg!(target_os = "macos") {
    "/private/tmp"
} else {
    "/tmp"
};

pub struct Fixture {
    pub root: PathBuf,
}
impl Fixture {
    pub fn new(label: &str) -> Self {
        let run = std::env::var("PW_RUN_ID").expect("PW_RUN_ID must isolate test runs");
        assert!(run.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        run.hash(&mut hash);
        std::thread::current().id().hash(&mut hash);
        let root = PathBuf::from(TMP).join(format!(
            "mz-{:x}-{:x}-{:x}-{label}",
            hash.finish(),
            std::process::id(),
            stamp
        ));
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        Self { root }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
