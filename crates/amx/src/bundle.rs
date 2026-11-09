//! `.amx` bundles (SPEC section 9): a directory holding a manifest, four
//! JSONL data files and SHA-256 checksums, optionally signed.
//!
//! Reading verifies everything before returning a single record: the exact
//! file set (no extras, no symlinks, no subdirectories), every checksum,
//! every record's validity, source hashes, namespace membership,
//! uniqueness and the manifest counts. Writing enforces the same rules, so
//! a bundle that writes is a bundle that reads.
use crate::{
    Error, Generator, Link, Manifest, NamespaceCursor, Record, Result, Source, Tombstone, VERSION,
    read_jsonl, validate_link, validate_manifest, validate_record, validate_source,
    validate_tombstone, write_jsonl,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::Path,
};

pub const MANIFEST: &str = "manifest.json";
pub const RECORDS: &str = "records.jsonl";
pub const SOURCES: &str = "sources.jsonl";
pub const LINKS: &str = "links.jsonl";
pub const TOMBSTONES: &str = "tombstones.jsonl";
pub const CHECKSUMS: &str = "CHECKSUMS";
pub const SIGNATURE: &str = "manifest.sig";
/// The files `CHECKSUMS` covers, in the sorted order it lists them.
pub const CHECKSUMMED: [&str; 5] = [LINKS, MANIFEST, RECORDS, SOURCES, TOMBSTONES];
/// The largest file a reader accepts.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new("invalid", message)
}

fn io(error: std::io::Error) -> Error {
    Error::new("io", error.to_string())
}

/// A bundle in memory.
#[derive(Clone, Debug, PartialEq)]
pub struct Bundle {
    pub manifest: Manifest,
    pub records: Vec<Record>,
    pub sources: Vec<Source>,
    pub links: Vec<Link>,
    pub tombstones: Vec<Tombstone>,
    /// The raw `manifest.sig` contents, if present. Verifying it is the
    /// reader's choice (see the `signing` feature).
    pub signature: Option<String>,
}

impl Bundle {
    /// An empty bundle. Counts are filled in when it is encoded.
    pub fn new(
        bundle_id: &str,
        generator: Generator,
        created_at: &str,
        namespaces: Vec<NamespaceCursor>,
    ) -> Self {
        Self {
            manifest: Manifest {
                amx: VERSION.into(),
                bundle_id: bundle_id.into(),
                generator,
                created_at: created_at.into(),
                namespaces,
                counts: Default::default(),
                capabilities: None,
                omitted: None,
                attributes: None,
                extra: Default::default(),
            },
            records: Vec::new(),
            sources: Vec::new(),
            links: Vec::new(),
            tombstones: Vec::new(),
            signature: None,
        }
    }

    /// Encode into canonical file contents, keyed by file name: data sorted
    /// by identity and revision, counts recomputed, checksums computed.
    /// Fails if the bundle would not read back.
    pub fn encode(&self) -> Result<BTreeMap<&'static str, Vec<u8>>> {
        let mut bundle = self.clone();
        bundle
            .records
            .sort_by(|a, b| (&a.id, a.revision).cmp(&(&b.id, b.revision)));
        bundle
            .sources
            .sort_by(|a, b| (&a.id, a.revision).cmp(&(&b.id, b.revision)));
        bundle
            .links
            .sort_by(|a, b| (&a.id, a.revision).cmp(&(&b.id, b.revision)));
        bundle.tombstones.sort_by(|a, b| a.id.cmp(&b.id));
        bundle.manifest.counts.records = bundle.records.len() as u64;
        bundle.manifest.counts.sources = bundle.sources.len() as u64;
        bundle.manifest.counts.links = bundle.links.len() as u64;
        bundle.manifest.counts.tombstones = bundle.tombstones.len() as u64;
        bundle.check()?;
        let manifest =
            serde_json::to_string(&bundle.manifest).map_err(|e| invalid(e.to_string()))? + "\n";
        let mut files = BTreeMap::new();
        files.insert(MANIFEST, manifest.into_bytes());
        files.insert(RECORDS, write_jsonl(&bundle.records)?.into_bytes());
        files.insert(SOURCES, write_jsonl(&bundle.sources)?.into_bytes());
        files.insert(LINKS, write_jsonl(&bundle.links)?.into_bytes());
        files.insert(TOMBSTONES, write_jsonl(&bundle.tombstones)?.into_bytes());
        let checksums = CHECKSUMMED
            .iter()
            .map(|name| format!("{}  {name}\n", sha256_hex(&files[name])))
            .collect::<String>();
        files.insert(CHECKSUMS, checksums.into_bytes());
        if let Some(signature) = &bundle.signature {
            files.insert(SIGNATURE, signature.clone().into_bytes());
        }
        Ok(files)
    }

    /// Decode and verify file contents keyed by file name.
    pub fn decode(files: &BTreeMap<String, Vec<u8>>) -> Result<Self> {
        for name in files.keys() {
            if !CHECKSUMMED.contains(&name.as_str()) && name != CHECKSUMS && name != SIGNATURE {
                return Err(invalid(format!("unexpected file {name:?} in bundle")));
            }
        }
        let file = |name: &str| -> Result<&str> {
            let bytes = files
                .get(name)
                .ok_or_else(|| invalid(format!("bundle is missing {name}")))?;
            std::str::from_utf8(bytes).map_err(|_| invalid(format!("{name} is not UTF-8")))
        };
        verify_checksums(file(CHECKSUMS)?, files)?;
        let manifest_text = file(MANIFEST)?;
        let manifest_line = manifest_text
            .strip_suffix('\n')
            .filter(|line| !line.contains('\n'))
            .ok_or_else(|| invalid("manifest.json must be one JSON object and a newline"))?;
        let manifest: Manifest =
            serde_json::from_str(manifest_line).map_err(|e| invalid(format!("manifest: {e}")))?;
        let in_file = |name: &'static str| {
            move |e: Error| Error {
                message: format!("{name}: {}", e.message),
                ..e
            }
        };
        let bundle = Self {
            manifest,
            records: read_jsonl(file(RECORDS)?).map_err(in_file(RECORDS))?,
            sources: read_jsonl(file(SOURCES)?).map_err(in_file(SOURCES))?,
            links: read_jsonl(file(LINKS)?).map_err(in_file(LINKS))?,
            tombstones: read_jsonl(file(TOMBSTONES)?).map_err(in_file(TOMBSTONES))?,
            signature: match files.get(SIGNATURE) {
                Some(bytes) => Some(
                    String::from_utf8(bytes.clone())
                        .map_err(|_| invalid("manifest.sig is not UTF-8"))?,
                ),
                None => None,
            },
        };
        bundle.check()?;
        let counts = &bundle.manifest.counts;
        let actual = [
            (counts.records, bundle.records.len(), RECORDS),
            (counts.sources, bundle.sources.len(), SOURCES),
            (counts.links, bundle.links.len(), LINKS),
            (counts.tombstones, bundle.tombstones.len(), TOMBSTONES),
        ];
        for (claimed, found, name) in actual {
            if claimed != found as u64 {
                return Err(invalid(format!(
                    "manifest counts {claimed} lines in {name}; found {found}"
                )));
            }
        }
        Ok(bundle)
    }

    /// Every rule a bundle's contents must meet, independent of encoding.
    fn check(&self) -> Result<()> {
        validate_manifest(&self.manifest)?;
        let namespaces: BTreeSet<&str> = self
            .manifest
            .namespaces
            .iter()
            .map(|n| n.id.as_str())
            .collect();
        let listed = |what: &str, id: &str, namespace: &str| -> Result<()> {
            if namespaces.contains(namespace) {
                Ok(())
            } else {
                Err(invalid(format!(
                    "{what} {id:?} is in namespace {namespace:?}, which the manifest does not list"
                )))
            }
        };
        let context = |what: &str, id: &str| {
            let label = format!("{what} {id:?}");
            move |e: Error| Error {
                message: format!("{label}: {}", e.message),
                ..e
            }
        };
        let mut seen = BTreeSet::new();
        for r in &self.records {
            validate_record(r).map_err(context("record", &r.id))?;
            listed("record", &r.id, &r.scope.namespace)?;
            unique(&mut seen, "record", &r.scope.namespace, &r.id, r.revision)?;
        }
        for s in &self.sources {
            validate_source(s).map_err(context("source", &s.id))?;
            listed("source", &s.id, &s.scope.namespace)?;
            unique(&mut seen, "source", &s.scope.namespace, &s.id, s.revision)?;
            let expected = format!("sha256:{}", sha256_hex(s.text.as_bytes()));
            if s.content_hash != expected {
                return Err(invalid(format!(
                    "source {:?} content_hash does not match its text",
                    s.id
                )));
            }
        }
        for l in &self.links {
            validate_link(l).map_err(context("link", &l.id))?;
            listed("link", &l.id, &l.scope.namespace)?;
            unique(&mut seen, "link", &l.scope.namespace, &l.id, l.revision)?;
        }
        for t in &self.tombstones {
            validate_tombstone(t).map_err(context("tombstone", &t.id))?;
            listed("tombstone", &t.id, &t.scope.namespace)?;
            unique(&mut seen, "tombstone", &t.scope.namespace, &t.id, 0)?;
        }
        Ok(())
    }

    /// Write the bundle to a new directory named `<name>.amx`. The
    /// directory is assembled beside the target and renamed into place,
    /// so a reader never sees half a bundle.
    pub fn write(&self, path: &Path) -> Result<()> {
        bundle_name(path)?;
        if fs::symlink_metadata(path).is_ok() {
            return Err(Error::new(
                "exists",
                format!("{} already exists", path.display()),
            ));
        }
        let files = self.encode()?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let staging = parent.join(format!(
            ".{}.partial-{}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&staging);
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&staging)
            .map_err(io)?;
        let result = (|| {
            for (name, bytes) in &files {
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(staging.join(name))
                    .map_err(io)?;
                file.write_all(bytes).map_err(io)?;
                file.sync_all().map_err(io)?;
            }
            fs::File::open(&staging)
                .and_then(|d| d.sync_all())
                .map_err(io)?;
            if fs::symlink_metadata(path).is_ok() {
                return Err(Error::new(
                    "exists",
                    format!("{} already exists", path.display()),
                ));
            }
            fs::rename(&staging, path).map_err(io)?;
            fs::File::open(parent)
                .and_then(|d| d.sync_all())
                .map_err(io)
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        result
    }

    /// Read and verify a bundle directory. The directory itself and every
    /// file in it must be real (not symbolic links); nothing but the bundle
    /// files may be present.
    pub fn read(path: &Path) -> Result<Self> {
        bundle_name(path)?;
        let meta = fs::symlink_metadata(path).map_err(io)?;
        if !meta.is_dir() {
            return Err(invalid(format!(
                "{} is not a bundle directory",
                path.display()
            )));
        }
        let mut files = BTreeMap::new();
        for entry in fs::read_dir(path).map_err(io)? {
            let entry = entry.map_err(io)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| invalid("bundle file names must be UTF-8"))?;
            let known =
                CHECKSUMMED.contains(&name.as_str()) || name == CHECKSUMS || name == SIGNATURE;
            if !known {
                return Err(invalid(format!("unexpected entry {name:?} in bundle")));
            }
            let full = path.join(&name);
            let before = fs::symlink_metadata(&full).map_err(io)?;
            if !before.file_type().is_file() {
                return Err(invalid(format!("{name} must be a regular file")));
            }
            if before.len() > MAX_FILE_BYTES {
                return Err(invalid(format!("{name} exceeds {MAX_FILE_BYTES} bytes")));
            }
            let mut file = fs::File::open(&full).map_err(io)?;
            let opened = file.metadata().map_err(io)?;
            if (opened.dev(), opened.ino()) != (before.dev(), before.ino()) {
                return Err(invalid(format!("{name} changed while it was opened")));
            }
            let mut bytes = Vec::new();
            Read::by_ref(&mut file)
                .take(MAX_FILE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(io)?;
            if bytes.len() as u64 > MAX_FILE_BYTES {
                return Err(invalid(format!("{name} exceeds {MAX_FILE_BYTES} bytes")));
            }
            files.insert(name, bytes);
        }
        Self::decode(&files)
    }
}

fn bundle_name(path: &Path) -> Result<()> {
    let ok = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".amx"))
        .is_some_and(|stem| {
            !stem.is_empty()
                && !stem.starts_with('.')
                && stem
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        });
    if ok {
        Ok(())
    } else {
        Err(invalid(format!(
            "{} is not a bundle name (<name>.amx)",
            path.display()
        )))
    }
}

fn unique<'a>(
    seen: &mut BTreeSet<(&'static str, &'a str, &'a str, u64)>,
    what: &'static str,
    namespace: &'a str,
    id: &'a str,
    revision: u64,
) -> Result<()> {
    if seen.insert((what, namespace, id, revision)) {
        Ok(())
    } else {
        Err(invalid(format!(
            "{what} {id:?} revision {revision} appears twice"
        )))
    }
}

fn verify_checksums(text: &str, files: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    let mut lines = text.lines();
    if !text.ends_with('\n') {
        return Err(invalid("CHECKSUMS must end with a newline"));
    }
    for name in CHECKSUMMED {
        let line = lines
            .next()
            .ok_or_else(|| invalid(format!("CHECKSUMS does not list {name}")))?;
        let (hash, listed) = line
            .split_once("  ")
            .ok_or_else(|| invalid("CHECKSUMS lines are <sha256>  <file>"))?;
        if listed != name {
            return Err(invalid(format!(
                "CHECKSUMS lists {listed:?} where {name:?} belongs"
            )));
        }
        let bytes = files
            .get(name)
            .ok_or_else(|| invalid(format!("bundle is missing {name}")))?;
        if sha256_hex(bytes) != hash {
            return Err(Error::new(
                "checksum",
                format!("{name} does not match its checksum"),
            ));
        }
    }
    if lines.next().is_some() {
        return Err(invalid("CHECKSUMS lists files the bundle may not contain"));
    }
    Ok(())
}
