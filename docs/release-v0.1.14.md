# rpi 0.1.14

This release improves provider failure handling and continues the native Pi
compatibility work across package loading, updates, skills, and the terminal UI.

## Highlights

- Show retry progress in the TUI, including the current retry number, retry
  budget, and live backoff countdown.
- Retry transient provider failures up to 10 times per request by default while
  leaving authentication, parameter, quota, and billing failures terminal.
- Emit structured `retry_scheduled` events in JSON mode for integrations.
- Preserve provider and abort diagnostics in the TUI when a request ultimately
  fails.
- Parse standard YAML block sequences in skill frontmatter so skills using lists
  such as `triggers:` are loaded instead of being discarded.
- Keep the native and Pi package update commands separated and load project
  packages by default, with the existing opt-out controls preserved.
- Remove the duplicate `Working...` footer status while retaining the active
  status indicator.

## Verification

- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets --locked`
- `cargo test --workspace --locked`
- `task dry-run RELEASE_VERSION=0.1.14`

## Install

```bash
cargo install rpi-cli --version 0.1.14
```
