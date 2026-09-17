# Extension Backends

> **Beta:** The Node backend and TUI skill-invocation rendering are experimental
> compatibility features. Use them for local evaluation and feedback only; do
> not rely on them for production workloads until this notice is removed.

RPI keeps extension orchestration independent from the extension implementation.
The CLI exposes a small capability contract in `pi-cli::extension_api` and
adapts each backend to it.

## Backends

- `native-rust` loads the existing Rust `cdylib` plugin ABI and exposes the
  complete native registry, event, provider, renderer, and runtime-action
  capabilities.
- `node` runs configured Pi JavaScript/TypeScript packages only when startup
  includes `--enable-pi-packages`. Startup performs a short, one-shot
  registration discovery pass. In the interactive TUI, the persistent Node
  process starts immediately before the first submitted prompt (or earlier for
  an invoked package command or tool); print/json execution starts it when a
  package capability is used. The Node host is kept in
  `crates/pi-cli/src/node_host.mjs` and embedded into the binary with
  `include_str!`, so installed binaries do not depend on a neighboring script
  file. `--no-extensions` remains a final kill switch.

Every backend reports an API version and explicit capabilities. Unsupported
capabilities should be reported as structured `unsupported_capability` errors;
they must not appear as JavaScript `undefined` failures.

The native loader uses a symbol-based ABI handshake. It prefers
`rpi_plugin_register_v2` with ABI version 2 and only looks for legacy
`rpi_plugin_register` with ABI version 1 when the v2 symbol is absent. A chosen
entrypoint is called once; registration failure never triggers cross-version
fallback. The frozen v1 action range is `0..=15`, while v2 currently accepts
`0..=16`; raw numeric ids are validated before host dispatch.

## Compatibility progression

The Node backend currently covers tools, commands, resources, model snapshots,
notifications, editor text access, and session snapshots. Session mutations
and event callbacks remain separate capabilities. Provider calls are available when a Rust provider is
installed: `modelRegistry.getProvider(id)` exposes `streamSimple()` (including
Pi-compatible `for await` events and `result()`) and `complete()`. The current
JSON-lines transport batches provider events before resolving the stream, so
extensions see the same contract without moving HTTP/authentication into Node.
Fullscreen `ui.custom` components use the same bridge: Node retains the
component and Rust owns terminal writes, focus, input, resize, and cleanup.

The host now has a bidirectional, multiplexed
`runtime_request`/`runtime_response` channel. Rust keeps a pending-request table
and dispatches replies by id, so a long-running package command does not block
unrelated tool or UI traffic. Rust can install a runtime handler, and a Node
extension can await `pi.runtimeRequest(action, args)`. Capabilities are only
advertised when their handler is actually installed, so unsupported provider/UI
features cannot be mistaken for working APIs.

Rust may send a `cancel_request` host event for an in-flight call. The Node host
maps it to the `AbortSignal` passed to Pi tools and command contexts. Package
code should stop promptly when that signal is aborted.

## Persistent packages and PTC

The multiplexed transport is shared infrastructure for two execution policies:

- Pi-compatible packages use a persistent Node process because registrations,
  event handlers, and package state live for the session.
- A future PTC executor should use an isolated worker or child process per run,
  expose only capability-checked host calls, and terminate the worker on
  completion, timeout, or cancellation.

PTC is therefore an execution policy, not a replacement for the Pi package
adapter. Both should speak the same request-id protocol and use the same Rust
capability handlers.

When adding a capability, prefer a small optional contract over expanding one
large trait. This keeps the native backend complete while allowing Node and
future backends to implement the contract incrementally.
