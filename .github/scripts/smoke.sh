#!/usr/bin/env bash
# End-to-end smoke test of a built memz binary in throwaway state.
#   smoke.sh /path/to/memz
# Set SMOKE_PEER=1 on Linux with passwordless sudo to also check that the
# daemon rejects a different OS user presenting a valid client token.
set -euo pipefail

BIN=$(cd "$(dirname "$1")" && pwd -P)/$(basename "$1")
# Physical paths: state roots reject symlinked components (macOS /tmp is one),
# and Unix socket paths must stay short.
TMP=$(cd /tmp && pwd -P)
WORK=$(mktemp -d "$TMP/mzs.XXXXXX")
export MEMZ_HOME="$WORK/home"
PID=
cleanup() {
  # The daemon is started by the first client command; its pid is in serve.log.
  PID=$(sed -nE 's/^memz ready \(pid ([0-9]+)\)$/\1/p' "$MEMZ_HOME/run/serve.log" 2>/dev/null | tail -1)
  if [ -n "$PID" ]; then kill "$PID" 2>/dev/null || true; fi
  rm -rf "$WORK"
}
trap cleanup EXIT
fail() { echo "smoke: FAIL: $*" >&2; exit 1; }

# 1. Bare init from a subdirectory registers the enclosing git work tree.
mkdir -p "$WORK/project/sub"
git -C "$WORK/project" init --quiet
INIT=$(cd "$WORK/project/sub" && "$BIN" init)
echo "init: $INIT"
case "$INIT" in *"\"root\":\"$WORK/project\""*) ;; *) fail "root is not the git work tree";; esac
case "$INIT" in *"\"principal\":\"$(id -un)\""*) ;; *) fail "principal is not the OS user";; esac
CLIENT=$(printf '%s' "$INIT" | sed -E 's/.*"client_file":"([^"]+)".*/\1/')
[ -f "$CLIENT" ] || fail "client file missing"
[ "$(stat -c %a "$CLIENT" 2>/dev/null || stat -f %Lp "$CLIENT")" = 600 ] || fail "client file is not 0600"

# 2. Init connected Claude Code and Codex in the project and installed the skill.
grep -q '"memz"' "$WORK/project/.mcp.json" || fail ".mcp.json lacks the memz server"
grep -q "$CLIENT" "$WORK/project/.mcp.json" || fail ".mcp.json does not use the new client"
grep -q '^\[mcp_servers.memz\]$' "$WORK/project/.codex/config.toml" || fail "Codex config lacks memz"
[ -f "$WORK/project/.claude/skills/memz/SKILL.md" ] || fail "Claude skill missing"
[ -f "$WORK/project/.agents/skills/memz/SKILL.md" ] || fail "Codex skill missing"
grep -qx '/.mcp.json' "$WORK/project/.git/info/exclude" || fail ".mcp.json not excluded from git"

# 3. No daemon is running: the first client command starts it.
SOCK="$MEMZ_HOME/run/memz.sock"
[ -S "$SOCK" ] && fail "daemon socket exists before any client command"
OUT=$(printf '%s\n' \
  '{"op":"remember","request_id":"smoke:1","text":"Smoke deploys happen on Tuesdays","kind":"decision"}' \
  '{"op":"recall","query":"smoke deploys","budget":1500}' \
  | "$BIN" call --client "$CLIENT")
case "$OUT" in *"Smoke deploys happen on Tuesdays"*) ;; *) echo "$OUT"; fail "recall did not return the memory";; esac

[ -S "$SOCK" ] || fail "daemon did not start"

# 4. MCP handshake, tool listing and a tool call.
MCP=$(printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"recall","arguments":{"query":"Tuesdays"}}}' \
  | "$BIN" mcp --client "$CLIENT")
case "$MCP" in *'"name":"remember"'*) ;; *) fail "MCP tools/list lacks remember";; esac
case "$MCP" in *"Smoke deploys happen on Tuesdays"*) ;; *) fail "MCP recall did not return the memory";; esac

# 5. Optional: another OS user holding a valid token is still denied.
if [ "${SMOKE_PEER:-0}" = 1 ]; then
  TOKEN=$(sed -E 's/.*"token":"([^"]+)".*/\1/' "$CLIENT")
  FRAME='{"version":1,"token":"'"$TOKEN"'","body":{"op":"status"}}'
  OWNER=$(printf '%s\n' "$FRAME" | nc -N -U "$SOCK")
  case "$OWNER" in *'"result"'*) ;; *) echo "$OWNER"; fail "owner raw frame was not served";; esac
  OTHER=$(printf '%s\n' "$FRAME" | sudo -n nc -N -U "$SOCK")
  case "$OTHER" in *'"code":"denied"'*) ;; *) echo "$OTHER"; fail "root with a stolen token was not denied";; esac
  echo "peer credentials: owner served, other user denied"
fi

echo "smoke: OK ($(uname -s), $(file -b "$BIN" | cut -d, -f1))"
