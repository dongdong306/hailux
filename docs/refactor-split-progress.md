# 巨型文件拆分重构：方案与进度记录

> **状态：✅ 全部完成（2026-10）**。10 个巨型文件均已拆分并通过全量验证
> （fmt / clippy -D warnings / 298 单元测试 + 2 集成测试 / web build /
> TUI-only build）。共 11 个提交，见文末提交清单。本文档保留作方法学参考。

## 一、背景与目标

项目中有若干 1000+ 行的巨型源文件，影响可读性与维护性。目标：**纯结构性拆分**
（零行为变更），将每个巨型文件拆为按职责划分的小模块文件，保持所有对外模块路径
与 API 不变，每拆一个文件独立提交，全程过 CI 门禁（fmt / clippy -D warnings / test）。

## 二、总体方法学（所有拆分必须遵守）

1. **目录化保路径**：`X.rs` → `X/mod.rs`，`crate::xxx::X` 等模块路径全部不变，
   外部 `use` 语句零改动。
2. **mod.rs re-export 一切搬走的名字**，blast radius 最小（动手前先
   `git grep` 确认外部引用了哪些名字，逐个 re-export）。
3. **测试外移**：`#[cfg(test)] mod tests;` + 独立 `tests.rs`，主文件立减 200–500 行。
4. **Rust 可见性技巧**（本次大量使用，后续步骤同样适用）：
   - 私有项（含私有字段）对**定义模块及其后代模块**可见 → `mod.rs` 的私有 helper
     可被子模块 `use super::xxx` 直接使用，子模块中的测试也能访问 mod.rs 的私有字段；
   - 子模块内定义、mod.rs（或其他兄弟子模块）需要用的项标 `pub(super)`；
   - `pub(crate) use` re-export 要求源项至少 `pub(crate)`，`pub(super)` 源不能被
     `pub(crate) use` 提升（会报 E0364）；
   - 同一类型可以有多个 `impl` 块分布在多个文件（db.rs 拆分即用此法）。
5. **每步验证**：`cargo fmt` → `cargo clippy --lib --tests -- -D warnings` →
   `cargo test --lib`；动 `web/` 后另跑 `cd web && npm run build`。
6. **一个文件一个 commit**，Conventional Commits 英文消息
   （如 `refactor(agent): split tools.rs into per-tool modules`）。
7. 平台注意：所有 `#[cfg(windows)] / #[cfg(not(windows))]` 变体搬运时保留标注；
   `.gitattributes` 强制 `*.rs` LF，交给 cargo fmt/git 处理，不手动转换。

## 三、巨型文件清单（拆分前实测行数）

| 文件 | ~行数 | 状态 |
|---|---|---|
| `src/agent/tools.rs` | 2600 | ✅ `e828def` |
| `src/tui/history_cell.rs` | 2530 | ✅ `d4b7dd0` |
| `src/agent/agent.rs` | 1800 | ✅ `87d1c4a` |
| `src/storage/db.rs` | 1730 | ✅ `72d1e6f` |
| `src/tui/app/overlay.rs` | 1700 | ✅ `11b68c2` |
| `src/web/handlers.rs` | 1480 | ✅ `e1c691a` |
| `src/config.rs` | 1320 | ✅ `2bac9b9` |
| `web/src/store/app-store.ts` | 1220 | ✅ `880a946` |
| `src/tui/app/chat_input.rs` | 1150 | ✅ `9c0f33b`（→ paste.rs + attachments.rs） |
| `src/agent/subagent.rs` | 1100 | ✅ `feb2f92` |

附加修复：`41d40e4`（config 的 `find_provider_def` re-export 加 `#[cfg(feature = "web")]`，
消除 TUI-only 构建的 unused import 告警）。

## 四、已完成工作（已提交）

### 1. `src/agent/tools.rs` → `src/agent/tools/`（commit `e828def`）

```
tools/
├── mod.rs        — Tool trait、ToolExecution、ToolExecuteError、ToolRegistry、
│                   共享 helpers（external_dir_*、resolve_tool_path、argument_str、
│                   is_absoluteish、display_canonical）+ 全量 re-export
├── ask.rs        — AskTool
├── bash.rs       — BashTool + decode_output/combine_output + Windows shell 探测/
│                   UTF-8 prologue + run_shell_command + 管道排空/杀树（cfg 变体保留）
├── diff.rs       — serialize_diff_data（edit/write 共用）
├── read.rs / edit.rs / write.rs / web_fetch.rs / grep.rs / glob.rs / todo.rs
└── tests.rs      — 原 520 行测试
```

要点：`pub(crate) use bash::plan_mode_bash_denial`（agent 流程在用）；
`#[cfg(windows)] pub use bash::prewarm_shell_description`；
bash.rs 内部函数 `pub(super)` 供 tests.rs 经 `use super::bash::*` 访问。

### 2. `src/tui/history_cell.rs` → `src/tui/history_cell/`（commit `d4b7dd0`）

```
history_cell/
├── mod.rs           — HistoryCell trait、ToolCategory、颜色常量（CHAT_FG 等）、
│                      wrap_text/truncate_str/extract_xml_tag/format_seconds、
│                      messages_to_cells、tool_call_summary / tool_result_summary /
│                      subagent_step_summary + re-export
├── message_cells.rs — SessionHeader/UserMessage/AgentMarkdown/Reasoning/Tooltip/
│                      Done/Divider（DividerCell pub(super)，字段 pub(super)）
├── tool_cells.rs    — ToolCell、PlainStepCell(pub(super))、ExploringCell、
│                      is_readonly_tool/explore_item_from (pub(super))
├── task_cells.rs    — TaskToolCell、TaskStep、task_items_from_args、
│                      TaskOutcome(字段 pub(super))、task_outcomes_from_result
├── todo_cells.rs    — TodoStatus/TodoItem/TodoCell
├── diff.rs          — diff 渲染（render_diff_from_json pub(super)）
├── ask_user_render.rs — parse_ask_user_pairs + render_ask_user_result
│                        （4 处 lockstep 契约实现之一，纯搬动未改逻辑）
└── tests.rs
```

要点：`CHAT_FG` 等常量保持 `crate::tui::history_cell::XXX` 路径（chat_widget.rs 在用）。

### 3. `src/agent/agent.rs` → `src/agent/agent/`（commit `87d1c4a`）

```
agent/
├── mod.rs     — Agent struct + impl（new/chat_stream/sync/compaction）+ compact_retry
│                + CANCEL_POLL_INTERVAL(pub(super)) + `pub use messages::build_user_message`
├── messages.rs — build_user_message(pub)、sanitize_messages_for_request、
│                 strip_media_for_compaction、tool_attachment_note、image_content_part、
│                 vision 降级 helpers
├── stream.rs   — AgentStreamState(字段 pub(super))、PartialToolCall、StreamOutput、
│                 AttemptOutcome、attempt_once、run_stream_loop、push_partial_assistant、
│                 emit_persist_message、finalize_messages、PLAN_MODE_PROMPT
├── dispatch.rs — handle_tool_calls_stream、evaluate_permission_requests、
│                 finalize_cancelled（工具分发 + 权限 + 取消收尾）
└── tests.rs
```

要点：`agent/mod.rs`（上层）已有 `#[allow(clippy::module_inception)] mod agent;`，
目录化后无需改动；tests 直接访问 mod.rs 私有字段 `agent.messages`（后代模块可见）。

### 顺带修复

`cargo fmt` 修正了上一版本遗留的 `src/session/manager.rs` 格式漂移（随 commit
`e828def` 一并提交，与 CI `fmt --check` 门禁对齐）。

## 五、db.rs 拆分（✅ 已完成，commit `72d1e6f`）

拆分结构（恢复执行时的中断点，现已收尾）：

```
db/
├── mod.rs      — ChatStorage struct（pool 字段 pub(super)）+ new/migration_error/
│                 rebuild/run_migration(pub(super))/db_path/now_iso(pub(super))
│                 + ensure_verbatim / work_dir_variants (pub(super))
│                 + re-export convert 与 models 的公共名字
├── models.rs   — MessageRole + 全部 DTO（SessionSummary/StoredMessage/Usage* 等）
├── sessions.rs — impl ChatStorage：会话 CRUD/权限/subsession/delete/touch/list_*
├── messages.rs — impl ChatStorage：消息 CRUD/query_messages/压缩标记/摘要/
│                 repair_orphaned_tool_calls
├── usage.rs    — impl ChatStorage：usage_* 五个统计 + usage_filter_binds
├── convert.rs  — extract_*/compatible_*/to_stored_message/from_stored_message
└── tests.rs    — new_in_memory(pub(crate)) + 全部测试
```

`src/storage/mod.rs` 未改动（其 `pub use db::{...}` 路径不变）。

中断时的遗留 warning 已按推荐方案收尾：`convert.rs` 中 4 个 `compatible_*`
函数降为私有（原文件里它们虽是 `pub` 但无任何外部使用者），并从 `db/mod.rs`
删除了对应 re-export。

## 六、后续拆分清单（✅ 全部完成，各步骤均独立提交）

4. **`src/web/handlers.rs` → `web/handlers/`**（目录已建好空壳 `src/web/handlers/`，
   注意先删掉空目录或直接写入文件）：
   - `mod.rs`：`api_router` + `resolve_dir`/`err500` + 各域 re-export；
   - `sessions.rs`（会话/workdir/文件搜索/打断）、`permissions.rs`（permission_reply/ask_reply）、
     `models.rs`（模型与 provider 增删查 + remove_error_response）、
     `skills.rs`（~400 行，含 skill 文件系统 helpers）、`mcp.rs`（~270 行）、
     `misc.rs`（mode/commands/stats）、`tests.rs`。
   - 注意：`web/mod.rs` 中 `mod handlers;` 与 `handlers::api_router()` 路径不变。
5. **`src/tui/app/overlay.rs` → `src/tui/app/overlay/`**：
   - `mod.rs`（handle_picker_event_inner 分发 + open_* + filter_sessions）、
     `session.rs`、`model.rs`（picker + add model 表单 ~290 行）、`setup.rs`（~370 行）、
     `stats.rs`（load_stats_data/apply_stats_scope/stats_picker_step ~500 行）、
     `mcp.rs`（handle_mcp_* 三个）、`skills.rs`（handle_skills_* 两个 + switch_model 可留 mod.rs）。
   - 可见性：子文件里的 `pub(super) fn` 指 overlay 模块；若 app/mod.rs 直接调用需
     `pub(in crate::tui::app)` 或经 overlay/mod.rs re-export —— 动手前先
     `git grep -n "handle_.*_event\|open_.*_viewer" src/tui/app/mod.rs` 确认调用方。
6. **`src/config.rs` → `src/config/`**：
   - `types.rs`（PermissionConfig/RetryConfigToml/ProviderEntry/ModelEntry/RemoveError/
     ResolvedModel 等）、`providers.rs`（fallback_model_values/predefined_*）、
     `mod.rs`（Config + 两个 impl 块 + load/save + config_to_toml）、`tests.rs`。
   - `lib.rs` 等处 `use crate::config::{...}` 路径需保持（mod.rs re-export）。
7. **`web/src/store/app-store.ts`**（前端，纯 TS 重构）：
   - 抽 `store/types.ts`（ChatItem/ModeStatus/Skill*/Mcp* 等 interface）；
   - 抽 `store/shared-events.ts`（handleSharedEvent/itemOps/compactMarkerText/readError/
     isToolAttachmentMessage）；
   - 大 action 体（sendMessage ~180 行、switchSession ~170 行、runCompact）抽为接收
     `(set, get)` 的 helper（zustand 惯用法），主 store 文件保留 create() 骨架；
   - 验证：`cd web && npm run build`（tsc 严格模式兜底）。
8. **`src/agent/subagent.rs` → `subagent/`**：
   - `discovery.rs`（parse_agent_md/parse_tools_list/load_subagent/scan_root/
     discover_subagents/format_available_subagents/builtin_general_subagent）、
     `task_tool.rs`（TaskSpec/parse_task_specs/resolve_task_specs/TaskTool）、
     `runner.rs`（SharedTaskCtx + run_subagent_task + xml_escape/truncate_chars）、
     mod.rs re-export（TaskTool/SubagentConfig/SharedConfig 被 tui/types.rs 等引用）、
     `tests.rs`。
9. **`src/tui/app/chat_input.rs`**：
   - 抽 `tui/app/paste.rs`（PasteBurst 状态机 + CharDecision/FlushResult + retro 逻辑 +
     相关测试——AGENTS.md 标注的复杂状态机，**纯搬动零改动**）；
   - 抽 `tui/app/attachments.rs`（read_image_attachment/图片占位符/attach 相关 helper）；
   - 主文件保留 impl App 按键处理。注意 `tui/input.rs` 引用 ElementKind 不动。
10. **最终全量验证**：
    - `cargo fmt`、`cargo clippy --lib --tests -- -D warnings`、`cargo test`（全部，
      含 tests/integration.rs）、`cargo build`；
    - `cd web && npm run build`（本次只动了 app-store.ts，必须跑）；
    - `cargo build --no-default-features`（TUI-only 构建，确认无 web 依赖泄漏）。

## 七、风险与注意事项备忘

- **ask_user 契约**：`tui/ask_user.rs::ask_escape`、`history_cell/ask_user_render.rs::
  parse_ask_user_pairs`、`web/src/components/dialogs.tsx::formatAskReply`、
  `web/src/components/assistant-ui/tool-fallback.tsx::parseAskUserPairs` 四处 lockstep，
  本次重构任何一步都不得改动其逻辑。
- **Windows cfg 变体**：tools/bash.rs 的 `is_absoluteish`/`kill_process_tree`/
  `windows_shell_*`、db 的 `work_dir_variants` 均有双平台实现，搬运时保留 cfg 标注；
  本机为 Windows，非 Windows 侧依赖 CI（Ubuntu）兜底。
- **sqlx::migrate!("./migrations")** 相对 Cargo.toml 解析，db.rs 目录化不影响。
- **`pub(crate) use` 不能提升 pub(super) 源**（E0364），见方法学第 4 条。
- **前端行尾**：`web/*.ts(x)` 走 core.autocrlf，不手动转换。
- 提交信息一律英文 Conventional Commits，不加 AI 署名。

## 八、验证命令速查

```sh
cargo fmt
cargo clippy --lib --tests -- -D warnings
cargo test --lib          # 快速内环（297 个单元测试）
cargo test                # 全量（含 tests/integration.rs）
cd web && npm run build   # 动 web/src 后必跑
cargo build --no-default-features   # 最终验证时跑（TUI-only）
```

---
*本文档为已完成的拆分重构方法学记录；各提交哈希见第三节清单。*
