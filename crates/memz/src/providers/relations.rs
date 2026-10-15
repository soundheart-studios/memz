//! Optional local literal extraction. No network or generative model claims.
use crate::{
    Error, Result,
    graph::{Anchor, Node, RELATIONS, Relation},
};
use serde::Serialize;
#[derive(Clone, Copy)]
pub struct LiteralExtractor;
#[derive(Debug, Serialize)]
pub struct Selection {
    pub selected: bool,
    pub reason: &'static str,
    pub provider: &'static str,
    pub max_bytes: usize,
    pub max_relations: usize,
}
impl LiteralExtractor {
    pub fn selection(&self, origin: &str, kind: &str, text: &str) -> Selection {
        let reason = if origin != "user" {
            "non_user_origin"
        } else if !["decision", "procedure", "task", "incident"].contains(&kind) {
            "unselected_kind"
        } else if text.len() > 8192 {
            "source_too_large"
        } else if !text.contains("[[") {
            "no_annotation"
        } else {
            "revision_addressed_annotation"
        };
        Selection {
            selected: reason == "revision_addressed_annotation",
            reason,
            provider: "literal-v1",
            max_bytes: 8192,
            max_relations: 16,
        }
    }
    pub fn extract(&self, source: &Anchor, text: &str) -> Result<Vec<Relation>> {
        if text.len() > 8192 {
            return Err(Error::new(
                "extraction_limit",
                "literal source exceeds 8192 bytes",
            ));
        }
        let mut output = Vec::new();
        let mut cursor = 0;
        while let Some(offset) = text[cursor..].find("[[") {
            let start = cursor + offset;
            let Some(close) = text[start + 2..].find("]]") else {
                break;
            };
            let from_end = start + 2 + close;
            cursor = from_end + 2;
            let Some(from) = parse_node(&text[start + 2..from_end]) else {
                continue;
            };
            let rest = &text[cursor..];
            let Some(target_open) = rest.find("[[") else {
                break;
            };
            let relation = rest[..target_open].trim();
            if !RELATIONS.contains(&relation) {
                continue;
            }
            let target_start = cursor + target_open;
            let Some(close) = text[target_start + 2..].find("]]") else {
                break;
            };
            let end = target_start + 2 + close;
            let Some(to) = parse_node(&text[target_start + 2..end]) else {
                cursor = end + 2;
                continue;
            };
            cursor = end + 2;
            if from == to {
                continue;
            }
            if output.len() >= 16 {
                return Err(Error::new(
                    "extraction_limit",
                    "at most sixteen literal relationships per source",
                ));
            }
            output.push(Relation {
                from,
                to,
                relation: relation.into(),
                qualifiers: Default::default(),
                evidence: vec![Anchor {
                    source: source.source.clone(),
                    revision: source.revision,
                    start,
                    end: cursor,
                }],
            });
        }
        Ok(output)
    }
}
fn parse_node(s: &str) -> Option<Node> {
    let (id, revision) = s.split_once('@')?;
    if id.len() != 32 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let revision = revision.parse::<i64>().ok()?;
    if revision < 1 {
        return None;
    }
    Some(Node {
        id: id.into(),
        revision,
    })
}
