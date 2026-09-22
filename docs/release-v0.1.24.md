# rpi 0.1.24

This release adds **remote mode** — a headless server plus a zero-local-resource
terminal client — and rounds out the extension lifecycle with events, veto
support, and an optional JSONL event journal.

## Highlights

### Remote mode (`--server` / `--connect`)

- `rpi --mode rpc` is now a real JSONL command loop (the headless agent server)
  instead of a stub. It streams `AgentEvent`s and answers commands with
  `{"type":"response", …}`.
- `rpi --server [--port <n>] [--bind <ip>]` runs the agent headless over TCP and
  prints a token at startup.
- `rpi --connect <host:port> [--token <t>]` attaches a terminal client that holds
  **no local provider, tools, extensions, or session files**; all agent work
  happens on the server. The token may also come from `RPI_SERVER_TOKEN`.
- Token authentication is connection-level (`authenticate` before any other
  request; `-32001` on missing/invalid tokens). `--no-token` disables it.
- The wire types (`RemoteEvent` / `RemoteCommand` / `RemoteResponse`) are shared
  by the server and the client (`crates/pi-cli/src/remote/protocol.rs`), so the
  contract cannot drift between the two ends.

### Extension lifecycle

- Extension lifecycle events (P0–P5) with veto support: a handler can refuse
  startup by returning `EVENT_HANDLER_ABORT` (new `BeforeTuiStart` hook, exit
  code `3`).
- ABI v3 (`PluginApiVt3Ext.declare`) adds priority and platform declarations;
  the loader negotiates v3 → v2 → v1.
- Optional JSONL event journal (`RPI_EVENT_LOG=1`) with `rpi events tail` and
  `rpi events path`.

### Documentation

- New `docs/remote-mode.md` (protocol, token auth, architecture, limitations).
- `docs/lifescope.md` (lifecycle roadmap P0–P5).
- Updated `docs/user-guide.md`, `docs/native-pi-missing-features.md`, and the
  website package data.

## Verification

- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets --locked`
- `cargo test --workspace --locked`
- `task dry-run RELEASE_VERSION=0.1.24`

## Install

```bash
cargo install rpi-cli --version 0.1.24
```
