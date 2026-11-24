use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use super::super::protocol::{
    CommandInfoDto, EffortRequest, ModeStatus, PlanModeRequest, YoloRequest,
};
use super::super::state::WebServerState;
use super::{err500, resolve_dir, strip_verbatim};

fn mode_status(state: &WebServerState) -> ModeStatus {
    ModeStatus {
        yolo: state.manager.global_yolo(),
        plan_mode: state.manager.global_plan(),
        reasoning_effort: state.manager.global_effort(),
    }
}

/// Current global mode (for reconciliation). Reads atomic flags only,
/// never the session lock — returns immediately even while an SSE stream
/// holds it for a long time.
pub(super) async fn get_mode(State(state): State<Arc<WebServerState>>) -> Response {
    Json(mode_status(&state)).into_response()
}

/// YOLO / Plan are server-level global modes (kept across work dirs and
/// sessions): the broadcast applies immediately (YOLO bypasses the session
/// lock). A stale client sending `work_dir` is tolerated (serde skips
/// unknown fields). Returns the resulting state — the frontend must treat
/// it as authoritative instead of guessing locally.
pub(super) async fn set_plan_mode(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<PlanModeRequest>,
) -> Response {
    state.manager.set_global_plan(req.enabled);
    Json(mode_status(&state)).into_response()
}

pub(super) async fn set_yolo(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<YoloRequest>,
) -> Response {
    // Persisted to config.toml (the mode survives restarts / new sessions;
    // empty string = Normal, omitted on save to keep the file minimal). The
    // config write lock serializes read-modify-write so concurrent requests
    // can't overwrite each other (mirrors switch_model). A save failure
    // leaves the in-memory mode untouched.
    let _guard = state.config_write_lock.lock().await;
    let mut cfg = state.manager.cfg();
    cfg.permission.mode = if req.enabled {
        "yolo".to_string()
    } else {
        String::new()
    };
    if let Err(e) = cfg.save() {
        return err500(e);
    }
    state.manager.set_cfg(cfg);
    state.manager.set_global_yolo(req.enabled);
    Json(mode_status(&state)).into_response()
}

/// Set reasoning effort (server-level global): writes config.toml and
/// broadcasts to all built sessions. Locked sessions are skipped; the chat
/// entry re-syncs as a fallback (mirrors set_global_plan).
pub(super) async fn set_effort(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<EffortRequest>,
) -> Response {
    let _guard = state.config_write_lock.lock().await;
    let mut cfg = state.manager.cfg();
    cfg.reasoning_effort = req.effort;
    if let Err(e) = cfg.save() {
        return err500(e);
    }
    state.manager.set_cfg(cfg);
    state.manager.set_global_effort(req.effort);
    Json(mode_status(&state)).into_response()
}

// ── Slash commands ───────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct CommandsQuery {
    work_dir: Option<String>,
}

/// Slash commands available on the web (input `/` completion).
///
/// - prompt: `/init` and custom commands, expanded by the backend on
///   `POST /api/chat` / `/api/interject`
/// - view: pure UI actions (open Skills/MCP/stats panels, model menu),
///   immediate in any state
/// - setting: /plan /yolo /effort match the toolbar toggles (immediate in
///   any state); /compact opens a new run, so while running the frontend
///   defers it until this run's RunComplete (with the queue drained).
///   Other TUI UI-action commands (/new /sessions /tasks /share /exit) all
///   have web UI entries or are meaningless here, so they are not listed.
pub(super) async fn list_commands(
    State(state): State<Arc<WebServerState>>,
    Query(q): Query<CommandsQuery>,
) -> Response {
    let dir = resolve_dir(&state, q.work_dir.as_deref());
    let registry = crate::agent::CommandRegistry::discover(&dir).unwrap_or_default();

    let mut commands: Vec<CommandInfoDto> = [
        ("compact", "压缩上下文（总结历史对话）", "setting"),
        ("plan", "切换规划模式（只读）", "setting"),
        ("yolo", "切换 YOLO 模式（跳过权限确认）", "setting"),
        (
            "effort",
            "设置思考强度（/effort low|high|max 直接设置）",
            "setting",
        ),
        ("skills", "查看已加载的 skill", "view"),
        ("mcp", "查看 MCP 服务器", "view"),
        ("stats", "查看 token 用量统计", "view"),
        ("models", "切换模型", "view"),
    ]
    .into_iter()
    .map(|(name, description, kind)| CommandInfoDto {
        name: name.to_string(),
        description: description.to_string(),
        kind: kind.to_string(),
    })
    .collect();
    commands.extend(
        registry
            .list()
            .into_iter()
            .map(|(name, description)| CommandInfoDto {
                name: name.to_string(),
                description: description.to_string(),
                kind: "prompt".to_string(),
            }),
    );
    Json(commands).into_response()
}

// ── Usage stats ──────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct StatsQuery {
    /// Optional project filter (empty/absent = global stats)
    work_dir: Option<String>,
    /// Stats window in days (default 30; 0 = all)
    #[serde(default)]
    days: Option<u32>,
    /// Recent-request detail row count (default 50)
    #[serde(default)]
    limit: Option<u32>,
}

/// Token usage stats (global or per project, based on per-request usage
/// rows in the messages table)
pub(super) async fn get_stats(
    State(state): State<Arc<WebServerState>>,
    Query(q): Query<StatsQuery>,
) -> Response {
    // work_dir semantics: explicit empty/absent = global; a value = that project
    let filter = q
        .work_dir
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty());
    // work_dir=all stays global (the frontend project filter's "all projects"
    // option never sends this param)
    let filter = if filter.as_deref() == Some("all") {
        None
    } else {
        filter
    };
    let days = q.days.unwrap_or(30).min(365);
    let limit = q.limit.unwrap_or(50).clamp(1, 500);

    let result = async {
        let storage = state.manager.storage();
        let summary = storage.usage_summary(filter.as_deref(), days).await?;
        let total = storage.usage_summary(filter.as_deref(), 0).await?;
        let daily = storage.usage_daily(filter.as_deref(), days).await?;
        let by_model = storage.usage_by_model(filter.as_deref(), days).await?;
        let mut recent = storage
            .list_recent_usage(filter.as_deref(), days, limit)
            .await?;
        // Match the /api/workdirs contract: strip the verbatim prefix stored
        // in the DB before sending
        for r in &mut recent {
            r.work_dir = strip_verbatim(&r.work_dir);
        }
        Ok::<_, color_eyre::eyre::Report>(super::super::protocol::StatsResponse {
            days,
            summary,
            total,
            daily,
            by_model,
            recent,
        })
    }
    .await;

    match result {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => err500(e),
    }
}
