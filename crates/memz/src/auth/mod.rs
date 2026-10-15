//! Trusted embedding/admin API. Adapters must never expose grant or identity minting as model tools.
use crate::{Error, Result, catalog::Catalog};
use rusqlite::{OptionalExtension, params};
use std::path::Path;
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Visibility {
    Project,
    Session(String),
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Scope {
    pub namespace: String,
    pub visibility: Visibility,
}
impl Scope {
    pub fn project(namespace: &str) -> Self {
        Self {
            namespace: namespace.into(),
            visibility: Visibility::Project,
        }
    }
    pub fn session(namespace: &str, session: &str) -> Self {
        Self {
            namespace: namespace.into(),
            visibility: Visibility::Session(session.into()),
        }
    }
    pub fn derive(inputs: &[Self], output: &Self) -> Result<()> {
        if inputs.is_empty() || inputs.iter().any(|s| s != output) {
            return Err(denied());
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct Capability {
    principal: String,
    session: String,
    revision: i64,
    catalog: std::path::PathBuf,
}
impl Capability {
    pub fn principal(&self) -> &str {
        &self.principal
    }
    pub fn session(&self) -> &str {
        &self.session
    }
    pub fn revision(&self) -> i64 {
        self.revision
    }
}
fn denied() -> Error {
    Error::new("denied", "access is not authorized")
}
fn identifier(s: &str) -> Result<()> {
    if s.is_empty() || s.len() > 256 || s.chars().any(char::is_control) {
        Err(denied())
    } else {
        Ok(())
    }
}
impl Catalog {
    /// Trusted host registration, never a model-supplied identity claim.
    pub fn bind_session(&mut self, principal: &str, session: &str) -> Result<()> {
        identifier(principal)?;
        identifier(session)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO principals(id,revision) VALUES(?1,1)",
            [principal],
        )?;
        let owner: Option<String> = tx
            .query_row(
                "SELECT principal FROM sessions WHERE id=?1",
                [session],
                |r| r.get(0),
            )
            .optional()?;
        if owner.as_deref().is_some_and(|p| p != principal) {
            return Err(denied());
        }
        tx.execute(
            "INSERT OR IGNORE INTO sessions(id,principal) VALUES(?1,?2)",
            params![session, principal],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// Trusted explicit grant administration.
    pub fn grant(&mut self, principal: &str, namespace: &str, write: bool) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("INSERT INTO grants(principal,namespace,can_write) VALUES(?1,?2,?3) ON CONFLICT(principal,namespace) DO UPDATE SET can_write=excluded.can_write",params![principal,namespace,write])?;
        tx.execute(
            "UPDATE principals SET revision=revision+1 WHERE id=?1",
            [principal],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn revoke(&mut self, principal: &str, namespace: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM grants WHERE principal=?1 AND namespace=?2",
            params![principal, namespace],
        )?;
        tx.execute(
            "UPDATE principals SET revision=revision+1 WHERE id=?1",
            [principal],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// Mint only after a trusted transport has bound principal and session.
    pub fn capability(&self, principal: &str, session: &str) -> Result<Capability> {
        let revision=self.conn.query_row("SELECT p.revision FROM principals p JOIN sessions s ON s.principal=p.id WHERE p.id=?1 AND s.id=?2",params![principal,session],|r|r.get(0)).optional()?.ok_or_else(denied)?;
        Ok(Capability {
            principal: principal.into(),
            session: session.into(),
            revision,
            catalog: self.state.path.clone(),
        })
    }
    pub fn authorize(&self, cap: &Capability, scope: &Scope, write: bool) -> Result<()> {
        if cap.catalog != self.state.path
            || matches!(&scope.visibility,Visibility::Session(s) if s!=&cap.session)
        {
            return Err(denied());
        }
        let allowed:bool=self.conn.query_row("SELECT EXISTS(SELECT 1 FROM grants g JOIN principals p ON p.id=g.principal JOIN sessions s ON s.principal=p.id WHERE p.id=?1 AND p.revision=?2 AND s.id=?3 AND g.namespace=?4 AND (?5=0 OR g.can_write=1))",params![cap.principal,cap.revision,cap.session,scope.namespace,write],|r|r.get(0))?;
        if allowed { Ok(()) } else { Err(denied()) }
    }
    pub fn resolve_authorized(
        &self,
        cap: &Capability,
        hint: &Path,
        explicit: Option<&str>,
        write: bool,
    ) -> Result<String> {
        let namespace = self.resolve(hint, explicit).map_err(|_| denied())?;
        self.authorize(cap, &Scope::project(&namespace), write)?;
        Ok(namespace)
    }
}
