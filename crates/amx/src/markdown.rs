//! The readable profile (SPEC section 8): one Markdown file per record,
//! front matter holding the envelope as `field: <JSON value>` lines (JSON is
//! a subset of YAML) and the body holding `content` byte for byte.
//!
//! The profile is a view. [`diff_tree`] reports how a tree of profile files
//! differs from a store's records; nothing here applies a difference. A
//! missing or edited file is never a deletion or a correction: those go
//! through the store's operations.
use crate::{Error, Object, Record, Result, validate_record};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

const DELIMITER: &str = "---";

fn invalid(message: impl Into<String>) -> Error {
    Error::new("invalid", message)
}

fn line<T: Serialize>(out: &mut String, field: &str, value: &T) -> Result<()> {
    let json = serde_json::to_string(value).map_err(|e| invalid(e.to_string()))?;
    out.push_str(field);
    out.push_str(": ");
    out.push_str(&json);
    out.push('\n');
    Ok(())
}

fn optional<T: Serialize>(out: &mut String, field: &str, value: &Option<T>) -> Result<()> {
    match value {
        Some(value) => line(out, field, value),
        None => Ok(()),
    }
}

/// Render a record in the readable profile. Fields appear in canonical
/// order; unknown fields follow, sorted.
pub fn to_markdown(record: &Record) -> Result<String> {
    let mut out = String::from("---\n");
    line(&mut out, "amx", &record.amx)?;
    line(&mut out, "id", &record.id)?;
    line(&mut out, "revision", &record.revision)?;
    line(&mut out, "kind", &record.kind)?;
    optional(&mut out, "content_type", &record.content_type)?;
    optional(&mut out, "key", &record.key)?;
    line(&mut out, "scope", &record.scope)?;
    line(&mut out, "provenance", &record.provenance)?;
    optional(&mut out, "evidence", &record.evidence)?;
    line(&mut out, "status", &record.status)?;
    optional(&mut out, "supersedes", &record.supersedes)?;
    line(&mut out, "valid_from", &record.valid_from)?;
    optional(&mut out, "valid_to", &record.valid_to)?;
    line(&mut out, "recorded_at", &record.recorded_at)?;
    optional(&mut out, "event_at", &record.event_at)?;
    optional(&mut out, "expires_at", &record.expires_at)?;
    optional(&mut out, "attributes", &record.attributes)?;
    for (field, value) in &record.extra {
        if field == "content" || field.contains(['\n', ':']) {
            return Err(invalid(format!("field {field:?} cannot be rendered")));
        }
        line(&mut out, field, value)?;
    }
    out.push_str(DELIMITER);
    out.push('\n');
    out.push_str(&record.content);
    Ok(out)
}

/// Parse a profile file back into the record it renders.
pub fn from_markdown(text: &str) -> Result<Record> {
    let rest = text
        .strip_prefix("---\n")
        .ok_or_else(|| invalid("a profile file starts with a --- line"))?;
    let mut fields = Object::new();
    let mut remaining = rest;
    loop {
        let (current, after) = remaining
            .split_once('\n')
            .ok_or_else(|| invalid("front matter has no closing --- line"))?;
        remaining = after;
        if current == DELIMITER {
            break;
        }
        let (field, json) = current
            .split_once(": ")
            .ok_or_else(|| invalid(format!("front matter line {current:?} is not field: value")))?;
        if field == "content" {
            return Err(invalid("content belongs in the body, not the front matter"));
        }
        let value: Value = serde_json::from_str(json)
            .map_err(|e| invalid(format!("front matter field {field:?}: {e}")))?;
        if fields.insert(field.to_string(), value).is_some() {
            return Err(invalid(format!("front matter repeats {field:?}")));
        }
    }
    fields.insert("content".into(), Value::String(remaining.to_string()));
    serde_json::from_value(Value::Object(fields)).map_err(|e| invalid(e.to_string()))
}

/// Render the newest revision of each identity as `<id>.md`.
pub fn render_tree(records: &[Record]) -> Result<BTreeMap<String, String>> {
    let mut newest: BTreeMap<&str, &Record> = BTreeMap::new();
    for record in records {
        let slot = newest.entry(&record.id).or_insert(record);
        if record.revision > slot.revision {
            *slot = record;
        }
    }
    newest
        .into_values()
        .map(|record| Ok((format!("{}.md", record.id), to_markdown(record)?)))
        .collect()
}

/// How a profile file differs from the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// The store holds the identity but the tree has no file for it.
    Missing,
    /// The file differs from the store's newest revision.
    Edited,
    /// The file names an identity the store does not hold.
    Added,
    /// The file holds a record whose id does not match its name.
    Misnamed,
    /// The file is not a valid profile file; the error code says why.
    Invalid(String),
}

/// One difference between a tree and a store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Difference {
    pub file: String,
    pub id: Option<String>,
    pub change: Change,
}

/// Compare a tree of profile files (file name to contents; names not
/// ending in `.md` are ignored) with a store's records. The result is a
/// report sorted by file name. Applying any of it is an explicit operation
/// for the caller, never a side effect of this comparison.
pub fn diff_tree(records: &[Record], files: &BTreeMap<String, String>) -> Vec<Difference> {
    let expected = match render_tree(records) {
        Ok(tree) => tree,
        Err(error) => {
            return vec![Difference {
                file: String::new(),
                id: None,
                change: Change::Invalid(error.code.into()),
            }];
        }
    };
    let mut out = Vec::new();
    for (file, text) in files.iter().filter(|(name, _)| name.ends_with(".md")) {
        let parsed = from_markdown(text).and_then(|r| validate_record(&r).map(|()| r));
        let record = match parsed {
            Ok(record) => record,
            Err(error) => {
                out.push(Difference {
                    file: file.clone(),
                    id: None,
                    change: Change::Invalid(error.code.into()),
                });
                continue;
            }
        };
        let id = Some(record.id.clone());
        let change = if format!("{}.md", record.id) != *file {
            Some(Change::Misnamed)
        } else {
            match expected.get(file) {
                None => Some(Change::Added),
                Some(rendered) if rendered != text => Some(Change::Edited),
                Some(_) => None,
            }
        };
        if let Some(change) = change {
            out.push(Difference {
                file: file.clone(),
                id,
                change,
            });
        }
    }
    for file in expected.keys().filter(|f| !files.contains_key(*f)) {
        out.push(Difference {
            file: file.clone(),
            id: file.strip_suffix(".md").map(String::from),
            change: Change::Missing,
        });
    }
    out.sort_by(|a, b| a.file.cmp(&b.file));
    out
}
