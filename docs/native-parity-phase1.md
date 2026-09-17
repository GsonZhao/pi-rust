# Native parity phase 1

分支：`codex/native-parity-phase1`

本阶段已完成的用户可见能力：

- `--list-models [search]`：无需 API key 即可列出内建及 `models.json` 模型；
- `@image`：PNG/JPEG/GIF/WebP/BMP 按内容签名识别，编码为 `ImageContent`，传给首个 prompt；
- `--offline`：跳过启动更新检查；
- `--approve/-a` 与 `--no-approve/-na`：控制项目级资源和扩展是否加载；
- JSON mode：输出 agent/turn/message/delta/tool execution 事件，保留完整
  `assistantMessageEvent`、tool args/result，并在最终 `result` 前等待终止事件；
- `--export <session.jsonl> [output.html]`：导出独立 HTML；`.jsonl` 目标保留 JSONL；
- Ctrl+G：使用 `RPI_EXTERNAL_EDITOR`、`VISUAL`、`EDITOR` 或平台默认编辑器编辑当前草稿；
- Shift+Tab / BackTab：循环当前模型支持的 thinking level；
- `--no-themes`：禁用包/自定义主题加载；与 `--theme` 冲突时 fail-safe 使用内置默认主题；
- Ctrl+O：在紧凑/展开之间切换所有工具输出面板；
- Ctrl+T：切换 reasoning/thinking 区块的可见性，新消息继承该偏好；
- `--tui-mode regular|fullscreen`：选择终端原生 scrollback 或 alternate-screen；
- 双 Esc：空编辑器内 500ms 内连续按两次时按 `doubleEscapeAction` 打开 tree/fork；
- settings `keybindings`：支持覆盖 interrupt、clear、exit、model、tool、thinking、
  external editor 等 TUI action 的组合键；
- `/changelog`：显示当前版本和本阶段已实现的变更；
- OpenAI-compatible provider：`models.json` 支持显式 `apiKey`；自定义 provider
  通过 `apiKey: "$ENV"` 绑定环境变量，已知规范 provider 可使用
  `DEEPSEEK_API_KEY`、`GROQ_API_KEY`、`OPENROUTER_API_KEY` 等内建别名；
- 相关 parser、图片处理和 trust gate 单元测试。

Trust 行为：

- 显式 `--approve` 优先；
- 否则读取 `~/.rpi/agent/trust.json` 中当前 cwd 的决定；
- 没有决定时 fail-closed，禁用项目级 settings、packages、extensions、skills、prompt templates、SYSTEM/APPEND_SYSTEM 和 context files；
- 全局资源及显式 `--extension`、`--skill`、`--prompt-template` 仍可使用。

验证命令：

```text
cargo check -p rpi-cli
cargo test -p rpi-cli args::tests::
cargo test -p rpi-cli app::tests::process_file_args_attaches_supported_images
cargo test -p rpi-cli session::tests::project_trust_override_fails_closed_by_default
```

仍待后续阶段：clipboard image/drag-and-drop，以及完整多 provider/OAuth/RPC parity。
