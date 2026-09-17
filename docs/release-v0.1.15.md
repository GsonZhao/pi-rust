# rpi 0.1.15

This release makes long-running LLM requests configurable and restores native
terminal text selection on macOS.

## Highlights

- Use a consistent 600-second default timeout for Anthropic Messages, OpenAI
  Chat Completions, and OpenAI Responses requests, including streamed bodies.
- Add `--timeout <seconds>` and `--timeout=<seconds>` CLI overrides with strict
  positive-integer validation.
- Disable terminal mouse tracking by default on macOS so users can select and
  copy TUI content with the terminal's native selection behavior.
- Preserve the existing mouse wheel behavior on Windows and Linux.

## Verification

- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets --locked`
- `cargo test --workspace --locked`
- `task dry-run RELEASE_VERSION=0.1.15`

## Install

```bash
cargo install rpi-cli --version 0.1.15
```
