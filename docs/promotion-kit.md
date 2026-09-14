# rpi 投放素材

本文件用于发布 rpi 的开源推广内容。发布前请根据平台规则调整措辞，不要在多个社区原样重复发帖。

## 统一信息

- 项目：rpi，基于 Pi agent 的 Rust 原生 Agent SDK 和终端 coding agent
- 稳定版本：0.1.10
- Beta：Node/TypeScript 扩展加载、TUI 原生技能渲染（来自 dev 分支，暂不建议生产使用）
- GitHub：https://github.com/bigfish1913/pi-rust
- 官网：https://rpi.laofu.online/
- 文档：https://rpi.laofu.online/docs.html
- 安装：`cargo install rpi-cli`
- 启动：`rpi -p "hello"`
- 许可证：MIT

## 中文短文案

### V2EX / Linux.do / Rust 中文社区

标题：

> rpi：基于 Pi agent 的 Rust 原生 Agent SDK，支持工具调用、会话和插件

正文：

> rpi 是一个基于 Pi agent 的 Rust 原生实现，采用 library-first、多 crate 的设计，目标是让开发者可以把 Agent runtime 嵌入自己的应用或工作流。
>
> 目前包含：
> - 异步、流式的 Agent loop
> - Anthropic Messages、OpenAI-compatible 和 faux provider
> - 与 Pi 对齐的 `read`、`write`、`edit`、`bash` coding-agent 工具
> - session、JSONL 持久化、上下文压缩和 prompt templates
> - `rpi-plugin-sdk` 与稳定 ABI 扩展能力
> - `rpi-cli` 终端编码 Agent
>
> Node/TypeScript 扩展加载和 TUI 原生技能渲染目前属于 Beta，建议仅用于本地评估。
>
> 直接安装：
>
> ```bash
> cargo install rpi-cli
> rpi -p "hello"
> ```
>
> 项目地址：https://github.com/bigfish1913/pi-rust
>
> 官网和文档：https://rpi.laofu.online/
>
> 欢迎 Rust、LLM Agent 和开发者工具方向的朋友试用、反馈和贡献。

## Hacker News

标题：

> Show HN: rpi – A Rust-native Pi agent SDK and terminal coding agent

正文：

> rpi is a Rust-native implementation of the Pi agent SDK. It is library-first and split into composable crates for providers, agent runtime, tools, sessions, harnesses, plugins, and a terminal coding-agent CLI.
>
> Highlights:
> - Async, streaming agent runtime
> - Anthropic Messages, OpenAI-compatible chat completions, and a faux provider
> - Built-in coding tools: read, write, edit, and bash (Pi-compatible defaults)
> - Durable JSONL sessions, context compaction, hooks, queues, and prompt templates
> - Plugin SDK and stable ABI registration for tools, providers, events, and resources
>
> Node/TypeScript extension loading and native skill rendering are beta compatibility features and are not recommended for production.
>
> ```bash
> cargo install rpi-cli
> rpi -p "hello"
> ```
>
> GitHub: https://github.com/bigfish1913/pi-rust
>
> Website: https://rpi.laofu.online/
>
> The project is MIT licensed. The current stable release is v0.1.10; the Node/TypeScript and native skill rendering updates are beta work on the dev branch.

## Reddit

### r/rust

标题：

> [Showcase] rpi: a Rust-native Pi agent SDK with composable crates and plugin ABI

重点：Rust crate 分层、异步流式 runtime、trait 设计、插件 ABI、测试和 faux provider。避免只强调“又一个 AI CLI”。

### r/LocalLLaMA / r/LLMDevs

标题：

> rpi: a Rust coding-agent toolkit with provider adapters, built-in tools, sessions, and plugins

重点：Provider 适配、工具循环、会话持久化、OpenAI-compatible endpoint 和不需要 API key 的本地示例。发布前查看各版自荐规则。

## GitHub Release

建议 Release 标题：

> rpi v0.1.10 — Rust-native Pi agent SDK and CLI

Release 摘要：

> rpi v0.1.10 provides the complete path from provider and agent loop to Pi-compatible coding tools, sessions, harness, plugins, and the `rpi` terminal CLI.
>
> Install the CLI with:
>
> ```bash
> cargo install rpi-cli
> ```
>
> See the documentation at https://rpi.laofu.online/docs.html and the architecture guide at https://github.com/bigfish1913/pi-rust/blob/main/docs/architecture.md.

## This Week in Rust

Issue title:

> Project Submission: rpi — Rust-native Pi agent SDK and terminal coding agent

Issue body:

> Hi TWiR team,
>
> I would like to propose rpi for the Project/Crate of the Week.
>
> rpi is a Rust-native implementation of the Pi agent SDK and a terminal coding-agent CLI. It follows a library-first design and splits providers, the async agent loop, coding tools, durable sessions, harnesses, plugins, and the CLI into composable crates.
>
> Highlights:
> - Async, streaming Agent runtime with tool calls, hooks, queues, and cancellation.
> - Anthropic Messages, OpenAI-compatible Chat Completions, and a deterministic faux provider.
> - Built-in Pi-compatible `read`, `write`, `edit`, and `bash` tools.
> - JSONL session persistence, branching, prompt templates, and context compaction.
> - A stable `#[repr(C)]` plugin ABI through `rpi-plugin-sdk`, with a host-side dynamic loader.
> - A terminal CLI that can be installed with `cargo install rpi-cli`.
>
> Quick start:
>
> ```bash
> cargo install rpi-cli
> rpi -p "hello"
> ```
>
> Links:
> - Repository: https://github.com/bigfish1913/pi-rust
> - Website: https://rpi.laofu.online/
> - Crates.io: https://crates.io/crates/rpi-cli
> - Docs.rs: https://docs.rs/rpi-cli
> - Plugin SDK: https://crates.io/crates/rpi-plugin-sdk

## Awesome Rust PR

Suggested list entry for an appropriate AI / agent section:

> - [rpi](https://github.com/bigfish1913/pi-rust) - Rust-native Pi agent SDK and terminal coding-agent CLI with composable providers, tools, sessions, and a stable plugin ABI.

Use the list maintainer's preferred category and contribution format. Submit the
entry as a focused PR rather than opening multiple issues.

## X / Bluesky / LinkedIn

> Introducing rpi: a Rust-native implementation of the Pi agent SDK.
>
> Build composable coding agents with async streaming, provider adapters, built-in tools, durable sessions, and a plugin ABI.
>
> Install: `cargo install rpi-cli`
>
> GitHub: https://github.com/bigfish1913/pi-rust
> Website: https://rpi.laofu.online/

## 其他渠道（RustCC 已发布）

RustCC 已有项目介绍后，不再重复投放同一篇文章。后续按平台调整内容角度：

### OSCHINA

标题：

> rpi：用 Rust 构建可嵌入的 coding-agent runtime

文章角度：

> 重点介绍 `rpi-ai`、`rpi-agent`、`rpi-tools`、`rpi-harness` 的单向依赖，解释为什么采用 library-first 设计，以及如何从 faux provider 开始做离线测试。结尾放 CLI 安装命令和 GitHub 链接。不要把 Beta 的 Node/TypeScript 扩展或 TUI 技能渲染当作稳定卖点。

### 掘金

标题：

> 从 Prompt 到工具循环：一个 Rust Agent SDK 的分层实践

文章结构：

> 1. Agent runtime 需要解决哪些问题；
> 2. Provider、Agent loop、Tool 和 Session 如何分层；
> 3. 用 `InMemoryExecutionEnv` 写不依赖网络的测试；
> 4. 用 `cargo run -p minimal` 跑通第一个 Agent；
> 5. rpi 与 CLI、插件 ABI 的关系。
>
> 文章主体写技术实践，项目介绍放在末尾，避免被识别为纯广告。

### 知乎

建议采用问答形式，不直接复制项目公告：

> 问题方向：Rust 适合用来构建 LLM Agent 吗？
>
> 回答重点：Rust 的 trait、异步流式、可测试执行环境和稳定 ABI 如何帮助构建长期运行的 coding agent。用 rpi 的四层代码示例说明，再附项目链接。Node/TypeScript 扩展和 TUI 技能渲染只作为实验性兼容工作的补充说明。

### DEV.to / Hashnode

标题：

> Building a Testable Coding Agent in Rust with rpi

重点展示：

> Start with the offline faux provider, add `read`/`write`/`edit`/`bash`, subscribe to streaming events, then introduce sessions. Keep the Node/TypeScript bridge and native skill rendering in a clearly marked Beta section.

### Lobsters

标题：

> rpi: a Rust-native, library-first coding-agent runtime

正文保持短小，强调 crate 分层、离线测试和 `rpi-plugin-sdk` 的 ABI 设计；附 GitHub、架构文档和最小运行命令。先确认账号满足社区发帖要求。

### 发布节奏

- RustCC 已发布，不在相邻几天内重复同类中文文章。
- OSCHINA 和掘金间隔 3 至 5 天，使用不同标题和文章主体。
- 知乎、DEV.to/Hashnode 作为技术跟进，间隔 5 至 7 天。
- This Week in Rust 和 Awesome Rust 采用项目提交/PR 形式，不要当作普通软文重复发布。

## 发布检查清单

- 确认链接使用 `https://github.com/bigfish1913/pi-rust`，不要使用旧仓库地址。
- 稳定版宣传使用 `0.1.10`；Beta 功能须标注来自 dev 分支，不要写成已发布的稳定版本。
- 确认安装命令是 `cargo install rpi-cli`。
- 每个平台使用一张最相关的截图，正文中说明截图展示的是 CLI 启动或工作状态。
- Hacker News 和 Reddit 使用英文；中文社区使用中文，并按版规选择分类。
- 发帖后优先回复安装、Provider 配置和插件扩展问题，不要连续重复推送。
- GitHub Issue、论坛帖子和社交平台公开提交前，确认标题、链接、截图和项目状态无误。
