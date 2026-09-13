# Native parity phase 2

本阶段聚焦日常交互输入和高收益 settings 行为。

已实现：

- `settings.json` 支持 `hideThinkingBlock`、`quietStartup`、
  `showTerminalProgress`、`editorPaddingX`、`autocompleteMaxVisible`；
- `quietStartup` 跳过交互启动更新提示；
- `showTerminalProgress` 控制工作状态 loader；
- 编辑器 padding 与补全列表上限从 settings 生效；
- crossterm `Event::Paste` 支持普通文本粘贴；
- 粘贴/拖拽单个图片文件路径时自动识别并附加到下一条 prompt；
- 剪贴板位图转 PNG，显示终端预览，并附加到下一条 prompt；
- 图片尺寸限制为 1..=16384 像素，非法图片不会进入 prompt。

验证：

```text
cargo check -p rpi-cli
cargo test -p rpi-cli
cargo test --workspace
```

仍待后续：多文件拖拽、移动端/特殊终端剪贴板协议、tree/session 过滤增强、
完整 JS/TS extension UI bridge、RPC 和 OAuth。
