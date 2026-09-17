# rpi 0.1.17

This patch release restores native terminal scrollback alongside text selection
on macOS.

## Highlights

- Default interactive sessions to regular/main-screen mode on macOS so the
  terminal owns both text selection and conversation scrollback.
- Preserve fullscreen as the default on Windows and Linux.
- Keep `--tui-mode regular` and `--tui-mode fullscreen` as explicit overrides
  on every platform.
- Document the platform default in `--help` and add regression coverage for
  both platform branches.

## Verification

- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets --locked`
- `cargo test --workspace --locked`
- `task dry-run RELEASE_VERSION=0.1.17`

## Install

```bash
cargo install rpi-cli --version 0.1.17
```
