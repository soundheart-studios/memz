# memz-amx-mcp-bundle: Let the MCP validate_import tool accept a bundle name

- 2026-11-10T03:09:30.417Z dev@soundheart.io: memz-amx-validate added `bundle` to the validate_import schema and dispatcher but not to the tools/call argument allowlist, so MCP callers always got -32602. Its tests used dispatch directly. New test amx_mcp.rs drives tools/list and tools/call end to end: schema, remember, export, bundle dry run, and path refusal.
- 2026-11-10T03:10:03.228Z check: red (validate_import over MCP tools/call with {bundle} returns a dry-run report and still refuses path-like names)
- 2026-11-10T03:10:22.429Z check: green
