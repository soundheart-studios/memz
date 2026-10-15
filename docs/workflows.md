# Rust evidence runners

`memz-qa` accepts a checker name and one JSON object on stdin. It emits one
JSON result. Infrastructure errors exit 2 and never count as behavioral evidence.
Release/schema command failures exit 1; their successful reports exit 0.

- `verify-evidence`: committed task-prefixed history, declared territory,
  Cargo integration-test refs, and criterion-to-ref mappings. Run a committed
  green baseline in a disposable clone before mutating anything. Historical
  reversion is the default. Explicit `mode: "rust-function-bodies"` removes Rust
  function implementations while retaining API signatures/types, which allows
  new APIs to fail behaviorally instead of failing to compile. This mode reports
  its identity and is not equivalent to exhaustive mutation testing. Tests and
  Cargo build scaffolding remain present. Missing refs, zero executed tests,
  compilation failures, and unrecognized subprocess outcomes are errors.
- `territory-check`: compare all files in exact task-prefixed commits to declared
  paths. The report lists uncovered paths rather than silently ignoring them.
- `schema-contract`: read-only SQLite inspection compares each declared column's
  type, optional nullability/primary-key position, required index names and SQL
  constraint fragments, plus database and foreign-key integrity.
- `release-validation`: execute every explicit argv in a JSON manifest, require
  unique check IDs and nonempty existing evidence artifacts, report failed checks.
  Final workload/quality manifests are provided by the later qualification task.

The current Planwright pin is `verify_evidence@r4`, derived from the Rust r3
runner. It requires at least one behavioral failure and a failing mapped test
for every declared criterion. Unchanged regression commands may survive;
compilation failures and unavailable checks never count as proof. The frozen
definition is in `contracts/workflows/verify-evidence-r4.json`. Earlier completed
tasks retain their original r3 evidence. The original
`memz_verify_evidence@r1` is retained as planning provenance; product tasks cite
the canonical pin because the Planwright completion gate matches that name.
`memz_territory_check@r1`, `memz_schema_contract@r1` and
`memz_release_validation@r1` remain the other required pins.

Release CLI equivalent:

```sh
cargo run --locked -p memz-qa -- release-validation --manifest qualification/release.json --run-id-env PW_RUN_ID
```

No runner writes to the real Memz home store. Tests create disposable repositories
and databases. A runner passing does not establish memory-engine performance,
security, or retrieval quality; the corresponding product tests and actual
qualification experiments must supply that evidence.
