# amx-validate: Enforce the AMX non-negotiable record rules

- 2026-11-09T23:31:16.220Z check: red (each non-negotiable rule has a passing and a rejected case)
- 2026-11-09T23:32:10.401Z check: green
- 2026-11-09T23:32:14.974Z dev@soundheart.io: Source content_hash is checked for shape only here: verifying it against the text needs sha2, and this job may not touch Cargo.toml. amx-bundle (which owns Cargo.toml) verifies hashes when it reads a bundle.
