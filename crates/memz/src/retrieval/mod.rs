use crate::{
    Error, Result,
    auth::{Capability, Scope},
    capture::now,
    catalog::Catalog,
    store::NamespaceStore,
};
use rusqlite::params;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug)]
pub struct Query {
    pub text: String,
    pub max_bytes: usize,
    pub max_tokens: usize,
    pub max_items: usize,
    pub min_revision: Option<i64>,
    pub graph: Option<crate::graph::GraphOptions>,
    pub impact: bool,
}
impl Query {
    pub fn new(text: &str) -> Self {
        Self {
            text: text.into(),
            max_bytes: 8000,
            max_tokens: 2000,
            max_items: 32,
            min_revision: None,
            graph: None,
            impact: false,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub revision: i64,
    pub content: String,
    pub kind: String,
    pub source: String,
    pub source_revision: i64,
    pub author: String,
    pub origin: String,
    pub namespace: String,
    pub visibility: String,
    pub session: String,
    pub status: String,
    pub explicit: bool,
    pub slot: Option<String>,
    pub reason: String,
    pub span_start: usize,
    pub span_end: usize,
    pub truncated: bool,
    pub cross_project: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub via: Vec<crate::graph::Hop>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Packet {
    pub status: String,
    pub namespace: String,
    pub mutation_seq: i64,
    pub history: bool,
    pub items: Vec<Item>,
    pub omitted: usize,
    pub candidate_limit_hit: bool,
    pub counting: String,
    pub degraded: Vec<String>,
    pub follow_up: String,
    pub conflicts: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<crate::graph::GraphReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact: Option<crate::graph::Impact>,
}
impl Packet {
    pub(crate) fn trim_one(&mut self) -> bool {
        if let Some(impact) = &mut self.impact
            && (impact.edges.pop().is_some() || impact.items.pop().is_some())
        {
            impact.limited("packet_budget");
            self.status = "partial".into();
            return true;
        }
        let Some(item) = self.items.pop() else {
            return false;
        };
        self.omitted += 1;
        self.status = "partial".into();
        if let Some(graph) = &mut self.graph {
            graph.returned_nodes = self.items.iter().filter(|i| !i.via.is_empty()).count();
            if !item.via.is_empty() {
                graph.partial = true;
                if !graph.limits.iter().any(|s| s == "packet_budget") {
                    graph.limits.push("packet_budget".into());
                }
            }
        }
        true
    }
}
pub fn recall(
    cat: &Catalog,
    cap: &Capability,
    store: &mut NamespaceStore,
    query: &Query,
) -> Result<Packet> {
    select(cat, cap, store, query, None, false)
}
pub(crate) fn select(
    cat: &Catalog,
    cap: &Capability,
    store: &mut NamespaceStore,
    q: &Query,
    item: Option<&str>,
    history: bool,
) -> Result<Packet> {
    select_ranked(cat, cap, store, q, item, history, None)
}
struct Semantic<'a> {
    identity: &'a crate::providers::Identity,
    vector: &'a [f32],
    deadline: std::time::Instant,
}
fn select_ranked(
    cat: &Catalog,
    cap: &Capability,
    store: &mut NamespaceStore,
    q: &Query,
    item: Option<&str>,
    history: bool,
    semantic: Option<Semantic<'_>>,
) -> Result<Packet> {
    let _auth_lock =
        rusqlite::Transaction::new_unchecked(&cat.conn, rusqlite::TransactionBehavior::Immediate)?;
    cat.authorize(cap, &Scope::project(&store.namespace), false)?;
    if cat.state.path != store.state_path {
        return Err(Error::new("denied", "access is not authorized"));
    }
    if q.text.len() > 2048
        || q.max_tokens > 8000
        || q.max_tokens < 256
        || q.max_bytes < 256
        || q.max_bytes > 32768
        || q.max_items == 0
        || q.max_items > 128
    {
        return Err(Error::new(
            "invalid_budget",
            "query or budget is outside supported bounds",
        ));
    }
    if let Some(options) = &q.graph {
        options.validate()?;
    }
    if (history && (q.graph.is_some() || q.impact)) || (q.impact && item.is_none()) {
        return Err(Error::new(
            "invalid_graph",
            "graph and impact views require current-state inspect; recall does not accept impact",
        ));
    }
    let budget = q.max_bytes.min(q.max_tokens); // One UTF-8 byte per token is a conservative bound.
    let terms: Vec<_> = q.text.split_whitespace().take(33).collect();
    if terms.len() > 32 {
        return Err(Error::new("invalid_query", "at most 32 search terms"));
    }
    let expression = terms
        .iter()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ");
    let tx = store.conn.transaction()?;
    let revision: i64 = tx
        .query_row("SELECT mutation_seq FROM namespace_meta", [], |r| r.get(0))
        .map_err(unavailable)?;
    if q.min_revision.is_some_and(|minimum| revision < minimum) {
        return Err(Error::new(
            "revision_unavailable",
            "requested snapshot has not committed",
        ));
    }
    let mut packet = Packet {
        status: "empty".into(),
        namespace: store.namespace.clone(),
        mutation_seq: revision,
        history,
        items: vec![],
        omitted: 0,
        candidate_limit_hit: false,
        counting: "serialized_utf8_bytes_upper_bound".into(),
        degraded: vec!["semantic_unavailable".into()],
        follow_up: "inspect by ID for full authorized detail".into(),
        conflicts: false,
        graph: None,
        impact: None,
    };
    if item.is_none() && expression.is_empty() && semantic.is_none() {
        return fit(packet, budget);
    }
    let filter = if item.is_some() {
        "i.id=?4"
    } else {
        "(i.id,i.revision) IN (SELECT item_id,revision FROM memory_fts WHERE memory_fts MATCH ?4)"
    };
    let sql = format!(
        "SELECT i.id,i.revision,i.content,i.kind,i.source_id,i.source_revision,s.author,s.origin,i.visibility,i.session_id,i.status,i.explicit,i.slot FROM items i JOIN sources s ON s.id=i.source_id AND s.revision=i.source_revision WHERE {filter} AND (i.visibility='project' OR (i.visibility='session' AND i.session_id=?1)) AND s.visibility=i.visibility AND s.session_id=i.session_id AND i.status<>'deleted' AND s.status<>'deleted' AND NOT EXISTS(SELECT 1 FROM sources deleted WHERE deleted.id=s.id AND deleted.status='deleted') AND (?2 OR (i.status='active' AND s.status='active' AND (i.valid_to IS NULL OR i.valid_to>?3))) AND (s.expires_at IS NULL OR s.expires_at>?3) AND NOT EXISTS(SELECT 1 FROM evidence e JOIN sources es ON es.id=e.source_id AND es.revision=e.source_revision WHERE e.item_id=i.id AND e.item_revision=i.revision AND (EXISTS(SELECT 1 FROM sources deleted WHERE deleted.id=es.id AND deleted.status='deleted') OR es.status='deleted' OR (NOT ?2 AND es.status<>'active') OR es.visibility<>i.visibility OR es.session_id<>i.session_id OR (es.expires_at IS NOT NULL AND es.expires_at<=?3))) AND (?2 OR i.slot IS NULL OR i.explicit=0 OR NOT EXISTS(SELECT 1 FROM items newer JOIN sources ns ON ns.id=newer.source_id AND ns.revision=newer.source_revision WHERE newer.slot=i.slot AND newer.explicit=1 AND newer.status='active' AND ns.status='active' AND newer.visibility=i.visibility AND newer.session_id=i.session_id AND newer.rowid>i.rowid AND (ns.expires_at IS NULL OR ns.expires_at>?3))) ORDER BY i.explicit DESC,i.valid_from DESC,i.id,i.revision DESC LIMIT 129"
    );
    let map_row = |r: &rusqlite::Row<'_>| {
        let content: String = r.get(2)?;
        let length = content.len();
        Ok(Item {
            id: r.get(0)?,
            revision: r.get(1)?,
            content,
            kind: r.get(3)?,
            source: r.get(4)?,
            source_revision: r.get(5)?,
            author: r.get(6)?,
            origin: r.get(7)?,
            namespace: store.namespace.clone(),
            visibility: r.get(8)?,
            session: r.get(9)?,
            status: r.get(10)?,
            explicit: r.get(11)?,
            slot: r.get(12)?,
            reason: if history {
                "authorized history"
            } else {
                "current lexical match"
            }
            .into(),
            span_start: 0,
            span_end: length,
            truncated: false,
            cross_project: false,
            via: vec![],
        })
    };
    let mut stmt = tx.prepare(&sql).map_err(unavailable)?;
    let mut candidates = stmt
        .query_map(
            params![cap.session(), history, now(), item.unwrap_or(&expression)],
            map_row,
        )
        .map_err(unavailable)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(unavailable)?;
    let lexical_ids: Vec<String> = candidates.iter().map(|i| i.id.clone()).collect();
    if let Some(semantic) = semantic {
        let generation = semantic.identity.generation()?;
        let blob = crate::vector::bytes(semantic.vector)?;
        let deadline = semantic.deadline;
        let discovery = (|| -> Result<(Vec<String>, bool)> {
            if std::time::Instant::now() >= deadline {
                return Err(Error::new("semantic_deadline", "semantic scan deadline"));
            }
            let stored: String = tx.query_row(
                "SELECT identity FROM embedding_generations WHERE id=?1",
                [&generation],
                |r| r.get(0),
            )?;
            if serde_json::from_str::<crate::providers::Identity>(&stored)
                .ok()
                .as_ref()
                != Some(semantic.identity)
            {
                return Err(Error::new("unknown_identity", "generation mismatch"));
            }
            let base = sql.split(" ORDER BY i.explicit").next().unwrap();
            let from = base.split_once(" FROM items i ").unwrap().1;
            let from=from.replace(&format!("WHERE {filter}"),"JOIN memory_vectors v ON v.source_id=s.id AND v.source_revision=s.revision WHERE v.generation=?5");
            let scan = format!(
                "WITH scoped AS MATERIALIZED (SELECT i.id,i.revision,v.embedding FROM items i {from} ORDER BY i.id LIMIT 4097), bounded AS MATERIALIZED (SELECT * FROM scoped LIMIT 4096) SELECT id,vec_distance_cosine(embedding,?6) AS distance,(SELECT count(*) FROM scoped) FROM bounded ORDER BY distance,id,revision LIMIT 128"
            );
            tx.progress_handler(1000, Some(move || std::time::Instant::now() >= deadline));
            let result = (|| -> Result<(Vec<String>, bool)> {
                let mut statement = tx.prepare(&scan)?;
                let rows = statement
                    .query_map(
                        params![cap.session(), false, now(), expression, generation, blob],
                        |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, f64>(1)?,
                                r.get::<_, usize>(2)?,
                            ))
                        },
                    )?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                if rows.iter().any(|(_, d, _)| !d.is_finite()) {
                    return Err(Error::new(
                        "invalid_embedding",
                        "invalid stored vector distance",
                    ));
                }
                let limited = rows.first().is_some_and(|r| r.2 > 4096);
                Ok((rows.into_iter().map(|r| r.0).collect(), limited))
            })();
            tx.progress_handler(0, None::<fn() -> bool>);
            result
        })();
        match discovery {
            Ok((ids, limited)) => {
                let sem_sql = sql.replace(filter, "i.id IN (SELECT value FROM json_each(?4))");
                let mut statement = tx.prepare(&sem_sql).map_err(unavailable)?;
                let extra = statement
                    .query_map(
                        params![
                            cap.session(),
                            false,
                            now(),
                            serde_json::to_string(&ids).unwrap()
                        ],
                        map_row,
                    )
                    .map_err(unavailable)?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(unavailable)?;
                for mut candidate in extra {
                    if !candidates.iter().any(|i| i.id == candidate.id) {
                        candidate.reason = "current semantic match".into();
                        candidates.push(candidate);
                    }
                }
                let score = |id: &str| -> u64 {
                    [&lexical_ids, &ids]
                        .iter()
                        .filter_map(|list| list.iter().position(|s| s == id))
                        .map(|rank| 1_000_000 / (60 + rank as u64 + 1))
                        .sum()
                };
                candidates.sort_by(|a, b| {
                    b.explicit
                        .cmp(&a.explicit)
                        .then_with(|| score(&b.id).cmp(&score(&a.id)))
                        .then_with(|| a.id.cmp(&b.id))
                        .then_with(|| b.revision.cmp(&a.revision))
                });
                packet.degraded.clear();
                if limited {
                    packet.degraded.push("semantic_scan_limit".into());
                    packet.candidate_limit_hit = true;
                }
            }
            Err(e) => {
                packet.degraded = vec![
                    if std::time::Instant::now() >= deadline {
                        "semantic_deadline"
                    } else {
                        e.code
                    }
                    .into(),
                ];
            }
        }
    }
    packet.candidate_limit_hit |= candidates.len() > 128;
    if candidates.len() > 128 {
        packet.omitted += candidates.len() - 128;
        candidates.truncate(128);
    }
    let mut seen = std::collections::HashMap::new();
    for candidate in &candidates {
        if let Some(slot) = &candidate.slot {
            let count = seen
                .entry((slot, &candidate.visibility, &candidate.session))
                .or_insert(0);
            *count += 1;
            if *count > 1 {
                packet.conflicts = true;
            }
        }
    }
    for candidate in candidates {
        if packet.items.len() < q.max_items {
            packet.items.push(candidate);
        } else {
            packet.omitted += 1;
        }
    }
    if let Some(options) = &q.graph {
        crate::graph::expand(&tx, cap.session(), &mut packet, options, q.max_items)?;
    }
    if q.impact
        && let Some(target) = packet.items.first()
    {
        packet.impact = Some(crate::graph::impact(
            &tx,
            cap.session(),
            &store.namespace,
            &crate::graph::Node {
                id: target.id.clone(),
                revision: target.revision,
            },
        )?);
    }
    fit(packet, budget)
}
fn unavailable(_: rusqlite::Error) -> Error {
    Error::new("unavailable", "memory storage is unavailable")
}
pub(crate) fn fit(mut packet: Packet, budget: usize) -> Result<Packet> {
    loop {
        if let Some(graph) = &mut packet.graph {
            graph.returned_nodes = packet.items.iter().filter(|i| !i.via.is_empty()).count();
        }
        packet.status = if packet.omitted > 0
            || packet.graph.as_ref().is_some_and(|g| g.partial)
            || packet.impact.as_ref().is_some_and(|i| i.partial)
        {
            "partial"
        } else if packet.items.is_empty() {
            "empty"
        } else {
            "ok"
        }
        .into();
        let bytes = serde_json::to_vec(&packet)
            .map_err(|_| Error::new("serialization", "packet encoding failed"))?
            .len();
        if bytes <= budget {
            return Ok(packet);
        }
        if !packet.trim_one() {
            return Err(Error::new(
                "budget_too_small",
                "budget cannot hold packet metadata",
            ));
        }
    }
}

/// Trusted embedding input; authorization and current-state checks still occur in one snapshot.
pub fn hybrid_with_vector(
    cat: &Catalog,
    cap: &Capability,
    store: &mut NamespaceStore,
    q: &Query,
    identity: &crate::providers::Identity,
    vector: &[f32],
    deadline: std::time::Instant,
) -> Result<Packet> {
    select_ranked(
        cat,
        cap,
        store,
        q,
        None,
        false,
        Some(Semantic {
            identity,
            vector,
            deadline,
        }),
    )
}
pub fn hybrid(
    cat: &Catalog,
    cap: &Capability,
    store: &mut NamespaceStore,
    q: &Query,
    engine: Option<&crate::providers::Engine>,
) -> Result<Packet> {
    // Validate authorization before sending query text to the trusted local model.
    cat.authorize(cap, &Scope::project(&store.namespace), false)?;
    if cat.state.path != store.state_path {
        return Err(Error::new("denied", "state mismatch"));
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(350);
    let attempt = (|| -> Result<Packet> {
        let engine = engine
            .ok_or_else(|| Error::new("semantic_unavailable", "no local provider configured"))?;
        let identity = engine.identity().ok_or_else(|| {
            Error::new("semantic_unavailable", "local provider cold or unavailable")
        })?;
        let vector = engine.query(
            q.text.clone(),
            deadline.saturating_duration_since(std::time::Instant::now()),
        )?;
        crate::vector::attach(store)?;
        hybrid_with_vector(cat, cap, store, q, &identity, &vector, deadline)
    })();
    match attempt {
        Ok(packet) => Ok(packet),
        Err(e) => {
            let mut packet = recall(cat, cap, store, q)?;
            packet.degraded = vec![e.code.into()];
            fit(packet, q.max_bytes.min(q.max_tokens))
        }
    }
}
