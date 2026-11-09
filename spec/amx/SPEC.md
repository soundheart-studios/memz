# AMX: Agent Memory eXchange

Version 0.1 (draft). Status: request for comments.

AMX is a record format and an operations contract for memory that AI agents
write and read: facts, preferences, decisions, procedures and the evidence
behind them. It plays the role for agent memory that OTLP plays for telemetry:
it fixes the records and the operations, and leaves storage, ranking,
embeddings, extraction and user interfaces to implementations.

The key words MUST, MUST NOT, SHOULD, SHOULD NOT and MAY are used as in
RFC 2119.

## 1. Principles

These are the lines AMX does not trade away for convenience or adoption. Every
later section is a consequence of one of them.

1. **Deletion is a protocol guarantee.** A forgotten memory never comes back:
   not through a retry, an import, a replica or an old bundle.
2. **Scope fails closed.** A reader that does not understand a record's scope
   treats the record as not visible. Nothing is global by default.
3. **No anonymous memory.** Every record says who asserted it and whether it was
   stated or inferred. Inferred records carry evidence.
4. **Inferred never silently overrides stated.** Automatic processes may
   propose; only an explicit act replaces an explicit memory.
5. **Never overwrite.** Changes create revisions. Writers name the revision they
   replace; a mismatch is a conflict, never last-write-wins.
6. **One canonical form.** JSONL is canonical. The Markdown profile is a view;
   deletion and correction happen only through operations.
7. **Signals, not truth.** AMX carries what a reader needs to judge a memory. It
   never decides which memory is correct.
8. **Records, operations and conformance only.** Storage, retrieval and ranking
   are out of scope.
9. **No secrets.** Records reference where a secret lives, never its value.

## 2. Records

An AMX stream is UTF-8 JSON Lines: one JSON object per line, each line ending
in `\n`. Four record types exist, each in its own file in a bundle (section 9):
memory records, sources, links and tombstones.

### 2.1 Common conventions

- `amx` (string, required): the spec version the record follows, `"0.1"`.
- Identifiers (`id`) are opaque strings of 1 to 128 bytes from
  `[A-Za-z0-9._:-]`. They are unique per record type within a namespace.
- Revisions are integers starting at 1.
- Timestamps are RFC 3339 UTC strings with second precision and a `Z` suffix:
  `"2026-11-09T23:20:03Z"`.
- `attributes` (object, optional): extension data. Keys are namespaced with a
  dot (`memz.slot`, `vcs.commit`). Values are any JSON. Readers MUST preserve
  attributes they do not understand when they re-export a record.
- Unknown top-level fields MUST be preserved on re-export and otherwise ignored,
  except inside `scope` (section 3).

### 2.2 Memory record (`records.jsonl`)

| Field | Type | Required | Meaning |
|---|---|---|---|
| `amx` | string | yes | Spec version |
| `id` | string | yes | Stable identity across revisions |
| `revision` | integer ≥ 1 | yes | Revision of this identity |
| `kind` | string | yes | `fact`, `preference`, `decision`, `procedure`, `task`, `incident`, `entity`, `secret_ref`, or a namespaced custom kind (`x.lesson`) |
| `content` | string | yes | The memory itself |
| `content_type` | string | no | `text/plain` (default) or `text/markdown` |
| `key` | string | no | Address within the scope; a newer explicit record with the same `key` replaces older explicit ones for readers |
| `scope` | object | yes | Section 3 |
| `provenance` | object | yes | Section 4 |
| `evidence` | array | conditional | Section 4.2; required when `provenance.explicit` is false |
| `status` | string | yes | `active`, `superseded` or `invalid` |
| `supersedes` | object | no | `{"id","revision"}` of a *different* record this one replaces |
| `valid_from` | timestamp | yes | When the memory became true |
| `valid_to` | timestamp | no | When it stopped being true |
| `recorded_at` | timestamp | yes | When the producing store learned this revision |
| `event_at` | timestamp | no | When the underlying event happened |
| `expires_at` | timestamp | no | Retention: readers MUST NOT return the record after this time |
| `attributes` | object | no | Extensions |

### 2.3 Source (`sources.jsonl`)

A source is the original passage a memory came from.

| Field | Type | Required | Meaning |
|---|---|---|---|
| `amx`, `id`, `revision` | | yes | As above |
| `origin` | string | yes | Section 4.1 |
| `author` | string | yes | Section 4.1 |
| `external_ref` | string | no | Where it came from (URL, message id) |
| `text` | string | yes | The passage |
| `content_hash` | string | yes | `sha256:` + lowercase hex of `text` in UTF-8 |
| `scope` | object | yes | Section 3 |
| `recorded_at` | timestamp | yes | Ingestion time |
| `event_at`, `expires_at` | timestamp | no | As above |
| `attributes` | object | no | Extensions |

### 2.4 Link (`links.jsonl`)

A typed, directed relation between two memory record revisions.

| Field | Type | Required | Meaning |
|---|---|---|---|
| `amx`, `id`, `revision` | | yes | As above |
| `from`, `to` | object | yes | `{"id","revision"}` of memory records |
| `relation` | string | yes | `supports`, `supersedes`, `contradicts`, `applies_to`, `requires`, `mitigates`, or namespaced custom |
| `qualifiers` | object | no | Relation details |
| `scope`, `provenance`, `evidence`, `status`, `valid_from`, `valid_to`, `recorded_at`, `attributes` | | as for records | |

Traversing a link backwards does not reverse its meaning. A link never grants
access to a record the reader could not otherwise see.

### 2.5 Tombstone (`tombstones.jsonl`)

| Field | Type | Required | Meaning |
|---|---|---|---|
| `amx` | string | yes | Spec version |
| `id` | string | yes | Tombstone identity |
| `target_type` | string | yes | `record`, `source` or `link` |
| `target_id` | string | yes | The deleted identity (all revisions) |
| `scope` | object | yes | The scope the target lived in |
| `deleted_at` | timestamp | yes | When deletion took effect |
| `reason` | string | yes | `user_request`, `retention`, `policy`, `correction` or `other` |
| `actor` | string | yes | Who deleted it (opaque) |
| `derived` | array of strings | no | Ids of records and links removed because they depended on the target |
| `attributes` | object | no | Extensions; MUST NOT contain removed content |

Tombstones are content-free: they MUST NOT carry `content`, `text` or any field
copied from the deleted target.

## 3. Scope

`scope` is an object of typed dimensions. Each key names a dimension and its
value is a string.

| Dimension | Required | Meaning |
|---|---|---|
| `namespace` | yes | The memory space (normally one project) |
| `visibility` | yes | `project`, `session` or `personal` |
| `session` | when `visibility` is `session` | The session the record is private to |
| `repo` | no | A repository within the namespace |
| `branch` | no | A branch within `repo`; requires `repo` |
| `user` | no | The person the memory is about or belongs to |

Custom dimensions are namespaced (`x.team`).

**Fail closed.** A reader that encounters a dimension it does not support MUST
treat the record as not visible: it does not return it, import it as visible,
or widen it to a broader scope. It MAY keep it opaque for re-export. A missing
required dimension makes the record invalid.

Cross-namespace access is an implementation's authorization decision. Records
read across namespaces MUST keep their original `scope` so consumers can show
where they came from.

## 4. Provenance

### 4.1 Who asserted it

`provenance` object:

| Field | Type | Required | Meaning |
|---|---|---|---|
| `origin` | string | yes | `user`, `agent`, `extractor`, `import` or `system` |
| `author` | string | yes | Who (opaque principal, agent name, extractor name) |
| `explicit` | boolean | yes | `true` if stated by the author; `false` if inferred |
| `method` | string | when `explicit` is false | The derivation method |
| `method_version` | string | when `explicit` is false | Its version |

A claimed author is a label, not a credential. Implementations decide what
principals they trust.

### 4.2 Evidence

`evidence` is an array of objects:

| Field | Type | Required | Meaning |
|---|---|---|---|
| `source` | string | yes | Source id in the same namespace |
| `source_revision` | integer | yes | Source revision |
| `span` | `[start, end]` | no | Byte offsets into the source `text`, `0 ≤ start ≤ end` |
| `role` | string | yes | `supports`, `contradicts` or `context` |

An inferred record or link (`explicit: false`) MUST have at least one evidence
entry. When a source is deleted, records whose only support was that source
MUST become invisible at once, before any physical cleanup.

### 4.3 Explicit beats inferred

An inferred record MUST NOT supersede, correct or invalidate an explicit one,
either through `supersedes`, a higher revision of the same id, or a shared
`key`. Importers report such a record as a conflict.

## 5. Revisions

- A **correction** of the same claim keeps `id` and increments `revision`. The
  previous revision becomes `superseded`.
- A **replacement** by a different claim is a new `id` starting at revision 1,
  with `supersedes` pointing to the old record.
- At most one revision of an `id` is `active`.
- Writers name the revision they expect to replace (`expected_revision`). If the
  current revision differs, the write fails with `conflict`. There is no
  last-write-wins.
- Identical text is not identity. Two records with the same `content` but
  different authors, scopes or sources are distinct.

## 6. Deletion

- Deleting an identity produces a tombstone and removes every revision of it.
- **Order.** Consumers MUST apply all tombstones in a bundle or change batch
  before any record, source or link in it.
- **No resurrection.** A consumer that holds a tombstone for an identity MUST
  refuse any later record, source or link with that identity, whatever its
  revision, from any bundle, retry or replica.
- Tombstones are kept at least as long as the consumer accepts imports or
  retries that could carry the deleted identity.
- Deletion is logical first: the content MUST be excluded from all reads at
  once. Physical removal from indexes and caches MAY follow. External copies
  (earlier exports) are outside the store's reach and implementations SHOULD
  say so.

## 7. Secrets

Records MUST NOT contain secret values, encrypted or not. A record of kind
`secret_ref` has empty `content` and an `attributes["secret.locator"]` string
naming where the secret lives (for example a vault path). Any attribute key
beginning with `secret.value` makes the record invalid.

## 8. Readable profile

The Markdown profile renders one memory record per file, `<id>.md`:

```markdown
---
amx: "0.1"
id: "note-1"
revision: 2
kind: "decision"
scope: {"namespace":"ns-1","visibility":"project"}
provenance: {"origin":"user","author":"dev","explicit":true}
status: "active"
valid_from: "2026-11-09T23:20:03Z"
recorded_at: "2026-11-09T23:20:03Z"
---
Use SQLite for every namespace.
```

- Front matter lines are `field: <JSON value>`, in canonical field order. JSON
  is a subset of YAML, so YAML tools can read them.
- The body after the closing `---` line is `content`, byte for byte.
- Conversion between the profile and canonical records is lossless.
- The profile is a view. A missing, renamed or edited file MUST NOT be read as a
  deletion or correction. Tools that compare a profile tree with a store report
  differences; applying them goes through the operations in section 9.

## 9. Bundles

A bundle is a directory named `<name>.amx`:

```text
<name>.amx/
  manifest.json
  records.jsonl
  sources.jsonl
  links.jsonl
  tombstones.jsonl
  CHECKSUMS
  manifest.sig        (optional, section 9.2)
```

All five data files are present, possibly empty. No other files, directories or
symbolic links are allowed.

### 9.1 Manifest and checksums

`manifest.json`:

| Field | Type | Required | Meaning |
|---|---|---|---|
| `amx` | string | yes | Spec version |
| `bundle_id` | string | yes | Identity of this bundle |
| `generator` | object | yes | `{"name","version"}` of the producer |
| `created_at` | timestamp | yes | |
| `namespaces` | array | yes | `[{"id","cursor"}]`: each namespace and the change cursor it was exported at |
| `counts` | object | yes | `{"records","sources","links","tombstones"}` |
| `capabilities` | array of strings | no | Optional features used (`signing`) |
| `omitted` | integer | no | Records left out by filters or authorization; never their identities |
| `attributes` | object | no | Extensions |

`CHECKSUMS` lists every other file except `manifest.sig`, sorted by name, one
per line: lowercase hex SHA-256, two spaces, file name. Readers MUST verify
every checksum and every count before using any record.

### 9.2 Signatures

Signing is optional. `manifest.sig` holds a detached Ed25519 signature: one
line, `ed25519:` + base64 public key + `:` + base64 signature over the bytes of
`CHECKSUMS`. Because `CHECKSUMS` covers the manifest and every data file, the
signature covers the bundle. A reader that requires signatures MUST reject an
unsigned or wrongly signed bundle.

### 9.3 Alignment with Portable Memory

AMX packaging follows the shape of MacPaw's Portable Memory (`.mem`): JSONL
data, a manifest, per-file SHA-256 checksums, deletions applied first and
optional Ed25519 signatures. AMX adds normative scope, provenance, revision
conflicts and the live operations contract.

## 10. Operations

The live contract a store exposes, over any transport (MCP, HTTP, in-process).

| Operation | Input | Result |
|---|---|---|
| `remember` | content, kind, scope, provenance, optional key, evidence, request id | `{id, revision}` |
| `recall` | query, scope, limit | Visible active records |
| `correct` | id, expected_revision, new content, provenance | `{id, revision}` or `conflict` |
| `forget` | id, reason | Tombstone |
| `changes` | cursor | `{cursor, records, sources, links, tombstones}` since the cursor, tombstones first |
| `export` | filters | A bundle |
| `plan_import` | bundle | An import plan (section 10.1); writes nothing |
| `apply_import` | bundle, plan | Applied plan or `conflict` |

Request ids make writes idempotent: the same request id with the same payload
returns the first result; with a different payload it is a `conflict`.

Errors are `conflict`, `not_found`, `invalid`, `denied` and `unavailable`.
`denied` MUST NOT reveal whether hidden content exists.

### 10.1 Import plan

Planning compares a bundle with the store and returns, in order:

1. `tombstones`: identities to delete.
2. `adds`: records, sources and links new to the store.
3. `supersedes`: higher revisions of identities the store holds, where the
   bundle's previous revision is the store's current one.
4. `noops`: identical revisions the store already holds.
5. `refused`: identities the store holds a tombstone for.
6. `conflicts`: everything else: a revision gap or fork, the same revision with
   different content, inferred over explicit, or a scope the store cannot
   represent.

Applying a plan twice is the same as applying it once.

## 11. Conformance

| Level | Name | Requirements |
|---|---|---|
| L0 | Producer | Writes valid bundles: schemas, checksums, required provenance and scope, content-free tombstones |
| L1 | Consumer | L0, plus import: verifies bundles, applies tombstones first, refuses resurrection, fails closed on scope, reports conflicts, idempotent re-import |
| L2 | Live | L1, plus the operations: expected-revision conflicts, request-id idempotency and the change feed |

Conformance is shown by passing the language-neutral vectors in
`spec/amx/conformance/`. Each vector is a JSON file with a `level`, a sequence
of `steps` (operations or imports) and the expected result of each.

## 12. Versioning

`amx` uses `major.minor`. Minor versions add optional fields and dimensions;
readers of an older minor version preserve what they do not understand (and fail
closed on unknown scope dimensions). A new major version may change meanings and
is rejected by readers that do not support it.

## Appendix A. Memz mapping

| Memz | AMX |
|---|---|
| `items` row | Memory record (`id`, `revision`, `kind`, `content`, `status`, `valid_from`, `valid_to`) |
| `items.explicit` | `provenance.explicit` |
| `items.slot` | `key` |
| `sources` row | Source; `origin`, `author` also fill the record's `provenance` |
| `sources.ingested_at` | `recorded_at` |
| `sources.retention`/`expires_at` | `expires_at` |
| `evidence` row | `evidence` entry (`span_start`/`span_end` to `span`; `method` to provenance) |
| `edges` row | Link |
| deletion tombstone in `operations` | Tombstone |
| namespace id, `visibility`, `session_id` | `scope.namespace`, `scope.visibility`, `scope.session` |
| `namespace_meta.mutation_seq` | Change cursor |

Memz supports the `project` and `session` visibilities. It treats `personal`,
`repo`, `branch`, `user` and custom dimensions as unsupported and fails closed.
