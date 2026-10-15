//! Trusted bounded worker primitives. Inference runs outside SQLite transactions.
use crate::{Error, Result, capture::id, catalog::Catalog, store::NamespaceStore};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use std::{
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
#[derive(Clone, Debug)]
pub struct Lease {
    job: String,
    operation: String,
    owner: String,
    namespace: String,
    state: std::path::PathBuf,
    pub source_id: String,
    pub source_revision: i64,
    pub text: String,
    pub visibility: String,
    pub session: String,
    pub principal: String,
    pub capability_revision: i64,
}
impl Lease {
    pub fn operation_key(&self) -> &str {
        &self.operation
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Publication {
    Published,
    AlreadyPublished,
    Obsolete,
    LostLease,
}
fn same_state(cat: &Catalog, store: &NamespaceStore) -> Result<()> {
    if cat.state.path != store.state_path {
        Err(Error::new("denied", "worker state mismatch"))
    } else {
        Ok(())
    }
}
fn eligible(cat: &Catalog, tx: &Transaction<'_>, job: &Lease, now: i64) -> Result<bool> {
    let allowed:bool=cat.conn.query_row("SELECT EXISTS(SELECT 1 FROM principals p JOIN grants g ON g.principal=p.id WHERE p.id=?1 AND p.revision=?2 AND g.namespace=?3 AND g.can_write=1 AND (?4='' OR EXISTS(SELECT 1 FROM sessions s WHERE s.id=?4 AND s.principal=p.id)))",params![job.principal,job.capability_revision,job.namespace,job.session],|r|r.get(0))?;
    let current:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sources WHERE id=?1 AND revision=?2 AND status='active' AND visibility=?3 AND session_id=?4 AND (expires_at IS NULL OR expires_at>?5) AND NOT EXISTS(SELECT 1 FROM sources deleted WHERE deleted.id=?1 AND deleted.status='deleted'))",params![job.source_id,job.source_revision,job.visibility,job.session,now],|r|r.get(0))?;
    Ok(allowed && current)
}
pub fn claim(
    cat: &Catalog,
    store: &mut NamespaceStore,
    now: i64,
    lease_seconds: i64,
    foreground_busy: bool,
) -> Result<Option<Lease>> {
    claim_filtered(
        cat,
        store,
        now,
        lease_seconds,
        foreground_busy,
        ClaimFilter {
            kind: "embedding",
            principal: None,
            session: None,
        },
    )
}
pub fn claim_relations(
    cat: &Catalog,
    store: &mut NamespaceStore,
    now: i64,
    lease_seconds: i64,
    foreground_busy: bool,
    cap: &crate::auth::Capability,
) -> Result<Option<Lease>> {
    claim_filtered(
        cat,
        store,
        now,
        lease_seconds,
        foreground_busy,
        ClaimFilter {
            kind: "relations",
            principal: Some(cap.principal()),
            session: Some(cap.session()),
        },
    )
}
struct ClaimFilter<'a> {
    kind: &'a str,
    principal: Option<&'a str>,
    session: Option<&'a str>,
}
fn claim_filtered(
    cat: &Catalog,
    store: &mut NamespaceStore,
    now: i64,
    lease_seconds: i64,
    foreground_busy: bool,
    filter: ClaimFilter<'_>,
) -> Result<Option<Lease>> {
    same_state(cat, store)?;
    if foreground_busy {
        return Ok(None);
    }
    if !(1..=300).contains(&lease_seconds) {
        return Err(Error::new(
            "invalid_limit",
            "lease must be 1 to 300 seconds",
        ));
    }
    let _guard = Transaction::new_unchecked(&cat.conn, TransactionBehavior::Immediate)?;
    let tx = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    for _ in 0..32 {
        let candidate:Option<Lease>=tx.query_row("SELECT j.id,j.operation_key,j.source_id,j.source_revision,s.text,s.visibility,s.session_id,j.principal,j.capability_revision FROM jobs j JOIN sources s ON s.id=j.source_id AND s.revision=j.source_revision WHERE ((j.state='pending' AND j.next_attempt<=?1) OR (j.state='leased' AND j.lease_expiry<=?1)) AND ((?2='embedding' AND j.job_type IN ('enrich','embedding')) OR j.job_type=?2) AND (?3 IS NULL OR j.principal=?3) AND (?4 IS NULL OR s.visibility='project' OR s.session_id=?4) ORDER BY (s.origin='user') DESC,j.next_attempt,j.id LIMIT 1",params![now,filter.kind,filter.principal,filter.session],|r|Ok(Lease{job:r.get(0)?,operation:r.get(1)?,owner:String::new(),namespace:store.namespace.clone(),state:store.state_path.clone(),source_id:r.get(2)?,source_revision:r.get(3)?,text:r.get(4)?,visibility:r.get(5)?,session:r.get(6)?,principal:r.get(7)?,capability_revision:r.get(8)?})).optional()?;
        let Some(mut candidate) = candidate else {
            tx.commit()?;
            return Ok(None);
        };
        if !eligible(cat, &tx, &candidate, now)? {
            tx.execute("UPDATE jobs SET state='obsolete',lease_owner=NULL,lease_expiry=NULL,error_code='obsolete_input' WHERE id=?1",[&candidate.job])?;
            continue;
        }
        let attempts: i64 = tx.query_row(
            "SELECT attempts FROM jobs WHERE id=?1",
            [&candidate.job],
            |r| r.get(0),
        )?;
        if attempts >= 3 {
            tx.execute("UPDATE jobs SET state='failed',lease_owner=NULL,lease_expiry=NULL,error_code='retry_exhausted' WHERE id=?1",[&candidate.job])?;
            continue;
        }
        candidate.owner = id()?;
        tx.execute("UPDATE jobs SET state='leased',attempts=attempts+1,lease_owner=?2,lease_expiry=?3,error_code=NULL WHERE id=?1",params![candidate.job,candidate.owner,now.saturating_add(lease_seconds)])?;
        tx.commit()?;
        return Ok(Some(candidate));
    }
    tx.commit()?;
    Ok(None)
}
pub fn publish<F>(
    cat: &Catalog,
    store: &mut NamespaceStore,
    lease: &Lease,
    now: i64,
    publisher: F,
) -> Result<Publication>
where
    F: FnOnce(&Transaction<'_>, &Lease) -> Result<()>,
{
    same_state(cat, store)?;
    if lease.namespace != store.namespace || lease.state != store.state_path {
        return Err(Error::new("denied", "lease state mismatch"));
    }
    let _guard = Transaction::new_unchecked(&cat.conn, TransactionBehavior::Immediate)?;
    let tx = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let state: Option<(String, Option<String>, Option<i64>)> = tx
        .query_row(
            "SELECT state,lease_owner,lease_expiry FROM jobs WHERE id=?1",
            [&lease.job],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((state, owner, expiry)) = state else {
        return Ok(Publication::Obsolete);
    };
    if state == "completed" && owner.as_deref() == Some(&lease.owner) {
        return Ok(Publication::AlreadyPublished);
    }
    if state == "obsolete" {
        return Ok(Publication::Obsolete);
    }
    if state != "leased"
        || owner.as_deref() != Some(&lease.owner)
        || expiry.is_none_or(|t| t <= now)
    {
        return Ok(Publication::LostLease);
    }
    if !eligible(cat, &tx, lease, now)? {
        tx.execute("UPDATE jobs SET state='obsolete',lease_owner=NULL,lease_expiry=NULL,error_code='obsolete_input' WHERE id=?1",[&lease.job])?;
        tx.commit()?;
        return Ok(Publication::Obsolete);
    }
    publisher(&tx, lease)?;
    tx.execute(
        "UPDATE jobs SET state='completed',lease_expiry=NULL,error_code=NULL WHERE id=?1",
        [&lease.job],
    )?;
    tx.commit()?;
    Ok(Publication::Published)
}
pub fn fail(store: &mut NamespaceStore, lease: &Lease, now: i64, code: &str) -> Result<()> {
    if lease.namespace != store.namespace || lease.state != store.state_path {
        return Err(Error::new("denied", "lease state mismatch"));
    }
    let code = match code {
        "provider_unavailable" | "invalid_embedding" | "timeout" => code,
        _ => "worker_failed",
    };
    store.conn.execute("UPDATE jobs SET state=CASE WHEN attempts>=3 THEN 'failed' ELSE 'pending' END,next_attempt=?3+(1<<attempts),error_code=?4,lease_owner=NULL,lease_expiry=NULL WHERE id=?1 AND state='leased' AND lease_owner=?2",params![lease.job,lease.owner,now,code])?;
    Ok(())
}
#[derive(Default)]
struct Usage {
    active: usize,
    bytes: usize,
}
#[derive(Clone)]
pub struct WorkerPool {
    usage: Arc<Mutex<Usage>>,
    max: usize,
    max_bytes: usize,
}
struct Permit {
    usage: Arc<Mutex<Usage>>,
    bytes: usize,
}
impl Drop for Permit {
    fn drop(&mut self) {
        let mut usage = self.usage.lock().unwrap();
        usage.active -= 1;
        usage.bytes -= self.bytes;
    }
}
pub struct Running<T> {
    receiver: mpsc::Receiver<T>,
}
impl<T> Running<T> {
    pub fn wait(self, timeout: Duration) -> Result<T> {
        self.receiver.recv_timeout(timeout).map_err(|_| {
            Error::new(
                "timeout",
                "worker unavailable within deadline; running work retains its permit",
            )
        })
    }
}
impl WorkerPool {
    pub fn new(max: usize, max_bytes: usize) -> Result<Self> {
        if max == 0 || max > 8 || max_bytes == 0 || max_bytes > 1073741824 {
            return Err(Error::new(
                "invalid_limit",
                "invalid worker resource limits",
            ));
        }
        Ok(Self {
            usage: Arc::new(Mutex::new(Usage::default())),
            max,
            max_bytes,
        })
    }
    pub fn active(&self) -> usize {
        self.usage.lock().unwrap().active
    }
    pub fn start<T, F>(&self, bytes: usize, work: F) -> Result<Running<T>>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let mut usage = self.usage.lock().unwrap();
        if usage.active >= self.max || bytes > self.max_bytes.saturating_sub(usage.bytes) {
            return Err(Error::new("busy", "worker resource capacity reached"));
        }
        usage.active += 1;
        usage.bytes += bytes;
        drop(usage);
        let permit = Permit {
            usage: self.usage.clone(),
            bytes,
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("memz-enrichment".into())
            .spawn(move || {
                let _permit = permit;
                let result = work();
                let _ = sender.send(result);
            })?;
        Ok(Running { receiver })
    }
}
