# rpi 0.1.20

This patch release fixes queued-message delivery when aborting long-running
tools and makes cancellation of blocking shell commands reliable on Windows.

## Highlights

- Preserve steering messages in the active transcript when a blocking tool is
  cancelled.
- Keep Abort-state input on the steering path so it is consumed during tool
  batch cleanup instead of waiting indefinitely for a future run.
- Wait for Windows process-tree termination and stop stdout/stderr readers when
  cancellation fires, preventing listener commands from hanging the TUI.

## Verification

- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets --locked`
- `cargo test --workspace --locked`
- `task dry-run RELEASE_VERSION=0.1.20`

## Install

```bash
cargo install rpi-cli --version 0.1.20
```
