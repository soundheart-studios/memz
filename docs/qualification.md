# Qualification method and evidence

The Rust `memz-qa` executable separates tooling tests from empirical acceptance.
Passing a harness test is not a latency or answer-quality claim. Task 5.3 implements
and tests the runners; task 5.3a executes the fixed workloads and accepts their raw
reports; task 5.4 performs the final integrated release checks.

## Correctness coverage

The qualification gate combines the new concurrent export/correction/cache/worker
case with the existing executable suites:

| Invariant | Behavioral evidence |
|---|---|
| C1 atomic acknowledgement | capture crash tests, atomic source/item/evidence/FTS/operation/job assertions |
| C2 small-text path has no inference | capture dependency tripwire and real capture API tests |
| C3 scope before discovery and traversal | authorization, hybrid scoped discovery, graph edge checks |
| C4 derivatives/caches/exports cannot broaden scope | authorization, health/cache, viewer and graph tests |
| C5 exact revision provenance | relations, graph recall, viewer and recovery tests |
| C6 correction/deletion is authoritative | lifecycle, warmed-cache qualification and graph invalidation |
| C7 obsolete/revoked workers cannot publish | jobs and concurrent qualification tests |
| C8 age is not deletion | explicit expiry/retention tests |
| C9 resource bounds | graph/hybrid/capture/export/daemon/worker tests and measured load outcomes |
| C10 shared adapter semantics | SDK/CLI/MCP adapter parity tests |
| C11 observable degradation/partial/error | hybrid fallback, graph budgets, workload raw error samples |
| C12 incompatible schema/provider rejected | schema and embedding identity tests |

Full release validation runs every suite, including independently provisioned
real-model integration tests. Tests and workloads use private temporary roots
identified by `PW_RUN_ID`, never the actual home memory store.

## Performance protocol

```sh
PW_RUN_ID=release-performance MEMZ_TEST_MODEL=/absolute/local/model \
  cargo run --release --locked -p memz-qa -- qualify-performance \
  --manifest qualification/performance.json --run-id-env PW_RUN_ID
```

The versioned workload requests 10k, 100k and 1m authoritative item/source/evidence
rows. Corpus construction uses a bulk SQLite fixture transaction and is explicitly
reported as **seed time, not capture throughput**. Ninety percent of fixture rows
are session-hidden, exercising skewed authorized discovery. Actual timed writes
use the production capture API; concurrent samples traverse the daemon socket,
worker queue and durable commit path. These measurements are distinguished in the
report rather than combining SDK and daemon timings.

Each size retains raw latency/error samples, nearest-rank p50/p95/p99, database
size, cache hits/bytes, scoped queue health, process peak RSS/CPU time, and exact
configuration. Cold means a new connection or starting provider; the operating
system file cache is not forcibly cleared. Simultaneous exports use normal bounds
and may explicitly reject large snapshots. An explicitly supplied BGE model enables
real background enrichment and cold/warm hybrid probes. No synthetic vectors are
used for model performance claims. Report vector coverage/provider readiness rather
than assuming every large-corpus row was embedded.

Seed feasibility has a recorded 180-second bound per size. An unavailable size
retains its reason; it is not claimed supported. Provisional write/lexical/hybrid
targets remain targets until the empirical report records the supported envelope.
Raw unsuccessful requests stay in error counts and are not silently included as
successful latency samples. Resource counters are process-wide high-water marks;
they are not isolated per-scenario allocations.

## Retrieval and answer-quality protocol

```sh
PW_RUN_ID=release-quality MEMZ_TEST_MODEL=/absolute/local/model \
  cargo run --release --locked -p memz-qa -- qualify-quality \
  --manifest qualification/quality.json --run-id-env PW_RUN_ID
```

The fixed synthetic corpus and its held-out queries are in
`qualification/quality-corpus.json`. Cases cover decisions, preferences, incidents,
corrections, relationships, contradictions, retained content, forgotten/private
content and unanswerable questions. It is a small controlled fixture, not evidence
of general-domain accuracy. Do not tune corpus expectations after looking at model
results; retain corpus/version changes as new experiments.

Every query runs no-memory, lexical, hybrid and hybrid+graph with the same packet
and item bounds. All document/query embeddings come from explicitly provisioned
local BGE weights and record full provider identity. Query vectors are computed
before the hybrid/graph comparison to remove cold-query cache asymmetry; their
cost is recorded separately. The performance runner measures cold/warm latency.

Retrieval reports relevant, missed, irrelevant and stale evidence, plus abstention,
separately from answer behavior. Private or stale injection is a correctness failure.
Graph stays off by default unless a separate equal-budget comparison demonstrates
benefit. Source, workload and corpus fingerprints bind retained evidence to code.

The first run writes actual retrieval observations and exact answer prompts to
`reports/quality-retrieval.json` and `reports/quality-answer-inputs.json`. Without an
answer-model report, it explicitly exits with `answer_comparison_unavailable` after
saving those observations; this does not pass empirical acceptance. Answer prompts
ask the same model/configuration to answer from only the supplied memory, or UNKNOWN
when insufficient. Each method uses the same maximum output-token budget.

An explicitly selected answer provider must retain raw responses, model identity,
configuration, output-token usage and the SHA-256 of each exact input. Set
`MEMZ_ANSWER_REPORT` to its report and rerun the command to score it against the
retained retrieval snapshot. Changed source/corpus/configuration or embedding assets
reject reuse. Missing methods, duplicate cases, changed prompts or unequal budgets
are rejected. Answer scoring uses the corpus's case-insensitive required-substring
rubric, not an LLM judge; report that limitation alongside results. A rubric match
is not proof that every part of an answer is correct.

Independent report validation recomputes percentiles and evidence/answer scores
from raw records and rejects stale identities, absent model evidence or fabricated
summary values. It supplements actual execution logs; a self-authored JSON file is
not independent attestation that a measurement occurred.

## Results

> **Publication notes.** Before publication, two identifying fields in the retained
> reports were redacted: the operator's home directory prefix is shown as `~`, and
> the machine hostname in `uname` output as `build-host.local`. No measurement,
> sample, hash or score was changed; the validators do not read these fields. Commit
> identifiers below refer to the pre-publication development history, which was
> squashed into a single public commit. The reports' source fingerprint predates
> later source changes, so the two `memz-qa` empirical tests report them as stale
> until qualification is re-run on the current tree.

The retained performance and retrieval reports were executed in Planwright run
`7aba6628-871d-4438-86a8-bc73d3120c9c` (fixture run `8542b43687c0`) against
product commit `1c28bd26fede062db021ca2ef520f36851c6f4b3`. Source fingerprint
`da8f4558baab34414c69820431b79109b4f4e84a83f84a3a45e0ba8a808484b1`
also includes the independent empirical and release-contract tests. That initial
recorded run was red because answers were absent. The subsequent authorized Pi
comparison supplied all 64 answers; both retained reports now pass independent
Rust validation. The raw performance measurements remain from the named run.

Hardware: Apple M1 Max, 10 logical CPUs, 32 GiB RAM, macOS arm64, Rust 1.98.0,
optimized release build, bundled SQLite with WAL/FULL durability. Fixtures used
the local `/private/tmp` filesystem; OS caches were not cleared. Process peak RSS
was 466,419,712 bytes; user/system CPU time was 76.89/41.51 seconds. These are
process totals, not per-scenario allocations. Full model identities, configuration,
raw samples and resource counters are in [performance.json](../qualification/reports/performance.json).

Successful-request latency in milliseconds (p50 / p95 / p99):

| Records | SDK capture | SDK lexical/cache path | Concurrent capture | Concurrent recall |
|---:|---:|---:|---:|---:|
| 10,000 | 1.90 / 2.36 / 3.60 | 1.73 / 2.40 / 2.49 | 9.01 / 49.02 / 210.87 | 7.44 / 29.20 / 51.14 |
| 100,000 | 14.26 / 15.70 / 18.84 | 19.95 / 21.07 / 21.13 | 23.51 / 144.01 / 392.48 | 22.40 / 104.27 / 914.09 |
| 1,000,000 | 135.39 / 140.31 / 157.92 | 195.37 / 200.39 / 203.41 | 140.29 / 387.46 / 593.77 | 148.94 / 1377.25 / 1992.97 |

Each SDK column has 100 samples. Concurrent capture/recall attempted 28/72
requests per size. At one million, 5 captures and 10 recalls failed with
`storage: database is locked`; their times remain in raw error records and are
excluded from successful latency percentiles. The smaller two sizes had no failed
foreground requests. All acknowledged captures were present afterward. This is a
finite workload result, not a guarantee under arbitrary load.

The demonstrated low-p95 envelope is **10k records with this four-client
lexical workload**, but its 210.87 ms capture p99 exceeds the provisional 100 ms
target. It is not qualified for a strict p99 guarantee. The 100k single-writer SDK path meets the provisional capture
and lexical targets, but its concurrent p95/p99 does not. One-million concurrent
operation is not qualified for those targets. Database sizes were approximately
10.7 MB, 101.8 MB and 1.01 GB; bulk seed times were 0.15, 1.47 and 16.79 seconds.

Cold connection lexical calls took 1.08 / 7.70 / 73.95 ms respectively.
The SDK lexical/cache path interleaves each recall with a write, invalidating
cached snapshots; it does not demonstrate an unchanged-store warm-cache speedup.

All bounded adapter exports rejected these snapshots with `export_limit` because
they exceed the combined 64 KiB export budget (4 / 4 / 20 attempts). This exercises
concurrent export rejection; successful large exports are not claimed. Use the
separate bounded recovery API for full-authoritative backups.

**Large-corpus semantic throughput remains unqualified.** The provider loaded,
but only 3 / 1 / 1 vectors were present at the end of these short runs. Cold hybrid
calls degraded to lexical at all sizes. Of 100 warm probes, 3 at 10k reported busy degradation, none at 100k
reported degradation, and all 100 at one million reported a semantic deadline.
Their p95 times (13.51 / 64.98 / 357.30 ms) must not be interpreted as
successful fully embedded semantic search latency. No synthetic vectors were used
to conceal this coverage gap.

The separate fully embedded small-corpus retrieval experiment used 21 active
sources and 16 fixed queries. Hybrid and hybrid+graph each returned all 14 expected
evidence references, plus 114 irrelevant references and no stale/private evidence.
Full-question lexical matching returned no evidence (14 missed references).
No-memory also missed all 14. On the three unanswerable queries, lexical/no-memory
returned empty packets; hybrid/graph did not. Graph added no benefit under these
bounds and remains off by default. These are retrieval results, not answer scores;
see [quality.json](../qualification/reports/quality.json) for raw packets and prompts.

## Authorized Pi answer comparison

The user-approved comparison used Pi 0.85.1 with
`openai-codex/gpt-6.1-sol`, reasoning off, provider-default temperature and an
explicit `max_output_tokens: 128`. The endpoint rejected an explicit numeric
temperature in a preliminary capability probe; no numeric temperature is claimed.
Each of the 64 requests used a fresh, unsaved session with tools, skills and context
file discovery disabled, identical fixed system instructions, and the exact stored
synthetic prompt. Output usage ranged from 11 to 31 tokens. This external answer
experiment does not add a remote provider to Memz; all Memz embeddings stayed local.

[answers.json](../qualification/reports/answers.json) retains actual provider
payloads, assistant messages, usage and input hashes. The Rust scorer validated
all methods, budgets and input identities before producing the retained quality
report (`pi-answer-scoring-20261015`).

| Method | Fixed substring rubric matches | Relevant evidence | Irrelevant evidence | Stale evidence |
|---|---:|---:|---:|---:|
| No memory | 5 / 16 | 0 | 0 | 0 |
| Lexical | 5 / 16 | 0 | 0 | 0 |
| Hybrid | 15 / 16 | 14 | 114 | 0 |
| Hybrid + graph | 15 / 16 | 14 | 114 | 0 |

These are **rubric matches, not an independently established accuracy rate**.
The fixed substring rubric gives false credit to the two no-memory/lexical
answers about retention and an unresolved date because their UNKNOWN explanations
contain “no.” It also rejects the correct hybrid/graph Nimbus answer “5% ... 40
minutes” because its expected substrings are “five” and “forty.” The rubric and
held-out corpus were not tuned after observing those results. All four methods
answered UNKNOWN on the three deliberately unanswerable cases; the semantic
methods did so despite receiving irrelevant context.

Hybrid supplies useful facts in this small controlled fixture, but its large amount
of irrelevant retrieval remains a limitation. Graph adds no observed evidence or
answer benefit here, so it remains off by default. No general-domain accuracy or
large-corpus semantic-performance claim follows from this experiment. Final
integrated release checks and Planwright evidence are recorded in
[release evidence](release-evidence.md).
