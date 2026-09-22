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

## ABI negotiation (v3 → v2 → v1)

The native loader uses a symbol-based ABI handshake. It prefers
`rpi_plugin_register_v3` (ABI version 3), then `rpi_plugin_register_v2` (ABI
version 2), and only falls back to legacy `rpi_plugin_register` (ABI version 1)
when neither v3 nor v2 symbol is present. A chosen entrypoint is called once;
registration failure never triggers cross-version fallback.

ABI v3 adds a `PluginApiVt3Ext::declare` hook for declaring a numeric
`priority` and a supported `platforms` array (e.g. `["linux","windows","macos"]`).
The v3 `api` vtable is otherwise identical to v2. The frozen v1 action range is
`0..=15`; v2/v3 currently accept `0..=17` (`GetCliFlag = 16`, `UiDialog = 17`);
raw numeric ids are validated before host dispatch.

## Lifecycle events and veto (P1)

The host dispatches lifecycle events to extension event handlers. The event
space covers 36 tags (33 Pi `on()` categories + rpi-specific `BeforeTuiStart` +
`UiPromptStart`/`UiPromptEnd`), spanning session lifecycle, agent loop, tool
execution, and model selection.

`BeforeTuiStart` is a veto-capable rpi-specific hook: the first handler to return
`EVENT_HANDLER_ABORT` aborts startup before the TUI initializes, and the CLI
exits with code `3`. `SessionStart`/`SessionShutdown` vetoes are advisory —
headless mode logs a warning, shutdown ignores them. Dispatch is
`catch_unwind`-wrapped and timeout-bounded so a slow or panicking handler does
not hang the host.

## Event journal (P3)

Extension event-handler invocations can be recorded to an opt-in JSONL journal:

```bash
export RPI_EVENT_LOG=1
rpi            # every handler invocation is logged
```

The journal defaults to `<root>/logs/events.jsonl` (`<root>` is
`$RPI_CODING_AGENT_DIR`'s parent, else `~/.rpi`); `RPI_EVENT_LOG_PATH` overrides
the path. When disabled the logger is a no-op (zero disk writes, zero overhead).

Inspect it with:

```bash
rpi events path   # print the resolved log path
rpi events tail   # print + follow new entries (Ctrl+C to stop)
```

Each line is a JSON object with `ts_ms`, `event` (tag Debug form, e.g.
`BeforeTuiStart`), `plugin` (extension display name), `result`
(`Continue`/`Error`/`Abort`/`Timeout`/`Panic`/`JoinFailed`), `duration_ms`, and
optional `detail`. Use it to answer "why didn't my extension run?" and to audit
handler behavior after the fact.

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
