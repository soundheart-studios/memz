# Platforms

| Target | Status | CI | Release binary |
|---|---|---|---|
| macOS arm64 (`aarch64-apple-darwin`) | Supported; qualification hardware | test + smoke | yes |
| macOS x86_64 (`x86_64-apple-darwin`) | Supported | build + smoke under Rosetta | yes |
| Linux x86_64 (`x86_64-unknown-linux-gnu`) | Supported | test + smoke + peer check | yes (glibc 2.35+) |
| Linux arm64 (`aarch64-unknown-linux-gnu`) | Supported, ARMv8.2-A or newer | test + smoke + peer check | yes (glibc 2.35+) |
| Windows via WSL2 | Supported through the Linux x86_64 build | covered by Linux x86_64 | use the Linux archive |
| Windows (native) | Not supported | none | no |

Every supported target runs the same test suite, apart from the two retained-evidence
checks described in [qualification](qualification.md), plus
[`.github/scripts/smoke.sh`](../.github/scripts/smoke.sh). That script runs bare
`init` from a subdirectory, starts the daemon, and exercises CLI remember/recall and
the MCP handshake, tool listing and tool call. On Linux, `SMOKE_PEER=1` also checks that a
different OS user holding a valid client token is denied by the socket's peer
credentials.

## Releases

Pushing a tag that matches the workspace version (`v0.1.0` for `version = "0.1.0"`)
runs [`release.yml`](../.github/workflows/release.yml). It runs the full CI gate, checks
the tag against `Cargo.toml`, builds and smoke-tests the four targets above, and publishes
a GitHub release with one `.tar.gz` per target plus a combined `SHA256SUMS`. A tag
that doesn't match the version, or any failing gate, stops the release before
anything is published.

## Windows via WSL2

Memz runs unchanged inside WSL2, where it is the Linux build with Linux's peer-credential
and file-permission checks. Keep `MEMZ_HOME` on the WSL Linux filesystem (the default
`~/.memz`). DrvFs mounts such as `/mnt/c` do not preserve Unix ownership and modes by
default, so Memz rejects state there. The supported setup runs the agent inside WSL as
well. Launching `wsl.exe -e …/memz mcp --client …` from a Windows-native MCP host
should pass stdio through, but CI does not test it. See the
[README](../README.md#windows-wsl2) for setup.

## Linux arm64 needs ARMv8.2-A

Candle's `gemm-f16` dependency contains fp16 inline assembly. The generic
`aarch64-unknown-linux-gnu` target does not enable that extension, so
[`.cargo/config.toml`](../.cargo/config.toml) adds `target-feature=+fp16` for it.
macOS arm64 enables it by default. Supported CPUs include AWS Graviton2 and newer,
Ampere Altra, and Raspberry Pi 5. Raspberry Pi 4 and other ARMv8.0 cores are not supported.

## Local cross-platform checks

The CI Linux job can be reproduced in Docker from the repository root:

```sh
docker run --rm --platform linux/arm64 -v "$PWD:/src:ro" rust:1.98-bookworm bash -c '
  apt-get update -qq && apt-get install -y -qq sudo netcat-openbsd file >/dev/null
  useradd -m runner && echo "runner ALL=(ALL) NOPASSWD:ALL" >/etc/sudoers.d/runner
  cp -a /src /work && rm -rf /work/target && chown -R runner /work /usr/local/cargo
  su runner -c "export PATH=/usr/local/cargo/bin:\$PATH CARGO_PROFILE_DEV_DEBUG=line-tables-only PW_RUN_ID=local
    cd /work && cargo test --workspace --all-features --locked -- \
      --skip retained_performance_matches_source_and_raw_measurements \
      --skip retained_quality_matches_source_and_actual_answer_evidence &&
    cargo build --release --locked -p memz && SMOKE_PEER=1 .github/scripts/smoke.sh target/release/memz"'
```

Use `--platform linux/amd64` for x86_64; under emulation it is slower but works. On
an Apple Silicon Mac, `rustup target add x86_64-apple-darwin` and
`--target x86_64-apple-darwin` run the x86_64 tests under Rosetta. If a Homebrew
`rust` precedes `~/.cargo/bin` on `PATH`, `rust-toolchain.toml` is ignored and extra
targets are unavailable. Put rustup's directory first.

## Windows porting inventory

`cargo check --target x86_64-pc-windows-gnu` compiles every dependency, including
Candle, bundled SQLite and tokenizers, and fails only in Memz's own code: 55 errors
across `state`, `daemon`, `backup`, `adapters` and `export`. Unsupported platforms also
hit an explicit `compile_error!`, because Memz will not run without a peer-credential
check. A port has to replace security-relevant Unix primitives, not merely compile:

| Unix primitive | Purpose | Windows direction |
|---|---|---|
| Unix domain socket + `getpeereid`/`SO_PEERCRED` | daemon transport; same-user peer check | named pipe with a restrictive DACL and client SID verification |
| `geteuid`, mode `0600`/`0700`, `nlink`, `O_NOFOLLOW` | private, unaliased state and client files | owner/ACL checks, reparse-point refusal |
| `flock` | daemon ownership lock | `LockFileEx` |
| `renamex_np` / `renameat2(RENAME_NOREPLACE)` | atomic no-replace recovery publish | `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` |
| `gmtime_r` | export timestamps | `gmtime_s` or a pure-Rust conversion |
