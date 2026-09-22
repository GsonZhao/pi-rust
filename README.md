# rpi (pi-rust) — Rust-native coding-agent runtime

[![rpi-cli on crates.io](https://img.shields.io/crates/v/rpi-cli.svg)](https://crates.io/crates/rpi-cli)
[![rpi-plugin-sdk docs](https://docs.rs/rpi-plugin-sdk/badge.svg)](https://docs.rs/rpi-plugin-sdk)
[![CI](https://github.com/bigfish1913/pi-rust/actions/workflows/ci.yml/badge.svg)](https://github.com/bigfish1913/pi-rust/actions)
[![GitHub stars](https://img.shields.io/github/stars/bigfish1913/pi-rust?style=flat)](https://github.com/bigfish1913/pi-rust/stargazers)
[![Latest release](https://img.shields.io/github/v/release/bigfish1913/pi-rust)](https://github.com/bigfish1913/pi-rust/releases/latest)

`rpi` is a Rust-native, library-first coding-agent runtime and terminal CLI.
It is a multi-crate Rust implementation of the
[earendil-works/pi](https://github.com/earendil-works/pi) SDK layer for building
composable LLM agents with providers, tools, sessions, and plugins.

Repository: `bigfish1913/pi-rust` · Website: [https://rpi.laofu.online/](https://rpi.laofu.online/)

The project is useful both as a Rust Agent SDK and as a ready-to-run terminal
coding agent. Core crates can be embedded independently; the `rpi` CLI provides
the fastest way to try the complete loop.

> **Naming.** The published crates use the `rpi-` prefix (the upstream `pi-*`
> names are owned on crates.io by a parallel port). The on-disk directories stay
> `crates/pi-*` for history; the `package.name` in each `Cargo.toml` is
> `rpi-*`, so `extern crate` / `use` paths are `rpi_ai`, `rpi_agent`, etc.

## Crates (published as `rpi-*`)

| Crate (crates.io)  | On-disk dir         | What it is                                                                            |
| ------------------ | ------------------- | ------------------------------------------------------------------------------------- |
| `rpi-telemetry`  | `pi-telemetry/`   | Telemetry span/event contracts (noop default).                                        |
| `rpi-ai`         | `pi-ai/`          | Unified multi-provider LLM types + streaming (Anthropic + faux).                      |
| `rpi-agent`      | `pi-agent/`       | Agent runtime + loop,`AgentTool` trait, events, hooks, queues.                      |
| `rpi-tools`      | `pi-tools/`       | Pi-compatible coding tools (`read`/`write`/`edit`/`bash`) + `ExecutionEnv`. |
| `rpi-harness`    | `pi-harness/`     | `AgentHarness`: session tree, JSONL persistence, compaction, run loop.              |
| `rpi-cli`        | `pi-cli/`         | Terminal coding-agent CLI (`rpi` binary) on top of the library crates.              |
| `rpi-plugin-sdk` | `rpi-plugin-sdk/` | Stable C ABI for Rust-native plugins and extension discovery.                         |
| `rpi-extensions` | `rpi-extensions/` | Dynamic plugin loader and`AgentTool` adapter.                                       |
| `rpi-tui`        | `pi-tui/`         | Terminal UI primitives used by the interactive CLI.                                   |

Dependency direction: `rpi-telemetry → rpi-ai → rpi-agent → rpi-tools → rpi-harness → rpi-cli`.

### Rust registry links

| Package            | crates.io                                           | docs.rs                                  |
| ------------------ | --------------------------------------------------- | ---------------------------------------- |
| `rpi-cli`        | [crates.io](https://crates.io/crates/rpi-cli)        | [docs.rs](https://docs.rs/rpi-cli)        |
| `rpi-agent`      | [crates.io](https://crates.io/crates/rpi-agent)      | [docs.rs](https://docs.rs/rpi-agent)      |
| `rpi-plugin-sdk` | [crates.io](https://crates.io/crates/rpi-plugin-sdk) | [docs.rs](https://docs.rs/rpi-plugin-sdk) |
| `rpi-extensions` | [crates.io](https://crates.io/crates/rpi-extensions) | [docs.rs](https://docs.rs/rpi-extensions) |

The registry pages are the canonical entry points for installing the CLI or
embedding the SDK. The repository may contain unreleased changes; check the
published version shown on crates.io before depending on a new API.

### Extension package repository

Ready-to-install Rust-native extensions are maintained in the companion
[`pi-rust/rpi-package`](https://github.com/pi-rust/rpi-package) repository.
Browse its [`packages/`](https://github.com/pi-rust/rpi-package/tree/master/packages)
directory for package source, usage documentation, and release metadata, or use
the [online package catalog](https://rpi.laofu.online/packages.html).

## Relationship to the TypeScript source

The TypeScript reference is checked out under `.reference/pi/` (read-only). Every
Rust module names the TS file it mirrors in its module-level doc comment. The
crate family is a Rust-native reimplementation, not a thin wrapper — it ports the
SDK surface (`pi-ai`, `pi-agent-core`, the harness tools, the session layer) and
the CLI, keeping the layering and behavior faithful while using idiomatic Rust
(`async`/`await`, `Arc`, `serde`, `tokio`).

## How you build an agent

```rust
use rpi_agent::{AgentBuilder, AgentEvent};
use rpi_ai::providers::faux::{FauxProvider, FauxScript};

let provider = std::sync::Arc::new(FauxProvider::new(FauxScript::new().with_text("Hello!")));
let model = provider.default_model().clone();
let agent = AgentBuilder::new()
    .model(model)
    .system_prompt("You are a helpful assistant.")
    .tools(vec![/* MyTool */])
    .stream_fn(make_stream_fn(provider))
    .build()
    .unwrap();

let mut events = agent.subscribe();
tokio::spawn(async move {
    while let Some(ev) = events.recv().await {
        match ev { /* AgentEvent::MessageUpdate { .. }, etc. */ }
    }
});

agent.prompt("Hello!").await.unwrap();
```

See [docs/architecture.md](docs/architecture.md) for the full design and
[docs/agent-project.md](docs/agent-project.md) for the recommended project
structure when you build your own agent.

## Build a plugin

Plugins are Rust `cdylib` libraries loaded through the stable ABI exposed by
`rpi-plugin-sdk`. The repository includes a complete `echo` tool example that
also exercises event and resource discovery:

```bash
cargo build -p plugin-stub
```

Then point the CLI at the directory containing the generated library (the
extension is named `plugin_stub.dll`, `libplugin_stub.so`, or
`libplugin_stub.dylib` depending on the platform):

```bash
rpi --extensions-dir target/debug -p 'echo "hi"'
```

The plugin depends on `rpi-plugin-sdk` only; the host-side loader lives in
`rpi-extensions`. See [`examples/plugin-stub`](examples/plugin-stub) and the
[`rpi-plugin-sdk` API docs](https://docs.rs/rpi-plugin-sdk) for the ABI
contract.

### Install a crates.io extension

`rpi install` installs Rust-native extensions directly from Cargo. The package
must expose an `rpi-plugin-sdk` compatible `cdylib` target:

```bash
rpi install rpi-extension-example
rpi install rpi-extension-example --version 0.1.0
rpi install rpi-extension-example --force
```

The command resolves and builds the crate with Cargo in release mode, then
copies its `.dll`, `.so`, or `.dylib` into `~/.rpi/agent/extensions` (or the
directory selected by `RPI_CODING_AGENT_DIR`). The extension is loaded on the
next `rpi` start. For local development, use
`rpi install my-extension --path ../my-rpi-extension --force`.

This is intentionally different from plain `cargo install`: `cargo install`
only copies executable targets, while rpi loads dynamic-library extensions.

### Develop an extension with watch mode

From a Rust extension crate (`[lib] crate-type` contains `"cdylib"`), start the
development host with:

```bash
rpi dev
```

The command detects the Cargo package, performs an initial build, stages a
versioned library under `.rpi/extensions/.dev`, and watches the crate sources.
Successful source changes trigger a rebuild and the same live reload used by
the TUI's `/reload` command. A failed build keeps the currently loaded plugin.
For a workspace containing multiple extensions, select one explicitly:

```bash
rpi dev --package rpi-todo
rpi dev --release
rpi dev --no-watch
```

See [`docs/extension-authoring.md`](docs/extension-authoring.md) for complete
Rust extension templates, safety rules, testing, and release checklists.

## Status (v1)

- **Providers:** Anthropic Messages and OpenAI-compatible Chat Completions,
  plus a faux provider for tests. Third-party endpoints are configured through
  `~/.rpi/agent/models.json`; Anthropic endpoint overrides also support
  `ANTHROPIC_BASE_URL`/`ANTHROPIC_AUTH_TOKEN`.
- **Auth (in priority order):** `--api-key` → `~/.rpi/auth.json` (set via
  `rpi auth login`) → `~/.rpi/agent/models.json` `apiKey` → provider environment
  variables (`OPENAI_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_API_KEY`). `rpi auth login`/`check`/`logout` manage the stored credential.
- **Tools:** the CLI defaults to Pi's `read`, `write`, `edit`, and `bash`
  tools. The former rpi-only `grep`, `find`, `ls`, `docs`, and `powershell`
  implementations remain library code but are not loaded by default.
- **Extensions:** Rust `cdylib` plugins can be installed with `rpi install` and
  are discovered from project `.rpi/extensions`, legacy `.pi/extensions`,
  global `~/.rpi/agent/extensions`, and `--extensions-dir`.
- **Project resources:** rpi-owned skills, prompts, system instructions, and
  extensions use `.rpi/` first; the original Pi `.pi/` layout remains a
  compatibility fallback. When both contain the same skill or prompt name,
  `.rpi/` wins. Project `.rpi/settings.json` can add `skillDirs`, `promptDirs`,
  and `extensionDirs` (with `.pi/settings.json` as fallback).
- **Sessions:** JSONL v4 durable backend + in-memory ephemeral; compaction + a
  split-turn two-LLM-call invariant.
- **Remote mode:** `rpi --server` runs the agent headless over TCP, and
  `rpi --connect <host:port> [--token <t>]` attaches a zero-local-resource TUI
  client (token auth; the token may also come from `RPI_SERVER_TOKEN`). See
  [`docs/remote-mode.md`](docs/remote-mode.md).

## Configuration

`rpi` persists credentials under `~/.rpi/` (override the dir with the
`RPI_CODING_AGENT_DIR` env var):

```
~/.rpi/
└── agent/
    ├── auth.json     # set with `rpi auth login` (0o600 on Unix)
    └── models.json   # optional: custom providers/models
```

`auth.json` holds the stored API key for `anthropic` (written by
`rpi auth login`, removed by `rpi auth logout`); `auth check` reports whether
any auth source is ready without touching the network.

`models.json` is a hand-edited file for custom Anthropic or OpenAI-compatible gateways:

```jsonc
{
  "providers": {
    "gateway": {
      "api": "anthropic-messages",
      "baseUrl": "https://gateway.example.com",
      "apiKey": "sk-gateway-secret",
      "authHeader": true,             // wrap apiKey as Authorization: Bearer
      "headers": { "x-portkey-key": "…" }, // optional extra headers
      "models": [
        { "id": "custom-claude", "name": "Custom Claude" }
      ]
    }
  }
}
```

Then `rpi --model gateway/custom-claude -p "hi"` routes to the gateway (the
`gateway/` prefix is CLI namespacing). For an OpenAI-compatible endpoint, set
`"api": "openai-completions"`; its `apiKey` is sent as a Bearer token and the
provider streams `/v1/chat/completions`.

**Default model (no `--model`).** A bare `rpi -p "hi"` picks the default the way
native pi does — the first *authenticated* model in the catalog when the
built-in default isn't authenticated. So a `models.json`-only Anthropic or
OpenAI gateway setup "just works": the gateway model is the only authenticated
one, so `rpi -p "hi"` routes through it — no `--model` needed. With a standard
`ANTHROPIC_API_KEY`/`auth.json`/`--api-key` setup, the native Pi default
`claude-opus-4-8` is selected when available.
See
[docs/m6-cli-open-questions.md §4–5](docs/m6-cli-open-questions.md) for the
full auth precedence, the default-selection rule, the `~/.rpi`-flat-vs-nested
divergence, and what's deferred (OAuth, `$ENV` credential expansion,
multi-provider registry).

## Releasing

The workspace `Taskfile.yml` is the canonical release entry point. Run
`cargo login` once first so `~/.cargo/credentials.toml` contains a
publish-scoped token; crates.io records are permanent.

```bash
task dry-run RELEASE_VERSION=0.1.18
task publish RELEASE_VERSION=0.1.18
```

Both commands require a clean worktree and one consistent version across all
nine release crates. `task publish` runs the locked workspace test and check
suites, publishes in dependency order, waits for each crate to reach the
crates.io index, and safely resumes by skipping exact versions already present.

## Star history

[![Star History Chart](https://api.star-history.com/svg?repos=bigfish1913/pi-rust,pi-rust/rpi-package&type=Date)](https://www.star-history.com/#bigfish1913/pi-rust&pi-rust/rpi-package&Date)

## License

MIT. See [LICENSE](LICENSE).

This is a Rust port of [earendil-works/pi](https://github.com/earendil-works/pi)
(© Mario Zechner, MIT). The `rpi-*` crates are a Rust-native reimplementation of
the original MIT-licensed TypeScript SDK; the upstream source is checked out
under `.reference/pi/` (read-only, gitignored).
