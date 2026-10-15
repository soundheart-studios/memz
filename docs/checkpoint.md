# Phase 4 checkpoint

This is the historical phase 4 checkpoint. Recovery and HTML viewing have since
landed; see [release evidence](release-evidence.md) for the current qualification
status and [qualification](qualification.md) for measured results.

The stopping point is task 4.2, before recovery and release qualification. The Rust core, CLI and stdio MCP adapter support scoped remember/recall/inspect/correct/forget, deterministic Markdown/JSONL views, sanitized health, durable enrichment jobs bounded hybrid recall and opt-in evidence-backed graph recall with correction-impact previews. Runtime state defaults to `~/.memz`: one catalog plus one database per stable namespace, outside repositories. [Setup and examples](harnesses.md) describe the executable and private client files; [local provider configuration](providers.md) describes explicit model provisioning.

The separate Planwright repairs were recorded in the human-selected “Plan-first workspace and autonomous agent coordination” plan: the conservative empty Rust harness exception and workflow-reference dispatch were committed and tested. Memz completion still requires real command tests, removal verification and pinned workflow evidence. Memz runtime and QA code are Rust; SQLite is bundled native code.

## Validation

Validated on macOS arm64 with Rust 1.98.0. Full checkpoint commands passed on 2026-10-14:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
PW_RUN_ID=phase4-final-fixed-20261014 cargo test --workspace --all-features --locked
cargo build --workspace --all-targets --all-features --locked
cargo build --release --locked -p memz
```

All 58 ordinary integration tests passed (47 product/harness tests and 11 QA-runner tests), including child-process crash tests, real Unix sockets and CLI/MCP subprocesses. Two asset-dependent tests are explicitly ignored by ordinary runs and separately exercised with `MEMZ_TEST_MODEL`: real model normalization/reuse/timeout and real daemon enrichment/restart/paraphrase/deletion. Both asset-dependent tests passed again after the phase 4 retrieval changes. Per-task red, post-commit green, removal and territory runs are recorded in Planwright. No benchmark or power-cut hardware result is claimed.

Three fresh Pi sessions made eight actual Memz MCP calls against an isolated synthetic store. A query asking who provides medical care and how to arrange a consultation returned the stored physician, clinic and codeword on its first call, with `current semantic match` and no degradation. A later session recalled corrected revision 2, forgot it and received empty recall with zero visible records. The release binary used local Candle embeddings; Pi used its existing OpenAI Codex authentication with an explicit model override. Pi defaults and the real user memory store were unchanged. Local evidence is in `reports/pi-semantic-memory.json` (ignored runtime artifact).

Three additional fresh Pi sessions made 18 actual Memz MCP calls for phase 4. Graph recall added a rollback procedure using an inferred `requires` edge with an exact evidence quote. Impact preview identified the supporting source dependency; correction removed the old edge from recall. Recreating the proposal against source revision 3 restored the graph result, and forgetting that evidence removed it again. This run used lexical roots with Memz embedding/extraction disabled, isolating graph behavior. Evidence is in `reports/pi-graph-memory.json` (ignored runtime artifact). A delayed-client regression also exposed and fixed inherited nonblocking accepted sockets on macOS; worker read timeouts remain bounded.

## Boundaries of this checkpoint

- Graph expansion remains opt-in pending phase 5 quality qualification. Optional local literal relation extraction is disabled by default; it parses revision-addressed annotations, not natural-language meaning. [Graph contracts and examples](graph.md) document authority, evidence and bounds (ADR 9).
- Static viewer and full backup/fresh-instance restore remain in phase 5.
- Local embeddings require explicit `MEMZ_MODEL_DIR`; lexical-only operation is the default. No downloads or remote embedding inference occur. Foreground capture never calls the model.
- MCP is stdio-only. Client credentials are loaded at daemon startup; restart after creating or retiring a client. Same-user processes with access to private state are trusted administrators.
- The generic NDJSON bridge is explicit capture, not automatic interception of a named chat application's conversations. Cross-project automatic injection remains disabled.
- Capture admits at most 64 KiB text, 10,000 pending jobs, and configured namespace quotas. Leased workers retry at most three times and revalidate before publication. Cold or failed providers leave lexical content available.
- The daemon has four request workers and sixteen queued sockets by default. SQLite busy waits are bounded. Recall returns at most 128 candidates, with partial-result flags; large-scale latency is not yet qualified.
- Lexical cache storage is bounded per worker, with authorization and mutation-revision checks on every hit and expiry-aware reuse. Query vectors use a bounded identity-compatible cache; hybrid packets are rebuilt against current state. Retention only removes explicitly temporary content; age/rank never expires durable memories.
- Forgetting hides content immediately and cleanup scrubs active logical rows/indexes. Existing exports, filesystem snapshots, freed SQLite pages and backups can retain old bytes; secure physical erasure is not claimed.
- Export/import validation is bounded and non-mutating. Current JSONL views are not a full-fidelity backup format, and Markdown edits are not memory writes.

No actual user `~/.memz` database was used by the tests. Fixtures use isolated `PW_RUN_ID` paths. Phase 5 is outside this checkpoint.
