# pi-rust 7 天学习计划

按**依赖方向自底向上**(telemetry → ai → agent → tools → harness → cli)设计。这个顺序不是任意的:它既是 crate 的真实分层,也是 `docs/architecture.md §9` 里项目本身的构建顺序 —— 每一层只依赖下面的层,每天读的代码都能用前一天建立的模型解释。`crates/pi-agent/src/agent_loop.rs` 是全工程的皇冠,放在第 3 天深入。

> 假设每天 3–4 小时。读代码时,留意每个 Rust 模块首行 `//! Mirrors ...ts` 注释 —— 它告诉你对应 TS 源文件,可对照 `.reference/pi/` 读。

---

## Day 1 · 定向:跑起来,建立全局心智模型

**目标**:把"这是什么"彻底搞清楚,并让它在本地编译运行。

**阅读**
- `README.md` —— 全貌、命名规则(`rpi-` vs `pi-`)、安装/扩展
- `docs/architecture.md` —— **今天最重要的文件**,逐节读,尤其 §1(TS Pi 是什么)、§3-4(核心类型与 loop)、§9(构建顺序)
- 根 `Cargo.toml` —— 9 个 workspace member 与 `rpi-*` 别名

**动手**
```bash
cargo build -p rpi-cli          # 确认全 workspace 编译
cargo test -p rpi-agent --lib   # 跑 faux 下的单元测试
cargo run -p rpi-cli -- --help  # 看实际 CLI 表面
```
- 浏览 `crates/` 每个 crate 的 `lib.rs` 顶部 doc + `pub mod` 列表,只看"有哪些模块",不读实现。

**自检**
- 用一句话说出 `StreamFn` 为什么是"唯一的 LLM 边界"。
- `pi-agent` 为什么不依赖文件系统/shell?(答案:那些在 tools 里)
- 9 个 crate 的依赖箭头方向你能默写出来吗?

---

## Day 2 · 基座:`rpi-ai` 类型与流式协议

**目标**:理解 loop 之下"语言"的词汇表 —— 所有上层都只谈这些类型。

**阅读(按此顺序)**
1. `crates/pi-ai/src/types.rs` —— `Message`(User/Assistant/ToolResult)、`Content`(Text/Thinking/Image/ToolCall)、`StopReason`、`Usage`、`Context`、`Tool`、`Model`。对照架构 §3。
2. `crates/pi-ai/src/schema.rs` + `strict_schema.rs` —— JSON Schema 表示 + `validate_tool_arguments` 与 coerce(松散模型输出仍能通过校验)。
3. `crates/pi-ai/src/event_stream.rs` —— **重点**:`AssistantMessageEvent`(start / text·thinking·toolcall 的 start/delta/end / done / error)与 `AssistantMessageEventStream`(mpsc + oneshot,单生产多消费)。
4. `crates/pi-ai/src/provider.rs` —— `Provider` trait(`stream_simple`)+ `Models` 注册表。
5. `crates/pi-ai/src/providers/faux.rs` —— 确定性脚本化 provider,**所有测试都靠它**,务必读懂它如何"假装"流式事件。

**动手**
- 读 `examples/minimal/src/main.rs`,它用 faux 跑通一个最小 agent —— Day 2→3 的桥梁。
- `cargo test -p rpi-ai` 看 event_stream / coerce 的测试如何断言。

**自检**
- 为什么 `AssistantMessage` 用 `Box`?(`Content` 重的变体)
- `AssistantMessageEventStream::result()` 靠什么 resolve?(Done/Error 时的 oneshot)
- coerce 解决了什么问题?(模型给的 `"3"` 要能当 number 通过)

---

## Day 3 · 皇冠:`rpi-agent` 与 agent loop

**目标**:精读 `agent_loop.rs`,吃透它的不变量。这是全工程最值得花时间的一天。

**阅读顺序**
1. `crates/pi-agent/src/message.rs` —— `AgentMessage` 开放枚举(`Llm(Message) | Custom(Arc<dyn Any>)`),`convert_to_llm` 如何剥掉 Custom。
2. `crates/pi-agent/src/agent_tool.rs` —— `AgentTool` trait(schema/label/execution_mode/prepare_arguments/execute + on_update)。
3. `crates/pi-agent/src/types.rs` —— `AgentContext`、`AgentState`、`ToolExecutionMode`、`QueueMode`、Before/AfterToolCall 上下文。
4. `hooks.rs` + `queue.rs` + `events.rs` + `abort.rs` + `stream_fn.rs` —— 快读,了解"零件"。
5. `crates/pi-agent/src/agent.rs` —— `Agent`/`AgentBuilder`,owner+subscriber 的薄壳。
6. **`crates/pi-agent/src/agent_loop.rs`** —— 精读 `run_agent_loop` / `run_agent_loop_continue`。

**重点盯 loop 里三个不变量**(文件首部 doc 已列出,代码里去对应):
- **工具执行顺序**:并行 batch 中 `ToolExecutionEnd` 按*完成序*,tool-result 的 `MessageStart/End` 按*源序*。
- **truncate-fail**:`stop_reason == Length` → 整条 tool_call 原地失败、不执行。
- **late-update 抑制**:`execute` resolve 后 `on_update` 变 no-op(`AtomicBool`)。

**动手**
```bash
cargo test -p rpi-agent          # tool_execution_ordering / truncate_fail / late_update_suppression
```
- 对照 `crates/pi-agent/tests/tool_execution_ordering.rs` 等测试,看不变量如何被断言。

**自检**
- 一轮 loop 从 `prompt` 到回到下一轮,数据经历了哪几步变换?
  (prompt → transform_context → convert_to_llm → stream_fn → 折叠 delta → 若 ToolUse 则 execute → push ToolResult → 回到循环)
- steering 队列和 follow-up 队列分别何时注入?(steering:本 turn 之后中途;follow-up:agent 本要停下时)

---

## Day 4 · 工具层:`rpi-tools` 与执行环境

**目标**:理解 loop 调到的工具如何落地到真实文件/shell,以及"接缝"`ExecutionEnv`。

**阅读**
1. `crates/pi-tools/src/env.rs` —— `ExecutionEnv` trait(读/写/编辑/列目录/stat/shell)—— 工具与宿主之间的 seam。
2. `crates/pi-tools/src/os_env.rs` —— 真实实现(tokio::fs + tokio::process)。
3. `crates/pi-tools/src/in_memory.rs` —— 测试用实现。
4. `tools/read.rs` + `write.rs` + `edit.rs` + `bash.rs` —— 四个内置工具,看 `create_*_tool` 工厂形状。
5. `crates/pi-tools/src/file_mutation_queue.rs` —— 对同一文件的编辑如何串行化。
6. `crates/pi-tools/src/truncate.rs` + `shell_output.rs` —— 截断与 shell 输出捕获。

**动手**
- 读 `examples/tools/src/main.rs`,看工具如何注册进 agent。
- `cargo test -p rpi-tools`(read_truncation / edit_fuzzy / bash_throttle / execution_env_conformance)。

**自检**
- 为什么工具层要抽 `ExecutionEnv` 而不直接 `std::fs`?(可测试 + 可替换宿主,如远程/沙箱)
- `FileMutationQueue` 解决什么?(并发编辑同一文件的竞态)

---

## Day 5 · 有状态层:`rpi-harness` 会话树与持久化

**目标**:理解在无状态 loop 之上,harness 如何加持久会话/分支/压缩/崩溃恢复。这层最重(1.8 万行),只抓主干。

**阅读**
1. `session/types.rs` + `session/session.rs` —— write-once entry、DAG、`Lane` 游标。
2. `session/jsonl/codec.rs` + `storage.rs` —— append-only JSONL、torn-tail 丢弃、原子发布。
3. `compaction/compaction.rs` + `cut_point.rs` —— `should_compact` / `find_cut_point` / `generate_summary`。
4. `crates/pi-harness/src/runtime.rs` —— **崩溃恢复**:admission / recover / reconcile,为何"只读+决策、不修改 session"。
5. `crates/pi-harness/src/agent_harness.rs` —— `AgentHarness` 如何沿分支路径驱动 `run_agent_loop`、写 `operation_started/finished`、把 stop_reason 映射成 `RunOutcome`(Completed/Aborted/Failed/Suspended)。
6. `system_prompt.rs` + `skills.rs` —— skills/模板如何注入系统提示。

**动手**
```bash
cargo test -p rpi-harness        # m5c_jsonl_torn_tail / compaction_cut_point / harness_run_e2e
```

**自检**
- 进程死在 `operation_started` 和 `operation_finished` 之间,重启后会发生什么?为什么恢复不能改 session?
- compaction 的 cut point 怎么选,为什么要在 run *之前*做?

---

## Day 6 · 插件轴 + CLI 顶层

**目标**:横切的插件 ABI,以及 CLI 如何把所有层拼成可运行产品。

**阅读 — 插件(先读,因为短)**
1. `crates/rpi-plugin-sdk/src/lib.rs` —— 稳定 ABI:4 个导出函数(execute→handle / poll / cancel / destroy)。
2. `crates/rpi-extensions/src/loader.rs` + `tool.rs` —— `libloading` 加载 + `PluginToolAdapter` 如何跨 FFI 桥接 async(ambient Handle + mpsc + oneshot + `spawn_blocking` + `catch_unwind` + 取消只设 `AtomicBool` 不丢 driver)。
3. 读 `examples/plugin-stub/src/lib.rs`,`cargo build -p plugin-stub` 看产物。

**阅读 — CLI(按请求生命周期)**
1. `args.rs` → `provider.rs`(auth 优先级链)→ `session.rs`(构建 env+tools+JSONL+harness)→ `modes.rs`(print/json/交互)→ `app.rs`(总分发)。
2. 选读 `remote/mod.rs`(`--server`/`--connect` 零本地资源 TUI)、`install.rs` + `dev_extension.rs`(`rpi install`/`dev` watch)。

**自检**
- 为什么 `rpi-extensions` 刻意不依赖 `rpi-harness`?harness 怎么拿到插件工具?(trait object 注入,防环)
- `rpi install` 与 `cargo install` 的本质区别?(前者拷贝 cdylib 到 `~/.rpi/agent/extensions`)

---

## Day 7 · TUI 表层 + 终局串联

**目标**:看 UI 如何消费 harness 事件;然后**从一封 prompt 追到屏幕上的 token**,完成闭环。

**阅读 — TUI(只抓架构,22K 行细节可跳)**
1. `crates/pi-tui/src/lib.rs` + `component.rs` + `tui.rs` —— 组件模型与主循环。
2. `crates/pi-cli/src/interactive_tui.rs` —— **粘合层**:harness `HarnessEvent` → TUI 渲染。

**终局练习(强烈建议动手)**
- 用 `examples/minimal/src/main.rs` 为模板,改造一个**自带一个自定义 tool**的小 agent:faux provider + 你的 `echo` tool,跑一次完整 loop,在 `subscribe()` 里打印每种 `AgentEvent`。这迫使你串起 Day 2-3 的全部知识。
- 然后复述这条 token 的完整旅程:
  ```
  prompt → transform_context → convert_to_llm → stream_fn(faux)
        → AssistantMessageEvent 折叠 → 广播 MessageUpdate
        → (若你触发了 tool)execute → ToolResultMessage → 下一轮
  ```

**自检(本周总复盘)**
- 你能否默写出 Day 1 那张依赖 DAG?
- 你能否讲清 loop 的三个不变量各解决什么真实 bug?
- 哪一层是"有状态"、哪一层是"无状态",为什么这样切?

---

## 两条提速建议

1. **对照 TS 源读**:每读一个 Rust 模块,顺手翻 `.reference/pi/packages/` 下对应 `.ts`。移植的取舍(如 TS 用 declaration merging,Rust 用 `AgentMessage` 开放枚举)最能加深理解。
2. **测试即文档**:每个 crate 的 `tests/` 比源码更直白地展示"期望行为"。读到困惑处,先 grep 测试名(如 `tool_execution_ordering`),看它断言什么。
