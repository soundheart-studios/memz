//! Trusted same-user administration. Never exposed as a model-callable operation.
use crate::{
    Error, Result,
    catalog::Catalog,
    state::{StateRoot, check_file, no_links, private_dir},
    store::NamespaceStore,
};
use rusqlite::{
    Connection, OpenFlags, TransactionBehavior,
    backup::{Backup, StepResult},
    types::{Value, ValueRef},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
const CATALOG: &[&str] = &["namespaces", "principals", "sessions", "grants"];
const NAMESPACE: &[&str] = &[
    "namespace_meta",
    "sources",
    "items",
    "edges",
    "evidence",
    "jobs",
    "operations",
    "migrations",
];
const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_ROWS: u64 = 2_000_000;
const MAX_LINE: u64 = 1024 * 1024;
#[derive(Clone, Copy, Debug)]
pub enum Format {
    Sqlite,
    Jsonl,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub name: String,
    pub schema: u32,
    pub sqlite_sha256: String,
    pub jsonl_sha256: String,
    pub sqlite_bytes: u64,
    pub jsonl_bytes: u64,
    pub rows: u64,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: String,
    pub version: u32,
    pub created_at: i64,
    pub derived_indexes: String,
    pub entries: Vec<Entry>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    table: String,
    values: Vec<Cell>,
}
#[derive(Serialize, Deserialize)]
enum Cell {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}
impl Cell {
    fn read(value: ValueRef<'_>) -> Result<Self> {
        Ok(match value {
            ValueRef::Null => Self::Null,
            ValueRef::Integer(n) => Self::Integer(n),
            ValueRef::Real(n) => Self::Real(n),
            ValueRef::Text(s) => Self::Text(
                std::str::from_utf8(s)
                    .map_err(|_| bad("non-UTF8 database text"))?
                    .into(),
            ),
            ValueRef::Blob(b) => Self::Blob(b.into()),
        })
    }
    fn value(self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Integer(n) => Value::Integer(n),
            Self::Real(n) => Value::Real(n),
            Self::Text(s) => Value::Text(s),
            Self::Blob(b) => Value::Blob(b),
        }
    }
}
fn bad(s: &str) -> Error {
    Error::new("invalid_backup", s)
}
fn namespace(s: &str) -> bool {
    s.len() == 34 && s.starts_with("n_") && s[2..].bytes().all(|b| b.is_ascii_hexdigit())
}
fn tables(name: &str) -> Result<&'static [&'static str]> {
    if name == "catalog" {
        Ok(CATALOG)
    } else if namespace(name) {
        Ok(NAMESPACE)
    } else {
        Err(bad("invalid manifest database name"))
    }
}
struct Work {
    start: Instant,
    bytes: u64,
    rows: u64,
}
impl Work {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            bytes: 0,
            rows: 0,
        }
    }
    fn check(&self) -> Result<()> {
        if self.start.elapsed() > Duration::from_secs(60)
            || self.bytes > MAX_BYTES
            || self.rows > MAX_ROWS
        {
            Err(Error::new(
                "recovery_limit",
                "recovery exceeds 60 seconds, 2 GiB or two million portable rows",
            ))
        } else {
            Ok(())
        }
    }
}
struct Stage {
    path: PathBuf,
}
impl Stage {
    fn new(destination: &Path) -> Result<Self> {
        no_links(destination)?;
        if fs::symlink_metadata(destination).is_ok() {
            return Err(bad(
                "destination must not exist; recovery never merges or overwrites",
            ));
        }
        let parent = destination
            .parent()
            .ok_or_else(|| bad("missing destination parent"))?;
        private_dir(parent)?;
        let path = parent.join(format!(".memz-recovery-{}", crate::capture::id()?));
        private_dir(&path)?;
        Ok(Self { path })
    }
    fn publish(&self, destination: &Path) -> Result<()> {
        sync_tree(&self.path)?;
        // Atomic no-replace publication: a competing process cannot substitute an existing destination.
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let from =
            CString::new(self.path.as_os_str().as_bytes()).map_err(|_| bad("invalid path"))?;
        let to =
            CString::new(destination.as_os_str().as_bytes()).map_err(|_| bad("invalid path"))?;
        #[cfg(target_os = "macos")]
        let rc = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
        #[cfg(target_os = "linux")]
        let rc = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        compile_error!(
            "atomic recovery publication needs a no-replace directory rename implementation"
        );
        if rc != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        File::open(destination.parent().unwrap())?.sync_all()?;
        Ok(())
    }
}
impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
fn sync_tree(path: &Path) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let p = entry?.path();
        if p.is_dir() {
            sync_tree(&p)?;
        } else {
            File::open(&p)?.sync_all()?;
        }
    }
    File::open(path)?.sync_all()?;
    Ok(())
}
fn output(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?)
}
fn hash(path: &Path, work: &mut Work) -> Result<(String, u64)> {
    check_file(path)?;
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    work.bytes = work.bytes.saturating_add(size);
    work.check()?;
    let mut digest = Sha256::new();
    let mut bytes = [0; 65536];
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        digest.update(&bytes[..n]);
        work.check()?;
    }
    Ok((format!("{:x}", digest.finalize()), size))
}
fn integrity(conn: &Connection) -> Result<()> {
    let value: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if value != "ok" {
        return Err(bad("SQLite integrity check failed"));
    }
    if conn
        .prepare("PRAGMA foreign_key_check")?
        .query([])?
        .next()?
        .is_some()
    {
        return Err(bad("foreign key check failed"));
    }
    Ok(())
}
fn readonly(path: &Path) -> Result<Connection> {
    check_file(path)?;
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA query_only=ON;")?;
    let deadline = Instant::now() + Duration::from_secs(60);
    conn.progress_handler(1000, Some(move || Instant::now() >= deadline));
    Ok(conn)
}
fn copy_database(source: &Connection, path: &Path, work: &mut Work) -> Result<()> {
    let pages: u64 = source.pragma_query_value(None, "page_count", |r| r.get(0))?;
    let page_size: u64 = source.pragma_query_value(None, "page_size", |r| r.get(0))?;
    if work.bytes.saturating_add(pages.saturating_mul(page_size)) > MAX_BYTES {
        return Err(Error::new(
            "recovery_limit",
            "SQLite snapshot exceeds disk limit",
        ));
    }
    work.bytes = work.bytes.saturating_add(pages.saturating_mul(page_size));
    output(path)?;
    let mut target = Connection::open(path)?;
    let deadline = work.start + Duration::from_secs(60);
    target.progress_handler(1000, Some(move || Instant::now() >= deadline));
    {
        let backup = Backup::new(source, &mut target)?;
        loop {
            work.check()?;
            match backup.step(128)? {
                StepResult::Done => break,
                StepResult::More => {}
                StepResult::Busy | StepResult::Locked => {
                    return Err(Error::new(
                        "busy",
                        "backup source is busy; retry the whole operation",
                    ));
                }
                _ => return Err(bad("unknown SQLite backup state")),
            }
        }
    }
    integrity(&target)?;
    drop(target);
    Ok(())
}
fn records(
    conn: &Connection,
    name: &str,
    mut emit: impl FnMut(Record) -> Result<()>,
) -> Result<u64> {
    let mut count = 0;
    for table in tables(name)? {
        // Names come exclusively from the fixed format schema, never from external SQL.
        let kind: String = conn.query_row(
            "SELECT type FROM sqlite_schema WHERE name=?1",
            [table],
            |r| r.get(0),
        )?;
        if kind != "table" {
            return Err(bad("recovery input requires ordinary tables"));
        }
        let mut stmt = conn.prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))?;
        let columns = stmt.column_count();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let mut values = Vec::with_capacity(columns);
            for i in 0..columns {
                values.push(Cell::read(row.get_ref(i)?)?);
            }
            emit(Record {
                table: (*table).into(),
                values,
            })?;
            count += 1;
        }
    }
    Ok(count)
}
/// Creates a complete administrative bundle. `namespace` selects one project or all projects.
pub fn create(state: &StateRoot, destination: &Path, namespace: Option<&str>) -> Result<Manifest> {
    create_with(state, destination, namespace, &mut |_| Ok(()))
}
/// Progress callbacks may cancel with an error; unpublished staging is removed on ordinary failure.
pub fn create_with(
    state: &StateRoot,
    destination: &Path,
    selected: Option<&str>,
    progress: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<Manifest> {
    let state = StateRoot::open(&state.path)?;
    let stage = Stage::new(destination)?;
    let mut work = Work::new();
    let cat = Catalog::open(state.clone())?;
    // All core writes authorize under this catalog lock. Each backup also uses SQLite's snapshot API.
    let guard = rusqlite::Transaction::new_unchecked(&cat.conn, TransactionBehavior::Immediate)?;
    let mut names = vec!["catalog".to_string()];
    let mut stmt = guard.prepare("SELECT id FROM namespaces ORDER BY id")?;
    let namespaces = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);
    if let Some(id) = selected
        && !namespaces.iter().any(|n| n == id)
    {
        return Err(bad("unknown namespace"));
    }
    names.extend(
        namespaces
            .into_iter()
            .filter(|n| selected.is_none_or(|s| s == n)),
    );
    if names.len() > 1025 {
        return Err(bad("at most 1024 namespaces per bundle"));
    }
    let catalog_path = stage.path.join("catalog.sqlite");
    let source = readonly(&state.path.join("catalog.sqlite"))?;
    copy_database(&source, &catalog_path, &mut work)?;
    drop(source);
    if let Some(id) = selected {
        let conn = Connection::open(&catalog_path)?;
        conn.execute("DELETE FROM grants WHERE namespace<>?1", [id])?;
        conn.execute("DELETE FROM namespaces WHERE id<>?1", [id])?;
    }
    progress("catalog_copied")?;
    for name in names.iter().skip(1) {
        let store = NamespaceStore::open(&state, name)?;
        copy_database(
            &store.conn,
            &stage.path.join(format!("{name}.sqlite")),
            &mut work,
        )?;
        progress("namespace_copied")?;
    }
    drop(guard); // Rendering and hashing do not hold up live writes.
    let mut manifest = Manifest {
        format: "memz-recovery".into(),
        version: 1,
        created_at: crate::capture::now(),
        derived_indexes: "rebuild-on-restore".into(),
        entries: vec![],
    };
    for name in names {
        let db = stage.path.join(format!("{name}.sqlite"));
        let conn = readonly(&db)?;
        integrity(&conn)?;
        let portable = stage.path.join(format!("{name}.jsonl"));
        let mut file = output(&portable)?;
        let rows = records(&conn, &name, |record| {
            work.rows += 1;
            work.check()?;
            let line = serde_json::to_vec(&record).map_err(|_| bad("record encoding failed"))?;
            if line.len() as u64 > MAX_LINE {
                return Err(bad("portable record too large"));
            }
            work.bytes = work.bytes.saturating_add(line.len() as u64 + 1);
            work.check()?;
            file.write_all(&line)?;
            file.write_all(b"\n")?;
            Ok(())
        })?;
        file.sync_all()?;
        let (sqlite_sha256, sqlite_bytes) = hash(&db, &mut work)?;
        let (jsonl_sha256, jsonl_bytes) = hash(&portable, &mut work)?;
        manifest.entries.push(Entry {
            name: name.clone(),
            schema: if name == "catalog" { 2 } else { 1 },
            sqlite_sha256,
            jsonl_sha256,
            sqlite_bytes,
            jsonl_bytes,
            rows,
        });
    }
    output(&stage.path.join("manifest.json"))?.write_all(
        &serde_json::to_vec_pretty(&manifest).map_err(|_| bad("manifest encoding failed"))?,
    )?;
    progress("validated")?;
    work.check()?;
    stage.publish(destination)?;
    Ok(manifest)
}
fn manifest(bundle: &Path) -> Result<Manifest> {
    no_links(bundle)?;
    private_dir(bundle)?;
    let path = bundle.join("manifest.json");
    check_file(&path)?;
    if fs::metadata(&path)?.len() > 1024 * 1024 {
        return Err(bad("manifest too large"));
    }
    let m: Manifest =
        serde_json::from_reader(File::open(path)?).map_err(|_| bad("invalid recovery manifest"))?;
    if m.format != "memz-recovery"
        || m.version != 1
        || m.derived_indexes != "rebuild-on-restore"
        || m.entries.is_empty()
        || m.entries.len() > 1025
        || m.entries[0].name != "catalog"
    {
        return Err(bad("unsupported recovery format"));
    }
    let mut seen = std::collections::HashSet::new();
    for e in &m.entries {
        tables(&e.name)?;
        if !seen.insert(&e.name)
            || e.schema != if e.name == "catalog" { 2 } else { 1 }
            || e.rows > MAX_ROWS
        {
            return Err(bad("invalid database manifest"));
        }
    }
    Ok(m)
}
fn insert(conn: &Connection, name: &str, record: Record) -> Result<()> {
    if !tables(name)?.contains(&record.table.as_str()) {
        return Err(bad("unknown portable table"));
    }
    let columns = conn
        .prepare(&format!("SELECT * FROM {} LIMIT 0", record.table))?
        .column_count();
    if record.values.len() != columns {
        return Err(bad("wrong portable column count"));
    }
    let placeholders = vec!["?"; columns].join(",");
    let values: Vec<_> = record.values.into_iter().map(Cell::value).collect();
    conn.execute(
        &format!("INSERT INTO {} VALUES({placeholders})", record.table),
        rusqlite::params_from_iter(values),
    )?;
    Ok(())
}
fn load(
    conn: &mut Connection,
    bundle: &Path,
    entry: &Entry,
    format: Format,
    work: &mut Work,
) -> Result<()> {
    let (suffix, expected, size) = match format {
        Format::Sqlite => ("sqlite", &entry.sqlite_sha256, entry.sqlite_bytes),
        Format::Jsonl => ("jsonl", &entry.jsonl_sha256, entry.jsonl_bytes),
    };
    let path = bundle.join(format!("{}.{suffix}", entry.name));
    let (digest, len) = hash(&path, work)?;
    if digest != *expected || len != size {
        return Err(bad("backup checksum or size mismatch"));
    }
    let deadline = work.start + Duration::from_secs(60);
    conn.progress_handler(1000, Some(move || Instant::now() >= deadline));
    conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
    let tx = conn.transaction()?;
    for table in tables(&entry.name)? {
        tx.execute(&format!("DELETE FROM {table}"), [])?;
    }
    let mut count = 0;
    let mut emit = |r: Record| {
        work.rows += 1;
        count += 1;
        work.check()?;
        insert(&tx, &entry.name, r)
    };
    match format {
        Format::Sqlite => {
            let source = readonly(&path)?;
            integrity(&source)?;
            let version: u32 = source.pragma_query_value(None, "user_version", |r| r.get(0))?;
            if version != entry.schema {
                return Err(bad("snapshot schema mismatch"));
            }
            records(&source, &entry.name, &mut emit)?;
        }
        Format::Jsonl => {
            let mut reader = BufReader::new(File::open(&path)?);
            loop {
                let mut line = Vec::new();
                let n = reader
                    .by_ref()
                    .take(MAX_LINE + 2)
                    .read_until(b'\n', &mut line)?;
                if n == 0 {
                    break;
                }
                if n as u64 > MAX_LINE + 1 || !line.ends_with(b"\n") {
                    return Err(bad("oversized or incomplete portable record"));
                }
                let r: Record =
                    serde_json::from_slice(&line).map_err(|_| bad("invalid portable record"))?;
                emit(r)?;
            }
        }
    }
    if count != entry.rows {
        return Err(bad("portable row count mismatch"));
    }
    integrity(&tx)?;
    tx.commit()?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    Ok(())
}
/// Restore only into an absent state root. Never merges a historical backup into current state.
pub fn restore(bundle: &Path, destination: &Path, format: Format) -> Result<Manifest> {
    restore_with(bundle, destination, format, &mut |_| Ok(()))
}
pub fn restore_with(
    bundle: &Path,
    destination: &Path,
    format: Format,
    progress: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<Manifest> {
    let manifest = manifest(bundle)?;
    let stage = Stage::new(destination)?;
    // Validate both final location and staging against the live-state path rules.
    if destination.ancestors().any(|p| p.join(".git").exists()) {
        return Err(bad("state cannot be restored inside a Git checkout"));
    }
    let state = StateRoot::open(&stage.path)?;
    let mut work = Work::new();
    let mut cat = Catalog::open(state.clone())?;
    load(
        &mut cat.conn,
        bundle,
        &manifest.entries[0],
        format,
        &mut work,
    )?;
    let mut stmt = cat
        .conn
        .prepare("SELECT id,root FROM namespaces ORDER BY id")?;
    let namespaces = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);
    if namespaces.len() + 1 != manifest.entries.len() {
        return Err(bad("catalog and manifest namespaces disagree"));
    }
    for (id, root) in &namespaces {
        if !Path::new(root).is_absolute()
            || destination.starts_with(root)
            || Path::new(root).starts_with(destination)
        {
            return Err(bad("restored state and project roots must be disjoint"));
        }
        let entry = manifest
            .entries
            .iter()
            .find(|e| &e.name == id)
            .ok_or_else(|| bad("missing namespace snapshot"))?;
        let mut store = NamespaceStore::open(&state, id)?;
        load(&mut store.conn, bundle, entry, format, &mut work)?;
        crate::store::migrate(&mut store.conn)?;
        // Portable authoritative data is independent of provider identity/extension ABI.
        // FTS is rebuilt; vector generations are absent and configured providers backfill them.
        store.conn.execute_batch("DELETE FROM memory_fts; INSERT INTO memory_fts(item_id,revision,content) SELECT id,revision,content FROM items WHERE status='active'; UPDATE jobs SET state='pending',lease_owner=NULL,lease_expiry=NULL WHERE state='leased'; PRAGMA wal_checkpoint(TRUNCATE);")?;
        integrity(&store.conn)?;
        progress("namespace_restored")?;
    }
    cat.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    drop(cat);
    // Reopen using ordinary startup validation before publication.
    drop(Catalog::open(state)?);
    progress("validated")?;
    work.check()?;
    stage.publish(destination)?;
    Ok(manifest)
}
struct Stopped(File);
impl Stopped {
    fn acquire(state: &StateRoot) -> Result<Self> {
        let path = state.path.join("run/daemon.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        check_file(&path)?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(Error::new(
                "already_running",
                "stop the daemon before relocation",
            ));
        }
        Ok(Self(file))
    }
}
impl Drop for Stopped {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}
/// Validated stopped-daemon copy; original stays intact for explicit rollback.
pub fn relocate(state: &StateRoot, destination: &Path) -> Result<Manifest> {
    let state = StateRoot::open(&state.path)?;
    let _stopped = Stopped::acquire(&state)?;
    if destination.starts_with(&state.path) || state.path.starts_with(destination) {
        return Err(bad("relocation paths must be disjoint"));
    }
    let scratch = state
        .path
        .join("backups")
        .join(format!("relocation-{}", crate::capture::id()?));
    let result = (|| {
        create(&state, &scratch, None)?;
        restore(&scratch, destination, Format::Sqlite)
    })();
    let _ = fs::remove_dir_all(&scratch);
    result
}
