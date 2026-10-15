# Memz

[![CI](https://github.com/soundheart-studios/memz/actions/workflows/ci.yml/badge.svg)](https://github.com/soundheart-studios/memz/actions/workflows/ci.yml)

**Local-first, durable memory for AI agents.** Memz is a Rust daemon, CLI and
MCP server that gives coding agents and other harnesses scoped project memory:
durable capture, lexical and local semantic recall, corrections that actually
replace stale facts, evidence-backed relationships, and readable exports. All
state stays on your machine, with no network service, telemetry or automatic
model downloads.

```text
 Claude Code / Codex / any MCP host            your terminal
            │  stdio MCP                             │  memz call / capture
            ▼                                        ▼
      memz mcp --client FILE  ──── local socket ────▶ memz serve (daemon)
                                                       │
                                ~/.memz/catalog.sqlite + one SQLite DB per project
                                (FTS5 lexical index, optional sqlite-vec embeddings)
```

> **Status: 0.1, a locally qualified release candidate.** It's fit for cautious personal
> use: one user, modest workloads, local embeddings enabled, regular backups.
> It is **not** yet proven for large semantic stores or heavy concurrency. See
> [what's measured](#what-has-been-measured) before relying on it.

## Why Memz

- **Your memory, your disk.** Everything lives under `~/.memz`, with one catalog
  and a separate database per project, and never inside a git checkout, so
  worktrees can't share or corrupt it.
- **Fast, inference-free capture.** A small write commits durably without calling
  a model; it is acknowledged only after the transaction commits. Embedding happens
  later in bounded background jobs.
- **Corrections are authoritative.** `correct` and `forget` invalidate
  derivatives, caches, vectors and graph edges immediately. Values that hold one
  current answer (`slot`) replace instead of piling up contradictions.
- **Scope before search.** Authorization is checked before discovery and graph
  traversal. A project client cannot see another project's memories, and
  session-private memories stay private to their session.
- **Provenance.** Every item keeps its exact source revision, author and history.
  Inferred relationships carry exact evidence quotes and are never silently
  promoted to user-confirmed facts.
- **Observable degradation.** When embeddings are cold, busy or missing, recall
  says so (`"degraded": ["semantic_unavailable"]`) and falls back to lexical
  results.

## Quick start

Runs on macOS (arm64, x86_64) and Linux (x86_64, arm64 ARMv8.2+); Windows runs it via [WSL2](#windows-wsl2). See [platforms](docs/platforms.md). Building needs Rust 1.88+; the repo pins 1.98.0 in `rust-toolchain.toml`. Performance qualification was done on macOS arm64. Tagged releases also publish prebuilt binaries.

```sh
git clone https://github.com/soundheart-studios/memz.git && cd memz
cargo build --release --locked -p memz
export PATH="$PWD/target/release:$PATH"

cd /path/to/your/project
memz init          # registers the project, prints a private client file
memz serve         # run the daemon (separate terminal; Ctrl-C to stop)
```

`memz init` with no arguments uses the enclosing git work tree (or the current
directory) as the project, your OS user as the principal, and a freshly generated
private session. It prints what it chose:

```json
{"client_file":"~/.memz/run/client-….json","namespace":"n_…","principal":"you","root":"/path/to/your/project","session":"s_…"}
```

The general form is `memz init [ROOT [PRINCIPAL [SESSION]]]`. Client files are
secrets (mode 0600), and the daemon reads them at startup, so **restart
`memz serve` after creating or removing a client**.

Try it from the shell, one JSON request per line:

```sh
CLIENT=~/.memz/run/client-….json
printf '%s\n' \
  '{"op":"remember","request_id":"decision:deploy-window","text":"Deploys happen on Tuesdays because Monday has the release freeze.","kind":"decision"}' \
  '{"op":"recall","query":"deploy window","budget":1500}' \
  | memz call --client "$CLIENT"
```

## Connect an agent

Memz is a stdio MCP server (protocol `2025-11-25`). The daemon must already be
running.

**Claude Code**

```sh
claude mcp add memz -- /abs/path/to/memz mcp --client /abs/path/to/client.json
```

**Codex** (`~/.codex/config.toml`)

```toml
[mcp_servers.memz]
command = "/abs/path/to/memz"
args = ["mcp", "--client", "/abs/path/to/client.json"]
```

Any other stdio MCP host works the same way. Set `MEMZ_HOME` in the server's
environment only if you use a non-default state directory.

### Windows (WSL2)

Windows runs Memz through [WSL2](https://learn.microsoft.com/windows/wsl/install)
using the Linux x86_64 build. Native Windows is not supported yet.

```powershell
wsl --install            # once, then open the Ubuntu shell
```

Inside WSL, build Memz as in the [quick start](#quick-start) or unpack the
`x86_64-unknown-linux-gnu` release archive. Then run `memz init` and `memz serve`
as usual. Two rules:

- **Keep state on the Linux filesystem.** The default `~/.memz` inside WSL is
  right. Don't point `MEMZ_HOME` at `/mnt/c/...`: Windows drives don't keep the
  Unix permissions Memz checks, so it will refuse that state directory.
- **Run the agent inside WSL too** (Claude Code or Codex installed in the WSL
  shell), using the same MCP setup as above. Projects can live in WSL (fastest)
  or under `/mnt/c/...`.

A Windows-native agent may be able to launch the server through `wsl.exe`, e.g.
command `wsl.exe` with args `-e /home/you/memz/target/release/memz mcp --client
/home/you/.memz/run/client-….json`. That route is untested; the daemon must
already be running inside WSL.

### Teach the agent when to use memory

Tools alone don't make good memory; the agent needs judgment about what is worth
keeping. This repo ships a memory policy skill for both Claude Code and Codex:

- [`.claude/skills/memz/SKILL.md`](.claude/skills/memz/SKILL.md) (also mirrored in `.agents/skills/memz/`)
- [`triggers.md`](.claude/skills/memz/triggers.md): signals that should make an
  agent *consider* memory, designed so a future hook can nudge the same way.

The policy in brief: **recall** before related work, keyword-style; **remember**
only facts that are durable, not derivable from the repo, actionable, and either
user-stated or verified; **correct** instead of duplicating; **never** store
secrets; and treat current code and the user's latest instruction as stronger than
memory. Copy the `memz` skill directory into your own project's `.claude/skills/`
or `.agents/skills/` to use it there.

## Operations

| Operation | Surface | What it does |
|---|---|---|
| `remember` | MCP, CLI, SDK | Durable capture. `kind`: `fact`, `preference`, `decision`, `procedure`, `task`, `incident`, `entity`. Optional `slot`, `visibility: "session"` (and `expires_at` via CLI/SDK). `request_id` makes retries idempotent. |
| `recall` | MCP, CLI, SDK | Bounded lexical + semantic retrieval within the bound project; `budget` 256–8000. Optional `graph` expansion. |
| `inspect` | MCP, CLI, SDK | Full item detail, `history`, or `impact` preview before a correction. |
| `correct` / `forget` | MCP, CLI, SDK | Revise or remove an item; requires `id` and `expected_revision`. |
| `relate` | MCP, CLI, SDK | Propose an inferred, evidence-backed relationship. |
| `export` | MCP, CLI, SDK | Deterministic JSONL / Markdown / static HTML snapshot. |
| `status` | MCP, CLI, SDK | Scoped counts, quotas, queue and checkpoint health. |
| `memz capture` | CLI | Line-oriented bridge for piping trusted harness events into session-private memory. |
| `grant` / `revoke` / `maintain` | CLI only | Trusted administration. Never expose these as model tools. |
| `backup` / `restore` / `relocate` / `rebind` | CLI only | Full-fidelity recovery and moving state. |

Budgets count serialized UTF-8 bytes conservatively. Nothing is auto-injected into
conversations, and cross-project injection is disabled in the agent adapter.

## Semantic recall (recommended)

Lexical recall works out of the box but matches terms. It is good for keyword queries
and poor for full natural-language questions. For semantic recall, provision
[BAAI/bge-small-en-v1.5](https://huggingface.co/BAAI/bge-small-en-v1.5) yourself
and point the daemon at it:

```sh
MEMZ_MODEL_DIR=/abs/path/with/model.safetensors+tokenizer.json+config.json memz serve
```

Inference runs locally on CPU (Candle) and vectors live in the project's SQLite
database (sqlite-vec). Memz never downloads weights. Existing memories are
embedded gradually in the background; until coverage is complete, recall combines
lexical results with whatever embeddings are ready. See
[providers](docs/providers.md).

## Backups, exports and deletion

- **Backups** (`memz backup DEST [NAMESPACE]`) are full-fidelity and include all
  session-private content in the selected namespaces. Restore always goes into a fresh
  directory and never merges. Then re-issue clients. See [recovery](docs/recovery.md).
- **Exports** are readable views of what the exporting client may see, including
  its own session-private memories. The HTML view has no JavaScript and no network
  access. Exports are not backups. See [viewer](docs/viewer.md).
- **Forgetting** stops live retrieval immediately and schedules logical cleanup.
  It does **not** guarantee secure erasure of disk bytes or of existing exports and
  backups, so don't store secrets in memory.

## What has been measured

All numbers come from one Apple M1 Max (release build, SQLite WAL/FULL). They are
measurements, not guarantees. Full method and raw data:
[qualification](docs/qualification.md).

**Retrieval** (small synthetic fixture: 21 sources, 16 held-out queries, local BGE):

| Method | Expected evidence found | Answer rubric matches |
|---|---:|---:|
| No memory | 0 / 14 | 5 / 16 |
| Lexical (full-question query) | 0 / 14 | 5 / 16 |
| Hybrid | 14 / 14 | 15 / 16 |
| Hybrid + graph | 14 / 14 | 15 / 16 |

Hybrid also returned many irrelevant items (114), and graph expansion showed no
added benefit, so it stays opt-in. The rubric is a fixed substring check with known
false positives and negatives. It is not a general accuracy rate.

**Latency** (successful requests, p50 / p99 ms):

| Records | Single-writer capture | Single-writer recall | 4-client capture | 4-client recall |
|---:|---:|---:|---:|---:|
| 10k | 1.9 / 3.6 | 1.7 / 2.5 | 9.0 / 211 | 7.4 / 51 |
| 100k | 14 / 19 | 20 / 21 | 24 / 392 | 22 / 914 |
| 1M | 135 / 158 | 195 / 203 | 140 / 594 | 149 / 1993 |

**Known limits**
- Concurrent use at 1M records is not qualified: the tested workload produced
  `database is locked` failures. Keeping a project in the tens of thousands of items
  is a reasonable starting point, not a proven capacity limit.
- Large-corpus semantic recall is unqualified. Fully embedded semantic recall was
  tested only on the small fixture.
- Once 10,000 enrichment jobs are pending, new writes return `busy`.
- The MCP adapter is stdio-only. The daemon does not start itself.
- Windows is supported through [WSL2](#windows-wsl2) only; see the [porting inventory](docs/platforms.md#windows-porting-inventory) for native support.
- MCP `remember` currently records writes as user-origin. The bundled skill asks
  agents to prefix memories with `User:` or `Verified:` until provenance is
  carried through.

## Documentation

| Guide | Contents |
|---|---|
| [Setup and harnesses](docs/harnesses.md) | Daemon, clients, CLI protocol, MCP, capture bridge, maintenance |
| [Local embeddings](docs/providers.md) | Model provisioning, identity, bounds, hybrid recall |
| [Relationships](docs/graph.md) | Evidence-backed edges, authority, graph recall, impact preview |
| [Recovery](docs/recovery.md) | Backup, restore, relocation, retained copies |
| [Viewer](docs/viewer.md) | Static HTML/Markdown/JSONL exports |
| [Platforms](docs/platforms.md) | Supported targets, CI, local cross-platform checks, Windows inventory |
| [Qualification](docs/qualification.md) | Measurement protocol and results |
| [Release evidence](docs/release-evidence.md) | What was validated, what remains bounded |
| [SPEC.md](SPEC.md) | Original design specification and contracts |

## Development

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
PW_RUN_ID=dev-$(date +%s) cargo test --workspace --all-features --locked
```

CI runs the same commands, plus `.github/scripts/smoke.sh` against the release
binary, on Linux x86_64/arm64 and macOS. See [platforms](docs/platforms.md) to
reproduce the Linux job locally in Docker. `PW_RUN_ID` is required. It isolates every test in a private temporary root, so
tests never touch your real `~/.memz`. Two real-model integration tests are ignored
by default; run them with `MEMZ_TEST_MODEL=/abs/model/dir` and `-- --ignored`.
The two `memz-qa` empirical tests validate the retained reports in
`qualification/reports/` against the current source fingerprint, so they fail after
any source change until qualification is re-run (see [qualification](docs/qualification.md)).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in this work by you, as defined in the Apache-2.0 license, shall be dual
licensed as above, without any additional terms or conditions.
