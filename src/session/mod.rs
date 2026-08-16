//! 会话编排层：UI 无关的 ChatSession / SessionManager。
//!
//! 层级：`tui/`、`web/`（表现层）→ `session/`（本模块）→ `agent/` + 领域层。
//! 封装 "Agent + Storage + MCP + Permission + work_dir 发现" 的组合，
//! TUI 与 Web 共用同一套会话生命周期语义。

pub mod manager;
#[allow(clippy::module_inception)]
pub mod session;

pub use manager::SessionManager;
pub use session::{ChatSession, SessionShared};
