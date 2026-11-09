//! Record types and their canonical JSONL encoding (SPEC sections 2 and 9.1).
//!
//! Types are deliberately permissive about values (kinds, statuses and
//! visibilities are strings) so that a reader can hold and re-export what it
//! does not understand. The rules live in validation, not in parsing.
//! Field declaration order is the canonical field order; unknown fields are
//! kept in `extra` and written after the known ones, sorted by name.
use crate::{Error, Result};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// The spec version these types implement.
pub const VERSION: &str = "0.1";

/// A JSON object with sorted keys, used for attributes, qualifiers and
/// unknown fields.
pub type Object = serde_json::Map<String, serde_json::Value>;

/// A reference to one revision of an identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Ref {
    pub id: String,
    pub revision: u64,
}

/// Typed scope dimensions (SPEC section 3). Unknown dimensions land in
/// `extra`; a reader that does not support them must fail closed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Scope {
    pub namespace: String,
    pub visibility: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(flatten)]
    pub extra: Object,
}

/// Who asserted a record, and whether it was stated or inferred (SPEC 4.1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    pub origin: String,
    pub author: String,
    pub explicit: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method_version: Option<String>,
    #[serde(flatten)]
    pub extra: Object,
}

/// A pointer from a record or link to the source passage behind it (SPEC 4.2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub source: String,
    pub source_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<[u64; 2]>,
    pub role: String,
    #[serde(flatten)]
    pub extra: Object,
}

/// A memory record (SPEC 2.2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub amx: String,
    pub id: String,
    pub revision: u64,
    pub kind: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub scope: Scope,
    pub provenance: Provenance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Vec<Evidence>>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<Ref>,
    pub valid_from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_to: Option<String>,
    pub recorded_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Object>,
    #[serde(flatten)]
    pub extra: Object,
}

/// An original passage (SPEC 2.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub amx: String,
    pub id: String,
    pub revision: u64,
    pub origin: String,
    pub author: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
    pub text: String,
    pub content_hash: String,
    pub scope: Scope,
    pub recorded_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Object>,
    #[serde(flatten)]
    pub extra: Object,
}

/// A typed, directed relation between two record revisions (SPEC 2.4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Link {
    pub amx: String,
    pub id: String,
    pub revision: u64,
    pub from: Ref,
    pub to: Ref,
    pub relation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualifiers: Option<Object>,
    pub scope: Scope,
    pub provenance: Provenance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Vec<Evidence>>,
    pub status: String,
    pub valid_from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_to: Option<String>,
    pub recorded_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Object>,
    #[serde(flatten)]
    pub extra: Object,
}

/// A content-free deletion marker (SPEC 2.5).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tombstone {
    pub amx: String,
    pub id: String,
    pub target_type: String,
    pub target_id: String,
    pub scope: Scope,
    pub deleted_at: String,
    pub reason: String,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Object>,
    #[serde(flatten)]
    pub extra: Object,
}

/// The producer of a bundle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Generator {
    pub name: String,
    pub version: String,
}

/// A namespace in a bundle and the change cursor it was exported at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamespaceCursor {
    pub id: String,
    pub cursor: u64,
}

/// Record counts per data file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub records: u64,
    pub sources: u64,
    pub links: u64,
    pub tombstones: u64,
}

/// A bundle manifest (SPEC 9.1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub amx: String,
    pub bundle_id: String,
    pub generator: Generator,
    pub created_at: String,
    pub namespaces: Vec<NamespaceCursor>,
    pub counts: Counts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Object>,
    #[serde(flatten)]
    pub extra: Object,
}

/// Parse a JSON Lines stream: every line one object, every line ending in
/// `\n`, no blank lines. Errors name the 1-based line.
pub fn read_jsonl<T: DeserializeOwned>(text: &str) -> Result<Vec<T>> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let Some(body) = text.strip_suffix('\n') else {
        let lines = text.split('\n').count();
        return Err(Error::new("invalid", "stream must end with a newline").at_line(lines));
    };
    body.split('\n')
        .enumerate()
        .map(|(index, line)| {
            if line.trim().is_empty() {
                return Err(Error::new("invalid", "blank line").at_line(index + 1));
            }
            serde_json::from_str(line)
                .map_err(|e| Error::new("invalid", e.to_string()).at_line(index + 1))
        })
        .collect()
}

/// Write items as canonical JSON Lines.
pub fn write_jsonl<T: Serialize>(items: &[T]) -> Result<String> {
    let mut out = String::new();
    for item in items {
        let line = serde_json::to_string(item).map_err(|e| Error::new("invalid", e.to_string()))?;
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out)
}

// Civil-date conversions after Howard Hinnant's `days_from_civil` algorithms.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        _ => 28,
    }
}

/// Format Unix seconds as an AMX timestamp (`YYYY-MM-DDTHH:MM:SSZ`).
pub fn format_timestamp(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Parse an AMX timestamp into Unix seconds; `None` unless it is exactly
/// `YYYY-MM-DDTHH:MM:SSZ` and names a real UTC second.
pub fn parse_timestamp(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let digits = &b[range];
        if !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        Some(
            digits
                .iter()
                .fold(0i64, |n, d| n * 10 + i64::from(d - b'0')),
        )
    };
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}
