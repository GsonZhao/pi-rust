# rpi 0.1.18

This patch release stabilizes regular/main-screen rendering on macOS.

## Highlights

- Redraw only the actual changed line range, so frequent status updates no
  longer rewrite the input editor and footer.
- Avoid all terminal output for unchanged frames, preventing background render
  ticks from pulling the native terminal viewport back to the bottom.
- Track the current terminal height and cursor position precisely across
  renders and resizes.
- Clear removed tail rows without adding artificial scrollback lines.

## Verification

- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets --locked`
- `cargo test --workspace --locked`
- `task dry-run RELEASE_VERSION=0.1.18`

## Install

```bash
cargo install rpi-cli --version 0.1.18
```
