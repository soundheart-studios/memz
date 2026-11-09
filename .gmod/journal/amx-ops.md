# amx-ops: Define the AMX operations contract and conformance runner

- 2026-11-09T23:39:08.947Z check: red (the in-memory reference store passes every L0, L1 and L2 vector)
- 2026-11-09T23:43:26.616Z check: green
- 2026-11-09T23:43:32.690Z dev@soundheart.io: Forgetting a record also deletes the sources only it cited, each under its own source tombstone: otherwise the forgotten text survives in exports and the change feed. Vectors check this with an `absent` expectation over the exported/changed bytes. `<field>_include` expectations allow stores to add extra tombstones (such as these source ones) without failing exact-match checks.
