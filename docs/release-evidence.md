# Release qualification

Status: **locally qualified release candidate**. Recovery, safe HTML viewing,
adversarial checks and measured performance/retrieval/answer evidence are complete.
The full release command passed in Planwright run
`60509a09-805b-4273-a481-bfb8a2dd3c1b`. Plan archival still requires the human-only
completion decision. Qualification documents observed limits rather than claiming
all provisional performance targets were met.

## Reproduce

Provision local model files explicitly as described in [providers](providers.md).
Use an absolute `MEMZ_TEST_MODEL` path for qualification; normal daemon setup uses
`MEMZ_MODEL_DIR`. Set a unique `PW_RUN_ID` for each run.

The [qualification protocol](qualification.md) generates raw performance and
retrieval observations. Answer comparison requires the selected answer provider's
exact responses, input hashes, model identity and equal budgets. Retained reports
live in `qualification/reports/`; Rust acceptance tests reject missing, stale or
inconsistent reports. General-domain accuracy is not inferred from this small
synthetic corpus.

After the empirical reports are accepted, run:

```sh
PW_RUN_ID=final-release MEMZ_TEST_MODEL=/absolute/local/model \
  cargo run --locked -p memz-qa -- release-validation \
  --manifest qualification/release.json --run-id-env PW_RUN_ID
```

The manifest requires formatting, warnings-as-errors lint, the full ordinary
suite, both normally ignored real-model integrations, debug/release builds and
independent empirical-report checks. It also requires the raw reports and setup,
provider, recovery and viewer documentation. Execution uses isolated fixture
databases and local sockets; it does not operate on the user's live memory store.
The full gate passed every listed check with no failed checks. Post-commit and
removal/territory workflow receipts are recorded against tasks 5.3a and 5.4 in
the Memz Planwright plan; those receipts, rather than a prose claim, govern closure.

Earlier negative checks rejected the absent answer comparison, and the release
contract test rejected an omitted required check. The pinned release workflow's
negative run `f2edeabb-ffdd-4e3e-8cef-d00cde8a66f4` likewise rejected the incomplete
report. None of those earlier red runs was represented as a passing release.

## Implemented and bounded

- Runtime state stays under the hidden home directory: one catalog and separate
  databases per stable namespace. See [setup](harnesses.md).
- SDK, CLI and stdio MCP share scoped core operations. Capture acknowledges only
  committed writes; corrections and forgetting invalidate obsolete derivatives.
- Local embeddings are optional and explicitly provisioned. No automatic model
  downloads or remote Memz inference occur. Cold/unavailable embeddings degrade
  explicitly to lexical retrieval.
- Graph expansion remains opt-in. Inferred relationships retain exact evidence;
  repeated model assertions do not become user-confirmed facts.
- [Recovery](recovery.md) restores validated SQLite or full-authoritative JSONL
  bundles into a fresh private state. Existing instances are never overwritten.
- [HTML views](viewer.md) are escaped static snapshots with no JavaScript or model
  runtime. Exports and backups can retain bytes after live-state forgetting;
  physical secure erasure is not claimed.
- The generic capture bridge needs explicit host integration. It does not silently
  intercept conversations or inject memory across projects.
- Workload results, observed degradation and supported scale must be read with
  [qualification](qualification.md). Provisional latency targets are not service
  guarantees; process-crash tests are not physical power-loss testing.

## Completed evidence

Recovery commit `8829e52856a1fa9a361e2a7dc31880c2e06b6cdd` and viewer commit
`c3db012014f09988d42545c52e6db018904d99a0` passed their per-task post-commit,
removal and territory checks. Qualification harness commits
`5e21bc7660baa3bc62fea5355b5200dd70bceafa` and
`550de7449a3b6bea704af7ba8bdf791af01578f3` passed post-commit run
`8ff37856-238c-429f-965e-68080c9cad61`, removal run
`176f75c0-8670-4cb0-bdfa-6044b2a7a972` and territory run
`2bc5382f-0023-401f-a556-728bbf45ec10`. The new verifier's negative control
`3dd91cee-8944-41c4-99fd-5ac9984b5000` rejected surviving, unproved criteria.

The final source also includes socket error preservation (`47be1cb`) and faithful
provider-default sampling validation (`1c28bd2`). Empirical run
`42b61e14-585c-46b1-a351-295cbf9c3e8b` passed with all 64 authorized Pi/ChatGPT
answers and the real local embedding workloads. The retained report fingerprint is
`da8f4558baab34414c69820431b79109b4f4e84a83f84a3a45e0ba8a808484b1`.

The fixed answer rubric matched 15/16 hybrid and graph responses versus 5/16
lexical/no-memory responses; [qualification](qualification.md) documents its false
positives and numeric-format false negative. Graph remains optional because no
benefit was measured. Large-scale semantic coverage is unqualified; concurrent
tail latency and million-record lock failures are explicit supported-envelope
limits. These results do not establish general-domain accuracy or strict latency
guarantees. Planwright's final plan-completion decision remains human-only.
