# memz-amx-export: Memz exports its memory as an .amx bundle

- 2026-11-09T23:49:56.096Z check: red (an export produces a bundle that amx verifies, with provenance, evidence, links and tombstones for remembered, corrected, related and forgotten memories)
- 2026-11-09T23:52:21.631Z check: red (an export produces a bundle that amx verifies, with provenance, evidence, links and tombstones for remembered, corrected, related and forgotten memories)
- 2026-11-09T23:52:36.585Z check: green
- 2026-11-09T23:52:42.522Z dev@soundheart.io: The bundle is built inside the existing snapshot transaction and carried on Export as #[serde(skip)] file contents, so publish() (already called by the MCP export tool) writes <name>.amx beside the .jsonl/.md/.html without touching adapters. created_at is the newest timestamp in the exported data and bundle_id is memz-<mutation_seq>, keeping snapshots deterministic as the existing export test requires. Forgotten memories become two content-free tombstones (record and source); origins map user->user, assistant->agent, import->import, tool->system.
