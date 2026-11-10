# AMX: portable memory

AMX (Agent Memory eXchange) is an open record format and operations contract for
agent memory. It is drafted in this repository: the specification is
[`spec/amx/SPEC.md`](../spec/amx/SPEC.md), the JSON Schemas are in
[`spec/amx/schema/`](../spec/amx/schema), and the reference library is the `amx`
crate in [`crates/amx`](../crates/amx). Memz is the first store that speaks it.

AMX fixes the records (memories, source passages, links, tombstones) and the
operations (remember, recall, correct, forget, change feed, export, import). It
leaves storage, ranking and embeddings to each implementation. Every AMX reader
holds to the same lines:

- **Deletion is a protocol guarantee.** A forgotten memory never comes back through
  an import, retry or old bundle.
- **Scope fails closed.** A record with a scope Memz does not support is never
  imported as visible.
- **No anonymous memory.** Every record says who asserted it and whether it was
  stated or inferred. Inferred records carry evidence.
- **Inferred never silently overrides stated.**
- **Never overwrite.** Changes are revisions, and a stale revision is a conflict.

## Bundles

A bundle is a directory named `<name>.amx`:

```text
<name>.amx/
  manifest.json      spec version, generator, namespace and cursor, counts
  records.jsonl      memories, every revision
  sources.jsonl      the original passages
  links.jsonl        typed relations between memories
  tombstones.jsonl   content-free deletion markers
  CHECKSUMS          SHA-256 of every file above
  manifest.sig       optional Ed25519 signature
```

Readers verify every checksum, every record and every count before using
anything. A bundle with extra files, symbolic links or a wrong checksum is
rejected.

## Export

Every Memz export also writes an AMX bundle, next to the JSONL, Markdown and HTML
views:

```sh
# MCP / CLI call: export with a name
{"op":"export","name":"snapshot"}
# writes ~/.memz/exports/snapshot.jsonl, .md, .html and snapshot.amx/
```

The tool's response includes `amx_path`. Like the other views, the bundle holds
what the exporting session may see: project memories and that session's own.

| Memz | AMX |
|---|---|
| memory (`items` row) | record. `slot` is `key`; `explicit` is `provenance.explicit` |
| source passage | source. `ingested_at` is `recorded_at` |
| evidence span | evidence entry. An inferred memory's method is its provenance method |
| relationship (`edges` row) | link |
| forgotten memory | two tombstones, for the record and for its source. No text |
| namespace mutation sequence | manifest cursor |

Origins map one to one: `user` to `user`, `assistant` to `agent`, `tool` to
`system`, `import` to `import`.

Forgotten content never appears in a bundle, and expired content is left out.
Already-written bundles are copies outside Memz: forgetting a memory later
cannot reach them. A later export carries the tombstone instead, and every AMX
reader refuses to bring the memory back.

## Validate (dry run)

`validate_import` previews what importing a bundle would change and writes
nothing. Models may call it; it is read-only.

```sh
{"op":"validate_import","bundle":"snapshot"}
```

The bundle is looked up by name in the state's `exports/` folder, then
`imports/`. Drop a bundle from elsewhere into `~/.memz/imports/` to validate it.
The report counts what would happen:

| Field | Meaning |
|---|---|
| `tombstones` | Deletions Memz does not hold yet. Applied first |
| `adds` | Records, sources and links new to this project |
| `supersedes` | Next revisions of memories this project holds |
| `noops` | Revisions already held unchanged |
| `refused` | Items whose memory, source or endpoint was deleted. They stay deleted |
| `conflicts` | Items that cannot be applied safely, each with a reason |
| `clean` | `true` when there are no conflicts |

Conflicts include:

- a revision that forks or skips a memory's history;
- an inferred revision over a stated one;
- a scope Memz does not support (anything beyond project and session, or another session's private memory);
- content Memz could not give back unchanged on export (attributes, unknown fields, non-plain content types, custom kinds or relations).

Bundles are re-homed into the importing project, so an export from one project
can be validated against another.

## Import

Importing is trusted and **CLI-only**. It is never an MCP tool.

```sh
memz import --client CLIENT_FILE BUNDLE                # dry run, same report
memz import --client CLIENT_FILE BUNDLE --apply        # import
memz import --client CLIENT_FILE BUNDLE --apply --skip-conflicts
```

`BUNDLE` is a path to a `.amx` directory, or a bundle name in `exports/` or
`imports/`. The import runs as the client file's principal and session.

- The plan is made again inside the write transaction and applied atomically.
  Deletions go first, then sources, memories and links.
- A plan with conflicts writes nothing unless you pass `--skip-conflicts`. Then
  only the clean part is applied.
- Importing the same bundle twice writes nothing the second time.
- A tombstone for a memory this project never held is kept as a content-free
  marker, so later imports stay refused.
- Imported memories become searchable at once (lexical) and are queued for
  embedding like new memories.

## Conformance

The language-neutral vectors in
[`spec/amx/conformance/`](../spec/amx/conformance) define three levels:

| Level | What it proves |
|---|---|
| L0 Producer | Exports are valid bundles; forgotten content leaves only as tombstones |
| L1 Consumer | Imports verify, apply tombstones first, never resurrect, fail closed on scope, report conflicts, and are idempotent |
| L2 Live | Expected-revision conflicts, request-id idempotency, session visibility and the change feed |

Memz passes all three. It runs the vectors through `memz::interop::MemzStore`, an
adapter implementing the `amx::Store` contract over a real namespace
(`crates/memz/tests/amx_conformance.rs`). The `amx` crate's in-memory reference
store passes the same vectors, and a deliberately broken store fails them.

Known limits:

- **Change feed cursors.** The cursor is the namespace mutation sequence. A
  cursor the adapter returned earlier gives an exact diff, and the current cursor
  gives nothing. Any other cursor returns everything again. That is safe because
  consumers apply changes idempotently, but it is not minimal. A durable per-row
  change log would need a schema migration.
- **Unknown link deletions.** A tombstone for a link this project never held
  is not persisted: a link cannot exist without its endpoints.

## Using the `amx` crate

```rust
let bundle = amx::Bundle::read(Path::new("snapshot.amx"))?;   // verified
let plan = amx::plan_import(&bundle, &holdings, &amx::Support::new(&["project"], &[]));
let markdown = amx::to_markdown(&bundle.records[0])?;           // readable profile
```

Signing is behind the `signing` feature (`amx::sign_bundle`,
`amx::read_verified`). Python bindings are planned as a separate package over the
same Rust core.
