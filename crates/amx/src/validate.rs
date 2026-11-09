//! The record rules AMX does not trade away (SPEC sections 1 to 7).
//!
//! Validation is structural: it proves a record carries what readers need
//! (provenance, evidence, complete scope, content-free tombstones, secret
//! references only). It does not judge whether a memory is true.
//! Source `content_hash` values are checked for shape here and against the
//! text when a bundle is read.
use crate::{
    Error, Evidence, Link, Manifest, Object, Provenance, Record, Ref, Result, Scope, Source,
    Tombstone, parse_timestamp,
};
use std::collections::BTreeSet;

const KINDS: &[&str] = &[
    "fact",
    "preference",
    "decision",
    "procedure",
    "task",
    "incident",
    "entity",
    "secret_ref",
];
const RELATIONS: &[&str] = &[
    "supports",
    "supersedes",
    "contradicts",
    "applies_to",
    "requires",
    "mitigates",
];
const ORIGINS: &[&str] = &["user", "agent", "extractor", "import", "system"];
const VISIBILITIES: &[&str] = &["project", "session", "personal"];
const STATUSES: &[&str] = &["active", "superseded", "invalid"];
const ROLES: &[&str] = &["supports", "contradicts", "context"];
const TARGETS: &[&str] = &["record", "source", "link"];
const REASONS: &[&str] = &["user_request", "retention", "policy", "correction", "other"];
const CONTENT_TYPES: &[&str] = &["text/plain", "text/markdown"];
/// Dimensions every reader understands: they are required (or tied to
/// visibility) and so can never make a record unreadable on their own.
const CORE_DIMENSIONS: &[&str] = &["namespace", "visibility", "session"];

fn invalid(message: impl Into<String>) -> Error {
    Error::new("invalid", message)
}

fn check(ok: bool, message: impl FnOnce() -> String) -> Result<()> {
    if ok { Ok(()) } else { Err(invalid(message())) }
}

/// Whether `text` is a valid AMX identifier.
pub fn is_id(text: &str) -> bool {
    (1..=128).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// `prefix.name`: a lowercase prefix, a dot, then a non-empty name.
fn is_namespaced(text: &str) -> bool {
    let Some((prefix, name)) = text.split_once('.') else {
        return false;
    };
    let mut prefix_bytes = prefix.bytes();
    matches!(prefix_bytes.next(), Some(b'a'..=b'z'))
        && prefix_bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

fn attribute_key(text: &str) -> bool {
    let Some((prefix, name)) = text.split_once('.') else {
        return false;
    };
    !prefix.is_empty()
        && !name.is_empty()
        && prefix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

fn version(amx: &str) -> Result<()> {
    let ok = amx.split_once('.').is_some_and(|(major, minor)| {
        major == "0" && !minor.is_empty() && minor.bytes().all(|b| b.is_ascii_digit())
    });
    check(ok, || format!("unsupported AMX version {amx:?}"))
}

fn id(field: &str, value: &str) -> Result<()> {
    check(is_id(value), || {
        format!("{field} is not a valid identifier")
    })
}

fn revision(field: &str, value: u64) -> Result<()> {
    check(value >= 1, || format!("{field} must be at least 1"))
}

fn reference(field: &str, value: &Ref) -> Result<()> {
    id(field, &value.id)?;
    revision(field, value.revision)
}

fn one_of(field: &str, value: &str, allowed: &[&str]) -> Result<()> {
    check(allowed.contains(&value), || {
        format!("{field} {value:?} is not one of {allowed:?}")
    })
}

fn non_empty(field: &str, value: &str) -> Result<()> {
    check(!value.is_empty(), || format!("{field} must not be empty"))
}

fn timestamp(field: &str, value: &str) -> Result<i64> {
    parse_timestamp(value).ok_or_else(|| invalid(format!("{field} is not an AMX timestamp")))
}

fn optional_timestamp(field: &str, value: Option<&String>) -> Result<Option<i64>> {
    value.map(|v| timestamp(field, v)).transpose()
}

fn validity(from: &str, to: Option<&String>) -> Result<()> {
    let start = timestamp("valid_from", from)?;
    if let Some(end) = optional_timestamp("valid_to", to)? {
        check(end >= start, || "valid_to is before valid_from".into())?;
    }
    Ok(())
}

fn attributes(value: Option<&Object>) -> Result<()> {
    for key in value.into_iter().flat_map(|o| o.keys()) {
        check(attribute_key(key), || {
            format!("attribute {key:?} must be namespaced (prefix.name)")
        })?;
        check(!key.starts_with("secret.value"), || {
            format!("attribute {key:?} would carry a secret value")
        })?;
    }
    Ok(())
}

/// Check a scope's shape: required dimensions present, values strings,
/// custom dimensions namespaced. Whether a reader may *see* it is
/// [`Support::visible`].
pub fn validate_scope(scope: &Scope) -> Result<()> {
    non_empty("scope.namespace", &scope.namespace)?;
    one_of("scope.visibility", &scope.visibility, VISIBILITIES)?;
    if scope.visibility == "session" {
        check(
            scope.session.as_deref().is_some_and(|s| !s.is_empty()),
            || "session visibility requires scope.session".into(),
        )?;
    }
    for (name, value) in [
        ("session", &scope.session),
        ("repo", &scope.repo),
        ("branch", &scope.branch),
        ("user", &scope.user),
    ] {
        if let Some(value) = value {
            non_empty(&format!("scope.{name}"), value)?;
        }
    }
    if scope.branch.is_some() {
        check(scope.repo.is_some(), || {
            "scope.branch requires scope.repo".into()
        })?;
    }
    for (name, value) in &scope.extra {
        check(is_namespaced(name), || {
            format!("custom scope dimension {name:?} must be namespaced (prefix.name)")
        })?;
        check(value.as_str().is_some_and(|s| !s.is_empty()), || {
            format!("scope dimension {name:?} must be a non-empty string")
        })?;
    }
    Ok(())
}

fn provenance(value: &Provenance) -> Result<()> {
    one_of("provenance.origin", &value.origin, ORIGINS)?;
    non_empty("provenance.author", &value.author)?;
    if !value.explicit {
        check(
            value.method.as_deref().is_some_and(|m| !m.is_empty()),
            || "inferred provenance requires provenance.method".into(),
        )?;
        check(
            value
                .method_version
                .as_deref()
                .is_some_and(|m| !m.is_empty()),
            || "inferred provenance requires provenance.method_version".into(),
        )?;
    }
    Ok(())
}

fn evidence(explicit: bool, value: Option<&Vec<Evidence>>) -> Result<()> {
    let entries = value.map(Vec::as_slice).unwrap_or_default();
    if !explicit {
        check(!entries.is_empty(), || {
            "inferred records and links require evidence".into()
        })?;
    }
    for entry in entries {
        id("evidence.source", &entry.source)?;
        revision("evidence.source_revision", entry.source_revision)?;
        one_of("evidence.role", &entry.role, ROLES)?;
        if let Some([start, end]) = entry.span {
            check(start <= end, || {
                "evidence.span start is after its end".into()
            })?;
        }
    }
    Ok(())
}

/// Validate a memory record (SPEC 2.2, 4, 5 and 7).
pub fn validate_record(record: &Record) -> Result<()> {
    version(&record.amx)?;
    id("id", &record.id)?;
    revision("revision", record.revision)?;
    check(
        KINDS.contains(&record.kind.as_str()) || is_namespaced(&record.kind),
        || format!("kind {:?} is neither standard nor namespaced", record.kind),
    )?;
    if let Some(content_type) = &record.content_type {
        one_of("content_type", content_type, CONTENT_TYPES)?;
    }
    if let Some(key) = &record.key {
        check((1..=256).contains(&key.len()), || {
            "key must be 1 to 256 bytes".into()
        })?;
    }
    validate_scope(&record.scope)?;
    provenance(&record.provenance)?;
    evidence(record.provenance.explicit, record.evidence.as_ref())?;
    one_of("status", &record.status, STATUSES)?;
    if let Some(target) = &record.supersedes {
        reference("supersedes", target)?;
        check(target.id != record.id, || {
            "supersedes names a different record; corrections raise the revision".into()
        })?;
    }
    validity(&record.valid_from, record.valid_to.as_ref())?;
    timestamp("recorded_at", &record.recorded_at)?;
    optional_timestamp("event_at", record.event_at.as_ref())?;
    optional_timestamp("expires_at", record.expires_at.as_ref())?;
    attributes(record.attributes.as_ref())?;
    if record.kind == "secret_ref" {
        check(record.content.is_empty(), || {
            "secret_ref records must have empty content".into()
        })?;
        let locator = record
            .attributes
            .as_ref()
            .and_then(|a| a.get("secret.locator"))
            .and_then(|v| v.as_str());
        check(locator.is_some_and(|l| !l.is_empty()), || {
            "secret_ref records need attributes[\"secret.locator\"]".into()
        })?;
    }
    Ok(())
}

/// Validate a source (SPEC 2.3).
pub fn validate_source(source: &Source) -> Result<()> {
    version(&source.amx)?;
    id("id", &source.id)?;
    revision("revision", source.revision)?;
    one_of("origin", &source.origin, ORIGINS)?;
    non_empty("author", &source.author)?;
    let hash_ok = source
        .content_hash
        .strip_prefix("sha256:")
        .is_some_and(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        });
    check(hash_ok, || {
        "content_hash must be sha256: and 64 lowercase hex digits".into()
    })?;
    validate_scope(&source.scope)?;
    timestamp("recorded_at", &source.recorded_at)?;
    optional_timestamp("event_at", source.event_at.as_ref())?;
    optional_timestamp("expires_at", source.expires_at.as_ref())?;
    attributes(source.attributes.as_ref())
}

/// Validate a link (SPEC 2.4).
pub fn validate_link(link: &Link) -> Result<()> {
    version(&link.amx)?;
    id("id", &link.id)?;
    revision("revision", link.revision)?;
    reference("from", &link.from)?;
    reference("to", &link.to)?;
    check(
        RELATIONS.contains(&link.relation.as_str()) || is_namespaced(&link.relation),
        || {
            format!(
                "relation {:?} is neither standard nor namespaced",
                link.relation
            )
        },
    )?;
    validate_scope(&link.scope)?;
    provenance(&link.provenance)?;
    evidence(link.provenance.explicit, link.evidence.as_ref())?;
    one_of("status", &link.status, STATUSES)?;
    validity(&link.valid_from, link.valid_to.as_ref())?;
    timestamp("recorded_at", &link.recorded_at)?;
    attributes(link.attributes.as_ref())
}

/// Validate a tombstone (SPEC 2.5): it must be content-free.
pub fn validate_tombstone(tombstone: &Tombstone) -> Result<()> {
    version(&tombstone.amx)?;
    id("id", &tombstone.id)?;
    one_of("target_type", &tombstone.target_type, TARGETS)?;
    id("target_id", &tombstone.target_id)?;
    validate_scope(&tombstone.scope)?;
    timestamp("deleted_at", &tombstone.deleted_at)?;
    one_of("reason", &tombstone.reason, REASONS)?;
    non_empty("actor", &tombstone.actor)?;
    for derived in tombstone.derived.iter().flatten() {
        id("derived", derived)?;
    }
    attributes(tombstone.attributes.as_ref())?;
    let carries_content = |key: &str| {
        let last = key.rsplit('.').next().unwrap_or(key);
        matches!(last, "content" | "text")
    };
    let fields = tombstone.extra.keys();
    let attribute_keys = tombstone.attributes.iter().flat_map(|a| a.keys());
    if let Some(key) = fields.chain(attribute_keys).find(|k| carries_content(k)) {
        return Err(invalid(format!(
            "tombstones are content-free; {key:?} is not allowed"
        )));
    }
    Ok(())
}

/// Validate a bundle manifest (SPEC 9.1).
pub fn validate_manifest(manifest: &Manifest) -> Result<()> {
    version(&manifest.amx)?;
    id("bundle_id", &manifest.bundle_id)?;
    non_empty("generator.name", &manifest.generator.name)?;
    non_empty("generator.version", &manifest.generator.version)?;
    timestamp("created_at", &manifest.created_at)?;
    let mut seen = BTreeSet::new();
    for namespace in &manifest.namespaces {
        non_empty("namespaces.id", &namespace.id)?;
        check(seen.insert(&namespace.id), || {
            format!("namespace {:?} is listed twice", namespace.id)
        })?;
    }
    for capability in manifest.capabilities.iter().flatten() {
        non_empty("capabilities", capability)?;
    }
    attributes(manifest.attributes.as_ref())
}

/// What scope a reader understands. Anything else fails closed (SPEC 3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Support {
    visibilities: BTreeSet<String>,
    dimensions: BTreeSet<String>,
}

impl Support {
    /// A reader that supports these visibilities and these optional
    /// dimensions beyond `namespace`, `visibility` and `session`.
    pub fn new(visibilities: &[&str], dimensions: &[&str]) -> Self {
        Self {
            visibilities: visibilities.iter().map(|v| v.to_string()).collect(),
            dimensions: dimensions.iter().map(|d| d.to_string()).collect(),
        }
    }

    /// `Ok` when this reader may treat a record with `scope` as visible;
    /// `unsupported_scope` when it must hide it.
    pub fn visible(&self, scope: &Scope) -> Result<()> {
        validate_scope(scope)?;
        let unsupported = |what: String| {
            Err(Error::new(
                "unsupported_scope",
                format!("{what} is not supported by this reader; the record is not visible"),
            ))
        };
        if !self.visibilities.contains(&scope.visibility) {
            return unsupported(format!("visibility {:?}", scope.visibility));
        }
        let present = [
            ("repo", scope.repo.is_some()),
            ("branch", scope.branch.is_some()),
            ("user", scope.user.is_some()),
        ];
        let names = present
            .into_iter()
            .filter(|(_, set)| *set)
            .map(|(name, _)| name)
            .chain(scope.extra.keys().map(String::as_str));
        for name in names {
            if !CORE_DIMENSIONS.contains(&name) && !self.dimensions.contains(name) {
                return unsupported(format!("scope dimension {name:?}"));
            }
        }
        Ok(())
    }
}
