# Local embeddings

Memz uses Rust Candle 0.11.0 CPU inference and statically linked sqlite-vec 0.1.9.
The supported model is BAAI/bge-small-en-v1.5. Supply an absolute directory with
`model.safetensors`, `tokenizer.json`, and `config.json` using `MEMZ_MODEL_DIR`
when starting `memz serve`. The operator provisions these files; Memz never
downloads weights, searches caches, or calls an inference server. Without a
configured, valid model, lexical memory remains available with declared semantic
degradation. Changing configuration requires restarting the daemon.

The [BAAI model card](https://huggingface.co/BAAI/bge-small-en-v1.5) specifies CLS
pooling, normalized embeddings, and a retrieval instruction for queries. Memz
uses these settings, 384 dimensions, and a maximum of 512 tokens. Longer inputs
are truncated for embedding only; their original durable text remains intact.

Each generation records model, tokenizer, and config SHA256s, provider and vector
backend revisions, model family, dimensions, pooling, normalization, cosine metric,
query/document prefixes, and token limit. Unknown processing identities are
rejected. Different artifact bytes create separate generations; no vector is
compared across them. The model directory is trusted operator configuration;
hashes identify loaded bytes, rather than certify a publisher signature.

Derived vectors live in the private namespace database under the home state
directory. Transactional source triggers remove corrected or deleted vectors.
The scalar cosine API is qualified against the pinned extension version.

One model instance is loaded asynchronously per daemon. One inference may run at
a time, with a 512 MiB reservation and bounded input/model artifacts. A caller
timeout does not release the permit while computation continues. This bounds
concurrency, not an operating-system RSS limit. Failed model loading is not
automatically retried. Rust 1.88 or newer is required by the dependency graph.

The ordinary test suite checks invalid configuration, dimensions, identity
isolation and lifecycle cleanup without model assets. To additionally run real
model qualification, set `MEMZ_TEST_MODEL` to the provisioned directory when
running `cargo test --locked -p memz --test embedding -- --ignored`. That test checks normalized
vectors, repeated inference, and paraphrase ranking against an unrelated passage.

The daemon checks for background enrichment every 250 ms, yielding when request
workers are active. Jobs use 30-second leases, at most three attempts, and backoff;
provider work runs outside transactions and publication rechecks authorization
and source state. Generation backfill adds at most 32 jobs per pass within the
10,000-job queue bound. At most eight generations are retained; further changes
fail explicitly until an administrator retires old derived data. Vector storage
also checks namespace record/byte allowances at publication.

Hybrid recall gives query embedding and semantic discovery a shared 350 ms
allowance. SQLite semantic scanning checks a progress deadline and compares at
most 4,096 scope-eligible candidates, returning at most 128 semantic candidates.
Scope and current evidence/state eligibility precede ranking. Scan truncation is
reported. Reciprocal rank fusion uses deterministic ties and keeps explicit
current state ahead of other candidates. Final serialized packet and MCP envelope
limits still apply. This is a semantic-work deadline, not a hard real-time bound
on OS scheduling or the existing lexical/SQLite busy-wait path.

There is one 64-entry query-vector cache per engine, keyed by exact input and
complete embedding identity. A timed-out computation may warm that cache when
it finishes, while retaining its permit. Hybrid result packets are not cached;
each read rechecks grants, evidence, expiry and current revisions. Unconfigured
providers retain the existing revision-aware lexical cache.

With `MEMZ_TEST_MODEL` supplied, `cargo test --locked -p memz --test hybrid --
--ignored` exercises real daemon restart, paraphrased recall and forgetting over
local sockets. Ordinary suites explicitly ignore these asset-dependent tests.
