use super::*;
use crate::retrieval::{Item, Packet};
use std::{
    collections::{HashSet, VecDeque},
    time::{Duration, Instant},
};
#[derive(Clone, Debug)]
pub struct GraphOptions {
    pub relations: Vec<String>,
    pub max_hops: usize,
    pub max_added_nodes: usize,
    pub max_edges: usize,
    pub timeout: Duration,
}
impl GraphOptions {
    pub fn new(relations: Vec<String>) -> Self {
        Self {
            relations,
            max_hops: 2,
            max_added_nodes: 64,
            max_edges: 512,
            timeout: Duration::from_millis(50),
        }
    }
    pub(crate) fn validate(&self) -> Result<()> {
        if self.relations.is_empty()
            || self.relations.len() > 6
            || self
                .relations
                .iter()
                .any(|s| !RELATIONS.contains(&s.as_str()))
            || !(1..=2).contains(&self.max_hops)
            || !(1..=64).contains(&self.max_added_nodes)
            || !(1..=512).contains(&self.max_edges)
            || self.timeout > Duration::from_millis(50)
        {
            return Err(Error::new(
                "invalid_graph",
                "graph options exceed supported relation/work bounds",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hop {
    pub edge: Edge,
    pub traversed_from: Node,
    pub traversed_to: Node,
    pub direction: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct GraphReport {
    pub added_nodes: usize,
    pub returned_nodes: usize,
    pub visited_nodes: usize,
    pub scanned_edges: usize,
    pub partial: bool,
    pub limits: Vec<String>,
}
impl GraphReport {
    fn limit(&mut self, reason: &str) {
        self.partial = true;
        if !self.limits.iter().any(|s| s == reason) {
            self.limits.push(reason.into());
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImpactEdge {
    pub id: String,
    pub revision: i64,
    pub from: Node,
    pub to: Node,
    pub relation: String,
    pub dependency: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Impact {
    pub target: Node,
    pub source: String,
    pub source_revision: i64,
    pub edges: Vec<ImpactEdge>,
    pub items: Vec<Node>,
    pub partial: bool,
    pub limits: Vec<String>,
}
impl Impact {
    pub(crate) fn limited(&mut self, reason: &str) {
        self.partial = true;
        if !self.limits.iter().any(|s| s == reason) {
            self.limits.push(reason.into());
        }
    }
}
fn load_item(tx: &Transaction<'_>, session: &str, namespace: &str, node: &Node) -> Result<Item> {
    item(tx, session, namespace, node)?;
    Ok(tx.query_row("SELECT i.content,i.kind,i.source_id,i.source_revision,s.author,s.origin,i.visibility,i.session_id,i.status,i.explicit,i.slot FROM items i JOIN sources s ON s.id=i.source_id AND s.revision=i.source_revision WHERE i.id=?1 AND i.revision=?2",params![node.id,node.revision],|r|{let content:String=r.get(0)?;let len=content.len();Ok(Item{id:node.id.clone(),revision:node.revision,content,kind:r.get(1)?,source:r.get(2)?,source_revision:r.get(3)?,author:r.get(4)?,origin:r.get(5)?,namespace:namespace.into(),visibility:r.get(6)?,session:r.get(7)?,status:r.get(8)?,explicit:r.get(9)?,slot:r.get(10)?,reason:"related memory; evidence establishes provenance, not truth".into(),span_start:0,span_end:len,truncated:false,cross_project:false,via:vec![]})})?)
}
/// Runs inside the same read snapshot and authorization lock as the base retrieval.
pub(crate) fn expand(
    tx: &Transaction<'_>,
    session: &str,
    packet: &mut Packet,
    options: &GraphOptions,
    max_items: usize,
) -> Result<()> {
    options.validate()?;
    let deadline = Instant::now() + options.timeout;
    let mut report = GraphReport::default();
    let mut visited: HashSet<Node> = packet
        .items
        .iter()
        .map(|i| Node {
            id: i.id.clone(),
            revision: i.revision,
        })
        .collect();
    let mut queue: VecDeque<(Node, Vec<Hop>)> = packet
        .items
        .iter()
        .map(|i| {
            (
                Node {
                    id: i.id.clone(),
                    revision: i.revision,
                },
                vec![],
            )
        })
        .collect();
    let mut edges = HashSet::new();
    let mut additions = Vec::new();
    tx.progress_handler(500, Some(move || Instant::now() >= deadline));
    let outcome = (|| -> Result<()> {
        while let Some((node, path)) = queue.pop_front() {
            if Instant::now() >= deadline {
                report.limit("deadline");
                break;
            }
            if path.len() >= options.max_hops {
                report.limit("hop_limit");
                continue;
            }
            let remaining = options.max_edges.saturating_sub(report.scanned_edges);
            if remaining == 0 {
                report.limit("edge_limit");
                break;
            }
            // Filter requested relation types and visibility before the bounded discovery limit.
            let mut statement=tx.prepare("SELECT id FROM edges WHERE status='active' AND (visibility='project' OR session_id=?3) AND relation IN (SELECT value FROM json_each(?4)) AND ((from_id=?1 AND from_revision=?2) OR (to_id=?1 AND to_revision=?2)) ORDER BY explicit DESC,id LIMIT ?5")?;
            let ids = statement
                .query_map(
                    params![
                        node.id,
                        node.revision,
                        session,
                        serde_json::to_string(&options.relations).unwrap(),
                        (remaining + 1) as i64
                    ],
                    |r| r.get::<_, String>(0),
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            if ids.len() > remaining {
                report.limit("edge_limit");
            }
            for id in ids.into_iter().take(remaining) {
                if Instant::now() >= deadline {
                    report.limit("deadline");
                    return Ok(());
                }
                report.scanned_edges += 1;
                if !edges.insert(id.clone()) {
                    continue;
                }
                let Some(edge) = read_edge(tx, session, &packet.namespace, &id)? else {
                    continue;
                };
                let (next, direction) = if edge.from == node {
                    (edge.to.clone(), "forward")
                } else {
                    (edge.from.clone(), "reverse")
                };
                if visited.contains(&next) {
                    continue;
                }
                if report.added_nodes >= options.max_added_nodes {
                    report.limit("node_limit");
                    return Ok(());
                }
                let mut candidate = load_item(tx, session, &packet.namespace, &next)?;
                let mut next_path = path.clone();
                next_path.push(Hop {
                    edge,
                    traversed_from: node.clone(),
                    traversed_to: next.clone(),
                    direction: direction.into(),
                });
                candidate.via = next_path.clone();
                visited.insert(next.clone());
                queue.push_back((next, next_path));
                additions.push(candidate);
                report.added_nodes += 1;
            }
        }
        Ok(())
    })();
    tx.progress_handler(0, None::<fn() -> bool>);
    if let Err(e) = outcome {
        if Instant::now() >= deadline {
            report.limit("deadline");
        } else {
            return Err(e);
        }
    }
    report.visited_nodes = visited.len();
    for candidate in additions {
        if packet.items.len() < max_items {
            packet.items.push(candidate);
        } else {
            packet.omitted += 1;
            report.limit("item_limit");
        }
    }
    report.returned_nodes = packet.items.iter().filter(|i| !i.via.is_empty()).count();
    packet.graph = Some(report);
    Ok(())
}

pub(crate) fn impact(
    tx: &Transaction<'_>,
    session: &str,
    namespace: &str,
    target: &Node,
) -> Result<Impact> {
    item(tx, session, namespace, target)?;
    let (source, source_revision): (String, i64) = tx.query_row(
        "SELECT source_id,source_revision FROM items WHERE id=?1 AND revision=?2",
        params![target.id, target.revision],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let mut output = Impact {
        target: target.clone(),
        source: source.clone(),
        source_revision,
        edges: vec![],
        items: vec![],
        partial: false,
        limits: vec![],
    };
    let deadline = Instant::now() + Duration::from_millis(50);
    tx.progress_handler(500, Some(move || Instant::now() >= deadline));
    let outcome = (|| -> Result<()> {
        let mut statement=tx.prepare("SELECT id FROM edges e WHERE e.status='active' AND (e.visibility='project' OR e.session_id=?3) AND ((e.from_id=?1 AND e.from_revision=?2) OR (e.to_id=?1 AND e.to_revision=?2) OR EXISTS(SELECT 1 FROM evidence v WHERE v.edge_id=e.id AND v.edge_revision=e.revision AND v.source_id=?4 AND v.source_revision=?5)) ORDER BY e.id LIMIT 513")?;
        let ids = statement
            .query_map(
                params![target.id, target.revision, session, source, source_revision],
                |r| r.get::<_, String>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if ids.len() > 512 {
            output.limited("scan_limit");
        }
        for id in ids.into_iter().take(512) {
            if Instant::now() >= deadline {
                output.limited("deadline");
                return Ok(());
            }
            let Some(edge) = read_edge(tx, session, namespace, &id)? else {
                continue;
            };
            if output.edges.len() >= 64 {
                output.limited("record_limit");
                break;
            }
            let dependency = if edge.from == *target || edge.to == *target {
                "endpoint_revision"
            } else {
                "supporting_source_revision"
            };
            output.edges.push(ImpactEdge {
                id: edge.id,
                revision: edge.revision,
                from: edge.from,
                to: edge.to,
                relation: edge.relation,
                dependency: dependency.into(),
            });
        }
        let mut statement=tx.prepare("SELECT DISTINCT i.id,i.revision FROM items i JOIN evidence v ON v.item_id=i.id AND v.item_revision=i.revision WHERE v.source_id=?1 AND v.source_revision=?2 AND i.status='active' AND (i.visibility='project' OR i.session_id=?3) AND i.id<>?4 ORDER BY i.id LIMIT 513")?;
        let nodes = statement
            .query_map(params![source, source_revision, session, target.id], |r| {
                Ok(Node {
                    id: r.get(0)?,
                    revision: r.get(1)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if nodes.len() > 512 {
            output.limited("scan_limit");
        }
        for node in nodes.into_iter().take(512) {
            if Instant::now() >= deadline {
                output.limited("deadline");
                return Ok(());
            }
            if item(tx, session, namespace, &node).is_err() {
                continue;
            }
            if output.edges.len() + output.items.len() >= 64 {
                output.limited("record_limit");
                break;
            }
            output.items.push(node);
        }
        Ok(())
    })();
    tx.progress_handler(0, None::<fn() -> bool>);
    if let Err(e) = outcome {
        if Instant::now() >= deadline {
            output.limited("deadline");
        } else {
            return Err(e);
        }
    }
    Ok(output)
}
