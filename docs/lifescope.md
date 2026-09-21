# rpi 客户端生命周期事件总线设计文档

**版本**：1.0
**状态**：定稿
**关联组件**：rpi-cli、rpi-plugin-sdk、rpi-extension-rpc、rpi-server
**适用环境**：Linux / macOS / Windows / Termux(Android)

---

## 1. 背景与问题

rpi-cli 是一个基于 Rust 的终端智能助手。在设计扩展机制时遇到两个问题：

### 1.1 具体问题

- **Termux 兼容**：`arboard` 剪贴板库不支持 Android，导致 `rpi-cli` 在 Termux 下编译失败。
- **RPC 连接时机**：当用户安装 `rpi-extension-rpc` 时，需要在进入 TUI 之前建立 RPC 连接，连接成功后再启动 TUI；未安装时完全跳过。

### 1.2 早期方案的问题

早期在主程序中硬编码检测 server 插件，存在以下缺陷：

- 主程序需要感知具体扩展的存在；
- 新增启动前逻辑必须修改主程序；
- 无法支持多个扩展协作；
- 无法覆盖 rpi 的完整生命周期。

### 1.3 解决思路

引入**生命周期事件总线（Lifecycle Event Bus）**，将“进入 TUI 前建立连接”抽象为一个事件钩子，由扩展自行实现。主程序只负责在合适的时机广播事件，完全不需要感知具体扩展。

---

## 2. 目标与非目标

### 2.1 目标

| 目标 | 说明 |
|---|---|
| **解耦** | 主程序不感知任何具体扩展 |
| **可扩展** | 新增逻辑只需新增扩展，无需改主程序 |
| **零开销** | 无扩展实现事件时，不产生连接、等待或错误 |
| **资源传递** | 扩展可在事件中注入资源（如 `RpcClient`），供 TUI 使用 |
| **完整生命周期** | 覆盖启动到关闭的各个阶段 |
| **Termux 兼容** | 不依赖 `arboard` 等不兼容库 |
| **可演进** | 支持优先级、条件、异步、持久化、动态加载 |

### 2.2 非目标

- 不定义 RPC 协议或消息格式；
- 不修改 rpi Agent 核心循环逻辑；
- 不强制所有扩展实现生命周期钩子。

---

## 3. 总体设计

### 3.1 架构总览

```
┌──────────────────────────────────────────────────┐
│                    rpi-cli (主程序)                │
│                                                   │
│  main() ──► PluginRegistry ──► EventBus ──► TUI  │
│                │                 │               │
│                │                 │               │
│                ▼                 ▼               │
│         ┌─────────────┐   ┌────────────┐          │
│         │  加载插件   │   │ 分发事件   │          │
│         └─────────────┘   └────────────┘          │
└──────────────────────────────────────────────────┘
                 │                 │
                 │                 │
     ┌───────────┴─────┬───────────┴──────┬──────────────┐
     ▼                 ▼                  ▼              ▼
┌─────────┐    ┌────────────────┐  ┌──────────┐  ┌────────────┐
│ 日志插件 │    │ rpi-extension  │  │ 鉴权插件 │  │ 自定义插件 │
│         │    │     -rpc       │  │          │  │            │
└─────────┘    └────────────────┘  └──────────┘  └────────────┘
```

### 3.2 核心流程

```
加载插件 → 分发 BeforeTuiStart → 进入 TUI → 分发 SessionStart 
       → Agent 循环（AgentStart/TurnEnd/AgentEnd/ToolExecute/ToolDestroy）
       → 分发 SessionShutdown
```

主程序仅调用 `dispatch(event, &mut ctx)`，具体行为由扩展决定。

### 3.3 关键设计原则

1. **主程序只广播，不判断**：不写“如果有 server 插件则连接”这类逻辑。
2. **扩展按需订阅**：不订阅的事件不会调用。
3. **资源类型擦除**：`EventContext` 支持任意类型传递，扩展与 TUI 无编译期依赖。
4. **默认降级**：无扩展时零开销、零报错。

---

## 4. 事件模型

### 4.1 事件类型

```rust
pub enum LifecycleEvent {
    // ─── 启动阶段 ───
    /// TUI 初始化前，用于建立连接、加载资源
    BeforeTuiStart,

    // ─── 会话阶段 ───
    /// 会话开始（TUI 已就绪）
    SessionStart,
    /// 会话关闭，用于清理资源
    SessionShutdown,

    // ─── Agent 阶段 ───
    /// Agent 循环开始
    AgentStart,
    /// 一轮结束
    TurnEnd,
    /// Agent 循环结束
    AgentEnd,

    // ─── 工具生命周期 ───
    /// 工具执行前
    ToolExecute { tool_name: String, args: serde_json::Value },
    /// 工具执行中轮询
    ToolPoll    { tool_name: String, elapsed_ms: u64 },
    /// 工具取消
    ToolCancel  { tool_name: String, reason: String },
    /// 工具销毁/完成
    ToolDestroy { tool_name: String, success: bool },

    // ─── 动态加载 ───
    PluginLoaded   { name: String },
    PluginUnloaded { name: String },
}
```

### 4.2 事件匹配（支持通配符）

```rust
impl LifecycleEvent {
    pub fn matches(&self, subscribed: &LifecycleEvent) -> bool {
        match (subscribed, self) {
            (
                Self::ToolExecute { tool_name: a, .. },
                Self::ToolExecute { tool_name: b, .. },
            )
            | (
                Self::ToolDestroy { tool_name: a, .. },
                Self::ToolDestroy { tool_name: b, .. },
            ) => a == "*" || a == b,
            (a, b) => std::mem::discriminant(a) == std::mem::discriminant(b),
        }
    }
}
```

### 4.3 事件上下文

`EventContext` 是类型擦除的资源容器，用于在主程序与扩展之间传递任意数据：

```rust
pub struct EventContext {
    resources: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
    pub abort: Option<String>,
}

impl EventContext {
    pub fn insert<T: Any + Send + Sync>(&mut self, value: T) {
        self.resources.insert(TypeId::of::<T>(), Box::new(value));
    }

    pub fn get<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.resources.get(&TypeId::of::<T>())?.downcast_ref()
    }

    pub fn take<T: Any + Send + Sync>(&mut self) -> Option<T> {
        self.resources
            .remove(&TypeId::of::<T>())
            .and_then(|b| b.downcast::<T>().ok())
            .map(|b| *b)
    }

    pub fn contains_type<T: Any + Send + Sync>(&self) -> bool {
        self.resources.contains_key(&TypeId::of::<T>())
    }

    pub fn abort(&mut self, reason: impl Into<String>) {
        self.abort = Some(reason.into());
    }
}
```

---

## 5. 插件接口

### 5.1 Plugin Trait

```rust
use async_trait::async_trait;

#[async_trait]
pub trait Plugin: Send + Sync {
    /// 插件名称，用于日志与去重
    fn name(&self) -> &str;

    /// 执行优先级：数值越小越先执行，默认 100
    fn priority(&self) -> i32 { 100 }

    /// 声明本插件关心的事件
    fn subscribed_events(&self) -> Vec<LifecycleEvent> { vec![] }

    /// 条件过滤器：返回 None 表示不限
    fn event_filter(&self, _event: &LifecycleEvent) -> Option<EventFilter> {
        None
    }

    /// 事件回调，默认继续
    #[allow(unused_variables)]
    async fn on_event(
        &self,
        event: &LifecycleEvent,
        ctx: &mut EventContext,
    ) -> HookResult {
        HookResult::Continue
    }
}

pub enum HookResult {
    /// 继续执行下一个插件
    Continue,
    /// 中止当前流程（如连接失败）
    Abort(String),
}
```

### 5.2 条件过滤器

```rust
pub struct EventFilter {
    /// 仅在这些平台上触发（空表示不限）
    pub platforms: Vec<Platform>,
    /// 仅当这些资源已存在于 ctx 时触发
    pub requires: Vec<TypeId>,
    /// 自定义谓词
    pub predicate: Option<Arc<dyn Fn(&EventContext) -> bool + Send + Sync>>,
}

pub enum Platform { Linux, MacOS, Windows, Android, Termux }

impl EventFilter {
    pub fn matches(&self, ctx: &EventContext) -> bool {
        if !self.platforms.is_empty()
            && !self.platforms.contains(&current_platform()) {
            return false;
        }
        if !self.requires.iter().all(|t| ctx.contains_type_by_id(*t)) {
            return false;
        }
        if let Some(p) = &self.predicate {
            if !p(ctx) { return false; }
        }
        true
    }
}
```

### 5.3 优先级约定

| 数值范围 | 用途 |
|---|---|
| 0–49 | 基础设施（日志、配置加载） |
| 50–99 | 连接建立（RPC、消息服务） |
| 100–199 | 常规扩展（默认） |
| 200+ | 后置逻辑、清理 |

---

## 6. 事件分发器

### 6.1 PluginRegistry

```rust
pub struct PluginRegistry {
    plugins: RwLock<Vec<Arc<dyn Plugin>>>,
    logger: Option<Arc<EventLogger>>,
}

impl PluginRegistry {
    pub fn load_all() -> anyhow::Result<Self> {
        let mut plugins = scan_and_load()?;
        plugins.sort_by_key(|p| p.priority());
        Ok(Self {
            plugins: RwLock::new(plugins),
            logger: EventLogger::from_env(),
        })
    }

    pub async fn dispatch(
        &self,
        event: &LifecycleEvent,
        ctx: &mut EventContext,
    ) -> anyhow::Result<()> {
        let plugins = self.plugins.read().unwrap().clone();
        for plugin in plugins {
            // 1. 订阅匹配
            if !plugin.subscribed_events()
                .iter()
                .any(|s| s.matches(event)) {
                continue;
            }

            // 2. 条件过滤
            if let Some(filter) = plugin.event_filter(event) {
                if !filter.matches(ctx) { continue; }
            }

            // 3. 执行（带超时）
            let timeout = timeout_for_event(event);
            let start = Instant::now();
            let result = tokio::time::timeout(
                timeout,
                plugin.on_event(event, ctx),
            ).await;
            let duration_ms = start.elapsed().as_millis() as u64;

            // 4. 记录日志
            if let Some(logger) = &self.logger {
                logger.record(EventLogEntry::from_result(
                    event, plugin.name(), &result, duration_ms,
                )).await;
            }

            // 5. 处理结果
            match result {
                Ok(HookResult::Continue) => continue,
                Ok(HookResult::Abort(reason)) => {
                    anyhow::bail!("插件 {} 中止: {reason}", plugin.name());
                }
                Err(_) => {
                    tracing::warn!(
                        "插件 {} 处理 {:?} 超时（{}ms）",
                        plugin.name(), event, duration_ms
                    );
                    continue;
                }
            }
        }
        Ok(())
    }

    /// 运行时添加插件（热插拔）
    pub async fn add(&self, plugin: Arc<dyn Plugin>) {
        {
            let mut plugins = self.plugins.write().unwrap();
            plugins.push(plugin.clone());
            plugins.sort_by_key(|p| p.priority());
        }
        let _ = self.dispatch(
            &LifecycleEvent::PluginLoaded { name: plugin.name().into() },
            &mut EventContext::default(),
        ).await;
    }

    /// 运行时移除插件
    pub async fn remove(&self, name: &str) {
        let removed = {
            let mut plugins = self.plugins.write().unwrap();
            plugins.iter()
                .position(|p| p.name() == name)
                .map(|pos| plugins.remove(pos))
        };
        if let Some(p) = removed {
            let _ = self.dispatch(
                &LifecycleEvent::PluginUnloaded { name: p.name().into() },
                &mut EventContext::default(),
            ).await;
        }
    }
}

fn timeout_for_event(event: &LifecycleEvent) -> Duration {
    match event {
        LifecycleEvent::BeforeTuiStart => Duration::from_secs(5),
        LifecycleEvent::SessionShutdown => Duration::from_secs(15),
        LifecycleEvent::ToolExecute { .. } => Duration::from_secs(60),
        _ => Duration::from_secs(10),
    }
}
```

---

## 7. 启动流程

### 7.1 时序图

```
main()
 │
 ├─ 1. PluginRegistry::load_all()
 │     └─ 扫描插件目录 → 加载 → 按 priority 排序
 │
 ├─ 2. 创建 EventContext
 │
 ├─ 3. dispatch(BeforeTuiStart)
 │     ├─ 遍历订阅该事件的插件
 │     ├─ 条件过滤（平台、资源、谓词）
 │     ├─ 带超时执行
 │     ├─ 若 Abort → 打印原因并退出
 │     └─ 扩展 insert 资源（如 RpcClient）
 │
 ├─ 4. 从 ctx.take::<RpcClient>()（可能为 None）
 │
 ├─ 5. setup_terminal()
 │
 ├─ 6. App::new(rpc_client)
 │
 ├─ 7. dispatch(SessionStart)
 │
 ├─ 8. run_app(...) ─── Agent 循环
 │     ├─ dispatch(AgentStart)
 │     ├─ dispatch(ToolExecute { ... })
 │     ├─ dispatch(ToolDestroy { ... })
 │     ├─ dispatch(TurnEnd)
 │     └─ dispatch(AgentEnd)
 │
 ├─ 9. restore_terminal()
 │
 └─ 10. dispatch(SessionShutdown) → 扩展清理资源
```

### 7.2 主程序实现

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. 加载插件
    let registry = PluginRegistry::load_all()?;

    // 2. 创建上下文
    let mut ctx = EventContext::default();

    // 3. 启动前事件
    registry.dispatch(&LifecycleEvent::BeforeTuiStart, &mut ctx).await?;

    // 4. 取出扩展注入的资源（可能为 None）
    let rpc_client: Option<RpcClient> = ctx.take();

    // 5. 进入 TUI
    let mut terminal = setup_terminal()?;
    let mut app = App::new(rpc_client);

    // 6. 会话开始
    registry.dispatch(&LifecycleEvent::SessionStart, &mut ctx).await?;

    // 7. 运行 TUI 事件循环
    let result = run_app(&mut terminal, &mut app, &registry, &mut ctx).await;

    // 8. 恢复终端
    restore_terminal(&mut terminal)?;

    // 9. 退出清理
    registry.dispatch(&LifecycleEvent::SessionShutdown, &mut ctx).await?;

    result
}
```

**主程序里没有一行代码提到 server、RPC 或连接**。它只知道“广播事件”。

---

## 8. 资源传递与类型擦除

### 8.1 扩展侧注入

```rust
// rpi-extension-rpc
async fn on_event(&self, event: &LifecycleEvent, ctx: &mut EventContext) -> HookResult {
    if let LifecycleEvent::BeforeTuiStart = event {
        let client = RpcClient::connect(&self.config).await
            .map_err(|e| HookResult::Abort(format!("RPC 连接失败: {e}")))?;
        ctx.insert(client);
    }
    HookResult::Continue
}
```

### 8.2 TUI 侧取出

```rust
// rpi-cli 的 App
pub struct App {
    client: Option<RpcClient>,
    // ...
}

impl App {
    pub fn new(client: Option<RpcClient>) -> Self {
        Self { client, /* ... */ }
    }

    fn server_mode(&self) -> bool {
        self.client.is_some()
    }
}
```

### 8.3 无扩展场景

- `plugins` 为空或无插件订阅 `BeforeTuiStart`；
- `ctx` 保持为空；
- `ctx.take::<RpcClient>()` 返回 `None`；
- TUI 以纯本地模式启动，**零开销、零报错**。

---

## 9. 错误处理与超时

| 场景 | 策略 |
|---|---|
| 扩展返回 `Abort` | 主程序打印原因并退出，不进入 TUI |
| 扩展执行超时 | 记录 warn 日志，跳过该扩展，继续下一个 |
| 扩展 panic | 建议扩展内部捕获；主程序可用 `catch_unwind` 兜底 |
| 无扩展订阅 | 直接返回，不产生任何等待 |
| `ctx` 无目标资源 | `take` 返回 `None`，TUI 降级处理 |

**超时策略按事件区分**（见 6.1 的 `timeout_for_event`）：启动前 5s、工具执行 60s、会话关闭 15s。

---

## 10. 完整生命周期集成

| 阶段 | 事件 | 典型用途 |
|---|---|---|
| TUI 启动前 | `BeforeTuiStart` | 建立 RPC 连接、加载远程配置 |
| 会话开始 | `SessionStart` | 注册会话级工具、初始化状态 |
| Agent 开始 | `AgentStart` | 准备上下文、注入系统提示 |
| 工具执行 | `ToolExecute` | 远程工具代理、权限校验 |
| 工具完成 | `ToolDestroy` | 审计日志、资源回收 |
| 一轮结束 | `TurnEnd` | 收集统计、持久化中间结果 |
| Agent 结束 | `AgentEnd` | 汇总结果、触发后续动作 |
| 会话关闭 | `SessionShutdown` | 断开连接、释放资源、保存状态 |
| 插件加载 | `PluginLoaded` | 初始化依赖、注册子事件 |
| 插件卸载 | `PluginUnloaded` | 清理资源、注销订阅 |

主程序在相应位置调用 `dispatch`，扩展按需订阅。

---

## 11. 事件持久化

### 11.1 目标

- 排查“为什么扩展没执行”；
- 复盘启动耗时；
- 审计扩展行为。

### 11.2 日志格式（JSONL）

写入 `~/.rpi/logs/events.jsonl`，每行一个 JSON：

```jsonl
{"ts":"2026-09-21T10:00:01Z","event":"BeforeTuiStart","plugin":"rpi-logging","result":"Continue","duration_ms":1}
{"ts":"2026-09-21T10:00:01Z","event":"BeforeTuiStart","plugin":"rpi-extension-rpc","result":"Continue","duration_ms":42}
{"ts":"2026-09-21T10:00:02Z","event":"ToolExecute","plugin":"rpi-extension-rpc","result":"Abort","error":"远程工具失败: timeout","duration_ms":60321}
```

### 11.3 实现

```rust
#[derive(Serialize)]
pub struct EventLogEntry {
    pub ts: chrono::DateTime<chrono::Utc>,
    pub event: String,
    pub plugin: String,
    pub result: &'static str,  // "Continue" | "Abort" | "Timeout"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub duration_ms: u64,
}

pub struct EventLogger {
    writer: tokio::sync::Mutex<BufWriter<tokio::fs::File>>,
}

impl EventLogger {
    pub fn from_env() -> Option<Arc<Self>> {
        if std::env::var("RPI_EVENT_LOG").ok().as_deref() != Some("1") {
            return None;
        }
        // 打开/创建 ~/.rpi/logs/events.jsonl
        // ...
        Some(Arc::new(Self { writer: /* ... */ }))
    }

    pub async fn record(&self, entry: EventLogEntry) {
        let mut w = self.writer.lock().await;
        let _ = serde_json::to_writer(&mut *w, &entry);
        let _ = tokio::io::AsyncWriteExt::write_all(&mut *w, b"\n").await;
        let _ = tokio::io::AsyncWriteExt::flush(&mut *w).await;
    }
}
```

### 11.4 消费

- `rpi events tail` 实时查看；
- `RPI_EVENT_LOG=1` 开启（默认关闭，避免写盘）。

---

## 12. 动态插件加载

### 12.1 技术选型

Rust ABI 不稳定，采用 **C ABI + `libloading`**。

### 12.2 插件侧导出

```rust
#[no_mangle]
pub extern "C" fn rpi_plugin_abi_version() -> u32 { 1 }

#[no_mangle]
pub extern "C" fn rpi_plugin_create() -> *mut dyn Plugin {
    Box::into_raw(Box::new(RpcExtension::new()))
}

#[no_mangle]
pub extern "C" fn rpi_plugin_destroy(ptr: *mut dyn Plugin) {
    if !ptr.is_null() {
        unsafe { drop(Box::from_raw(ptr)); }
    }
}
```

### 12.3 宿主侧加载器

```rust
pub struct DynamicPlugin {
    _lib: libloading::Library,
    plugin: Arc<dyn Plugin>,
    destroy: unsafe extern "C" fn(*mut dyn Plugin),
}

impl DynamicPlugin {
    pub unsafe fn load(path: &Path) -> anyhow::Result<Self> {
        let lib = libloading::Library::new(path)?;
        let abi: libloading::Symbol<unsafe extern "C" fn() -> u32> =
            lib.get(b"rpi_plugin_abi_version")?;
        if abi() != 1 {
            anyhow::bail!("ABI 版本不匹配: {}", abi());
        }
        let create: libloading::Symbol<unsafe extern "C" fn() -> *mut dyn Plugin> =
            lib.get(b"rpi_plugin_create")?;
        let destroy: libloading::Symbol<unsafe extern "C" fn(*mut dyn Plugin)> =
            lib.get(b"rpi_plugin_destroy")?;

        let raw = create();
        if raw.is_null() {
            anyhow::bail!("插件创建失败: {:?}", path);
        }
        let plugin = Arc::from_raw(raw);

        Ok(Self { _lib: lib, plugin, destroy: *destroy })
    }
}
```

### 12.4 热插拔约束

| 场景 | 处理 |
|---|---|
| 卸载订阅了未来事件的插件 | 后续 `dispatch` 不再调用它，资源由 `PluginUnloaded` 清理 |
| 插入订阅了已发生事件的插件 | 不补发；插件可在 `PluginLoaded` 自行处理 |
| 并发访问注册表 | `RwLock<Vec<Arc<dyn Plugin>>>`，dispatch 读锁，add/remove 写锁 |
| 恶意/不兼容插件 | ABI 版本校验；可选签名校验 |
| 沙箱隔离 | 同进程无法沙箱；需隔离时走 `rpi-extension-rpc` 跨进程 |

---

## 13. 示例：完整 RPC 扩展

```rust
#[async_trait]
impl Plugin for RpcExtension {
    fn name(&self) -> &str { "rpi-extension-rpc" }
    fn priority(&self) -> i32 { 60 } // 连接类

    fn subscribed_events(&self) -> Vec<LifecycleEvent> {
        vec![
            LifecycleEvent::BeforeTuiStart,
            LifecycleEvent::SessionShutdown,
            LifecycleEvent::ToolExecute { tool_name: "*".into(), args: json!(null) },
            LifecycleEvent::ToolDestroy { tool_name: "*".into(), success: false },
        ]
    }

    fn event_filter(&self, event: &LifecycleEvent) -> Option<EventFilter> {
        match event {
            LifecycleEvent::BeforeTuiStart => Some(EventFilter {
                platforms: vec![Platform::Linux, Platform::MacOS, Platform::Windows],
                requires: vec![],
                predicate: Some(Arc::new(|ctx| !ctx.contains_type::<RpcClient>())),
            }),
            _ => None,
        }
    }

    async fn on_event(
        &self,
        event: &LifecycleEvent,
        ctx: &mut EventContext,
    ) -> HookResult {
        match event {
            LifecycleEvent::BeforeTuiStart => {
                if !self.config.enabled { return HookResult::Continue; }
                match RpcClient::connect(&self.config).await {
                    Ok(c) => { ctx.insert(c); HookResult::Continue }
                    Err(e) => HookResult::Abort(format!("RPC 连接失败: {e}")),
                }
            }
            LifecycleEvent::SessionShutdown => {
                if let Some(c) = ctx.get::<RpcClient>() {
                    c.shutdown().await;
                }
                HookResult::Continue
            }
            LifecycleEvent::ToolExecute { tool_name, args } => {
                if let Some(c) = ctx.get::<RpcClient>() {
                    match c.call_tool(tool_name, args).await {
                        Ok(_) => HookResult::Continue,
                        Err(e) => HookResult::Abort(format!("远程工具失败: {e}")),
                    }
                } else {
                    HookResult::Continue
                }
            }
            _ => HookResult::Continue,
        }
    }
}
```

**行为分析**：

- 有该扩展：`BeforeTuiStart` 时连接成功 → 注入 `RpcClient` → TUI 启用 server 模式；
- 无该扩展：`BeforeTuiStart` 无人处理 → `ctx` 空 → TUI 纯本地模式；
- Termux：`event_filter` 因平台不匹配而跳过 → 不尝试连接 → 直接进 TUI。

---

## 14. 兼容性与降级

| 场景 | 行为 |
|---|---|
| **无 server 扩展** | `BeforeTuiStart` 无人处理，TUI 直接启动 |
| **Termux/Android** | 通过 `EventFilter.platforms` 跳过不兼容扩展 |
| **旧版插件** | 未实现 `subscribed_events` 默认返回空，不被调用 |
| **超时扩展** | 记录 warn，跳过，不影响其他扩展 |
| **`ctx` 资源缺失** | TUI 侧 `take` 返回 `None`，功能降级 |
| **编译期排除** | 可通过 Cargo feature 排除某些扩展 |

---

## 15. 优缺点

### 优点

- **彻底解耦**：主程序不感知任何具体扩展；
- **高度可扩展**：新增逻辑只需新增插件；
- **资源灵活传递**：类型擦除上下文支持任意资源；
- **零开销降级**：无扩展时无额外操作；
- **覆盖完整生命周期**：连接、会话、工具、清理统一管理；
- **可观测**：事件日志便于排查；
- **可演进**：优先级、条件、异步、动态加载均可平滑叠加。

### 缺点

- **复杂度增加**：引入事件总线与上下文，调试链路变长；
- **执行顺序需约定**：依赖优先级或加载顺序；
- **类型安全降低**：类型擦除需运行时 downcast，错误在运行时暴露；
- **文档要求高**：扩展开发者需理解事件模型；
- **动态加载需 ABI 约束**：C ABI 跨版本兼容需谨慎维护。

---

## 16. 演进路线

按依赖关系分阶段落地，每阶段可独立发布，向后兼容：

| 阶段 | 内容 | 依赖 | 状态 |
|---|---|---|---|
| **P0** | 会话生命周期接线：`SessionStart`/`SessionShutdown` + 新增 `BeforeTuiStart` 标签并分发 | 无 | **已实现（2026-09）** |
| **P1** | 事件结果/中止通道 + 按事件超时 | P0 | **已实现（2026-09）** |
| **P2** | 优先级 + 条件过滤 | P0 | **已实现（2026-09）** |
| **P3** | 事件持久化（JSONL） | P0、P1 | **已实现（2026-09）** |
| **P4** | 工具生命周期四阶段 | P0 | **已具备（现已有 4-函数工具 ABI + 现测事件）** |
| **P5** | 动态插件加载 | P0–P4 | **已具备**（现有 C ABI + libloading cdylib 体系） |

### 全部完成状态

P0–P5 均已落地（P5 早已具备）。剩余未采用项仅为 `EventContext`（跨 FFI 的
`Box<dyn Any>` 资源注入）与 `EventFilter.{requires,predicate}`——这两者在 C ABI 边界
上不可行，已在各自落地说明中标注。

> **P0 落地说明（2026-09）**：实际实现不是另起一个 EventBus，而是直接接入现有
> 34-tag 插件 ABI——`BeforeTuiStart` 以 `EventTag` 追加（discriminant 33，ABI 安全），
> `SessionStart`/`SessionShutdown` 两个已有标签在 `pi-cli` interactive 模式接线分发。
> 资源传递沿用 JSON/out-param 模型（未引入跨 FFI 的 `Box<dyn Any>`）。
>
> **P1 落地说明（2026-09）**：否决通道复用 SDK 已有的 `EventHandlerFn` 返回码契约
> （`EVENT_HANDLER_CONTINUE = 0` / `EVENT_HANDLER_ERROR = 1` / `EVENT_HANDLER_ABORT = 2`），
> **未改动 ABI 布局**。宿主侧新增 `dispatch_lifecycle_event`：在 blocking 线程上以
> 每事件预算（`BeforeTuiStart` 5s、`SessionShutdown` 15s、其他 10s）运行每个 handler，
> 超时则记日志跳过；首个返回 `EVENT_HANDLER_ABORT` 的 handler 中止扇出并返回否决原因
> （带扩展名）。`BeforeTuiStart` 否决 → 不进入 TUI、以退出码 3（`EXIT_VETOED`）退出；
> `SessionStart`/`SessionShutdown` 否决为**咨询性**（会话已起/已关，仅警告）。
>
> **P2 落地说明（2026-09）**：没有改动**冻结的** `PluginApiVt`，而是新增 ABI v3
> **入口点 + 侧结构体** `PluginApiVt3Ext`（`rpi_plugin_register_v3(api, ext, ver)`）。
> 插件通过 `ext.declare(json)` 在 register 期间声明 `{"priority":N,"platforms":[...]}`。
> 加载器按 v3 → v2 → v1 顺序惰性协商（找到高版本就不再往下看）；宿主将 priority /
> platforms 盖章到每个 handler，snapshot 按 priority 稳定排序（小者先跑），dispatch
> 跳过平台不符的 handler。`requires`/`predicate` 过滤器**未采用**——其 `TypeId`/
> `Arc<dyn Fn>` 无法跨 C ABI，在 ABI 模型下不可行（平台过滤已足够）。
>
> **P3 落地说明（2026-09）**：`RPI_EVENT_LOG=1` 时，每次 handler 调用（观察扇出 +
> 生命周期 veto 路径）追加一行 JSONL 到 `$RPI_EVENT_LOG_PATH` 或 `~/.rpi/logs/events.jsonl`，
> 字段：`ts_ms`/`event`/`plugin`/`result`/`duration_ms`/`detail`。默认关闭（零磁盘写）。
> 消费者：`rpi events tail`（实时跟随）、`rpi events path`。
>
> **P4 评估（2026-09）**：已满足，无需新代码。工具驱动的四阶段（execute/poll/cancel/
> destroy）已是现有 4-函数工具 ABI；观察侧由 `tool_execution_start`/`update`/`end` +
> `tool_call`/`tool_result` 覆盖，与 Pi 的 `tool_execution_*` 表面一致。文档中的
> `ToolCancel` 单独事件**刻意不新增**——Pi 也无此事件，新增会破坏 parity，且取消已由
> `tool_execution_end { is_error }` 表达。

### 关键里程碑

- **P0+P1 完成后**：解决“有 server 插件才连接，没有就不连”的核心诉求；
- **P2 完成后**：多扩展协作、平台差异化；
- **P3 完成后**：可观测性达标；
- **P4 完成后**：工具层面可扩展；
- **P5 完成后**：真正的插件运行时。

---

## 17. 总结

本设计通过引入**生命周期事件总线**，把“进入 TUI 前建立 RPC 连接”这一需求抽象为 `BeforeTuiStart` 事件。核心特性：

1. **主程序只广播事件**，不感知任何具体扩展；
2. **扩展按需订阅**，通过 `priority` 排序、`EventFilter` 过滤；
3. **资源类型擦除传递**，`RpcClient` 等由扩展注入、TUI 取出；
4. **异步 + 超时**，单个扩展卡死不阻塞整体；
5. **零开销降级**：无扩展时直接进 TUI，无连接、无等待、无报错；
6. **覆盖完整生命周期**：启动前、会话、Agent、工具、关闭；
7. **可观测 + 可热插拔**：事件日志 + 动态加载。

无扩展时，程序以纯本地模式启动；有扩展时，先执行连接，成功后再进入 TUI。该模型可自然扩展到 rpi 的所有生命周期阶段，同时保持主程序与具体扩展的彻底解耦。