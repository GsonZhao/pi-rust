# Native parity phase 1

分支：`codex/native-parity-phase1`

本阶段已完成的用户可见能力：

- `--list-models [search]`：无需 API key 即可列出内建及 `models.json` 模型；
- `@image`：PNG/JPEG/GIF/WebP/BMP 按内容签名识别，编码为 `ImageContent`，传给首个 prompt；
- `--offline`：跳过启动更新检查；
- `--approve/-a` 与 `--no-approve/-na`：控制项目级资源和扩展是否加载；
- JSON mode：输出 agent/turn/message/delta/tool execution 事件；
- `--export <session.jsonl> [output.html]`：导出独立 HTML；`.jsonl` 目标保留 JSONL；
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

仍待后续阶段：核心 TUI 快捷键和完整多 provider/OAuth/RPC parity。
