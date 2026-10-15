use crate::{Error, Result};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
};
#[derive(Debug, Clone)]
pub struct StateRoot {
    pub path: PathBuf,
}
fn invalid(message: &str) -> Error {
    Error::new("unsafe_state", message)
}
fn uid() -> u32 {
    // SAFETY: geteuid has no arguments or memory preconditions.
    unsafe { libc::geteuid() }
}
pub(crate) fn no_links(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(invalid("state path must be absolute"));
    }
    let mut prefix = PathBuf::new();
    for part in path.components() {
        if matches!(part, Component::ParentDir | Component::CurDir) {
            return Err(invalid("state path cannot contain relative components"));
        }
        prefix.push(part);
        match fs::symlink_metadata(&prefix) {
            Ok(m) if m.file_type().is_symlink() => return Err(invalid("symlink in state path")),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
pub(crate) fn private_dir(path: &Path) -> Result<()> {
    no_links(path)?;
    if !path.exists() {
        if let Some(parent) = path.parent()
            && !parent.exists()
        {
            private_dir(parent)?
        }
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir()
        || meta.file_type().is_symlink()
        || meta.uid() != uid()
        || meta.mode() & 0o077 != 0
    {
        return Err(invalid(
            "state directory must be owned by current user and private",
        ));
    }
    Ok(())
}
pub(crate) fn check_file(path: &Path) -> Result<()> {
    no_links(path)?;
    let m = fs::symlink_metadata(path)?;
    if !m.is_file() || m.uid() != uid() || m.mode() & 0o077 != 0 || m.nlink() != 1 {
        return Err(invalid(
            "database file must be private, owned and not hard-linked",
        ));
    }
    Ok(())
}
pub(crate) fn open_db(path: &Path) -> Result<rusqlite::Connection> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("missing database parent"))?;
    private_dir(parent)?;
    no_links(path)?;
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    check_file(path)?;
    for suffix in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{suffix}", path.display()));
        if fs::symlink_metadata(&side).is_ok() {
            check_file(&side)?;
        }
    }
    let conn = rusqlite::Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_millis(1000))?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
    )?;
    Ok(conn)
}
impl StateRoot {
    pub fn path_for(home: &Path, alternate: Option<&Path>) -> PathBuf {
        alternate
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.join(".memz"))
    }
    pub fn from_env() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| invalid("HOME is not set"))?;
        let alternate = std::env::var_os("MEMZ_HOME").map(PathBuf::from);
        Self::open(&Self::path_for(&home, alternate.as_deref()))
    }
    pub fn open(path: &Path) -> Result<Self> {
        no_links(path)?;
        if path.ancestors().any(|p| p.join(".git").exists()) {
            return Err(invalid("live state cannot reside inside a Git checkout"));
        }
        private_dir(path)?;
        for name in ["namespaces", "run", "cache", "exports", "backups"] {
            private_dir(&path.join(name))?;
        }
        Ok(Self {
            path: fs::canonicalize(path)?,
        })
    }
    pub fn namespace_db(&self, id: &str) -> Result<PathBuf> {
        if id.len() != 34
            || !id.starts_with("n_")
            || !id[2..].bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err(invalid("invalid opaque namespace ID"));
        }
        private_dir(&self.path)?;
        let directory = self.path.join("namespaces").join(id);
        private_dir(&directory)?;
        let db = directory.join("memory.sqlite");
        let _ = open_db(&db)?;
        Ok(db)
    }
}
