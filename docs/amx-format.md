# The AMX format

**AMX (Agent Memory eXchange)** is an open format for the memory AI agents keep:
the facts, preferences, decisions and procedures they learn, the passages those
came from, how they relate, and what was deleted. It also defines the few
operations a memory store must offer to read and write it safely.

This guide explains what AMX is, why it exists, how it differs from the other
proposals, and why each design choice was made. For the normative text, read
the [specification](../spec/amx/SPEC.md). For using AMX with Memz (export,
validate, import), read [AMX in Memz](amx.md).

> **Status:** AMX 0.1 is a draft for comment. It is incubated in this repository
> and Memz is its first implementation. The plan is to move it to its own
> repository before proposing it publicly.

## Why AMX exists

Every agent tool now keeps memory, and every tool keeps it differently. Claude
Code has its memory files, Codex its own, and libraries like Mem0, Letta or
Cognee each have a store and an export of their own. The result:

- **Memory is locked in.** What one agent learned about a project cannot move to
  another agent, or survive a change of tools, without a hand-written converter.
- **Deletion doesn't travel.** When a user tells one tool to forget something,
  every copy elsewhere (a backup, a sync partner, last month's export) still has
  it, and the next sync brings it back.
- **Nobody says who said what.** Most formats store a sentence. They don't
  record whether the user stated it or a model guessed it, so a reader can't
  tell a decision from a hallucination.
- **Concurrent agents overwrite each other.** Two agents updating the same
  memory end up with whoever wrote last.

Telemetry had the same problem until OpenTelemetry fixed the records and the
wire format (OTLP) and left everything else to the vendors. AMX does the same
for memory: **it fixes the records and the operations, and leaves storage,
retrieval, ranking, embeddings and user interfaces to each implementation.**
That is where tools compete; it is not what they should disagree on.

## What a bundle looks like

An AMX export is a plain directory of JSON Lines files:

```text
project-notes.amx/
  manifest.json      spec version, producer, namespace and change cursor, counts
  records.jsonl      the memories, every revision
  sources.jsonl      the original passages they came from
  links.jsonl        typed relations between memories
  tombstones.jsonl   content-free markers for what was deleted
  CHECKSUMS          SHA-256 of every file above
  manifest.sig       optional Ed25519 signature over CHECKSUMS
```

One line of `records.jsonl`, wrapped here for reading:

```json
{
  "amx": "0.1",
  "id": "deploy-window",
  "revision": 2,
  "kind": "decision",
  "content": "Deploys happen on Tuesdays because Monday has the release freeze.",
  "scope": {"namespace": "my-project", "visibility": "project"},
  "provenance": {"origin": "user", "author": "dev", "explicit": true},
  "status": "active",
  "valid_from": "2026-11-03T09:12:44Z",
  "recorded_at": "2026-11-03T09:12:44Z"
}
```

A memory someone inferred instead of stated has to show its work:

```json
{
  "amx": "0.1",
  "id": "ci-owner",
  "revision": 1,
  "kind": "fact",
  "content": "The platform team owns the CI pipeline.",
  "scope": {"namespace": "my-project", "visibility": "project"},
  "provenance": {"origin": "extractor", "author": "summarizer",
                 "explicit": false, "method": "llm-summary", "method_version": "3"},
  "evidence": [{"source": "msg-118", "source_revision": 1,
                "span": [0, 61], "role": "supports"}],
  "status": "active",
  "valid_from": "2026-11-04T16:02:10Z",
  "recorded_at": "2026-11-04T16:02:10Z"
}
```

And a deletion carries no content at all, only the fact that something is gone:

```json
{
  "amx": "0.1",
  "id": "t-31",
  "target_type": "record",
  "target_id": "old-api-key-location",
  "scope": {"namespace": "my-project", "visibility": "project"},
  "deleted_at": "2026-11-05T11:40:00Z",
  "reason": "user_request",
  "actor": "dev"
}
```

Everything is UTF-8 text a person can open, diff and `grep`. For reading and
reviewing, the same records also render as one Markdown file per memory, with
the fields as front matter (see [Markdown is a view](#6-one-canonical-form-markdown-is-a-view)).

## The nine principles, and why

These are the lines AMX does not trade away for convenience or adoption. Every
rule in the specification follows from one of them.

### 1. Deletion is a protocol guarantee

**Rule:** a forgotten memory never comes back, not through a retry, an import,
a replica or an old bundle. Deletions travel as tombstones; readers apply every
tombstone before any record, and refuse any later record with a deleted
identity, whatever its revision.

**Why:** in a syncing world, deletion that only works locally is not deletion.
If tool A forgets something and tool B still has it, B's next export would
quietly restore it in A. That is a privacy bug, not a missing feature. Making it
a rule every reader must follow is the only way "forget" means the same thing
everywhere. Tombstones are content-free so that recording a deletion never
leaks what was deleted, and a tombstone may list what was `derived` from the
deleted item so dependents go with it.

### 2. Scope fails closed

**Rule:** every record names its scope as typed dimensions
(`namespace`, `visibility`, and optionally `session`, `repo`, `branch`, `user`).
A reader that meets a dimension it does not understand treats the record as
**not visible**. It never widens it to something broader.

**Why:** scope is where memory systems differ most, and getting it wrong leaks
one project's or person's memory into another. The safe failure is to hide a
record, never to show it. A plain "context id" string can't express that, which
is why scope is a structured object with a short list of known dimensions and
namespaced custom ones (`x.team`).

### 3. No anonymous memory

**Rule:** every record says who asserted it (`origin`: user, agent, extractor,
import or system, plus an `author`) and whether it was **stated or inferred**
(`explicit`). An inferred record must point to its evidence, down to the exact
byte span of the source passage, and name the method and version that derived
it.

**Why:** "the user decided X" and "a model guessed X from a chat log" are
different things, and an agent acting on memory needs to know which it has.
Evidence spans make every inference checkable, and they let a store drop an
inference the moment its evidence is deleted.

### 4. Inferred never silently overrides stated

**Rule:** an inferred record may not supersede, correct or invalidate an
explicit one, whether through a link, a higher revision or a shared `key`.
Importers report such a record as a conflict.

**Why:** automatic extraction is useful and often wrong. If it can overwrite
what a person said, the memory slowly drifts away from what the person actually
wants, and nobody notices. Machines may propose; only an explicit act replaces
an explicit memory.

### 5. Never overwrite

**Rule:** changes create revisions. A correction keeps the `id` and bumps the
`revision`; a different claim gets a new `id` and a `supersedes` pointer. Every
write names the revision it expects to replace, and a mismatch is a
**conflict**, never last-write-wins. Request ids make retries idempotent.

**Why:** several agents now write to the same memory at once. Last-write-wins
loses one of them silently. Expected revisions make the race visible, so the
writer can re-read and decide. Keeping every revision also gives a history: you
can see what was believed when.

### 6. One canonical form; Markdown is a view

**Rule:** JSONL is canonical. The Markdown profile (one `<id>.md` per memory,
fields as front matter) converts to and from it losslessly, but it is only a
view: a deleted, renamed or edited file is never read as a deletion or
correction. Tools can diff a Markdown tree against a store and report
differences; applying them goes through the operations.

**Why:** people want to read and review memory in an editor, and plenty of
tools already keep memory as Markdown files. But if deleting a file meant
"forget", a stray `git checkout` or a sync conflict could erase or resurrect
memories with no record of who did it. Keeping changes in the operations keeps
principles 1, 4 and 5 intact.

### 7. Signals, not truth

**Rule:** AMX carries what a reader needs to judge a memory (provenance,
evidence, validity times, `supersedes` and `contradicts` links). It never
decides which memory is correct.

**Why:** "which of two conflicting memories is true" depends on the product,
the user and the moment. A standard that tried to decide would be wrong for
someone and ignored by everyone. A standard that carries the signals lets every
implementation decide its own way, from the same facts.

### 8. Records, operations and conformance only

**Rule:** storage, retrieval, ranking, embeddings and extraction are out of
scope.

**Why:** that is where tools compete and improve. Embeddings in particular are
not portable: a vector from one model means nothing to another. AMX stays small
so it is cheap to adopt.

### 9. No secrets

**Rule:** records never contain secret values, encrypted or not. A `secret_ref`
record holds only a locator (for example a vault path), and any attribute
starting with `secret.value` makes a record invalid.

**Why:** memory gets exported, synced, backed up and pasted into prompts. A
secret in memory will eventually leak. Encryption doesn't help much: the key
has to live somewhere the memory can be read.

## How AMX differs from other proposals

Several groups are working on the same problem. As of November 2026:

| | AMX | [Portable Memory](https://github.com/MacPaw/portable-memory) (`.mem`) | [OMPI](https://ai-disclosures.org/ompi) | Cognee COGX | [LoreKit post](https://www.lorekit.io/blog/open-agent-memory-format) |
|---|---|---|---|---|---|
| What it is | Format, operations and conformance suite | File format | Initiative (with IBM and Mozilla) | One vendor's import/export | A call for a standard |
| Packaging | JSONL + manifest + checksums + optional signature | JSONL + manifest + checksums + optional signature | Not yet specified | Vendor-defined | Illustrative fields |
| Deletion | Tombstones, applied first, no resurrection: required | Tombstones that list derived items | Not yet | Vendor-defined | Named as a gap |
| Scope / visibility | Typed dimensions, fail closed: required | `contextID`; visibility proposed (RFC-0001) | Not yet | Vendor-defined | Address levels proposed |
| Stated vs inferred | Required, with evidence spans | Proposed (RFC-0003) | Not yet | Vendor-defined | Named |
| Concurrent writers | Expected revisions, conflicts, idempotent requests | Not specified | Not yet | No | Named as a gap |
| Live operations | Defined (remember, recall, correct, forget, change feed, import) | No | Planned for coding agents | No | Suggested |
| Conformance tests | L0–L2 vectors | No | Not yet | No | Suggested |
| Implementations | Rust reference crate; Memz | Python and Swift SDKs | None yet | Cognee | None |

The short version:

- **Portable Memory (`.mem`)** has settled the *packaging*, and AMX deliberately
  matches it: JSONL files, a manifest, per-file SHA-256 checksums, tombstones
  applied first and optional Ed25519 signatures. The parts that matter most for
  safety (who can see a memory, who said it, what happens when two agents write
  at once) are still open RFCs or unspecified there. AMX makes them normative.
- **OMPI** is the right venue for coding-agent memory, but its first spec is
  planned for 2027 and has no record model yet. AMX is meant as a concrete,
  implemented input to that work.
- **COGX** is a product's export format, controlled by one vendor. It is useful
  for leaving Cognee, not as a common language.
- **The LoreKit post** asks for exactly this: an OpenTelemetry-style split
  between data model, conventions, protocol and conformance. AMX is one answer,
  with an implementation behind it.

**Why not just adopt `.mem`?** We looked hard at it, and AMX reuses its
packaging so the two stay close. But `.mem` leaves the guarantees we consider
non-negotiable (fail-closed scope, typed provenance, revision conflicts, the
live contract) unsettled, and those are what make shared memory safe. AMX
specifies them as **MUST** rules and proves them with tests. If `.mem` or OMPI
adopt the same rules, AMX can become a profile of theirs instead of a separate
format.

## Design choices in detail

**Records and sources are separate.** A memory is a short claim; a source is
the passage it came from. Keeping them apart lets many memories cite one
passage by byte span, lets a source be deleted (taking unsupported inferences
with it), and keeps memory text short enough to put in a prompt.

**Links are typed and directed.** `supports`, `supersedes`, `contradicts`,
`applies_to`, `requires` and `mitigates`, between specific revisions, with their
own provenance and evidence. A link never grants access to a record the reader
could not otherwise see.

**`key` for things with one current answer.** "Which database do we use" should
have one answer, not a pile of contradictory ones. A newer explicit record with
the same `key` replaces older explicit ones for readers. In Memz this is the
`slot`.

**Time has three meanings.** `valid_from`/`valid_to` say when a memory was
true, `recorded_at` when the store learned it, and `event_at` when the
underlying event happened. `expires_at` is retention: readers must not return a
record after it.

**Unknown fields survive.** Extensions live in namespaced `attributes`
(`memz.slot`, `vcs.commit`), and readers must preserve fields and attributes
they don't understand when they re-export. That lets tools add data without
breaking each other. Scope is the one exception, because there unknown means
hidden.

**Bundles are strict.** Readers verify every checksum and count before using
anything, and reject bundles with extra files, subdirectories or symbolic
links. A bundle is a unit of trust: either all of it checks out or none of it
is used.

**Import is a plan before it is a write.** Importing compares the bundle with
the store and produces a plan: tombstones, adds, supersedes, no-ops, refused
items and conflicts. Every store applies the same rules because the planner is
shared, and a person can review the plan before anything changes. In Memz, a
model may preview an import, but only the person can apply one, from the CLI.

**Three conformance levels**, so adoption can start small:

| Level | Name | A tool at this level |
|---|---|---|
| L0 | Producer | writes valid bundles |
| L1 | Consumer | imports them safely: verified, tombstones first, no resurrection, fail-closed scope, conflicts reported, idempotent |
| L2 | Live | also exposes the operations: expected-revision conflicts, idempotent requests, the change feed |

Conformance is proven by language-neutral test vectors in
[`spec/amx/conformance/`](../spec/amx/conformance), not by claims. A deliberately
broken store fails them.

## What AMX leaves out, on purpose

- **Retrieval and ranking.** How a store finds the right memory is its own business.
- **Embeddings.** Not portable between models; each store re-embeds on import.
- **Extraction.** How memories are derived from conversations is up to each tool;
  AMX only requires that inferences say how and from what.
- **Transport.** The operations work over MCP, HTTP or in-process calls.
- **Deciding truth.** See principle 7.
- **Secure erasure of old copies.** A bundle already written is outside any
  store's reach. Later bundles carry the tombstone, and every conforming reader
  honors it.

## Where AMX stands

- **Specification:** [`spec/amx/SPEC.md`](../spec/amx/SPEC.md), version 0.1 draft,
  with JSON Schemas in [`spec/amx/schema/`](../spec/amx/schema).
- **Reference library:** the `amx` Rust crate in [`crates/amx`](../crates/amx):
  types, validation, bundles, the import planner, the operations contract with
  an in-memory reference store, the Markdown profile and optional signing.
- **First implementation:** Memz exports AMX bundles, previews imports and
  imports from the CLI, and passes all L0–L2 vectors. See [AMX in Memz](amx.md).
- **Next:** Python bindings over the same Rust core (in a separate repository,
  since Memz is Rust-only), a second independent implementation, moving the
  spec to its own repository, and offering it to the Portable Memory and OMPI
  efforts.
