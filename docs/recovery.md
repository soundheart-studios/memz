# Recovery and deliberate relocation

Recovery is trusted same-user administration through the Rust SDK or CLI. It is
not exposed to model-callable MCP tools. A bundle contains **all session-private
content** in its selected namespaces. Keep it private; use ordinary scoped exports
when sharing readable content. The current item-view JSONL export is not a recovery
bundle and cannot be used as one.

## Create and restore

Build with `cargo build --release --locked -p memz`. Destination paths must be
absolute, absent, and have a private, owner-controlled parent. Symlink components,
hard-linked input files, unsafe permissions and existing destinations are rejected.

```sh
memz backup /absolute/private/backups/snapshot
# Or back up exactly one registered namespace:
memz backup /absolute/private/backups/project-snapshot n_0123456789abcdef0123456789abcdef

memz restore /absolute/private/backups/snapshot /absolute/private/new-memz sqlite
# Full-authoritative portable records can restore without copying SQLite schema:
memz restore /absolute/private/backups/snapshot /absolute/private/new-memz-jsonl jsonl
```

`MEMZ_HOME` selects the source; default is `~/.memz`. Restore does not open or create
the default store. It never merges into existing state, even an empty directory.
Old backups cannot overwrite current tombstones through an import. Restoring an
older backup into a separate fresh instance intentionally returns to that backup's
state: deletions made after the snapshot are not present. Do not use such a stale
instance as current state without reconciling those later deletions.

A versioned manifest lists catalog/namespace schema versions, SHA-256 checksums,
byte sizes and portable row counts. SQLite snapshots use SQLite's online backup API,
not a filesystem copy of a live database. The catalog authorization lock prevents
core writes during snapshot capture; rendering/hashing happens after release.
Each namespace is a SQLite-consistent snapshot; there is no distributed transaction
with external filesystem operations or uncoordinated direct SQLite writers.

Portable JSONL preserves catalog roots, principals, sessions and grants; source
and item IDs/revisions, scope, authorship and history; edge direction and evidence;
operation receipts and tombstones; quotas, jobs and migration records. Column order
and tagged SQLite value types belong to recovery format version 1. Restore accepts
only fixed known tables and bound data values, never SQL supplied by the bundle.
Checksums detect damage, not the identity or trustworthiness of the backup author.

Both restore paths construct the current schema from the shipped Rust/SQL code,
check foreign keys and SQLite integrity, and rebuild FTS. Vectors and provider
identity tables are not adopted; the explicitly configured provider rebuilds them
through normal bounded backfill. No assets are downloaded. Leased jobs return to
pending with their attempt counts retained. Lexical recall works immediately.
Unsupported format/schema versions, malformed records, mismatched catalog entries,
wrong hashes and unsafe paths fail before the destination is published.

Runtime sockets, client tokens, caches, existing exports and previous backups are
not part of a recovery bundle. Reissue trusted clients after restore:

```sh
# If project folders moved, explicitly rebind the stable namespace first:
MEMZ_HOME=/absolute/private/new-memz memz rebind NAMESPACE /new/project/root
MEMZ_HOME=/absolute/private/new-memz memz init /new/project/root PRINCIPAL SESSION
MEMZ_HOME=/absolute/private/new-memz memz serve
```

Use the returned client file when connecting an agent. `init` is a trusted grant
operation; choose the intended principal/session rather than copying an old token.

## Move state safely

Stop the daemon and all embedded SDK writers before relocation:

```sh
MEMZ_HOME=/absolute/private/old-memz memz relocate /absolute/private/new-memz
```

The daemon ownership lock is held for the operation, refusing a running daemon and
preventing another daemon from starting during the copy. Recovery checks finish
before the new directory is atomically published. The original store remains
intact for rollback. Explicitly switch `MEMZ_HOME`, reissue clients, verify recall
and then decide when to remove the original. Do not run both copies as independent
current stores: they do not synchronize. Relocation does not change project-root
mappings; use `rebind` explicitly if the project itself moved. Arbitrary direct
SQLite writers and already-open embedded SDK handles must be quiesced by the host.

## Failures, limits and retained copies

Recovery uses private sibling staging directories named `.memz-recovery-*` and an
atomic no-replace directory rename on macOS/Linux. Ordinary failures/cancellation
remove staging. A killed process may leave a private unpublished staging directory;
it is never automatically adopted. After verifying no recovery operation is running,
an operator may remove that exact stale directory. Do not treat it as a backup.
A failure after atomic publication can leave a complete destination even if the
caller did not receive success; inspect/verify it before deciding to retry elsewhere.

Work is bounded to 1,024 namespaces, two million portable rows, 1 MiB per record,
and a conservative 2 GiB aggregate I/O accounting budget (copies and checksum reads
count separately). The operation checks a 60-second budget between work units;
SQLite scans also have progress interruption. Blocking filesystem I/O/fsync and
user progress callbacks are not hard real-time bounded. Larger recoveries are
currently unsupported and fail explicitly; they are not silently truncated.
The SDK progress callback may cancel with an error at completed database stages.

There is no automatic backup schedule or pruning. Operators own retention and
removal of bundles, exports, interrupted staging and old relocated stores.
Forgetting hides live content immediately and later scrubs logical database rows;
it cannot erase prior backups, exports, freed pages, filesystem snapshots or external
copies. Secure physical erasure is not claimed. Preserve the backups needed for
recovery while applying your chosen retention policy to every external copy.
