use crate::{
    Error, Result,
    auth::{Capability, Scope},
    capture::{digest, now},
    catalog::Catalog,
    retrieval::{self, Packet, Query},
    store::NamespaceStore,
};
use std::collections::VecDeque;
struct Entry {
    key: String,
    packet: Packet,
    bytes: usize,
    expires: i64,
}
pub struct Cache {
    entries: VecDeque<Entry>,
    max_entries: usize,
    max_bytes: usize,
    bytes: usize,
    hits: u64,
}
impl Cache {
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            max_entries: max_entries.min(128),
            max_bytes: max_bytes.min(1048576),
            bytes: 0,
            hits: 0,
        }
    }
    pub fn hits(&self) -> u64 {
        self.hits
    }
    pub fn entries(&self) -> usize {
        self.entries.len()
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn recall(
        &mut self,
        cat: &Catalog,
        cap: &Capability,
        store: &mut NamespaceStore,
        query: &Query,
    ) -> Result<Packet> {
        if query.graph.is_some() || query.impact {
            return retrieval::recall(cat, cap, store, query);
        }
        let lock = rusqlite::Transaction::new_unchecked(
            &cat.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        if let Err(error) = cat.authorize(cap, &Scope::project(&store.namespace), false) {
            self.entries.clear();
            self.bytes = 0;
            return Err(error);
        }
        if cat.state.path != store.state_path {
            return Err(Error::new("denied", "access is not authorized"));
        }
        let tx = store.conn.transaction()?;
        let sequence: i64 =
            tx.query_row("SELECT mutation_seq FROM namespace_meta", [], |r| r.get(0))?;
        let key = digest(
            serde_json::json!([
                store.state_path,
                store.namespace,
                cap.principal(),
                cap.session(),
                cap.revision(),
                sequence,
                query.text,
                query.max_bytes,
                query.max_tokens,
                query.max_items,
                query.min_revision
            ])
            .to_string()
            .as_bytes(),
        );
        if let Some(index) = self
            .entries
            .iter()
            .position(|e| e.key == key && e.expires > now())
        {
            let entry = self.entries.remove(index).unwrap();
            let packet = entry.packet.clone();
            self.entries.push_back(entry);
            self.hits += 1;
            return Ok(packet);
        }
        let expires:i64=tx.query_row("SELECT coalesce(min(expires_at),9223372036854775807) FROM sources WHERE status='active' AND expires_at>unixepoch() AND (visibility='project' OR session_id=?1)",[cap.session()],|r|r.get(0))?;
        drop(tx);
        drop(lock);
        let packet = retrieval::recall(cat, cap, store, query)?;
        let bytes = serde_json::to_vec(&packet)
            .map_err(|_| Error::new("serialization", "cache encoding failed"))?
            .len()
            + key.len();
        if bytes <= self.max_bytes && self.max_entries > 0 && packet.mutation_seq == sequence {
            while self.entries.len() >= self.max_entries || self.bytes + bytes > self.max_bytes {
                if let Some(old) = self.entries.pop_front() {
                    self.bytes -= old.bytes;
                } else {
                    break;
                }
            }
            self.bytes += bytes;
            self.entries.push_back(Entry {
                key,
                packet: packet.clone(),
                bytes,
                expires,
            });
        }
        Ok(packet)
    }
}
