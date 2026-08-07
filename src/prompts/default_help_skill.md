---
name: help
description: Learn about hailux features, slash commands, keyboard shortcuts, @file/@folder mentions, and how to configure AGENTS.md, Skills, custom commands, MCP servers, and Subagents
---

# hailux User Guide

hailux is a terminal AI coding assistant that supports streaming conversations, tool calls, Skills, custom commands, MCP server integration, and subagent delegation.

## Command Line Interface

| Command | Description |
|---------|-------------|
| `hailux` | Launch the interactive TUI |
| `hailux --path <dir>` | Set the working directory |
| `hailux --rebuild-db` | Rebuild the database (clears all history), used when migration fails; the old database is backed up first |
| `hailux --yolo` | YOLO mode: skip all permission confirmations (works in both TUI and non-interactive mode) |
| `hailux run <message>` | Non-interactive mode: send a single message and print the reply (reads from stdin if the message is omitted) |
| `hailux run --model <provider/model>` | Override the model specified in config |
| `hailux run --no-tools` | Disable all tool calls (including MCP), pure chat mode |

## Permissions

By default, operations inside the working directory are allowed without confirmation:

- bash commands, `read`/`edit`/`write`/`grep`/`glob` inside the working directory run freely
- reading sensitive files such as `.env` / `.env.local` asks for confirmation (`.env.example` is allowed)
- any operation touching content outside the working directory (e.g. `cat ../secrets.txt`, `rm /tmp/x`, editing a file outside the project) asks for confirmation
- MCP tools always ask for confirmation

Override the defaults with rules in `~/.hailux/config.toml`:

```toml
[permission.bash]
"rm *" = "deny"
"git push *" = "ask"

[permission.read]
"*.env" = "allow"   # 关闭 .env 读取确认

[permission.external_directory]
"C:\\Users\\me\\shared\\*" = "allow"
```

Choosing "always" in the permission dialog persists the rule for the current session; `hailux --yolo` skips all confirmations.

## Slash Commands

Trigger them by starting with `/` in the input box. After typing `/`, a command completion list pops up automatically. Use `↑↓` to select, `Tab` or `Enter` to confirm:

| Command | Description |
|---------|-------------|
| `/sessions` | Open session selector, switch between historical sessions |
| `/new` | Create a new session |
| `/models` | Switch models or add a new provider |
| `/skills` | View currently loaded skills |
| `/mcp` | View MCP server connection status |
| `/plan` | Toggle planning mode (read-only, write operations disabled) |
| `/exit` | Exit the program (also `/quit`, `/q`) |

Custom commands also appear in the completion list (see the "Custom Commands" section below).

## Keyboard Shortcuts

### General

| Shortcut | Description |
|----------|-------------|
| `Enter` | Send message |
| `Shift+Enter` | New line in input box |
| `Tab` | Confirm command completion / Confirm file selection |
| `Esc` | Close completion list / Clear input box |
| `↑` / `↓` | Move cursor in input box; at first/last line, cycle through input history |
| `PageUp` / `PageDown` | Scroll chat history (20 lines at a time) |
| Mouse wheel | Scroll chat history (3 lines at a time) |

### Session & Model

| Shortcut | Description |
|----------|-------------|
| `Ctrl+X` | Open session selector |
| `Ctrl+N` | Create a new session |
| `Ctrl+M` | Open model selector |
| `Ctrl+C` / `Ctrl+D` | Exit program |

### While Processing (LLM is replying)

| Shortcut | Description |
|----------|-------------|
| `Esc` | First press shows a hint; press again within 5 seconds to interrupt generation |
| `↑` / `↓` | Scroll chat history (3 lines at a time) |
| `Ctrl+O` | Collapse/expand chain of thought (collapsed by default, showing first 6 lines preview) |

### Planning Mode

| Shortcut | Description |
|----------|-------------|
| `Shift+Tab` | Toggle planning mode (read-only mode, write operations disabled) |

## @ File Mentions

Typing `@` in a message triggers a file picker that automatically searches for matching files in the current working directory:

1. After typing `@`, continue typing a path fragment; the picker filters results in real time
2. Use `↑↓` to select a file, `Tab` or `Enter` to confirm insertion
3. Or directly enter a full path (e.g., `@D:\project\src\main.rs`) without using the picker
4. `Esc` closes the file picker

File contents are automatically read and included in the conversation context when sending a message. Pasted text is also recognized as a whole element and can be deleted as a single atomic unit.

## AGENTS.md Global Instructions

AGENTS.md is a custom instruction file injected into the LLM system prompt, used to define project conventions, coding style, business rules, etc.

### Lookup Locations

hailux searches for AGENTS.md in the following order (case-insensitive filename). All found files are concatenated in order of priority (lowest to highest):

1. `~/.hailux/AGENTS.md` — Global instructions (applies to all projects, lowest priority)
2. AGENTS.md in ancestor directories up to 3 levels above the working directory — suitable for monorepo shared configuration
3. `<working_dir>/AGENTS.md` — Project-level instructions (highest priority)

### Example

Create `AGENTS.md` in the working directory:

```markdown
# Project Conventions

- Use 4-space indentation
- All public functions must have doc comments
- Run cargo clippy before committing
- Use built-in #[test] for testing
```

No restart needed; it is loaded automatically in new sessions.

## How to Create a Custom Skill

A Skill is a lightweight mechanism for extending hailux's capabilities. Each skill is a `SKILL.md` file placed at:

- **Global**: `~/.hailux/skills/<skill-name>/SKILL.md` (available to all projects)
- **Project-level**: `<working_dir>/.hailux/skills/<skill-name>/SKILL.md` (current project only, overrides a global skill with the same name)

### SKILL.md Format

```markdown
---
name: my-skill
description: One-sentence description of when to use this skill
---

# My Skill

Write the specific instruction body here.
```

### Frontmatter Fields

| Field | Required | Description |
|-------|----------|-------------|
| `name` | Yes | Unique skill identifier |
| `description` | No | Summary description for the LLM to decide whether to load |

### Progressive Loading

Skills use progressive loading: only `name` and `description` are injected into the system prompt. When the LLM determines a skill is needed, it loads the full body via the `skill` tool. Relative paths in the skill body are relative to the directory containing SKILL.md.

### Example: Create a Code Review Skill

```sh
mkdir -p ~/.hailux/skills/code-review
```

Then edit `~/.hailux/skills/code-review/SKILL.md`:

```markdown
---
name: code-review
description: Perform structured code review on code changes, checking for security, performance, and maintainability issues
---

# Code Review

For the code the user requests to review, perform the following steps:
1. Identify changed files and functions
2. Check for potential security vulnerabilities
3. Check for performance issues
4. Check naming and style consistency
5. Provide improvement suggestions (prioritized)
```

Save and restart hailux to activate it.

## How to Create Custom Commands

Custom commands (prompt commands) are predefined prompt templates invoked via `/command_name arguments`. Arguments replace the `$ARGUMENTS` placeholder in the template before being sent to the LLM.

Command files are `.md` files placed at:

- **Global**: `~/.hailux/commands/<command-name>.md` (available to all projects)
- **Project-level**: `<working_dir>/.hailux/commands/<command-name>.md` (current project only, overrides a global command with the same name)

### Command File Format

```markdown
---
description: Brief description of this command
---

Template body, use $ARGUMENTS as a placeholder to receive user-provided parameters.
```

The `description` in the frontmatter will appear in the `/` command completion list. You can also omit the frontmatter and just write the template body.

### Example: Create a Code Review Command

Edit `~/.hailux/commands/review.md`:

```markdown
---
description: Review code quality of specified files
---

Please review the following files, focusing on security vulnerabilities, performance issues, and code style:
$ARGUMENTS
```

Save and restart hailux, then type `/review src/main.rs` to trigger it.

## How to Configure MCP Servers

MCP (Model Context Protocol) allows hailux to integrate with external tools. The configuration file is located at `~/.hailux/mcp.toml`.

> Most MCP server documentation provides configuration examples in YAML format, while hailux uses TOML. Conversion rules: `mcpServers` → `mcp_servers`, YAML nested mappings → `[mcp_servers.name]` headers, YAML lists `- item` → TOML arrays `["item"]`, YAML key-value `key: value` → TOML `key = "value"`. You can simply paste the YAML configuration to hailux and let it handle the conversion.

### Transport Methods

**1) stdio — Local Subprocess (Most Common)**

```toml
[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/allowed/dir"]
```

With environment variables:

```toml
[mcp_servers.context7]
command = "npx"
args = ["-y", "@upstash/context7-mcp"]
env = { API_KEY = "your-api-key-here" }
```

**2) http — Remote Server**

Without authentication:

```toml
[mcp_servers.remote]
url = "https://example.com/mcp"
```

With header authentication:

```toml
[mcp_servers.secure]
url = "https://example.com/mcp"
headers = { Authorization = "Bearer your-token-here" }
```

### Field Descriptions

| Field | Applicable | Description |
|-------|------------|-------------|
| `command` | stdio | Executable program (must be in PATH, e.g., npx / uvx / node / python) |
| `args` | stdio | List of arguments to pass to the program |
| `env` | stdio | Environment variables to inject into the subprocess (optional) |
| `url` | http | Remote MCP endpoint |
| `headers` | http | Custom HTTP headers (optional, commonly used for authentication) |

Save the configuration and restart hailux to activate it. Use the `/mcp` command to view connection status. MCP servers connect asynchronously in the background; once connected, their tools are automatically registered in the current session.

> **Tip**: You can directly ask hailux to create Skills, custom commands, or edit MCP configuration — just tell it what you want, and it will automatically write the corresponding files for you.

## Subagent

Subagents are child agents that run independently from the main session, with their own session context and tool set. They are suitable for delegating complex multi-step tasks such as large-scale code searches, batch refactoring, and independent research.

### How to Use

There are two ways to trigger a subagent:

**1. Manual Invocation**

Type `@subagent:` followed by the subagent name and task description in the input box:

```
@subagent: general Search all TODO comments in the project, list file paths and contents
```

The main interface only displays the user input and the final result; the subagent's intermediate execution process is hidden.

**2. LLM Automatic Delegation**

During normal conversation, the LLM automatically determines whether to call the `task` tool to delegate work to a subagent based on task complexity. You can also explicitly request it in the conversation, for example, "use a subagent to complete this task".

### Built-in Subagents

| Name | Description |
|------|-------------|
| `general` | General-purpose subagent capable of multi-step research and coding tasks, with access to all tools (except `task` and `ask_user`) |

### How to Create a Custom Subagent

Subagent configuration files are `AGENTS.md` files placed at:

- **Global**: `~/.hailux/agents/<name>/AGENTS.md` (available to all projects)
- **Project-level**: `<working_dir>/.hailux/agents/<name>/AGENTS.md` (current project only, overrides a global agent with the same name)

#### AGENTS.md Format

```markdown
---
name: code-reviewer
description: Review code for bugs, security issues, and code style
tools: [bash, read, grep, glob]
skills: [code-review, help]
mcp: [context7, zread]
model: deepseek/deepseek-chat
---

You are a code review expert. Your responsibilities are:
1. Identify changed files and functions
2. Check for potential security vulnerabilities
3. Check for performance issues
4. Provide improvement suggestions (prioritized)
```

#### Frontmatter Fields

| Field | Required | Description |
|-------|----------|-------------|
| `name` | Yes | Unique subagent identifier |
| `description` | No | Summary description for the LLM to decide when to delegate |
| `tools` | No | Tool whitelist (JSON array format); if omitted, all built-in tools are available (except `task` and `ask_user`) |
| `skills` | No | List of available skill names (e.g., `[code-review, help]`); if omitted, no skills are loaded |
| `mcp` | No | List of available MCP server names (e.g., `[context7, zread]`); if omitted, no MCP tools are loaded |
| `model` | No | Model selector, format is `provider/model` (e.g., `deepseek/deepseek-chat`); if omitted, the main agent's model is used |

The body below the frontmatter is the subagent's system prompt.

#### Example: Create a Code Search Subagent

```sh
mkdir -p ~/.hailux/agents/searcher
```

Edit `~/.hailux/agents/searcher/AGENTS.md`:

```markdown
---
name: searcher
description: Quickly search the codebase for specific patterns, definitions, and references
tools: [grep, glob, read, bash]
---

You are a code search expert. Your task is to efficiently search the codebase and return precise results.

Working principles:
- Prefer using grep and glob for parallel searches
- Include file paths and line numbers when returning results
- Briefly categorize and summarize the results
```

Save and restart hailux, then you can use `@subagent: searcher search all async fn definitions` or let the LLM automatically delegate it.

### Notes

- The subagent's execution process is invisible to the main interface; only the final result is returned
- Subagents cannot use the `task` tool (to prevent recursion) or the `ask_user` tool (fully autonomous, no user interaction)
- Subagent sessions are linked to the main session via `parent_id`, and messages are independently persisted to the database
- You can restore a previous subagent session context via the `task_id` parameter to continue the conversation

## Runtime File Locations

| Path | Description |
|------|-------------|
| `~/.hailux/config.toml` | Provider API Keys, model configuration |
| `~/.hailux/mcp.toml` | MCP server definitions |
| `~/.hailux/AGENTS.md` | Global instructions (injected into system prompt) |
| `~/.hailux/skills/<name>/SKILL.md` | Global skills |
| `~/.hailux/commands/<name>.md` | Global custom commands |
| `~/.hailux/agents/<name>/AGENTS.md` | Global subagent definitions |
| `~/.hailux/db/chat.db` | SQLite session history database |
| `<working_dir>/AGENTS.md` | Project-level instructions (injected into system prompt) |
| `<working_dir>/.hailux/skills/<name>/SKILL.md` | Project-level skills |
| `<working_dir>/.hailux/commands/<name>.md` | Project-level custom commands |
| `<working_dir>/.hailux/agents/<name>/AGENTS.md` | Project-level subagent definitions |