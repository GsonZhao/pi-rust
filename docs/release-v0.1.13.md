# rpi 0.1.13

This release adds an isolated local extension development workflow for faster
debugging of project-local skills and native extensions.

## Highlights

- Add `rpi dev-local` and `rpi dev --local-only` to load only resources from
  the current project and its active development extension.
- Load project-local `.rpi/skills`, `.pi/skills`, configured project resource
  paths, and the active development extension's resources in local-only mode.
- Keep `/reload` consistent with the local-only resource scope.
- Add the corresponding extension authoring documentation and update the
  native extension ABI/plugin SDK changes included in this release.

## Verification

- `cargo check --workspace --all-targets --locked`
- `cargo test --workspace --locked`
- `cargo fmt --all -- --check`

## Install

```bash
cargo install rpi-cli --version 0.1.13
```
