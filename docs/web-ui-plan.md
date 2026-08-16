# hailux Web UI 改造方案

> 使用 React + assistant-ui + SSE + 无状态 HTTP，将 hailux 从纯终端应用扩展为 Web 应用，核心逻辑完全复用。

---

## 一、当前架构分析

### 技术栈

| 层 | 当前技术 | 说明 |
|---|---|---|
| LLM 交互 | `async-openai` + BYOT 自定义层 | 支持 DeepSeek `reasoning_content` |
| TUI | `ratatui` + `crossterm` | 终端渲染、事件循环 |
| 存储 | SQLite (`sqlx`) | 会话 + 消息 + 权限规则 |
| MCP | `rmcp` | Model Context Protocol 客户端 |
| 权限 | 自研 `PermissionManager` | 规则匹配 + 弹窗确认 + DB 持久化 |

### 核心耦合点

| 层 | 耦合程度 | 说明 |
|---|---|---|
| `agent/` → `tui/event.rs` | **强耦合** | `agent.rs`、`tools.rs`、`subagent.rs` 直接 `use crate::tui::event::{AppEvent, EventTx, ...}`，共引用 ~30 处 `AppEvent::*` |
| `AppEvent` 枚举 | **混合关注** | 终端事件（`InputKey`、`InputPaste`、`Resize`、`MouseClick`）和领域事件（`AgentChunk`、`ToolCallStart`、`PermissionRequest`）混在同一个 enum |
| `main.rs` | **TUI 绑定** | 所有 agent 构建逻辑直接创建 TUI 事件通道 |
| `permission/mod.rs` | **低耦合** | 自带 `auto_deny` 机制，权限交互通过 `oneshot` channel 回传，UI 无关 |
| `storage/db.rs` | **零耦合** | 纯 async SQLite，不依赖任何 UI 层 |
| `mcp/` | **零耦合** | 纯 `rmcp` + tokio |
| `config.rs` | **零耦合** | 独立的 TOML 配置读写 |

### 结论

**Agent 核心（LLM 交互 + 工具调用 + 权限管理）可以完整复用**，唯一障碍是事件系统与 TUI 耦合。需要抽离领域事件层。

---

## 二、目标技术栈

| 层 | 技术 | 说明 |
|---|---|---|
| **后端** | axum + tokio | SSE 响应流 + 无状态 REST API |
| **前端框架** | React 19 + TypeScript + Vite | SPA，build 产物嵌入 Rust 二进制 |
| **AI 聊天组件** | `@assistant-ui/react` | ChatGPT 级聊天 UX，支持流式、工具调用渲染、思维链 |
| **样式** | Tailwind CSS 4 + shadcn/ui | 原子化 CSS + 可定制组件（shadcn/ui 与 assistant-ui 均基于 Tailwind） |
| **状态管理** | Zustand | 轻量级，管理会话列表 / 消息流 / 权限请求 / UI 状态 |
| **Markdown 渲染** | react-markdown + remark-gfm + rehype-highlight | GFM 表格、任务列表、代码高亮 |
| **工具库** | lucide-react + clsx + tailwind-merge | 图标、条件类名合并 |
| **嵌入** | `rust-embed` | 将 `dist/` 编译进单一二进制 |
| **通信** | SSE (Server-Sent Events) + REST | 无状态 HTTP，每个请求独立 |

### 前端技术选型说明

**React 19 + TypeScript**
- 纯函数组件 + Hooks：SSE 流式消息通过 `useState` / `useRef` 累积更新，无类组件
- 协议类型安全：`protocol.rs` 中的 JSON 类型在 `src/runtime/types.ts` 中一一对应（`ServerEvent`、`ChatRequest` 等）
- Vite 提供毫秒级 HMR，开发时与 Rust 后端（`:18080`）通过 proxy 独立热更新

**Tailwind CSS 4**
- 采用 v4 的 **CSS-first 配置**：`@import "tailwindcss"` + `@theme { ... }`，**无需** `tailwind.config.ts` 和 PostCSS
- 通过 `@tailwindcss/vite` 插件直接集成到 Vite，零额外构建链
- shadcn/ui 和 assistant-ui 的样式体系均基于 Tailwind，主题变量天然统一

**shadcn/ui**
- 不是 npm 依赖，而是把组件源码（Radix UI + CVA 实现）复制进 `src/components/ui/`，可自由定制
- 本项目需要的组件：`button`、`dialog`（权限/提问弹窗）、`collapsible`（工具卡片展开收起）、`dropdown-menu`（模型选择器）、`scroll-area`（消息滚动）、`tooltip`、`badge`（状态标记）、`skeleton`（加载占位）

**Zustand**
- 会话列表、当前会话、消息流、待处理的权限请求等跨组件状态统一放 store，避免 props 层层传递
- 数据流清晰：SSE 事件 → store action → React 组件自动重渲染 → `useExternalStoreRuntime` 同步给 assistant-ui

---

## 三、通信架构

### 设计原则

- **完全无状态**：每个 HTTP 请求独立处理，不绑定连接
- **SSE 单向推送**：`POST /api/chat` 的响应是一个 SSE 流，服务端 → 客户端单向推送事件
- **REST 回复**：权限确认、用户提问等交互通过独立 POST 请求回复，用 `request_id` 关联

### 请求流转

```
用户输入消息
  │
  ▼
POST /api/chat { message, session_id }
  │
  ▼
┌─────────────────────────────────────────────┐
│  Rust 后端                                   │
│  1. ChatSession.send_message() 启动 Agent    │
│  2. Agent 流式执行，事件流入 mpsc channel     │
│  3. channel → SSE 流 → HTTP 响应             │
│                                              │
│  Agent 遇到权限请求时:                        │
│  ├── 创建 oneshot channel                    │
│  ├── sender 存入 TaskRegistry[request_id]    │
│  ├── SSE 推送 PermissionRequest 事件         │
│  └── Agent task 阻塞等待 receiver            │
│                                              │
│  前端回复权限时:                              │
│  POST /api/permission/:id/reply              │
│  ├── 取出 TaskRegistry[:id] 的 sender        │
│  ├── 发送回复                                │
│  └── Agent task 唤醒，SSE 继续推送           │
└─────────────────────────────────────────────┘
  │
  ▼
SSE 流关闭 (AgentComplete)
```

### 与 WebSocket 方案对比

| 维度 | SSE + REST | WebSocket |
|---|---|---|
| 后端复杂度 | **低** — axum 内置 SSE，无连接管理 | 中 — 需 WebSocket upgrade + 连接生命周期 |
| 前端复杂度 | **低** — fetch + ReadableStream | 中 — WebSocket 连接管理 + 重连 |
| 权限/提问交互 | request_id 关联 + 独立 POST | 同连接回传（看似简单，实则耦合） |
| 无状态 | ✅ 每个 HTTP 请求独立 | ❌ 连接有状态 |
| 可调试性 | ✅ curl 直接测试 SSE | ❌ 需要 WebSocket 客户端 |
| 重连恢复 | `Last-Event-ID` 头（SSE 原生支持） | 需自行实现 |
| proxy/CDN 友好 | ✅ 标准 HTTP | 需特殊配置 |
| assistant-ui 兼容 | ✅ `useExternalStoreRuntime` 完美适配 | ✅ 同样适配 |

---

## 四、API 设计

### SSE 端点

```
POST /api/chat
  Body: { message: string, session_id?: string }
  Response: text/event-stream (SSE)

  SSE 事件:
  data: {"type":"AgentChunk","text":"Hello"}
  data: {"type":"AgentReasoningChunk","text":"Thinking..."}
  data: {"type":"ToolCallStart","name":"bash","arguments":"{...}"}
  data: {"type":"ToolResult","name":"bash","result":"..."}
  data: {"type":"PermissionRequest","request_id":"abc","description":"..."}
  data: {"type":"AskUser","request_id":"xyz","questions":[...]}
  data: {"type":"UsageUpdate","prompt_tokens":1234,"completion_tokens":567}
  data: {"type":"AgentComplete","status":"completed"}
  (stream closes)
```

> 前端用 `fetch().body.getReader()` 读取（不用 `EventSource`，因为 `EventSource` 只支持 GET）。

### REST 端点

```
# 会话管理
GET    /api/sessions                       → 会话列表（含 work_dir 字段，?work_dir= 过滤）
POST   /api/sessions                       → 新建会话 { work_dir?: string }（缺省 = 服务器启动目录）
GET    /api/sessions/:id                   → 会话历史消息
DELETE /api/sessions/:id                   → 删除会话

# 工作目录
GET    /api/workdirs                      → 最近使用的 work_dir 列表（sessions 表 DISTINCT）
POST   /api/workdirs/validate             → 校验目录存在并返回绝对路径 { path }
GET    /api/fs?path=&dirs_only=true       → 列出子目录（目录选择器浏览用）

# 文件提及（@ 补全）
GET    /api/files?q=keyword                → 工作区文件路径列表（大小写不敏感子串匹配）

# 运行时控制
POST   /api/interrupt                      → 中断当前对话 { session_id }
POST   /api/permission/:request_id/reply   → 权限回复 { allow, always }
POST   /api/ask/:request_id/reply          → 提问回复 { answer }

# 配置
GET    /api/models                         → 模型列表
PUT    /api/models                         → 添加/修改模型
GET    /api/skills                         → 可用技能列表
GET    /api/mcp                            → MCP 状态

# 模式
POST   /api/plan-mode                      → { enabled: bool }
POST   /api/yolo                           → { enabled: bool }

# 压缩
POST   /api/compact                        → 触发上下文压缩 { session_id }
```

---

## 五、详细改造步骤（7 个阶段）

### 阶段 0：源码层级重构（动工前置，对齐开源惯例）

对标 ripgrep / bat / fd 等单 crate 项目的标准形态，先做三件事：

1. **拆出 `src/lib.rs`**：`main.rs` 变薄入口（CLI 解析 → 调用 `hailux::tui::run` / `hailux::web::run`），模块声明全部移入 lib.rs。这是集成测试（`tests/`）和 web feature 门控的前提
2. **新增 `tests/`**：集成测试目录（SSE 流、权限往返、会话切换等端到端路径），与现有 inline 单测并存
3. **命名规范**：编排层目录叫 `session/` 而非 `core/`（"core" 在 Rust 生态指领域内核，hailux 的内核是 `agent/`）；前端目录单层 `web/`（Rust 侧）而非 `web/frontend/` 嵌套

**最终源码结构**：

```
src/
├── main.rs              薄入口：CLI 解析 → run_tui / run_web / run（单次）
├── lib.rs               模块声明 + pub use（新增）
│
├── agent/               领域内核：LLM 交互 + 工具 + 事件出口
│   ├── (现有文件)
│   └── event.rs         CoreEvent —— agent 自定义的事件出口（新增）
├── session/             编排层（新增）：UI 无关
│   ├── mod.rs
│   ├── session.rs       ChatSession：Agent + skills/subagents/commands 发现
│   └── manager.rs       SessionManager：work_dir → ChatSession 惰性注册表
│
├── tui/                 表现层（保留）：AppEvent 包装 CoreEvent + 终端事件
├── web/                 表现层（新增，feature = "web" 门控）：
│   ├── mod.rs           server 启动、路由、静态资源嵌入
│   ├── sse.rs / handlers.rs / task_registry.rs / protocol.rs / state.rs
├── permission/ storage/ mcp/ prompts/ config.rs updater.rs   不变
tests/                   集成测试（新增）
web/                     React 前端（独立 npm 项目，dist/ 由 rust-embed 嵌入）
build.rs                 feature 开启且 dist/ 缺失时自动 npm build
```

**层级依赖规则**（只允许向下）：

| 层 | 可依赖 | 禁止 |
|---|---|---|
| `tui/` `web/` | `session/`、`agent/`、领域层 | 互相依赖 |
| `session/` | `agent/`、领域层 | import tui/web |
| `agent/` | 领域层（permission/storage/mcp/config/prompts） | import tui/web/session |
| 领域层 | 彼此 | 知道任何上层存在 |

> 注意 `CoreEvent` 放在 `agent/event.rs` 而非 session/ —— agent 定义事件出口，session/tui/web 都是消费者。若放在 session/ 会导致 `agent → session` 反向依赖（session 构建时需要 agent），依赖图成环。

### 阶段 1：抽离事件出口（解耦 agent → tui）

**目标**：让 `agent/` 模块不再依赖 `tui/`，且事件出口归 agent 自己所有

**新建 `src/agent/event.rs`**（从 `tui::event::AppEvent` 中提取所有领域事件）：

```rust
// 从 tui::event::AppEvent 中提取所有领域事件
pub enum CoreEvent {
    AgentChunk(String),
    AgentReasoningChunk(String),
    AgentComplete {
        messages: Vec<SharedMessage>,
        usages: Vec<MessageUsage>,
        status: TaskStatus,
    },
    UsageUpdate {
        prompt_tokens: u32,
        completion_tokens: u32,
    },
    PersistMessage {
        msg: SharedMessage,
        usage: Option<(u32, u32)>,
        display: Option<String>,
    },
    ToolCallStart {
        name: String,
        arguments: String,
        subagent_name: Option<String>,
    },
    ToolResult {
        name: String,
        result: String,
        display: Option<String>,
        subagent_name: Option<String>,
    },
    AskUser {
        questions: Vec<QuestionInfo>,
        response_tx: oneshot::Sender<String>,
    },
    PermissionRequest {
        request: PermissionRequest,
        response_tx: oneshot::Sender<PermissionReply>,
        subagent_name: Option<String>,
    },
    CompactChunk(String),
    CompactComplete {
        summary: String,
        session_id: String,
        usage: Option<CompactUsage>,
    },
    CompactError(String),
}

pub type CoreEventTx = mpsc::Sender<CoreEvent>;
pub type CoreEventRx = mpsc::Receiver<CoreEvent>;
```

**改造后的 `AppEvent`（`tui/event.rs`）**：

```rust
pub enum AppEvent {
    // 包装领域事件
    Core(CoreEvent),
    // TUI 专有事件
    InputKey(KeyEvent),
    InputPaste(String),
    UserSubmit(String),
    Resize,
    ScrollUp,
    ScrollDown,
    MouseClick,
    McpReady(Vec<McpConnection>),
}
```

**修改的文件**：

| 文件 | 改动 | 说明 |
|---|---|---|
| `agent/agent.rs` | `use crate::tui::*` → `use crate::agent::event::*` | ~20 处 `AppEvent::` → `CoreEvent::` |
| `agent/tools.rs` | 同上 | ~3 处 |
| `agent/subagent.rs` | 同上 | ~15 处 |
| `tui/event.rs` | 拆分 AppEvent，包装 Core | 增加转发逻辑 |
| `tui/app/chat_events.rs` | match 增加 `AppEvent::Core(e)` 分支 | 在入口处加一层解包 |

TUI 的 `chat_events.rs` 解包示例：

```rust
match event {
    AppEvent::Core(e) => self.handle_core_event(e).await,
    AppEvent::UserSubmit(input) => { ... }
    // ...其他 TUI 事件不变
}
```

### 阶段 2：封装 ChatSession 管理层

**目标**：封装 "Agent + Storage + MCP + Permission + Session" 的组合，让 TUI 和 Web 共用

**关键变化：work_dir 从进程级变为会话级**。TUI 中 work_dir 是启动时常量（`resolve_work_dir()`），Web 端需要支持每个会话使用不同工作目录 —— 新建会话时选择目录，后端为每个 work_dir 惰性构建独立的 `ChatSession`（skills / subagents / commands / AGENTS.md 发现和 system prompt 都依赖 work_dir，必须重建），而 Storage / Config / MCP 连接全局共享。

**新建 `src/session/session.rs`**：

```rust
pub struct ChatSession {
    agent: Agent,
    storage: ChatStorage,          // 共享
    session_id: Option<String>,
    work_dir: PathBuf,             // ← 本会话的工作目录
    subagents: Vec<SubagentConfig>,
    skills: Vec<SkillInfo>,
    command_registry: CommandRegistry,
    mcp_backends: SharedMcpBackends, // 共享
    config: Arc<Mutex<Config>>,    // 共享
    resolved: ResolvedModel,
}

impl ChatSession {
    /// 从 main.rs::build_agent() + run_tui() 中提取共享逻辑。
    /// 按 work_dir 完成全部发现工作（skills/agents/commands/AGENTS.md）并构建 system prompt
    pub async fn new(cfg: Config, resolved: ResolvedModel, work_dir: PathBuf) -> Result<Self>;
    pub async fn connect_mcp(&mut self) -> Vec<McpConnection>;

    // 消息
    pub fn send_message(&mut self, msg: &str, tx: CoreEventTx) -> Result<()>;
    pub fn interrupt(&self);
    pub fn sync_messages(&mut self, messages: Vec<SharedMessage>);

    // 会话管理（session 记录带上本会话的 work_dir）
    pub async fn new_session(&mut self) -> Result<String>;
    pub async fn switch_session(&mut self, id: &str) -> Result<Vec<StoredMessage>>;
    pub async fn list_sessions(&self) -> Result<Vec<SessionSummary>>;

    // 模式
    pub fn set_plan_mode(&mut self, on: bool);
    pub fn set_yolo(&mut self, on: bool);
    pub fn switch_model(&mut self, config: OpenAIConfig, model: &str, max_tokens: u32);

    // 压缩
    pub fn request_compaction(&self, tx: CoreEventTx, session_id: String) -> Result<()>;
}
```

**新建 `src/session/manager.rs`**（Web 模式的多目录管理器）：

```rust
/// work_dir → ChatSession 的惰性注册表。
/// TUI 模式只有一个条目（保持现有行为）；Web 模式每个工作目录一个实例。
pub struct SessionManager {
    sessions: RwLock<HashMap<PathBuf, Arc<Mutex<ChatSession>>>>,
    storage: ChatStorage,
    config: Arc<Mutex<Config>>,
    mcp: SharedMcpBackends,
}

impl SessionManager {
    /// 已缓存的直接返回；新 work_dir 惰性构建（含目录校验）
    pub async fn get_or_create(&self, work_dir: &Path) -> Result<Arc<Mutex<ChatSession>>>;
    pub async fn list_work_dirs(&self) -> Result<Vec<String>>; // sessions 表 DISTINCT work_dir
    /// 切换会话时路由到对应 work_dir 的 ChatSession 实例
    pub async fn resolve_session(&self, session_id: &str) -> Result<Arc<Mutex<ChatSession>>>;
}
```

> **HashMap 键必须归一化**：Windows 下同一目录可能以 `D:\project\hailux`、`d:/project/hailux`、`\\?\D:\project\hailux` 三种形式出现（本仓库 cwd 就带 `\\?\` 前缀）。键统一用 canonicalize 后的路径，否则同一目录会构建多个 ChatSession 实例。
>
> **锁粒度按 session 而非 work_dir**：`get_or_create` 返回的锁在单次请求处理期间持有（send → SSE 流结束），同一 work_dir 的不同会话可并行；同一 session 的并发消息定义为排队（后续请求等待前一个 AgentComplete）。

**`main.rs` 大幅简化**：`build_agent` / `build_agent_base` 逻辑移入 `ChatSession::new()`，`run_tui()`（单目录，行为不变）和 `run_web()`（SessionManager 多目录）各自调用。

### 阶段 3：Web 后端（axum SSE + REST）

**新建 `src/web/`**：

```
src/web/
├── mod.rs              — server 启动、路由注册、静态资源嵌入
├── sse.rs              — SSE handler: POST /api/chat → Sse<Stream>
├── task_registry.rs    — 内存任务注册表 (request_id → oneshot Sender)
├── handlers.rs         — REST handlers: sessions, models, permission reply, etc.
├── protocol.rs         — SSE 事件 + 请求/响应 JSON 类型
└── state.rs            — WebServerState { session, registry, ... }
```

#### 新增依赖

```toml
# Cargo.toml — web 相关依赖全部挂在 feature 后，
# 纯 TUI 用户/贡献者无需 Node 和 web 依赖链即可构建
[features]
default = ["web"]
web = ["dep:axum", "dep:tower-http", "dep:rust-embed", "dep:async-stream"]

[dependencies]
axum = { version = "0.8", optional = true }        # 不需要 ws feature
tower-http = { version = "0.6", features = ["cors"], optional = true }
rust-embed = { version = "8", features = ["mime-guess"], optional = true }
async-stream = { version = "0.3", optional = true }
```

`src/web/` 整个模块用 `#[cfg(feature = "web")]` 门控，`main.rs` 的 `Web` 子命令同理（关闭 feature 时提示重新编译）。

#### TaskRegistry — 权限/提问的异步回复桥接

```rust
// src/web/task_registry.rs
pub struct TaskRegistry {
    tasks: Arc<Mutex<HashMap<String, PendingReply>>>,
}

enum PendingReply {
    Permission(oneshot::Sender<PermissionReply>),
    AskUser(oneshot::Sender<String>),
}

impl TaskRegistry {
    pub fn insert_permission(&self, id: &str, tx: oneshot::Sender<PermissionReply>);
    pub fn take_permission(&self, id: &str) -> Option<oneshot::Sender<PermissionReply>>;
    pub fn insert_ask(&self, id: &str, tx: oneshot::Sender<String>);
    pub fn take_ask(&self, id: &str) -> Option<oneshot::Sender<String>>;
}
```

> **断连防泄漏（关键）**：SSE 流随时可能断开（关标签页、刷新、网络抖动），前端死后无人 POST 回复，agent task 会永远阻塞在 `oneshot::Receiver` 上。因此 registry 条目必须挂接连接生命周期：SSE stream 被 drop 时（axum 感知客户端断开），该连接注册的所有 pending permission/ask 自动以 `Deny` / 空回复终结 —— 实现为 per-connection 的 drop guard（条目记录所属连接 ID，连接流结束时批量清理），而不是只靠裸 HashMap + 超时。TUI 无此问题（模态弹窗必有终局），但 Deny 兜底同样适用 `auto_deny` 语义。

#### SSE Handler

```rust
// src/web/sse.rs
async fn chat_handler(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<ChatRequest>,
) -> impl IntoResponse {
    let (event_tx, mut event_rx) = mpsc::channel(4096);

    // 启动 agent 流式处理（独立 task）
    let session = state.session.clone();
    let registry = state.registry.clone();
    tokio::spawn(async move {
        // 将 event_tx 注册到 registry，供 permission/ask handler 使用
        registry.set_event_tx(session_id, event_tx.clone());
        // agent 发送消息，事件流入 event_tx
        session.send_message(&req.message, event_tx).await;
    });

    // 将 mpsc Receiver 转为 SSE 流
    let stream = async_stream::stream! {
        while let Some(event) = event_rx.recv().await {
            let json = serde_json::to_string(&event).unwrap();
            yield Ok::<_, std::io::Error>(
                Event::default().data(json)
            );
        }
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}
```

#### 权限回复 Handler

```rust
// src/web/handlers.rs
async fn permission_reply(
    State(state): State<Arc<WebServerState>>,
    Path(request_id): Path<String>,
    Json(reply): Json<PermissionReplyBody>,
) -> impl IntoResponse {
    match state.registry.take_permission(&request_id) {
        Some(tx) => {
            let reply = if reply.allow {
                if reply.always { PermissionReply::Always }
                else { PermissionReply::Once }
            } else {
                PermissionReply::Deny
            };
            let _ = tx.send(reply);
            StatusCode::OK
        }
        None => StatusCode::NOT_FOUND,
    }
}
```

#### 前后端 JSON 协议定义

```rust
// src/web/protocol.rs

// SSE 推送的事件（后端 → 前端）
#[serde(tag = "type")]
pub enum ServerEvent {
    AgentChunk { text: String },
    AgentReasoningChunk { text: String },
    AgentComplete { status: String },
    ToolCallStart { name: String, arguments: String, subagent: Option<String> },
    ToolResult { name: String, result: String, display: Option<String> },
    PermissionRequest { request_id: String, description: String, patterns: Vec<String> },
    AskUser { request_id: String, questions: Vec<QuestionInfo> },
    UsageUpdate { prompt_tokens: u32, completion_tokens: u32 },
    Error { message: String },
}

// 请求体（前端 → 后端）
#[derive(Deserialize)]
pub struct ChatRequest {
    pub message: String,
    pub session_id: Option<String>,
}

#[derive(Deserialize)]
pub struct PermissionReplyBody {
    pub allow: bool,
    pub always: bool,
}
```

### 阶段 4：React 前端（assistant-ui）

#### 项目结构

```
web/                            ← 单层目录（非 web/frontend/）
├── package.json
├── vite.config.ts                  — React + Tailwind 4 插件 + /api proxy
├── tsconfig.json
├── index.html
└── src/
    ├── main.tsx                    — React 入口
    ├── App.tsx                     — 布局：侧边栏 + 聊天区
    ├── runtime/
    │   ├── sse-client.ts           — fetch + ReadableStream SSE 读取
    │   ├── hailux-runtime.tsx      — useExternalStoreRuntime 适配器
    │   └── types.ts                — 消息类型定义（对应 protocol.rs）
    ├── store/
    │   └── app-store.ts            — Zustand：会话/消息/权限请求状态
    ├── components/
    │   ├── ui/                     — shadcn/ui 组件（源码复制，可定制）
    │   ├── sidebar.tsx             — 会话列表（按 work_dir 分组）、新建会话
    │   ├── workdir-selector.tsx    — 工作目录选择器（新建会话时）
    │   ├── chat-thread.tsx         — assistant-ui Thread 封装
    │   ├── command-menu.tsx        — 斜杠命令补全面板（/ 前缀触发）
    │   ├── file-mention.tsx        — @ 文件提及选择器
    │   ├── tool-ui/
    │   │   ├── bash-tool.tsx       — bash 命令展示（可展开看输出）
    │   │   ├── edit-tool.tsx       — 文件编辑 diff 展示
    │   │   ├── read-tool.tsx       — 文件读取展示
    │   │   ├── write-tool.tsx      — 文件写入展示
    │   │   ├── grep-tool.tsx       — 搜索结果展示
    │   │   ├── web-fetch-tool.tsx  — 网页抓取展示
    │   │   ├── todo-tool.tsx       — Todo 列表展示
    │   │   ├── task-tool.tsx       — Subagent 任务展示
    │   │   └── index.tsx           — 工具 UI 注册表
    │   ├── permission-dialog.tsx   — 权限确认弹窗（shadcn Dialog）
    │   ├── ask-user-dialog.tsx     — 用户提问弹窗（shadcn Dialog）
    │   ├── model-picker.tsx        — 模型切换（shadcn DropdownMenu）
    │   ├── mode-toggle.tsx         — Plan / YOLO 模式切换
    │   └── status-bar.tsx          — Token 用量、MCP 状态
    └── styles/
        └── globals.css             — Tailwind 4 CSS-first 配置 + 主题变量
```

#### assistant-ui 集成 — `useExternalStoreRuntime`

assistant-ui 提供 `useExternalStoreRuntime`，完美适配自定义 SSE 后端：

```
hailux Agent (Rust)
  ↓ SSE 响应流 (JSON events)
React State (messages[], isRunning)
  ↓ useExternalStoreRuntime
assistant-ui <Thread /> 组件
  → 自动渲染: 流式文本、Markdown、代码高亮、思维链折叠、工具调用卡片
```

**映射关系**：

| hailux SSE 事件 | assistant-ui 消息类型 |
|---|---|
| `AgentChunk` | Assistant 消息的流式 text part |
| `AgentReasoningChunk` | Assistant 消息的 reasoning part（自动折叠） |
| `ToolCallStart` | `tool-call` part（status: running） |
| `ToolResult` | tool-call part（status: complete + result） |
| `PermissionRequest` | 自定义 tool-call UI（人机交互卡片） |
| `AskUser` | 自定义弹窗 / standalone tool UI |
| `AgentComplete` | isRunning = false |

#### 核心代码：`hailux-runtime.tsx`

```tsx
import { useState, useCallback } from "react";
import {
  useExternalStoreRuntime,
  ThreadMessageLike,
  AppendMessage,
  AssistantRuntimeProvider,
} from "@assistant-ui/react";

export function HailuxRuntimeProvider({ children }: { children: ReactNode }) {
  const [messages, setMessages] = useState<ThreadMessageLike[]>([]);
  const [isRunning, setIsRunning] = useState(false);
  const [permissionRequest, setPermissionRequest] = useState<PermissionRequest | null>(null);

  // SSE 流式读取
  const onNew = useCallback(async (message: AppendMessage) => {
    const text = message.content[0]?.type === "text" ? message.content[0].text : "";
    if (!text) throw new Error("Only text messages are supported");

    setIsRunning(true);
    setMessages(prev => [...prev, {
      role: "user",
      content: [{ type: "text", text }],
    }]);

    // POST + SSE 流式读取
    const response = await fetch("/api/chat", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ message: text }),
    });

    const reader = response.body!.getReader();
    const decoder = new TextDecoder();
    let buffer = "";

    while (true) {
      const { done, value } = await reader.read();
      if (done) break;

      buffer += decoder.decode(value, { stream: true });
      const events = buffer.split("\n\n");
      buffer = events.pop() || "";

      for (const raw of events) {
        if (!raw.startsWith("data: ")) continue;
        const event: ServerEvent = JSON.parse(raw.slice(6));
        handleServerEvent(event, setMessages, setPermissionRequest);
      }
    }

    setIsRunning(false);
  }, []);

  const onCancel = useCallback(async () => {
    await fetch("/api/interrupt", { method: "POST" });
  }, []);

  const runtime = useExternalStoreRuntime({
    isRunning,
    messages,
    convertMessage,
    onNew,
    onCancel,
  });

  return (
    <AssistantRuntimeProvider runtime={runtime}>
      {children}
      {permissionRequest && (
        <PermissionDialog
          request={permissionRequest}
          onClose={() => setPermissionRequest(null)}
        />
      )}
    </AssistantRuntimeProvider>
  );
}
```

#### 权限弹窗 — POST 回复

```tsx
function PermissionDialog({ request, onClose }: Props) {
  const handleReply = async (allow: boolean, always: boolean) => {
    await fetch(`/api/permission/${request.request_id}/reply`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ allow, always }),
    });
    onClose();
  };

  return (
    <Dialog>
      <DialogHeader>{request.description}</DialogHeader>
      <DialogFooter>
        <Button onClick={() => handleReply(false, false)}>拒绝</Button>
        <Button onClick={() => handleReply(true, false)}>允许一次</Button>
        <Button onClick={() => handleReply(true, true)}>始终允许</Button>
      </DialogFooter>
    </Dialog>
  );
}
```

#### 工具 UI 示例 — BashToolUI

```tsx
import type { ToolCallMessagePartComponent } from "@assistant-ui/react";

const BashToolUI: ToolCallMessagePartComponent<
  { command: string; workdir?: string },
  { output: string }
> = ({ args, status, result }) => {
  return (
    <CollapsibleCard
      title={`bash: ${args.command?.slice(0, 60) ?? ""}`}
      status={status.type}  // running | complete | error
    >
      <pre className="bg-gray-900 text-green-400 p-3 rounded overflow-x-auto">
        {result?.output ?? "执行中..."}
      </pre>
    </CollapsibleCard>
  );
};
```

### 阶段 5：静态资源嵌入 + CLI 入口

#### `Cargo.toml` 完整新增依赖

```toml
[features]
default = ["web"]
web = ["dep:axum", "dep:tower-http", "dep:rust-embed", "dep:async-stream"]

[dependencies]
axum = { version = "0.8", optional = true }
tower-http = { version = "0.6", features = ["cors"], optional = true }
rust-embed = { version = "8", features = ["mime-guess"], optional = true }
async-stream = { version = "0.3", optional = true }
```

#### 嵌入资源 (`src/web/mod.rs`)

```rust
#[derive(rust_embed::RustEmbed)]
#[folder = "web/dist"]
struct WebAssets;

// 路由:
// GET /          → index.html
// GET /assets/*  → JS/CSS/图片等静态资源
// 开发模式可 proxy 到 Vite dev server (localhost:5173)
```

#### CLI 新增子命令 (`main.rs`)

```rust
#[derive(Subcommand)]
enum Commands {
    /// 非交互模式：发送单条消息并打印回复
    Run {
        message: Option<String>,
        #[arg(short = 'm', long = "model")]
        model: Option<String>,
        #[arg(long = "no-tools")]
        no_tools: bool,
    },
    /// 启动 Web UI 服务器
    Web {
        #[arg(short, long, default_value = "127.0.0.1")]
        host: String,
        #[arg(short, long, default_value = "18080")]
        port: u16,
        #[arg(long)]
        open: bool,  // 自动打开浏览器
    },
}
```

### 阶段 6：构建集成

#### 前端项目初始化

```bash
# 创建 Vite + React + TypeScript 项目
cd web
npm create vite@latest . -- --template react-ts

# 安装核心依赖
npm install @assistant-ui/react
npm install tailwindcss @tailwindcss/vite
npm install react-markdown remark-gfm rehype-highlight
npm install lucide-react clsx tailwind-merge
npm install zustand

# shadcn/ui 初始化（组件按需添加：npx shadcn@latest add dialog collapsible ...）
npx shadcn@latest init
```

#### `vite.config.ts`

```ts
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "path";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: { "@": path.resolve(__dirname, "./src") },
  },
  server: {
    proxy: {
      // 开发模式：/api/* 转发到 Rust 后端，前后端独立热更新
      "/api": "http://localhost:18080",
    },
  },
});
```

#### `src/styles/globals.css`（Tailwind 4 CSS-first 配置）

```css
@import "tailwindcss";

@theme {
  --color-background: #0d1117;
  --color-foreground: #e6edf3;
  --color-primary: #2f81f7;
  --color-muted: #8b949e;
  --font-mono: "JetBrains Mono", "Cascadia Code", monospace;
}

/* 工具输出 / diff / 代码块通用样式 */
.hlx-code-block {
  @apply overflow-x-auto rounded-md bg-gray-900 p-3 font-mono text-sm;
}
```

> **注意**：Tailwind 4 无需 `tailwind.config.ts` —— 主题变量、自定义工具类直接写在 CSS 中，由 `@tailwindcss/vite` 插件编译。shadcn/ui 初始化时也选择 "Tailwind CSS v4" 配置模式。

#### `build.rs`（可选自动构建前端）

```rust
use std::path::Path;
use std::process::Command;

fn main() {
    // 仅在 web feature 开启时要求前端产物；纯 TUI 构建零 Node 依赖
    println!("cargo:rerun-if-changed=web/dist");
    if std::env::var("CARGO_FEATURE_WEB").is_err() {
        return;
    }
    let dist = Path::new("web/dist");
    if !dist.exists() {
        println!("cargo:warning=Building frontend...");
        Command::new("npm")
            .args(["run", "build"])
            .current_dir("web")
            .status()
            .expect("Failed to build frontend. Run `cd web && npm install && npm run build` manually.");
    }
}
```

#### CI 集成 (`.github/workflows/ci.yml`)

```yaml
jobs:
  build:
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        os: [ubuntu-latest, windows-latest]
    steps:
      - uses: actions/checkout@v4

      - name: Setup Node.js
        uses: actions/setup-node@v4
        with:
          node-version: '20'

      - name: Build frontend
        working-directory: web
        run: |
          npm ci
          npm run build

      - name: Setup Rust
        uses: dtolnay/rust-toolchain@stable

      - name: Build (cargo build 自动嵌入 dist/)
        run: cargo build --release
```

---

## 六、改造影响总表

| 文件/模块 | 改动类型 | 工作量 |
|---|---|---|
| **新建 `lib.rs`** | main.rs 模块声明迁出，main.rs 变薄入口 | 小 |
| **`agent/agent.rs`** | import 替换 `tui::event` → `agent::event`（~20 处） | 小 |
| **`agent/tools.rs`** | 同上（~3 处） | 小 |
| **`agent/subagent.rs`** | 同上（~15 处） | 小 |
| **新建 `agent/event.rs`** | 从 tui/event.rs 抽出领域事件（agent 事件出口） | 中 |
| **`tui/event.rs`** | 拆分 AppEvent，增加 `Core(T)` 包装 | 中 |
| **`tui/app/chat_events.rs`** | 事件 match 增加 `Core(e)` 解包 | 中 |
| **新建 `session/mod.rs`** | 模块声明 | 小 |
| **新建 `session/session.rs`** | 封装 Agent + Storage + MCP + Permission（含 work_dir 发现逻辑） | 大 |
| **新建 `session/manager.rs`** | SessionManager：work_dir → ChatSession 惰性注册表（键 canonicalize 归一） | 中 |
| **新建 `web/mod.rs`** | server 启动、路由、静态资源（`#[cfg(feature = "web")]`） | 大 |
| **新建 `web/sse.rs`** | SSE handler | 中 |
| **新建 `web/task_registry.rs`** | request_id → oneshot 注册表 + 断连 drop guard | 中 |
| **新建 `web/handlers.rs`** | REST API handlers | 大 |
| **新建 `web/protocol.rs`** | JSON 类型定义 | 小 |
| **新建 `web/state.rs`** | WebServerState | 小 |
| **新建 `web/`（前端）** | 完整 React + assistant-ui + Tailwind 4 前端 | 大 |
| **新建 `tests/`** | 集成测试：SSE 流、权限往返、会话切换 | 中 |
| **`main.rs`** | 薄入口化，新增 Web 子命令（feature 门控） | 中 |
| **`Cargo.toml`** | +`[features] web`，axum/tower-http/rust-embed/async-stream 设 optional | 小 |
| **`build.rs`** | 前端构建集成（仅 web feature 时生效） | 小 |
| **`.github/workflows/ci.yml`** | 前端构建步骤 | 小 |

---

## 七、功能覆盖矩阵

| 功能 | TUI | Web UI | 复用方式 |
|---|---|---|---|
| 流式聊天 | ✅ | ✅ | assistant-ui 原生支持 |
| 思维链 (reasoning) | ✅ | ✅ | assistant-ui reasoning part（自动折叠） |
| 工具调用展示 | ✅ | ✅ | assistant-ui ToolCallMessagePartComponent（每个工具定制 UI） |
| 权限确认弹窗 | ✅ | ✅ | SSE 推送 + `request_id` 关联 + POST 回复 |
| 用户提问弹窗 | ✅ | ✅ | SSE 推送 + `request_id` 关联 + POST 回复 |
| 会话管理 | ✅ | ✅ | REST API + 侧边栏会话列表 |
| 模型切换 | ✅ | ✅ | REST API + 模型选择器 |
| Plan 模式 | ✅ | ✅ | POST 切换 |
| YOLO 模式 | ✅ | ✅ | POST 切换 |
| MCP 工具 | ✅ | ✅ | 自动注册到 agent，工具调用走同一路径 |
| Subagent | ✅ | ✅ | TaskTool 工具调用展示 |
| 上下文压缩 | ✅ | ✅ | SSE 事件 + UI 标记 |
| 历史消息加载 | ✅ | ✅ | REST API → assistant-ui 历史导入 |
| `/init` 等自定义命令 | ✅ | ✅ | 前端输入框识别 `/` 前缀 |
| @文件提及 | ✅ | ✅ | 前端输入框识别 `@` 前缀 |
| 多工作目录 | ❌ 启动参数固定 | ✅ | 新建会话时选择目录，SessionManager 按目录管理会话（Web 专属增强） |

---

## 八、操作逻辑与 TUI 对齐

> Web UI 不是另一个产品，而是 hailux 操作心智的 Web 呈现。所有交互行为与 TUI 保持一致，习惯 TUI 的用户零成本切换。下列 TUI 行为均来自 `tui/app/chat_input.rs`、`tui/command.rs`、`tui/app/overlay.rs` 的现有实现。

### 1. 输入框行为

| 行为 | TUI | Web UI |
|---|---|---|
| 发送消息 | `Enter` | `Enter` |
| 插入换行 | `Shift+Enter` / `Alt+Enter` | 相同 |
| 粘贴后短时间按 Enter | 插入换行而非发送（PasteBurst 窗口检测） | `paste` 事件标记 + 相同时间窗口逻辑，多行粘贴后 Enter 不误发 |
| 输入历史 | 光标在首行按 `↑` 上一条；末行按 `↓` 下一条（草稿暂存） | 相同；历史持久化到 localStorage，按会话隔离 |
| 多行编辑 | `↑/↓` 在行间移动光标 | `textarea` 原生行为，无需实现 |
| 命令补全激活时 | `↑/↓` 导航建议列表优先于历史 | 相同 |

### 2. 斜杠命令（对齐 `tui/command.rs`）

输入 `/` 弹出命令面板：前缀过滤 + 描述展示，`↑/↓` 导航，`Tab` 或 `Enter` 补全，`Esc` 关闭。命令列表优先级与 TUI `COMMAND_PRIORITY` 一致。

**UI-action 命令**（触发界面操作，不发给 LLM）：

| 命令 | Web 行为 |
|---|---|
| `/new` | 新建会话，侧边栏同步高亮 |
| `/sessions` | 打开会话弹窗（移动端）/聚焦侧边栏（桌面） |
| `/plan` | 切换 Plan 模式，输入框徽标 + 状态栏同步 |
| `/models` | 打开模型选择器 |
| `/compact` | 触发上下文压缩，完成后消息流插入"已压缩"标记 |
| `/skills` | 弹窗列出已加载 skill（名称 + 描述） |
| `/mcp` | 弹窗列出 MCP 服务器状态 + 工具数 |
| `/tasks` | 弹窗列出子代理任务记录（运行中 / 完成 / 失败） |
| `/yolo` | 切换 YOLO 模式，状态栏同步显示 |
| `/exit` `/q` `/quit` | **不适用** — Web 无退出概念，命令面板中隐藏 |

**prompt 命令**（`/init` 及自定义命令）：`$ARGUMENTS` 展开后作为普通消息发送，走 `/api/chat` 同一链路。

### 3. @文件提及

- 输入 `@` 弹出文件选择器，输入关键字实时过滤（`GET /api/files?q=`，后端做大小写不敏感子串匹配，与 TUI `FilePickerState::cached` 行为一致）
- `↑/↓` 导航，`Tab` / `Enter` 选中并插入路径，`Esc` 关闭
- 发送时保留 `@path` 原文（与 TUI 一致，由 agent 侧读取文件）

### 4. 快捷键映射

| 功能 | TUI | Web |
|---|---|---|
| 中断 Agent | 处理中 5 秒内双击 `Esc` | 相同（首次 `Esc` 显示提示，5 秒内再按确认中断）+ 输入框旁"停止"按钮 |
| 新建会话 | `Ctrl+N` | `Ctrl+N`（macOS `Cmd+N`）+ 侧边栏按钮 |
| 会话列表 | `Ctrl+X` | `Ctrl+X` / `Ctrl+K` + 侧边栏常驻 |
| 模型切换 | `Ctrl+M` | `Ctrl+M` + 状态栏模型名点击 |
| 折叠/展开思维链 | `Ctrl+O` | `Ctrl+O` 全局切换 + 每条消息单独折叠 |
| Plan 模式 | `Shift+Tab` | `Shift+Tab`（`preventDefault` 阻止浏览器焦点切换）+ 输入框模式徽标点击 |
| 退出 | `Ctrl+C` / `Ctrl+D` | **不适用** — 关闭标签页即退出 |

### 5. 聊天区滚动

- 流式输出时自动滚动到底部（对齐 `should_auto_scroll`）
- 用户向上滚动（滚轮 / `↑` / `PageUp`）→ 停止自动滚动，右下角显示"↓ 回到底部"悬浮按钮
- 点击按钮或手动滚回底部 → 恢复自动滚动（对齐 TUI `scroll_offset == 0` 时恢复的逻辑）

### 6. 弹窗交互

- **权限确认**（shadcn `Dialog`）：展示规则描述，三按钮 — 拒绝 / 允许一次 / 始终允许（对应 `PermissionReply::Deny / Once / Always`），`Esc` = 拒绝；等待期间请求耗时冻结（对齐 `TimingStats::pause/resume`）
- **ask_user 提问**：单选（radio）/ 多选（checkbox）/ 自由输入，提交走 `POST /api/ask/:request_id/reply`
- **模型选择器**：模型列表 + "添加模型"入口（多步表单：Provider → 模型 ID → API Key → 上下文窗口，对齐 TUI `AddModelForm`）
- **会话管理**：侧边栏常驻列表（TUI 为 `Ctrl+X` 弹窗）；支持删除（对齐会话选择器 `Ctrl+D`）；当前会话高亮
- **新建会话**：新建时可选工作目录（Web 专属增强，TUI 目录由启动参数固定）：
  1. 默认 = 服务器启动目录（与 TUI 行为一致）
  2. 最近使用目录列表（`GET /api/workdirs`，来自 sessions 表 DISTINCT）
  3. 浏览文件系统选择（`GET /api/fs` 目录树，仅显示目录）
  4. 手动输入路径（`POST /api/workdirs/validate` 校验，规范化为绝对路径）
  - 换目录后：skills / subagents / 自定义命令 / AGENTS.md 按新目录重新发现，system prompt 重建；权限规则中 `external_directory` 判定基准同步切换（新目录内 allow、目录外 ask 的语义不变）

### 7. 状态栏

底部信息条对齐 TUI 状态栏：当前模型、**当前工作目录**（点击可查看完整路径）、Plan/YOLO 模式徽标、Token 用量（prompt/completion）、压缩阈值进度、请求耗时（扣除弹窗暂停时间）、MCP 连接状态图标。

侧边栏会话列表按工作目录分组展示（sessions 表本身带 `work_dir` 字段，`list_top_level_sessions` 即按目录过滤），切换分组 = 切换工作目录上下文。

---

## 九、关键设计决策

> **实施状态（2026-08）**：阶段 0–6 已全部落地。一处偏差：前端未引入
> `@assistant-ui/react`（其 API 版本变动大、与自建 store 的集成成本高），
> 改为自研轻量组件（chat-thread / dialogs / sidebar，Zustand 驱动），
> 组件结构保留了后续接入 assistant-ui 的空间。其余（SSE + REST、
> EventHub、SessionManager 多目录、断连 drop guard、feature 门控）
> 均按本文档实现，`cargo fmt / clippy / test` 全绿。

### 1. 事件解耦策略

采用 `CoreEvent`（定义于 `agent/event.rs`，agent 自有事件出口）+ `AppEvent::Core(T)` 包装方案。agent 模块对上层完全无感知，TUI 层在 `AppEvent` 中包装 `CoreEvent` 并附加终端专有事件，Web 层将其序列化为 SSE JSON。改动量最小，完全兼容现有 TUI 代码，且依赖图严格单向：`tui/web → session → agent → 领域层`。

### 2. 权限/提问的异步回复

使用 `TaskRegistry` + `oneshot` channel + `request_id` 方案。Agent task 遇到权限请求时阻塞在 `oneshot::Receiver`，前端通过独立 POST 请求回复，handler 从 registry 取出 sender 发送回复，Agent task 唤醒继续执行。

### 3. SSE 读取方式

前端使用 `fetch().body.getReader()` 而非 `EventSource`。因为 `EventSource` 只支持 GET 请求，而我们需要 POST 发送消息体。

### 4. 前端嵌入方案

使用 `rust-embed` 将 React build 产物 (`dist/`) 编译进 Rust 二进制。最终用户拿到的还是单个 `hailux.exe`，无需 Node.js。

### 5. 共享配置存储

Web 模式和 TUI 模式共享 `~/.hailux/` 配置目录和 SQLite 数据库。两种模式可交替使用，数据互通。

### 6. 开发模式

开发时前端 Vite dev server 运行在 `localhost:5173`，后端 axum 运行在 `localhost:18080`。Vite 配置 proxy 将 `/api/*` 转发到后端，实现前后端独立热更新。生产环境由 `rust-embed` 嵌入静态资源，单端口服务。

### 7. 操作逻辑与 TUI 对齐

Web UI 的交互行为（输入框、斜杠命令、@提及、快捷键、滚动、弹窗）逐项对齐 TUI 现有实现，仅做必要的媒介适配（如 `/exit` 在 Web 中隐藏、双击 `Esc` 中断额外提供停止按钮）。命令匹配逻辑（`match_command` 的 UI 命令 → prompt 命令优先级）在后端复用同一份代码，前端只做展示。

### 8. work_dir 会话级化（Web 多目录支持）

TUI 的 work_dir 是进程级常量（启动时 `resolve_work_dir()` 确定）；Web 端将其下放为**会话属性** —— `sessions` 表已有 `work_dir` 字段，天然支持按目录归类。实现上通过 `SessionManager`（work_dir → `ChatSession` 惰性注册表）管理：Storage / Config / MCP 连接全局共享，每个 work_dir 独立持有 Agent + skills/subagents/commands 发现结果 + system prompt。TUI 模式退化为单条目，行为完全不变。

**安全边界**：目录选择意味着 Web 端可访问本机任意目录 —— 服务默认只监听 `127.0.0.1`，视为与 TUI 同等信任级别（本机用户）；权限系统的 `external_directory ask` 规则在新目录上下文中照常生效。若部署到 `0.0.0.0` 暴露给局域网/公网，目录选择能力等同远程任意文件访问，需自行承担风险（后续可加 token 认证 + 目录白名单）。
