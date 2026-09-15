# rpi 0.1.19

This patch release stabilizes the interactive TUI around multiline input,
autocomplete, and provider failures.

## Highlights

- Soft-wrap long editor drafts within the bordered input area.
- Preserve the correct row and column after multiline file or command
  autocomplete replaces text.
- Clear stale autocomplete suggestions when selectors close.
- Keep regular-mode 405, authentication, and network diagnostics visible.
- Sanitize provider error bodies so terminal control sequences and oversized
  responses cannot corrupt the layout.

## Verification

- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets --locked`
- `cargo test --workspace --locked`
- `task dry-run RELEASE_VERSION=0.1.19`

## Install

```bash
cargo install rpi-cli --version 0.1.19
```
