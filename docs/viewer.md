# Local memory viewer

The existing authorized `export` operation now produces HTML alongside Markdown
and JSONL. With a running daemon and a bound client:

```sh
printf '%s\n' '{"op":"export","name":"review"}' | memz call --client /path/to/client.json
```

The response includes `html_path`, `jsonl_path`, `markdown_path`, namespace and
snapshot revision. Open `html_path` in a browser. MCP callers use the same `export`
tool with `name`. Omitting `name` returns the snapshot strings instead of files.
Files are placed under `MEMZ_HOME/exports` (default `~/.memz/exports`), never under
the project checkout. Existing files are not overwritten. Names accept only
letters, digits, hyphens and underscores.

The page contains memories/history, directed relationships/conflicts, and stable
local source links. Current labels use the same eligibility rules as graph recall,
including revision invalidation and replacement of explicit preference slots.
Historical rows retain their stored status and are labeled history. Source text,
revision, scope, authorship and exact relationship evidence are visible; inferred
relations remain labeled inferred. Conflict annotations identify stored
`contradicts` relations and do not claim an independent semantic judgment.

Use the browser's Find command to search text. Section links navigate the page;
source links jump to the cited revision. HTML, Markdown and JSONL are deterministic
renderings of one authorized database snapshot. Rendering occurs after its read
transaction is released. JSONL's header includes the same relationship data and
partial-neighborhood flag; it remains a scoped item view, not the full recovery
format described in [recovery](recovery.md).

No JavaScript, external assets, network requests or model runtime are used. Source
markup is escaped as text; links are local generated anchors. A restrictive
Content Security Policy adds defense in depth. CSS supports narrow screens,
dark mode and printing.

The snapshot permits at most 10,000 item revisions and 8 MiB combined rendered
output through the SDK. The adapter uses a smaller 64 KiB combined bound. Exceeding
these limits fails explicitly. Relationship collection scans at most 512 candidates
and includes at most 128 valid edges with both endpoints in the export; hitting a
bound labels the neighborhood partial. Deleted or unauthorized evidence cannot
enter via a relationship. Counts do not disclose identities of omitted private data.

Publication stages files privately and rolls back newly linked files on ordinary
name collisions. The three files are not an atomic directory bundle across a
process crash; each complete file carries the same snapshot revision, and an
operator should remove incomplete sets before re-exporting under that name.
Saved exports do not update when live memory changes. Sharing the file shares all
visible contents, including the bound session's private memories. Forgetting cannot
recall an existing copy; apply your retention policy to exports and backups.
