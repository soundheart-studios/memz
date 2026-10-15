# Rust local service and harness setup

Build with `cargo build --locked -p memz`. Run `memz init` from inside the project. It returns a private client-file path under `~/.memz/run` together with the chosen root, namespace, principal and session. The root defaults to the enclosing git work tree (otherwise the current directory), the principal to the effective OS user, and the session to a freshly generated ID. The general form is `memz init [ROOT [PRINCIPAL [SESSION]]] [--no-agents]`; explicit values override the defaults. Each `init` issues a new client with its own private session. Unless `--no-agents` is given, `init` also writes the project's agent configuration: a `memz` server in `.mcp.json`, pre-approved in `.claude/settings.local.json`, a managed `[mcp_servers.memz]` block in `.codex/config.toml`, and the memory skill under `.claude/skills/memz/` and `.agents/skills/memz/`. Untracked configuration files are added to `.git/info/exclude`, and the report lists every file touched.

`memz call`, `memz mcp` and `memz capture` start `memz serve` in the background when no daemon answers on the socket (new session, log in `run/serve.log`); the ownership lock keeps concurrent starts to one daemon. A running daemon admits client files issued after it started: an unknown token triggers at most one rescan of `run/` per second through the same ownership and permission checks as at startup. Stop the daemon with Ctrl-C or SIGTERM; the next start safely reclaims a stale socket after obtaining the ownership lock. No model downloads or network service are involved.

All state defaults to `~/.memz`; `MEMZ_HOME` selects another absolute private directory outside Git projects. Project paths only register stable namespace IDs. Never copy credentials into a repository.

Use `memz call --client /absolute/client-file.json` with one JSON request per stdin line:

```json
{"op":"remember","request_id":"stable-event-id","text":"The deployment window is Tuesday","kind":"decision"}
{"op":"recall","query":"deployment window","budget":2000}
```

The SDK is `memz::adapters::dispatch`, using a trusted `Client` binding, or the typed capture/retrieval/lifecycle modules. These are trusted host APIs. Only `remember`, `recall`, `inspect`, `correct`, and `forget` are model operations. `correct` and `forget` require `id` and `expected_revision`. `inspect` accepts `history`; `remember` accepts `visibility: "session"` for private session memory. Authors always come from the binding. Cross-project automatic injection is disabled in this adapter because no host disclosure UI is implemented.

For any MCP host supporting stdio, configure the absolute built Rust executable with arguments `mcp --client /absolute/client-file.json`. Set `MEMZ_HOME` in its environment only if using an override. The daemon must already be running. The adapter implements initialization, ping, tools/list and tools/call using protocol 2025-11-25. It writes only JSON-RPC frames to stdout. Tool failures use `isError`; unknown tools use a protocol error. See the official [stdio transport](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports) and [tool contract](https://modelcontextprotocol.io/specification/2025-11-25/server/tools).

Budgets count serialized UTF-8 bytes conservatively as tokens. SDK and CLI budgets include the full returned packet. MCP additionally measures the complete JSON-RPC response, including duplicated text/structured content and escaping, removing whole evidence items until it fits. It may therefore return fewer items than SDK with the same nominal budget. If even metadata cannot fit, it returns an explicit budget error. No tool auto-injects returned content into another conversation.

## Explicit capture bridge

The supported generic harness integration is a line-oriented event bridge: pipe trusted events into `memz capture --client /absolute/client-file.json`. Events are:

```json
{"role":"user","event_id":"stable-harness-event-id","text":"Use the green deployment target"}
```

The host must supply stable event IDs and bind the client file to the correct conversation. Captures are session-private. Assistant/tool events are excluded by default; `--include-generated` opts in and preserves their origin, with generated statements marked non-explicit. Unknown roles are rejected. This is an explicit bridge, not automatic interception of Codex/Claude conversations; no host-specific undocumented hooks are installed.

Trusted administration is CLI-only: `memz grant PRINCIPAL NAMESPACE [write]` or `memz revoke PRINCIPAL NAMESPACE`. Read-only grants omit `write`. These operations persist capability revisions and invalidate existing capabilities. Do not expose this CLI administration surface as a model tool. Client files are secrets (0600). The daemon loads them at startup and on demand for new tokens; remove a client file and restart the daemon to retire its token. Grants and session ownership still determine what the token can access.

## Exports, health and maintenance

`{"op":"export"}` returns deterministic JSONL/Markdown/HTML snapshots (64 KiB combined adapter limit). Add `"name":"my-snapshot"` to publish private `exports/my-snapshot.jsonl` and `.md`/`.html` files without overwriting existing files. The typed Rust export API supports cancellable snapshots up to 8 MiB and 10,000 records. Exports include authorized history and provenance; they are views, not complete recovery backups. Markdown is escaped, not executable.

`{"op":"validate_import","jsonl":"..."}` validates a current unedited authorized snapshot without applying it. Stale snapshots, altered manifests and deleted-content replay fail. For full-authoritative fresh-instance restore, use the separate [recovery bundle](recovery.md) commands.

`{"op":"status"}` returns sanitized scoped counts, mutation revision, quotas, WAL bytes and checkpoint pressure. This operation is also available over MCP. `{"op":"maintain"}` is available through the trusted CLI/SDK only: it processes a bounded expiry/cleanup batch and reports passive-checkpoint results. The daemon also runs 32-record maintenance batches every 30 seconds for its registered authorized clients, separately from capture. Expired records are excluded by reads immediately, even before maintenance. Durability is independent of cleanup scheduling.

Optional local semantic enrichment uses an explicitly supplied `MEMZ_MODEL_DIR`; see [providers](providers.md). Without a model, lexical recall remains available and jobs remain pending. The 10,000-pending-job admission bound returns `busy` before acknowledging another write. Workers revalidate source revisions and authorization before publishing. No model download occurs automatically. Graph expansion is opt-in; see [graph contracts](graph.md).
