//! Session orchestration layer: UI-agnostic ChatSession / SessionManager.
//!
//! Layering: `tui/`, `web/` (presentation) -> `session/` (this module) ->
//! `agent/` + the domain layer. Encapsulates the combination of
//! "Agent + Storage + MCP + Permission + work_dir discovery"; TUI and Web
//! share the same session lifecycle semantics.

pub mod manager;
#[allow(clippy::module_inception)]
pub mod session;
pub mod title;

pub use manager::SessionManager;
pub use session::{ChatSession, SessionShared};
