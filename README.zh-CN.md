[English](README.md) | 简体中文

# hailux

> 一个基于终端的 AI 编程助手，让你在命令行（或浏览器）中完成代码编写、审查、重构和问题排查。基于 Rust 构建，编译为单一可执行文件，无需安装任何外部运行时（Node、Python 等），低内存占用，开箱即用。内置 Web UI，一条命令切换到浏览器中使用。

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.96.0%2B-orange.svg)](https://www.rust-lang.org)
[![CI](https://github.com/dongdong306/hailux/actions/workflows/ci.yml/badge.svg)](https://github.com/dongdong306/hailux/actions/workflows/ci.yml)

## 功能特性

- **流式对话** — 实时显示 AI 回复，支持推理过程展示
- **工具调用** — 内置文件读写、搜索、Bash 执行、网页获取等工具，AI 可自主调用完成复杂任务
- **Web UI** — `hailux --web` 一键切换浏览器界面，与 TUI 共享会话与配置，支持技能 / MCP / 子代理可视化管理
- **MCP 协议支持** — 可接入任意 MCP 服务拓展能力
- **SKILL 系统** — 通过 SKILL.md 定义可复用的工作流和知识库，支持项目级和全局技能
- **规划模式** — 切换到只读模式，让 AI 先分析后执行，避免误操作
- **子代理** — 支持委派复杂任务给子代理并行处理
- **会话管理** — 自动保存对话历史，支持多会话切换
- **多模型支持** — 内置 DeepSeek、Zhipu AI 等模型，支持自定义 API 提供商
- **文件提及** — 使用 `@路径` 快速引用文件内容

## 快速开始

### 安装

**一键安装（推荐）：**

Linux：
```sh
curl -fsSL https://raw.githubusercontent.com/dongdong306/hailux/main/scripts/install.sh | bash
```

Windows (PowerShell)：
```powershell
irm https://raw.githubusercontent.com/dongdong306/hailux/main/scripts/install.ps1 | iex
```

**从源码编译：**

```sh
git clone https://github.com/dongdong306/hailux.git
cd hailux
cargo build --release
```

编译后的二进制文件位于 `target/release/hailux`。

**前提条件（源码编译）：**
- Rust 1.96.0+（Edition 2024）
- Windows / Linux / macOS

### 首次配置

首次运行 `hailux` 后，配置文件会自动生成在 `~/.hailux/` 目录下：

```
~/.hailux/
├── config.toml    # API 配置
├── mcp.toml       # MCP 服务器配置
├── db/chat.db     # 对话历史数据库
├── AGENTS.md      # 全局指令（注入到系统提示词）
└── skills/        # 自定义技能
```

编辑 `~/.hailux/config.toml` 配置 API Key：

```toml
main_model = "deepseek/deepseek-v4-pro"

[providers.deepseek]
api_key = "your-api-key-here"
```

### 启动

```sh
hailux              # 在当前目录启动 TUI
hailux --web        # 启动 Web UI（默认 http://127.0.0.1:18080）
hailux web --open   # 启动 Web UI 并自动打开浏览器
```

首次启动时会引导你选择模型并输入 API Key。

### Web UI

Web UI 与 TUI 共享配置、会话历史和技能体系：

```sh
hailux web --host 127.0.0.1 --port 18080 --open
```

- 默认仅监听本机回环地址（`127.0.0.1`）。服务器可访问本机任意目录，暴露到 `0.0.0.0` 属于显式信任决策，请注意安全。
- 页面资源已嵌入二进制，无需额外部署前端。
- 浏览器内可直接管理会话、技能和 MCP 服务器，权限确认 / ask_user 弹窗与 TUI 行为一致。

### 使用示例

```sh
$ hailux
> 这个项目的测试是怎么组织的？
```

hailux 会读取当前目录上下文（AGENTS.md、技能等），调用 `grep` / `read` 等工具定位测试代码后给出答案。涉及文件修改的任务（如"给 CLI 加个 --verbose 参数"），它会自动使用编辑工具修改代码并汇报 diff；输入 `@路径` 可强制引用指定文件内容。

## 使用指南

### 斜杠命令

在输入框中以 `/` 开头输入命令：

| 命令 | 说明 |
|------|------|
| `/sessions` | 打开会话选择器 |
| `/new` | 新建会话 |
| `/models` | 切换模型 |
| `/skills` | 查看已加载的技能 |
| `/mcp` | 查看 MCP 服务器状态 |
| `/tasks` | 查看子代理执行情况 |
| `/plan` | 切换规划模式（只读） |
| `/compact` | 压缩对话历史（节省上下文） |
| `/init` | 为当前目录生成 AGENTS.md 模板 |
| `/exit` | 退出程序（`quit` / `q` 亦可） |

### 快捷键

| 按键 | 功能 |
|------|------|
| `Enter` | 发送消息 |
| `Shift+Enter` / `Alt+Enter` | 换行 |
| `Tab` | 补全命令 / 确认建议 |
| `Shift+Tab` | 切换规划模式 |
| `Ctrl+O` | 折叠/展开推理过程 |
| `↑` / `↓` | 浏览历史输入 / 多行编辑中移动光标 |
| `PageUp` / `PageDown` | 滚动对话 |
| `Esc` | 关闭建议 / 清空输入 |
| `@` | 触发文件提及 |

### 配置自定义模型

在 `~/.hailux/config.toml` 中添加自定义提供商：

```toml
main_model = "my-provider/my-model"

[providers.my-provider]
api_key = "your-key"
base_url = "https://api.example.com/v1"

[providers.my-provider.models.my-model]
max_tokens = 8192
context_window = 32768
```

### MCP 服务器

编辑 `~/.hailux/mcp.toml` 添加 MCP 服务器：

```toml
# stdio 方式（本地进程）
[my-server]
command = "node"
args = ["/path/to/server.js"]

# http 方式（远程服务）
[remote-server]
url = "https://example.com/mcp"
```

### SKILL 系统

在 `~/.hailux/skills/<name>/SKILL.md` 创建技能：

```markdown
---
name: my-skill
description: 技能描述，会显示在系统提示词中
---

技能的完整内容，在用户请求时加载。
```

## 开发

```sh
cargo build     # 编译（默认启用 Web UI feature）
cargo run       # 运行 TUI
cargo test      # 运行测试
cargo clippy    # 代码检查
cargo fmt       # 格式化代码

cargo run -- --web              # 运行 Web UI
cd web && npm install && npm run build   # 修改 web/src 后需手动构建前端
cargo build --no-default-features       # 仅 TUI 构建，无需 Node
```

源码编译 Web UI 需要 Node.js 22+。

## 技术栈

- **语言**：Rust (Edition 2024)
- **TUI 框架**：[ratatui](https://ratatui.rs/) + [crossterm](https://github.com/crossterm-rs/crossterm)
- **Web 后端**：[axum](https://github.com/tokio-rs/axum)（SSE + REST，静态资源经 [rust-embed](https://github.com/pyrossh/rust-embed) 嵌入）
- **Web 前端**：[React 19](https://react.dev/) + [assistant-ui](https://www.assistant-ui.com/) + [zustand](https://github.com/pmndrs/zustand) + [vite](https://vitejs.dev/) + Tailwind 4
- **LLM 交互**：[async-openai](https://github.com/64bit/async-openai)
- **MCP 客户端**：[rmcp](https://github.com/modelcontextprotocol/rust-sdk)
- **存储**：SQLite ([sqlx](https://github.com/launchbadge/sqlx))
- **Markdown 渲染**：[pulldown-cmark](https://github.com/pulldown-cmark/pulldown-cmark) + [syntect](https://github.com/trishume/syntect)
- **代码搜索**：[grep](https://github.com/BurntSushi/ripgrep) + [ignore](https://github.com/BurntSushi/ripgrep/tree/master/crates/ignore)
- **CLI**：[clap](https://github.com/clap-rs/clap)

## 许可证

[MIT License](LICENSE)
