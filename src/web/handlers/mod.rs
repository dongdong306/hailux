//! REST handlers：会话管理、工作目录、权限/提问回复、模型、技能、MCP、模式。

use std::sync::Arc;

use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use super::sse;
use super::state::WebServerState;

mod mcp;
mod misc;
mod models;
mod permissions;
mod sessions;
mod skills;

#[cfg(test)]
mod tests;

pub fn api_router() -> Router<Arc<WebServerState>> {
    Router::new()
        .route("/api/chat", post(sse::chat_handler))
        .route("/api/compact", post(sse::compact_handler))
        .route(
            "/api/sessions",
            get(sessions::list_sessions).post(sessions::create_session),
        )
        .route(
            "/api/sessions/{id}",
            get(sessions::get_session).delete(sessions::delete_session),
        )
        .route("/api/workdirs", get(sessions::list_workdirs))
        .route("/api/default-workdir", get(sessions::get_default_workdir))
        .route("/api/workdirs/validate", post(sessions::validate_workdir))
        .route("/api/files", get(sessions::search_files))
        .route(
            "/api/permission/{request_id}/reply",
            post(permissions::permission_reply),
        )
        .route("/api/ask/{request_id}/reply", post(permissions::ask_reply))
        .route("/api/interrupt", post(sessions::interrupt))
        .route(
            "/api/models",
            get(models::list_models).post(models::switch_model),
        )
        .route(
            "/api/models/custom",
            post(models::create_model).delete(models::delete_model),
        )
        .route(
            "/api/providers",
            get(models::list_providers).delete(models::delete_provider),
        )
        .route(
            "/api/skills",
            get(skills::list_skills)
                .post(skills::create_skill)
                .put(skills::update_skill)
                .delete(skills::delete_skill),
        )
        .route("/api/skills/file", get(skills::read_skill_file))
        .route(
            "/api/mcp",
            get(mcp::mcp_status)
                .post(mcp::create_mcp_server)
                .put(mcp::update_mcp_server)
                .delete(mcp::delete_mcp_server),
        )
        .route("/api/plan-mode", post(misc::set_plan_mode))
        .route("/api/yolo", post(misc::set_yolo))
        .route("/api/mode", get(misc::get_mode))
        .route("/api/commands", get(misc::list_commands))
        .route("/api/stats", get(misc::get_stats))
        .route("/api/instance", get(instance_info))
}

// ── 实例探测 ─────────────────────────────────────────────────

/// 实例探测端点：`hailux web` 启动时用于识别同机其他 hailux Web UI
/// 实例。响应标记值与 `mod.rs::detect_hailux_instance` 锁步
/// （共享 [`super::INSTANCE_MARKER_APP`]）。
async fn instance_info() -> impl IntoResponse {
    Json(serde_json::json!({ "app": super::INSTANCE_MARKER_APP }))
}

// ── 辅助 ─────────────────────────────────────────────────────

pub(super) fn resolve_dir(state: &WebServerState, work_dir: Option<&str>) -> std::path::PathBuf {
    match work_dir {
        Some(d) if !d.is_empty() => std::path::PathBuf::from(d),
        _ => state.default_work_dir.clone(),
    }
}

pub(super) fn err500(e: color_eyre::eyre::Report) -> Response {
    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
}

/// 剥离 Windows verbatim 前缀，前端展示用
pub(crate) fn strip_verbatim(path: &str) -> String {
    path.strip_prefix(r"\\?\")
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.to_string())
}
