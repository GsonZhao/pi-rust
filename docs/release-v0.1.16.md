# rpi 0.1.16

This patch release fixes incomplete response tails in the interactive TUI.

## Highlights

- Reconcile the live assistant component from the harness's authoritative final
  message when prompt completion wins the race with asynchronous event delivery.
- Prevent the TUI from detaching a streaming component while it still contains
  an earlier partial snapshot.
- Keep the finalized response text used by `/copy` consistent with the content
  rendered in the transcript.
- Add a regression test covering a partial response whose final tail arrives at
  the prompt-completion boundary.

## Verification

- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets --locked`
- `cargo test --workspace --locked`
- `task dry-run RELEASE_VERSION=0.1.16`

## Install

```bash
cargo install rpi-cli --version 0.1.16
```
