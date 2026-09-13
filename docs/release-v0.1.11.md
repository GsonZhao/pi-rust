# rpi v0.1.11

`rpi v0.1.11` closes a broad set of native Pi compatibility gaps while
hardening provider streaming and extension startup behavior.

## Highlights

- Default provider and model selection now follows native Pi's precedence:
  saved defaults first, then known provider defaults, while preserving the
  declaration order of custom providers and their model arrays.
- OpenAI Responses streaming now handles sparse output indexes, reasoning and
  text phases, function/custom tool calls, terminal failures, incomplete
  responses, usage accounting, replay identifiers, malformed frames, and
  OpenAI-compatible gateway variants more reliably.
- Anthropic SSE decoding now streams incrementally, preserves split UTF-8,
  surfaces transport failures, and responds promptly to cancellation.
- Pi JavaScript/TypeScript packages are opt-in through
  `--enable-pi-packages`; their Node host starts lazily and supports concurrent
  requests, cancellation, runtime UI calls, and lifecycle hooks.
- The interactive TUI adds image attachments and clipboard paste, native
  keybinding configuration, model and thinking controls, settings panels,
  external-editor drafts, session navigation, and improved tool rendering.
- CLI parity adds `--list-models`, `--offline`, project trust controls,
  streamed JSON lifecycle events, and HTML/JSONL session export.

## Fixes

- Untrusted project package declarations are no longer re-read by the startup
  update checker; it uses the same trust-gated package set as the session.
- Rust 1.78 remains the supported minimum: source APIs and the locked
  dependency graph are both verified with the 1.78 toolchain.
- Timing-sensitive custom UI tests now wait for asynchronous Node protocol
  actions under parallel test load.
- The published `rpi-cli` crate now carries its embedded documentation inside
  the package, so the crates.io tarball builds independently of the workspace.

## Install

```bash
cargo install rpi-cli --version 0.1.11
```

For a local checkout:

```bash
cargo install --path crates/pi-cli --force
```
