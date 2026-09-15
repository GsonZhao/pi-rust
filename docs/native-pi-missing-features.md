# pi-rust 相对原生 Pi 的功能缺失审计

审计日期：2026-09-12  
当前 Rust：`a072f31570ee3477b45c14f8304c1abf21b1fcb4`  
原生 Pi：`earendil-works/pi@71dca871bc80b6bc97be37f0ca3189399d651fff`

## 判定标准

这里只记录“原生 Pi 已提供可用功能，而当前 `pi-rust` 没有可用实现”的缺失。以下情况不计入：

- 同一功能的目录、文件格式、默认路径或协议实现不同；
- Rust 已经能完成用户目标，只是 API 名称或内部架构不同；
- 原生 Pi 自身默认不提供的能力（例如 subagents、plan mode）；
- 已在当前分支实现的旧 gap 文档项目（例如 `/new`、`/resume`、基础 TUI、SQLite session）。

## 优先级总览

| 优先级 | 缺失范围 | 影响 |
|---|---|---|
| P0 | Provider/API 覆盖、OAuth、RPC、图片输入 | 大量原生配置无法运行，远程集成和多模态入口不可用 |
| P1 | Harness/runtime、JSON 事件、AgentSession、导出、模型 registry | SDK/自动化客户端无法获得原生生命周期和恢复能力 |
| P1 | TUI 编辑器/快捷键、Trust/resource gate、Settings | 交互工作流和项目安全策略不完整 |
| P2 | JS/TS 扩展桥接、TUI 基础组件、telemetry | 扩展生态和低层 SDK 兼容性受限 |

## P0：模型、认证和传输

### 1. 原生 provider/API 大量没有运行时实现

当前 Rust provider 目录只有 `faux`、`anthropic`、`openai_completions`、`openai_responses`（`crates/pi-ai/src/providers/mod.rs`）。原生 `packages/ai/src/providers` 还提供以下 provider，但 Rust 没有对应的 HTTP/鉴权/流式运行时：

`amazon-bedrock`、`ant-ling`、`azure-openai-responses`、`baseten`、`cerebras`、`cloudflare-ai-gateway`、`cloudflare-workers-ai`、`deepseek`、`fireworks`、`github-copilot`、`google`、`google-vertex`、`groq`、`huggingface`、`kimi-coding`、`minimax`、`minimax-cn`、`mistral`、`moonshotai`、`moonshotai-cn`、`nvidia`、`openai-codex`、`opencode`、`opencode-go`、`openrouter`、`qwen-token-plan*`、`together`、`vercel-ai-gateway`、`xai`、`xiaomi*`、`zai`、`zai-coding-cn`，以及 OpenRouter/provider-specific image API。

`Api` enum 中预留名称不等于 provider 已实现；没有 provider 注册和请求实现的 API 仍然不可用。

证据：

- Rust：`crates/pi-ai/src/providers/mod.rs`、`crates/pi-ai/src/types.rs`
- 原生：`packages/ai/src/providers/`、`packages/ai/src/api/`

### 2. OAuth、订阅登录和 credential 工具缺失

Rust `rpi auth` 当前只真正支持 Anthropic API key 的 `login/check/logout`，源码也明确标注 OAuth 未移植。缺失内容包括：

- Claude Pro/Max Anthropic OAuth/device-code 登录；
- OpenAI Codex、GitHub Copilot、OpenRouter、xAI、Kimi 等 OAuth；
- OAuth token refresh、过期时间和 `--min-expiry`；
- `auth print-api-key`、`auth print-bearer-token`；
- 原生 provider 选择器和按 provider 的完整 credential 解析。

证据：`crates/pi-cli/src/auth.rs`；原生 `packages/ai/src/auth/`、`packages/ai/src/auth/oauth/`、`packages/coding-agent/src/cli/auth-command.ts`、`credential-print.ts`。

### 3. RPC 模式、协议和远程 client/server 缺失

`--mode rpc` 虽可解析，但 `crates/pi-cli/src/app.rs:337` 直接输出 `rpc mode is not implemented in v1` 并退出。Rust 没有原生对应的 JSONL RPC mode、RPC types/client，也没有 `packages/protocol`、`packages/client`、`packages/server` 的 transport-neutral 协议和服务端能力。

证据：Rust `crates/pi-cli/src/app.rs`、`crates/pi-cli/src/modes.rs`；原生 `packages/coding-agent/src/modes/rpc/`、`packages/protocol/`、`packages/client/`、`packages/server/`。

### 4. 图片输入链路缺失

Rust `process_file_args` 对图片直接报 `image attachments are not supported in v1`（`crates/pi-cli/src/app.rs:416`），随后只把文本文件作为字符串传入，`prompt_text` 的 image 参数没有从 CLI 附件转发。缺失内容包括：

- `@image.png` 等初始附件的 MIME 检测和 base64 `ImageContent`；
- terminal clipboard 图片粘贴和拖拽；
- 图片自动缩放、`blockImages` 设置；
- tool result 中的图片内容。

证据：Rust `crates/pi-cli/src/app.rs`；原生 `packages/coding-agent/src/utils/image-process.ts`、`clipboard-image.ts`、`tool-result-images.ts`。

## P1：Harness、Session 和模型运行时

### 5. 原生 ModelRegistry/ModelRuntime 没有等价运行时

Rust 主要在启动时读取 provider/model 配置并建立快照。缺失的是原生统一运行时提供的动态能力：

- `models.json` 异步刷新和 provider availability refresh；
- `getProvider`、`getAuth`、provider auth status；
- `registerProvider` / `unregisterProvider`；
- extension provider 与内建 provider 的统一解析；
- 运行时模型 catalog 变更后向 TUI/会话同步。

证据：Rust `crates/pi-cli/src/provider.rs`、`crates/pi-cli/src/session.rs`；原生 `packages/coding-agent/src/core/model-registry.ts`、`model-runtime.ts`、`model-resolver.ts`。

### 6. 最新 AgentHarness/AgentLane operation API 缺失

当前 `crates/pi-harness/src/agent_harness.rs` 仍以旧的 prompt/queue/compact/navigation 接口为主，没有原生最新 lane/runtime 暴露的完整 operation surface，包括：

- `getTipId`、`findEntries`、`findEntry`、`appendMessage`、`appendCustomEntry`；
- `getResult`、`accept`、`drive`、`requestAbort`、`inspectExecution`；
- deferred operation 的 `resume`；
- `watch`/`watchSession` 和 lane snapshot；
- harness 名称/标签、stream options、retry、compaction、steering/follow-up 等完整 getter/setter。

Rust 能识别 `Suspended`，但 CLI 明确输出“resume is not supported in v1”，因此 deferred run 不能继续。

证据：Rust `crates/pi-harness/src/agent_harness.rs`、`crates/pi-cli/src/modes.rs:101`、`interactive_tui.rs:4067`；原生 `packages/agent/src/harness/agent-harness.ts`、`packages/agent/src/harness/runtime/`。

### 7. Durable operation runtime/recovery/value store 缺失

Rust 有 JSONL、内存和 SQLite session 存储，但没有原生新增的 durable operation 分层：admission/drive/recovery/reconcile/checkpoint、deferred polling/resume、operation state/value store、pending assistant/tool frame 持久化、lane snapshots 和 recovery events。原生 `packages/agent/src/harness/runtime/` 及 `packages/coding-agent/src/core/session/{commit,fork,fork-policy,values}.ts` 均有对应实现，Rust session 目录没有 `values` 和 operation runtime 层。

### 8. coding-agent 产品层 AgentSession 缺失

Rust CLI 直接操作 `AgentHarness`，没有原生 `AgentSession` 这一层统一承载：prompt/continue、queue、model/thinking mutation、scoped models、compaction/retry、bash、HTML/JSONL export、session switching、tree/fork、extension binding、usage/context stats、reload、auto compaction/retry。缺失会让依赖 coding-agent 产品 API 的调用方无法直接迁移。

证据：原生 `packages/coding-agent/src/core/agent-session.ts`；Rust `crates/pi-cli/src/interactive_tui.rs`、`crates/pi-cli/src/session.rs`。

### 9. JSON 输出没有原生细粒度事件流

Rust `--mode json` 的事件投影只有 `run_start`、`run_end` 和最终 `result`（`crates/pi-cli/src/modes.rs:222-235`）。原生 JSON mode 还会输出 agent/turn 生命周期、message start/update/end、文本和 thinking delta、tool execution start/update/end、compaction/retry/session 事件。当前 harness 虽有内部 event bus，但没有把这些细粒度事件投影到 CLI JSON 合同。

证据：Rust `crates/pi-cli/src/modes.rs`、`crates/pi-harness/src/events.rs`；原生 `packages/coding-agent/src/modes/json-event.ts`。

### 10. 原生导出格式和 CLI export 缺失

Rust `/export` 只生成当前目录下的 Markdown（`crates/pi-cli/src/interactive_tui.rs:2115` 附近），`--export <file>` 目前只是识别后忽略。缺失内容包括：

- `AgentSession.exportToHtml()`；
- `AgentSession.exportToJsonl()`；
- HTML theme、tool renderer、语法高亮和输出路径处理。

证据：原生 `packages/coding-agent/src/core/export-html/`、`agent-session.ts:3463-3488`；Rust `interactive_tui.rs`、`args.rs`。

## P1：CLI、TUI、资源和设置

### 11. 已识别但未生效的 CLI 功能

当前 parser 对以下原生参数只接受/警告，没有实现其原生行为：

- `--export <file>`；
- `--offline`；
- `--approve` / `--no-approve`（没有真正控制 trust）；
- `--tui-mode regular|fullscreen`；
- `--no-themes`；
- 原生语义的 `--use-theme <name>`；
- `--list-models [search]`（当前明确提示 unsupported/ignored）。

证据：Rust `crates/pi-cli/src/args.rs:339-390`、`app.rs`；原生 `packages/coding-agent/src/cli/args.ts`。

### 12. 交互快捷键和编辑器工作流缺失

当前 TUI 已有输入、模型切换、工具展开等基础操作，但仍缺少原生提供的独立功能：

- Ctrl+G 外部编辑器（`externalEditor` / `$VISUAL` / `$EDITOR`）；
- Ctrl+O 的 tool-output filter/collapse cycle；
- Ctrl+T 对 thinking block 的折叠/展开（Rust Ctrl+T 当前只作用于最近 tool）；
- Shift+Tab thinking level cycle；
- 配置化 keybindings、double-escape action、mouse region/click workflow；
- 图片粘贴/拖拽入口。

证据：Rust `crates/pi-cli/src/interactive_tui.rs:3151,3671`、`crates/pi-tui/src/keybindings.rs`；原生 `packages/coding-agent/README.md:167-218,263`、`settings-manager.ts`。

### 13. 原生 slash command / llama.cpp 集成缺失

当前 registry 已覆盖 `/new`、`/resume`（alias）以及 tree/fork/import 等命令，但仍缺少：

- `/changelog`：显示版本历史；
- `/llama`：连接 llama.cpp router，下载、加载、卸载模型，并配合 `/login llama.cpp` 和 `/model` 使用。

证据：Rust `crates/pi-cli/src/interactive_tui.rs:1317-1350`；原生 `packages/coding-agent/src/core/slash-commands.ts:31`、`packages/coding-agent/README.md:139,180,200`。

### 14. Trust gate 和资源发现仍不完整

Rust 已有 `.rpi/.pi` 资源发现、package 资源和部分 collision diagnostics，但仍缺少原生安全/兼容行为：

- `.agents/skills` 和 `~/.agents/skills`；
- 未信任项目时限制 project-local resources/extensions 的 trust prompt/gate；
- worktree shadowed context-file 去重；
- 完整 skill metadata 校验及结构化 winner/loser collision diagnostics。

Rust 代码明确说明项目资源当前无条件加载，`trust.json` 只做布局读写，不参与 gate。

证据：Rust `crates/pi-cli/src/resource_dirs.rs:20-29,106-107,212`、`session.rs:402`、`config.rs:364-366`；原生 `packages/coding-agent/src/core/resource-loader.ts`、`project-trust.ts`、`trust-manager.ts`。

### 15. Settings 可控功能面明显缺失

Rust `crates/pi-cli/src/settings.rs:19` 只真正建模并使用 provider/model/thinking/theme、scopedModels、packages 和 resource dirs；未知字段虽会保留，但不会产生行为。原生 settings 中以下功能因此不可用：retry、compaction、branch summary、steering/follow-up mode、transport（SSE/WebSocket/auto）、hide thinking、external editor、shell path/prefix、quiet startup、project trust、terminal image/progress/hyperlink/trueColor、image resize/block、enabled models/default tools、doubleEscapeAction、tree filters、thinking budgets、UI padding/autocomplete、markdown/mermaid/warning/http timeout 等。

证据：Rust `crates/pi-cli/src/settings.rs`；原生 `packages/coding-agent/src/core/settings-manager.ts:100-120` 及其 getter/setter 实现。

## P2：扩展和低层组件

### 16. JS/TS extension bridge 的能力缺失

Rust 原生 cdylib 扩展已有不少事件、provider、renderer 和 runtime bridge，不能整体视为缺失；但以下明确接口仍是 stub/unsupported：

- C ABI `register_shortcut` 返回成功但不注册（`crates/rpi-extensions/src/lib.rs:410-414`）；
- Node host 的 `on(event, handler)` 只处理 `resources_discover`，不是完整事件面；
- `ui.select`、`ui.confirm`、`ui.input`、`ui.editor` 直接抛 `unsupported capability`；
- `session.sendMessage`、`session.sendUserMessage` 直接抛 `unsupported capability`；
- message renderer、markdown transformer、entry renderer 等原生 JS extension 注册面未完整暴露。

证据：Rust `crates/rpi-extensions/src/lib.rs`、`crates/pi-cli/src/node_host.mjs:240-243,342-343,440`、`js_extensions.rs`；原生 `packages/coding-agent/src/core/extensions/types.ts`、`runner.ts`。

### 17. TUI 基础组件没有直接等价物

原生 `packages/tui/src/components` 的 `AltScreenFlash`、`MouseRegion` 以及 `editor-component.ts` 接口，在 `crates/pi-tui/src` 没有直接等价 API。因此依赖这些组件的原生 TUI/extension 不能直接迁移。

### 18. 生产 telemetry schema/backend 不完整（SDK 级）

Rust telemetry 目前以 noop backend 和少量 span contract/testing memory context 为主；原生还提供 typed telemetry schemas、typed span starter，以及 agent/AI/harness 的丰富事件和属性。若以 SDK 兼容为目标，这部分属于低优先级功能缺失；若只审计 CLI 用户功能，可暂不纳入发布阻断项。

证据：Rust `crates/pi-telemetry/src/{lib,types,noop,testing,context}.rs`；原生 `packages/telemetry/` 及 agent/AI/harness telemetry schema。

## 明确不算“缺失”的项目

- Rust 已有基础 TUI、assistant/tool/footer/selector 等组件；旧 `docs/tui-gap-analysis.md` 中“完整 TUI 缺失”的表述已过时。
- `/new`、`/resume` 已通过当前命令 registry 的 alias 提供；真正缺失的 `/changelog` 和 `/llama` 已在上文单列。
- Rust 已有 `/tree`、`/fork`、`/clone`、`/compact`、`/import`、`/share` 和基础 `/export`；缺失的是 export 格式和 CLI export 能力，不是整个命令不存在。
- Rust 已有 read/write/edit/bash 以及 grep/find/ls 工具；不把工具实现差异写成缺失。
- Rust 已有 SQLite session backend；`.rpi`/`.pi` 路径、默认 session 目录、provider route 等差异属于实现/兼容策略，不单列为功能缺失。
- 原生 Pi 默认也不带 subagents、plan mode，因此不列为 Rust 缺失。
