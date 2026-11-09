//! AMX (Agent Memory eXchange): the record types, canonical JSONL encoding,
//! validation, bundles and import planning defined by `spec/amx/SPEC.md`.
//!
//! The crate is pure logic: it never opens a database or a network
//! connection. Stores implement the operations contract on top of it.

mod bundle;
mod plan;
mod record;
mod validate;

pub use bundle::{
    Bundle, CHECKSUMMED, CHECKSUMS, LINKS, MANIFEST, MAX_FILE_BYTES, RECORDS, SIGNATURE, SOURCES,
    TOMBSTONES, sha256_hex,
};
pub use plan::{
    Conflict, Held, Holdings, ImportPlan, Item, ItemRef, link_fingerprint, plan_import,
    record_fingerprint, source_fingerprint,
};
pub use record::{
    Counts, Evidence, Generator, Link, Manifest, NamespaceCursor, Object, Provenance, Record, Ref,
    Scope, Source, Tombstone, VERSION, format_timestamp, parse_timestamp, read_jsonl, write_jsonl,
};
pub use validate::{
    Support, is_id, validate_link, validate_manifest, validate_record, validate_scope,
    validate_source, validate_tombstone,
};

/// An AMX error: a stable machine-readable `code`, a human message and,
/// for stream input, the 1-based line it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
    pub line: Option<usize>,
}

impl Error {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            line: None,
        }
    }

    pub fn at_line(mut self, line: usize) -> Self {
        self.line = Some(line);
        self
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(line) => write!(f, "{} (line {line}): {}", self.code, self.message),
            None => write!(f, "{}: {}", self.code, self.message),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
