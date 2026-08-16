English | [简体中文](README.zh-CN.md)

# hailux

> A terminal-based AI coding assistant that lets you write, review, refactor, and debug code right from your command line (or browser). Built in Rust and compiled to a single binary — no external runtimes required (Node, Python, etc.), low memory footprint, ready out of the box. Ships with a built-in Web UI: one command switches you to the browser.

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.96.0%2B-orange.svg)](https://www.rust-lang.org)
[![CI](https://github.com/dongdong306/hailux/actions/workflows/ci.yml/badge.svg)](https://github.com/dongdong306/hailux/actions/workflows/ci.yml)

## Features

- **Streaming chat** — Real-time AI responses with visible reasoning process
- **Tool calling** — Built-in file read/write, search, Bash execution, web fetching and more; the AI autonomously calls tools to complete complex tasks
- **Web UI** — `hailux --web` switches to a browser interface sharing sessions and config with the TUI, with visual management for skills / MCP / subagents
- **MCP support** — Connect any MCP server to extend capabilities
- **Skill system** — Define reusable workflows and knowledge bases via SKILL.md, at project or global level
- **Plan mode** — Switch to read-only mode so the AI analyzes before executing, avoiding unintended changes
- **Subagents** — Delegate complex tasks to subagents running in parallel
- **Session management** — Chat history saved automatically, switch between multiple sessions
- **Multi-model support** — DeepSeek, Zhipu AI and more built in; custom API providers supported
- **File mentions** — Reference file contents quickly with `@path`

## Getting Started

### Installation

**One-line install (recommended):**

Linux:
```sh
curl -fsSL https://raw.githubusercontent.com/dongdong306/hailux/main/scripts/install.sh | bash
```

Windows (PowerShell):
```powershell
irm https://raw.githubusercontent.com/dongdong306/hailux/main/scripts/install.ps1 | iex
```

**Build from source:**

```sh
git clone https://github.com/dongdong306/hailux.git
cd hailux
cargo build --release
```

The binary is located at `target/release/hailux`.

**Prerequisites (building from source):**
- Rust 1.96.0+ (Edition 2024)
- Windows / Linux / macOS

### First-time Setup

After running `hailux` for the first time, config files are generated under `~/.hailux/`:

```
~/.hailux/
├── config.toml    # API configuration
├── mcp.toml       # MCP server configuration
├── db/chat.db     # chat history database
├── AGENTS.md      # global instructions (injected into the system prompt)
└── skills/        # custom skills
```

Edit `~/.hailux/config.toml` to set your API key:

```toml
main_model = "deepseek/deepseek-v4-pro"

[providers.deepseek]
api_key = "your-api-key-here"
```

### Launch

```sh
hailux              # start the TUI in the current directory
hailux --web        # start the Web UI (default http://127.0.0.1:18080)
hailux web --open   # start the Web UI and open the browser
```

On first launch you will be guided to pick a model and enter an API key.

### Web UI

The Web UI shares configuration, session history, and the skill system with the TUI:

```sh
hailux web --host 127.0.0.1 --port 18080 --open
```

- Listens on the loopback address (`127.0.0.1`) by default. The server can access any local directory — exposing it on `0.0.0.0` is an explicit trust decision; be careful.
- Frontend assets are embedded in the binary; no separate frontend deployment needed.
- Manage sessions, skills, and MCP servers directly in the browser. Permission confirmations / ask_user dialogs behave the same as in the TUI.

### Example

```sh
$ hailux
> How are the tests organized in this project?
```

hailux reads the current directory's context (AGENTS.md, skills, etc.), calls tools like `grep` / `read` to locate test code, and answers. For tasks involving file changes (e.g. "add a --verbose flag to the CLI"), it edits the code automatically and reports a diff; type `@path` to force-include a file's content.

## Usage Guide

### Slash Commands

Type `/` in the input box:

| Command | Description |
|---------|-------------|
| `/sessions` | Open the session picker |
| `/new` | Start a new session |
| `/models` | Switch model |
| `/skills` | List loaded skills |
| `/mcp` | Show MCP server status |
| `/tasks` | Show subagent task status |
| `/plan` | Toggle plan mode (read-only) |
| `/compact` | Compact conversation history (saves context) |
| `/init` | Generate an AGENTS.md template for this directory |
| `/exit` | Quit (`quit` / `q` also work) |

### Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `Enter` | Send message |
| `Shift+Enter` / `Alt+Enter` | Newline |
| `Tab` | Complete command / accept suggestion |
| `Shift+Tab` | Toggle plan mode |
| `Ctrl+O` | Collapse/expand reasoning |
| `↑` / `↓` | Browse input history / move cursor in multi-line editing |
| `PageUp` / `PageDown` | Scroll conversation |
| `Esc` | Dismiss suggestion / clear input |
| `@` | Trigger file mention |

### Custom Models

Add a custom provider in `~/.hailux/config.toml`:

```toml
main_model = "my-provider/my-model"

[providers.my-provider]
api_key = "your-key"
base_url = "https://api.example.com/v1"

[providers.my-provider.models.my-model]
max_tokens = 8192
context_window = 32768
```

### MCP Servers

Edit `~/.hailux/mcp.toml` to add MCP servers:

```toml
# stdio (local process)
[my-server]
command = "node"
args = ["/path/to/server.js"]

# http (remote service)
[remote-server]
url = "https://example.com/mcp"
```

### Skill System

Create a skill at `~/.hailux/skills/<name>/SKILL.md`:

```markdown
---
name: my-skill
description: Skill description, shown in the system prompt
---

Full skill content, loaded on demand.
```

## Development

```sh
cargo build     # build (Web UI feature enabled by default)
cargo run       # run the TUI
cargo test      # run tests
cargo clippy    # lint
cargo fmt       # format code

cargo run -- --web              # run the Web UI
cd web && npm install && npm run build   # rebuild the frontend after editing web/src
cargo build --no-default-features       # TUI-only build, no Node required
```

Building the Web UI from source requires Node.js 22+.

## Tech Stack

- **Language**: Rust (Edition 2024)
- **TUI**: [ratatui](https://ratatui.rs/) + [crossterm](https://github.com/crossterm-rs/crossterm)
- **Web backend**: [axum](https://github.com/tokio-rs/axum) (SSE + REST, static assets embedded via [rust-embed](https://github.com/pyrossh/rust-embed))
- **Web frontend**: [React 19](https://react.dev/) + [assistant-ui](https://www.assistant-ui.com/) + [zustand](https://github.com/pmndrs/zustand) + [vite](https://vitejs.dev/) + Tailwind 4
- **LLM interaction**: [async-openai](https://github.com/64bit/async-openai)
- **MCP client**: [rmcp](https://github.com/modelcontextprotocol/rust-sdk)
- **Storage**: SQLite ([sqlx](https://github.com/launchbadge/sqlx))
- **Markdown rendering**: [pulldown-cmark](https://github.com/pulldown-cmark/pulldown-cmark) + [syntect](https://github.com/trishume/syntect)
- **Code search**: [grep](https://github.com/BurntSushi/ripgrep) + [ignore](https://github.com/BurntSushi/ripgrep/tree/master/crates/ignore)
- **CLI**: [clap](https://github.com/clap-rs/clap)

## License

[MIT License](LICENSE)
