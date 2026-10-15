# Evidence-backed relationships

Relationships live in the existing namespace SQLite database. Supported directed
relations are `supports`, `supersedes`, `contradicts`, `applies_to`, `requires`, and
`mitigates`. Both endpoints name exact item revisions. Every edge also retains one
to four exact source-revision spans, origin, author, and derivation method/version.
Qualifiers are bounded string maps. A citation establishes provenance, not truth.

All endpoints and evidence must share a namespace and an identical visibility
boundary: project or the same private session. A graph link grants no access.
Correction, forgetting, expiry, or replacement of a current explicit slot makes
an old relationship ineligible immediately. Bounded maintenance invalidates stale
rows and scrubs qualifiers when an endpoint or supporting source is forgotten.
History is not automatically reattached to new endpoint/source revisions.

## Authority

The `relate` MCP/daemon operation creates an **inferred proposal**. Its
`relationship` argument is:

```json
{
  "from": {"id": "FROM_ITEM_ID", "revision": 1},
  "to": {"id": "TO_ITEM_ID", "revision": 1},
  "relation": "requires",
  "qualifiers": {"when": "before deployment"},
  "evidence": [{"source": "SOURCE_ID", "revision": 1, "start": 0, "end": 34}]
}
```

Use `inspect` to resolve the supporting source and original content. Span offsets
are UTF-8 byte offsets, end-exclusive; each span contains 1–2,048 bytes. There is no
model-callable explicit-confirmation operation. Unknown fields such as `explicit`
inside the relationship are rejected. Repeating a proposal is idempotent and
never promotes it to a user-confirmed fact.

A trusted host may call `graph::assert_explicit`. A human operator may use
`memz assert-relation --client FILE --user-confirmed`, supplying the relationship
object as one newline-terminated JSON line on stdin. That assertion requires
user-origin evidence authored by the bound principal. It is a separate assertion
with its own identity; it does not silently upgrade a model proposal. This is a
trusted same-user administrative path, not protection against an administrator
who can modify the private store.

## Optional extraction

Set `MEMZ_RELATION_EXTRACTOR=literal-v1` when starting the daemon to enable the
Rust literal extractor. The default is off; unknown provider names fail startup.
It needs no model, network endpoint, or downloaded assets. It recognizes only:

```text
[[32_HEX_ITEM_ID@REVISION]] requires [[32_HEX_ITEM_ID@REVISION]]
```

Any supported relation may replace `requires`. The parser preserves the direction
and exact annotation span. It does not infer natural-language meaning, infer
missing endpoints, or treat arbitrary prose as a structured assertion.

Selection is limited to user-origin `decision`, `procedure`, `task`, and `incident`
sources containing annotations, at most 8,192 UTF-8 bytes. The `extraction_policy`
tool reports this policy and whether it is enabled. A source may produce at most
16 relations; excess output fails the job instead of silently dropping relations.
All extracted edges are **inferred**, even when the source was user-authored.

Background scheduling adds at most 32 jobs per pass, within the existing 10,000-job
queue bound. Extraction jobs are separate from embedding jobs and use the existing
30-second leases, three-attempt limit and backoff. Parsing runs outside database
transactions; source, endpoint and authority checks run again at publication.
Busy foreground work defers extraction. Failed jobs preserve lexical source text.

Existing portable exports remain item views; full relationship recovery and the
static viewer belong to phase 5. No relationship-quality benchmark is claimed.

## Recall and correction preview

Graph expansion is off by default. Request it explicitly on `recall` or `inspect`
with `"graph":true,"relations":["requires"]`. The relation allowlist must be
nonempty and contain only supported names. Expansion starts from authorized
lexical/semantic results in the same snapshot. It follows at most two hops, adds
at most 64 nodes, scans at most 512 edge candidates and has a 50 ms traversal
budget, including SQLite progress interruption. These are work bounds, not a
whole-request latency guarantee. Scope and current revision checks apply to every
edge, endpoint and evidence span.

Related items carry a `via` path with exact evidence quotes and source revisions.
`direction:"reverse"` describes how traversal reached the item; the edge's stored
`from`, `to` and relation retain their original meaning. Reaching a depth boundary
conservatively reports `hop_limit`; it does not assert that further edges exist.
Other exhausted bounds appear in `graph.limits` with `partial:true`. Whole items
and their paths are omitted together to fit the requested packet and MCP envelope
budget; graph results never bypass the total item cap.

Use `inspect` with `"impact":true` before correction. It previews currently valid,
authorized edges and derived items depending on the target endpoint revision or
its supporting source revision. It does not mutate or reattach relationships.
The preview is bounded to 512 candidates per dependency type (edges/items),
64 returned dependencies overall and 50 ms, and
shares the complete response budget. Partial previews report their limits.
History requests cannot be combined with graph expansion or impact previews.
Graph/impact packets bypass the lexical packet cache and are rebuilt from current
state; correcting or forgetting supporting evidence invalidates old links on the
next read, before background cleanup.
