# AGENTS.md

OpenCode instructions for the `hailux` repository. This file is for OpenCode sessions.
Note: hailux itself loads a separate `AGENTS.md` (case-insensitive) from
`~/.hailux/` and ancestor directories into its LLM system prompt — that is a different
feature from this file.

## Commit Convention

- All commit messages **must be in English**.
- Follow [Conventional Commits](https://www.conventionalcommits.org/): `feat:`, `fix:`, `docs:`, `refactor:`, `chore:`, `ci:`, etc.

## Build & Test

```sh
cargo build          # build (edition 2024 — requires Rust 1.96.0+)
cargo run            # launch the TUI app
cargo test           # run unit tests (inline #[cfg(test)] modules in agent/*, mcp/config.rs, storage/db.rs, tui/command.rs)
cargo clippy         # lint locally; CI runs `cargo clippy -- -D warnings` — match that before pushing
cargo fmt            # format (no config — uses defaults); CI runs `cargo fmt --check`
```

CI via GitHub Actions (`.github/workflows/ci.yml`): fmt check, clippy, test, build on Ubuntu + Windows.
See README.md and CONTRIBUTING.md for project documentation.

## What hailux Is

A terminal-based AI coding assistant (like OpenCode/Claude Code) written in Rust.
It uses `async-openai` with a **bring-your-own-transport** custom layer to support
DeepSeek-style `reasoning_content` fields. The TUI is built with `ratatui` + `crossterm`.
Features streaming chat, tool calling, MCP protocol, a progressive skill system,
subagent delegation, and custom slash commands.

## Architecture (src/)

- `main.rs` — Entry point. Builds the `Agent`, loads config/MCP/storage, launches TUI.
  `build_agent()` registers tools, discovers skills/subagents/commands, and delegates
  system-prompt assembly to `prompts::build_system_prompt()`.
- `agent/` — Core LLM interaction loop.
  - `agent.rs` — Streaming chat loop, tool-call dispatch, plan-mode, cancellation.
    Context compaction: `apply_compaction()` swaps the message list for a summary message;
    `request_compaction()` drives it from the TUI on a cloned list (not stored directly).
  - `models.rs` — **DeepSeek-extended message types**. `DeepSeekChatCompletionRequestMessage`
    wraps standard async-openai types and adds `reasoning_content` to Assistant messages.
    Uses custom `Serialize` to inject `thinking` config and `extra` fields into the
    request JSON. Requests go through `create_stream_byot` (BYOT = bring your own transport).
    Messages are stored as `SharedMessage = Arc<CompatibleChatCompletionRequestMessage>`:
    cloning a message list is O(n) pointer copies (no deep copy of content). In plan mode,
    `AgentStreamState::build_request()` uses `Arc::make_mut` to inject the read-only prompt —
    it deep-copies only the single message being modified, leaving the rest shared.
  - `tools.rs` — `Tool` trait + built-in tools (`bash`, `read`, `edit`, `write`, `grep`,
    `glob`, `web_fetch`, `todo_write`, `ask_user`). `ToolRegistry` converts tools to
    OpenAI function-calling schema. `allowed_in_plan_mode()` gates write tools.
    `execute_async_with_display()` returns optional UI display data (e.g. diffs).
  - `skill.rs` — Discovers `SKILL.md` files (YAML frontmatter with `name`/`description`)
    from `~/.hailux/skills/` and `<work_dir>/.hailux/skills/`. Progressive loading: only
    summaries go into the system prompt; full content loaded via the `skill` tool.
  - `subagent.rs` — **Subagent system**. Discovers `AGENTS.md` files (YAML frontmatter with
    `name`/`description`/`tools`/`skills`/`mcp`/`model`) from `~/.hailux/agents/` and
    `<work_dir>/.hailux/agents/`. Implements `TaskTool`: launches subagents in isolated
    sessions, each with its own tool set, skill whitelist, MCP server filter, and model.
    Supports session resume via `task_id` parameter. A builtin `general` subagent is always
    available. Tool-call progress is forwarded to the main TUI in real time.
  - `command_def.rs` — **Custom slash commands**. Discovers `.md` files (YAML frontmatter
    with `description`) from `~/.hailux/commands/` and `<work_dir>/.hailux/commands/`.
    `CommandRegistry` unifies builtin and custom prompt commands; priority: project > global > builtin.
  - `agents_md.rs` — Discovers `AGENTS.md` (case-insensitive) from `~/.hailux/` (global,
    lowest priority) then ancestor dirs up to 3 levels, with work-dir having highest priority.
  - `utils.rs` — Shared utilities: `compare_mtime` (file modification time sorting),
    `split_frontmatter` / `strip_frontmatter_value` (frontmatter parsing used by skill,
    subagent, and command_def modules).
- `prompts/` — **System prompt and tool description templates** (all `include_str!` at compile time).
  - `mod.rs` — `build_system_prompt()` assembles: base system prompt + working directory +
    available skills summary + AGENTS.md instructions + available subagents summary.
  - `system.txt` — Base system prompt (Chinese).
  - `plan_mode.txt` — Plan-mode system reminder injected on cloned last user message.
  - `general_subagent.txt` — System prompt for the builtin `general` subagent.
  - `task_tool.txt` — Description template for the `task` tool (includes `{agents_list}` placeholder).
  - `default_help_skill.md` — Default help skill content written on first run.
  - `compact.txt` — System prompt for the compaction call (summary of history).
  - `init.txt` — Template for the builtin `/init` prompt command (`{path}` → work dir).
  - `tools/` — Per-tool description templates (`bash_windows.txt` / `bash_unix.txt` selected via `cfg`).
- `config.rs` — Model/provider config. Reads/writes `~/.hailux/config.toml`.
  Predefined providers: `deepseek`, `zhipu-coding-plan`. Custom providers/models supported.
  Model selector format: `provider/model` (e.g. `deepseek/deepseek-v4-pro`).
  `resolve()` returns `ResolvedModel` with OpenAI config, model ID, max tokens, context window, display name.
- `mcp/` — MCP (Model Context Protocol) client via `rmcp` crate.
  - `config.rs` — Reads `~/.hailux/mcp.toml`. Auto-generates a commented sample on first run.
    Supports stdio (local subprocess) and http (remote) transports.
  - `client.rs` — Connects servers in background, registers their tools with the agent.
    `SharedMcpBackends` (`Arc<Mutex<Vec<McpToolBackend>>>`) is shared with `TaskTool` for subagent MCP access.
- `storage/db.rs` — SQLite chat history at `~/.hailux/db/chat.db`. **Versioned migrations via
    `sqlx::migrate!`** (`migrations/*.sql`, recorded in `_sqlx_migrations`). New schema changes =
    add a new migration file, no hand-written fallbacks. `upgrade_legacy_schema()` is a one-time
    bootstrap that only runs on pre-0.4.0 DBs (no `_sqlx_migrations` table yet) to backfill
    columns with idempotent `pragma_table_info` checks; it can be removed after a few releases.
    Max pool connections = 1.
    Supports subsessions (parent/child session relationships for subagent task isolation).
    `messages.compacted` + `sessions.compact_summary` support context compaction; active-context
    queries filter `compacted = 0`.
- `tui/` — Ratatui UI. `app.rs` is the main event loop and state machine.
  - `command.rs` — Slash commands: UI-action commands (`/sessions`, `/new`, `/models`,
    `/skills`, `/mcp`, `/tasks`, `/plan`, `/compact`, `/exit`; `quit`/`q` alias exit) +
    custom prompt commands via `CommandRegistry` (incl. builtin `/init`, which generates
    an AGENTS.md template for the work dir).
  - `event.rs` — Async event channel bridging terminal input, agent streaming, MCP.
    Events include `ToolCallStart`/`ToolResult` (with optional `subagent_name` for task delegation).
  - `input.rs` — Text input handling with paste detection.
  - `ask_user.rs` — Interactive question dialog for `ask_user` tool.
  - `history_cell.rs` — Chat history rendering units.
  - `terminal.rs` — Terminal init/restore + panic hook.
  - Other files render specific UI panels (chat, pickers, viewers, setup, markdown).

## Runtime File Locations (all under ~/.hailux/)

| Path | Purpose |
|------|---------|
| `config.toml` | Provider API keys, model config, `main_model` selector |
| `mcp.toml` | MCP server definitions (stdio + http) |
| `db/chat.db` | SQLite: sessions + messages tables (incl. subsessions for subagent tasks) |
| `AGENTS.md` | Global instructions injected into LLM system prompt |
| `skills/<name>/SKILL.md` | Skill definitions with YAML frontmatter |
| `agents/<name>/AGENTS.md` | Subagent definitions (name, description, tools, skills, mcp, model) |
| `commands/<name>.md` | Custom slash command templates with `$ARGUMENTS` placeholder |

Project-level overrides: `<work_dir>/.hailux/{skills,agents,commands}/` and ancestor `AGENTS.md` files.

## Key Quirks

- **Platform**: Primarily developed on Windows. `BashTool` runs `powershell.exe` on Windows
  and `bash -c` on Unix. The `workdir` parameter only exists on non-Windows. Windows has an
  extra `crossterm_winapi` dependency and different paste-detection timing constants.
  Tool description templates are platform-specific (`bash_windows.txt` vs `bash_unix.txt`).
- **Permission defaults**: In-workdir operations are allowed by default via
  builtin lowest-priority rules in `permission/mod.rs::default_rules()`: `* allow`,
  `external_directory * ask`, `read *.env` / `*.env.* ask` (`.env.example allow`), `mcp * ask`.
  Tools issue `external_directory` requests (not their category permission) when a path resolves
  outside the workdir — read/edit/write/grep/glob path args and `bash` file-path args
  (`BASH_FILE_COMMANDS`) plus the bash `workdir` param. `[permission.xxx]` config rules
  (session > config > builtin defaults) override the defaults; `read` in-workdir requests use
  absolute-path patterns so `.env` rules match via `wildcard_match` — a `\`→`/`-normalized,
  Windows case-insensitive, `*`/`?`-wildcard matcher with literal special chars.
- **Plan mode** (`/plan` or Shift+Tab): Filters out write/edit tools from the tool list
  and injects a read-only system-reminder into the last user message (on a clone, not stored).
- **Subagent isolation**: `TaskTool` creates subsessions in the DB. Subagents get their own
  event channel; only final text result is returned to the main agent. Tool-call progress is
  forwarded to the main TUI (truncated to 2000 chars). `ask_user` and `task` tools are never
  registered for subagents. Subagent tool/skill/MCP access is filtered by `AGENTS.md` frontmatter.
- **Paste detection**: `PasteBurst` in `app.rs` (via `input.rs`) distinguishes typed vs pasted
  input via timing heuristics. Complex state machine — be careful when modifying input handling.
- **CRLF handling**: `EditTool` normalizes line endings to match the file's style before
  string matching, to avoid LLM-provided `\n` failing on `\r\n` files.
- **Context compaction**: `/compact` or auto-trigger when usage crosses
  `compact_threshold` (config.toml) × max context tokens. Compaction marks messages
  `compacted=1` in SQLite (never deletes rows), stores the summary in
  `sessions.compact_summary`, and renders a "Context compacted" marker in the TUI.
- **System prompt assembly** is centralized in `prompts/mod.rs::build_system_prompt()`.
  `main.rs::build_agent()` calls it with discovered skills, AGENTS.md entries, and subagents.
- **Message sync**: After streaming completes, `Agent::sync_messages()` replaces the
  in-memory message list with the final streamed messages (which include tool results).
- **No `clippy` lints configured** — `#[allow(dead_code)]` is used liberally on builder methods.
- **Frontmatter parsing** is shared via `utils::split_frontmatter()` — used consistently
  across skill, subagent, and command_def modules for YAML frontmatter extraction.
