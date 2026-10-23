use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use super::super::protocol::{CommandInfoDto, ModeStatus, PlanModeRequest, YoloRequest};
use super::super::state::WebServerState;
use super::{err500, resolve_dir, strip_verbatim};

fn mode_status(state: &WebServerState) -> ModeStatus {
    ModeStatus {
        yolo: state.manager.global_yolo(),
        plan_mode: state.manager.global_plan(),
    }
}

/// 当前全局模式（对账用）。仅读原子标志，不触碰会话锁，
/// SSE 流长时间持锁时也能即时返回。
pub(super) async fn get_mode(State(state): State<Arc<WebServerState>>) -> Response {
    Json(mode_status(&state)).into_response()
}

/// YOLO / Plan 为服务器级全局模式（跨 work_dir、跨会话保持）：
/// 广播即时生效（YOLO 绕过会话锁）。旧客户端携带的 `work_dir` 字段
/// 会被 serde 忽略（未知字段默认跳过）。返回设置后的最终状态，
/// 前端以其为准（不做本地猜测）。
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
    // 持久化到 config.toml（下次启动 / 新建会话保持当前模式；空串 = Normal，
    // save 时不写该字段，保持文件最小化）。配置写锁串行化读-改-写，
    // 防止并发请求互相覆盖（对齐 switch_model）。保存失败不切换内存模式。
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

// ── 斜杠命令 ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct CommandsQuery {
    work_dir: Option<String>,
}

/// Web 端可用的斜杠命令列表（输入框 `/` 补全用）。
///
/// - prompt 型：`/init` 与自定义命令，`POST /api/chat` 时由后端展开为完整提示词
/// - ui 型：`/compact`（前端本地处理，走 `POST /api/compact`）。
///   其余 TUI UI-action 命令（/new /sessions /plan /models /skills /mcp /yolo /tasks /exit）
///   在 Web 端均有对应的界面入口或无意义，不列出。
pub(super) async fn list_commands(
    State(state): State<Arc<WebServerState>>,
    Query(q): Query<CommandsQuery>,
) -> Response {
    let dir = resolve_dir(&state, q.work_dir.as_deref());
    let registry = crate::agent::CommandRegistry::discover(&dir).unwrap_or_default();

    let mut commands = vec![CommandInfoDto {
        name: "compact".to_string(),
        description: "压缩上下文（总结历史对话）".to_string(),
        kind: "ui".to_string(),
    }];
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

// ── 用量统计 ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct StatsQuery {
    /// 可选项目筛选（空/缺省 = 全局统计）
    work_dir: Option<String>,
    /// 统计窗口天数（默认 30；0 = 全部）
    #[serde(default)]
    days: Option<u32>,
    /// 最近请求明细条数（默认 50）
    #[serde(default)]
    limit: Option<u32>,
}

/// token 用量统计（全局或按项目筛选，基于 messages 表的请求级 usage 行）
pub(super) async fn get_stats(
    State(state): State<Arc<WebServerState>>,
    Query(q): Query<StatsQuery>,
) -> Response {
    // work_dir 语义区分：显式传空串/缺省 = 全局；传值 = 该项目
    let filter = q
        .work_dir
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty());
    // work_dir=all 保持全局（前端项目筛选器的“全部项目”选项不发该参数）
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
        // 对齐 /api/workdirs 契约：剥离 DB 中的 verbatim 前缀后再下发
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
