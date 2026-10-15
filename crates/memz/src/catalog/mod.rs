use crate::{
    Error, Result,
    state::{StateRoot, open_db},
};
use rusqlite::OptionalExtension;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};
pub struct Catalog {
    pub(crate) conn: rusqlite::Connection,
    pub state: StateRoot,
}
fn root_path(path: &Path) -> Result<PathBuf> {
    let root = fs::canonicalize(path)?;
    if !root.is_dir() {
        return Err(Error::new(
            "invalid_root",
            "project root must be a directory",
        ));
    }
    Ok(root)
}
impl Catalog {
    pub fn open(state: StateRoot) -> Result<Self> {
        let mut conn = open_db(&state.path.join("catalog.sqlite"))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 2 {
            return Err(Error::new(
                "unsupported_schema",
                "catalog schema is newer than this binary",
            ));
        }
        let tx = conn.transaction()?;
        if version == 0 {
            tx.execute_batch("CREATE TABLE namespaces (id TEXT PRIMARY KEY, root TEXT NOT NULL UNIQUE, created_at INTEGER NOT NULL);")?;
        }
        if version < 2 {
            tx.execute_batch("CREATE TABLE principals(id TEXT PRIMARY KEY, revision INTEGER NOT NULL); CREATE TABLE sessions(id TEXT PRIMARY KEY, principal TEXT NOT NULL REFERENCES principals(id)); CREATE TABLE grants(principal TEXT NOT NULL REFERENCES principals(id), namespace TEXT NOT NULL REFERENCES namespaces(id), can_write INTEGER NOT NULL CHECK(can_write IN (0,1)), PRIMARY KEY(principal,namespace)); PRAGMA user_version=2;")?;
        }
        tx.commit()?;
        let cat = Self { conn, state };
        let mut stmt = cat.conn.prepare("SELECT root FROM namespaces")?;
        let roots = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for root in roots {
            if cat.state.path.starts_with(root) {
                return Err(Error::new(
                    "unsafe_state",
                    "state is inside a registered project",
                ));
            }
        }
        drop(stmt);
        Ok(cat)
    }
    pub fn register(&mut self, path: &Path) -> Result<String> {
        let root = root_path(path)?;
        if self.state.path.starts_with(&root) || root.starts_with(&self.state.path) {
            return Err(Error::new(
                "invalid_root",
                "project and state directories must be disjoint",
            ));
        }
        let text = root
            .to_str()
            .ok_or_else(|| Error::new("invalid_root", "root must be UTF-8"))?;
        if let Some(id) = self
            .conn
            .query_row("SELECT id FROM namespaces WHERE root=?1", [text], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
        {
            return Ok(id);
        }
        let mut bytes = [0u8; 16];
        fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        let id = format!(
            "n_{}",
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        self.state.namespace_db(&id)?;
        self.conn.execute(
            "INSERT INTO namespaces(id,root,created_at) VALUES(?1,?2,unixepoch())",
            [&id, text],
        )?;
        Ok(id)
    }
    pub fn resolve(&self, hint: &Path, explicit: Option<&str>) -> Result<String> {
        if let Some(id) = explicit {
            return self
                .conn
                .query_row("SELECT id FROM namespaces WHERE id=?1", [id], |r| r.get(0))
                .optional()?
                .ok_or_else(|| Error::new("unknown_project", "select a registered project"));
        }
        let hint = fs::canonicalize(hint)?;
        let mut stmt = self.conn.prepare("SELECT id,root FROM namespaces")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut matches = Vec::new();
        for row in rows {
            let (id, root) = row?;
            if hint.starts_with(root) {
                matches.push(id)
            }
        }
        match matches.len() {
            1 => Ok(matches.remove(0)),
            0 => Err(Error::new("unknown_project", "select a registered project")),
            _ => Err(Error::new(
                "ambiguous_project",
                "overlapping roots require explicit selection",
            )),
        }
    }
    pub fn reassign(&mut self, id: &str, path: &Path) -> Result<()> {
        let root = root_path(path)?;
        if self.state.path.starts_with(&root) || root.starts_with(&self.state.path) {
            return Err(Error::new(
                "invalid_root",
                "project and state directories must be disjoint",
            ));
        }
        let root = root
            .to_str()
            .ok_or_else(|| Error::new("invalid_root", "root must be UTF-8"))?;
        if self
            .conn
            .execute("UPDATE namespaces SET root=?1 WHERE id=?2", [root, id])?
            != 1
        {
            return Err(Error::new("unknown_project", "namespace does not exist"));
        }
        Ok(())
    }
}
