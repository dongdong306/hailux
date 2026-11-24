use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use super::super::protocol::{
    CreateSessionRequest, InterruptRequest, SessionInfo, ValidateWorkdirRequest, WorkdirInfo,
};
use super::super::state::WebServerState;
use super::{err500, resolve_dir, strip_verbatim};

#[derive(Deserialize)]
pub(super) struct SessionsQuery {
    work_dir: Option<String>,
    /// all=true：跨工作目录列出全部顶层会话（侧边栏按目录分组用）
    #[serde(default)]
    all: bool,
}

pub(super) async fn list_sessions(
    State(state): State<Arc<WebServerState>>,
    Query(q): Query<SessionsQuery>,
) -> Response {
    // One snapshot of the running set, to mark the sidebar live indicator
    let running_ids = state.buses.running_ids();
    if q.all {
        return match state.manager.storage().list_all_top_level_sessions().await {
            Ok(list) => Json(
                list.into_iter()
                    .map(|(s, dir)| SessionInfo {
                        running: running_ids.contains(&s.id),
                        id: s.id,
                        title: s.title,
                        model: s.model,
                        updated_at: s.updated_at,
                        work_dir: strip_verbatim(&dir),
                    })
                    .collect::<Vec<_>>(),
            )
            .into_response(),
            Err(e) => err500(e),
        };
    }

    // storage 层已做 work_dir 变体匹配（带/不带 `\\?\` 前缀），单次查询即可
    let requested = q
        .work_dir
        .unwrap_or_else(|| state.default_work_dir.display().to_string());
    match state
        .manager
        .storage()
        .list_top_level_sessions(&requested)
        .await
    {
        Ok(sessions) => Json(
            sessions
                .into_iter()
                .map(|s| SessionInfo {
                    running: running_ids.contains(&s.id),
                    id: s.id,
                    title: s.title,
                    model: s.model,
                    updated_at: s.updated_at,
                    work_dir: strip_verbatim(&requested),
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => err500(e),
    }
}

pub(super) async fn create_session(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<CreateSessionRequest>,
) -> Response {
    let dir = resolve_dir(&state, req.work_dir.as_deref());
    let resolved = state.manager.resolved();
    let dir_str = dir.display().to_string();
    // Reuse the latest session while it is still blank (empty title = never
    // messaged) to avoid stacking empty sessions; pure storage ops — no
    // ChatSession instance needed
    let reuse = match state
        .manager
        .storage()
        .list_top_level_sessions(&dir_str)
        .await
    {
        Ok(list) => list.first().filter(|s| s.title.is_empty()).cloned(),
        Err(_) => None,
    };
    if let Some(latest) = reuse {
        return Json(SessionInfo {
            running: false,
            id: latest.id,
            title: String::new(),
            model: latest.model,
            updated_at: latest.updated_at,
            work_dir: strip_verbatim(&dir_str),
        })
        .into_response();
    }
    match state
        .manager
        .storage()
        .create_session(&resolved.display, &dir_str)
        .await
    {
        Ok(id) => Json(SessionInfo {
            running: false,
            id,
            title: String::new(),
            model: resolved.display.clone(),
            updated_at: String::new(),
            work_dir: strip_verbatim(&dir_str),
        })
        .into_response(),
        Err(e) => err500(e),
    }
}

pub(super) async fn get_session(
    State(state): State<Arc<WebServerState>>,
    Path(id): Path<String>,
) -> Response {
    match state.manager.storage().load_messages(&id).await {
        Ok(messages) => {
            // 附带压缩摘要标记（前端渲染分隔线）
            let summary = state
                .manager
                .storage()
                .get_compact_summary(&id)
                .await
                .ok()
                .flatten();
            // Subscribe cursor baseline: the durable watermark (history covers
            // everything <= head_seq); no bus = the server never ran a turn
            // for this session, head_seq stays 0
            let bus = state.buses.get(&id);
            #[derive(serde::Serialize)]
            struct SessionDetail {
                messages: Vec<crate::storage::StoredMessage>,
                #[serde(skip_serializing_if = "Option::is_none")]
                compact_summary: Option<String>,
                epoch: u64,
                head_seq: u64,
                running: bool,
            }
            Json(SessionDetail {
                messages,
                compact_summary: summary,
                epoch: bus.as_ref().map(|b| b.epoch()).unwrap_or(state.epoch),
                head_seq: bus.as_ref().map(|b| b.durable_seq()).unwrap_or(0),
                running: bus.is_some_and(|b| b.is_running()),
            })
            .into_response()
        }
        Err(e) => err500(e),
    }
}

/// 导出会话为自包含 HTML 分享文件（复用 `share` 模块，与 TUI /share 同源）。
/// Content-Disposition 走 ASCII 回退名 + UTF-8 扩展名（RFC 5987），避免
/// 中文标题在不同浏览器的编码歧义。
pub(super) async fn export_session(
    State(state): State<Arc<WebServerState>>,
    Path(id): Path<String>,
) -> Response {
    match crate::share::export_session_html(state.manager.storage(), &id).await {
        Ok(export) => {
            let ascii_fallback = format!(
                "hailux-share-{}.html",
                id.chars().take(8).collect::<String>()
            );
            let utf8_name = format!("hailux-share-{}.html", percent_encode(&export.slug));
            let resp = axum::response::Response::builder()
                .header("Content-Type", "text/html; charset=utf-8")
                .header(
                    "Content-Disposition",
                    format!(
                        "attachment; filename=\"{ascii_fallback}\"; filename*=UTF-8''{utf8_name}"
                    ),
                )
                .body(axum::body::Body::from(export.html));
            match resp {
                Ok(r) => r,
                Err(e) => err500(color_eyre::eyre::eyre!(e.to_string())),
            }
        }
        Err(e) => err500(e),
    }
}

/// 百分号编码（RFC 3986 unreserved 之外全编码），用于 filename* 扩展名。
pub(super) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub(super) async fn delete_session(
    State(state): State<Arc<WebServerState>>,
    Path(id): Path<String>,
) -> Response {
    // Evict the in-memory instance and flip its cancel flag first (a running
    // task is interrupted immediately), then delete the DB row. The registry
    // key is the session_id — no owner-dir lookup needed.
    state.manager.remove_session(&id);
    match state.manager.storage().delete_session(&id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err500(e),
    }
}

pub(super) async fn list_workdirs(State(state): State<Arc<WebServerState>>) -> Response {
    match state.manager.list_work_dirs().await {
        Ok(dirs) => Json(
            dirs.into_iter()
                .map(|d| WorkdirInfo {
                    path: strip_verbatim(&d.work_dir),
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => err500(e),
    }
}

/// 服务器启动目录（canonical，剥离 verbatim 前缀）—— 前端初始项目目录
pub(super) async fn get_default_workdir(State(state): State<Arc<WebServerState>>) -> Response {
    Json(WorkdirInfo {
        path: strip_verbatim(&state.default_work_dir.display().to_string()),
    })
    .into_response()
}

pub(super) async fn validate_workdir(Json(req): Json<ValidateWorkdirRequest>) -> Response {
    let path = std::path::Path::new(&req.path);
    if !path.is_dir() {
        return (StatusCode::NOT_FOUND, format!("目录不存在: {}", req.path)).into_response();
    }
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let s = canonical.to_string_lossy().to_string();
    let s = s.strip_prefix(r"\\?\").map(|x| x.to_string()).unwrap_or(s);
    Json(WorkdirInfo { path: s }).into_response()
}

// ── 文件提及搜索（@ 补全）────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct FilesQuery {
    q: String,
    work_dir: Option<String>,
}

pub(super) async fn search_files(
    State(state): State<Arc<WebServerState>>,
    Query(q): Query<FilesQuery>,
) -> Response {
    let dir = resolve_dir(&state, q.work_dir.as_deref());
    let keyword = q.q.to_lowercase();
    let root = dir.clone();

    let result = tokio::task::spawn_blocking(move || -> Vec<String> {
        let walker = ignore::WalkBuilder::new(&root)
            .hidden(false)
            .git_ignore(true)
            .git_exclude(true)
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy().to_lowercase();
                let ignored = crate::agent::IGNORED_DIRS.contains(&name.as_str())
                    || name == "node_modules"
                    || name == "target";
                !ignored
            })
            .build();
        let mut hits = Vec::new();
        for entry in walker.flatten() {
            let Ok(rel) = entry.path().strip_prefix(&root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if rel.is_empty() {
                continue;
            }
            let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());
            let rel = if is_dir { format!("{rel}/") } else { rel };
            if keyword.is_empty() || rel.to_lowercase().contains(&keyword) {
                hits.push(rel);
            }
        }
        hits.sort();
        hits.truncate(50);
        hits
    })
    .await;

    match result {
        Ok(files) => Json(files).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ── 运行时控制 ───────────────────────────────────────────────

pub(super) async fn interrupt(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<InterruptRequest>,
) -> Response {
    // Precise interrupt: locate the instance by session_id only (no session
    // lock; the cancel flag flips immediately)
    let Some(session_id) = req.session_id.as_deref() else {
        return (
            StatusCode::BAD_REQUEST,
            "缺少 session_id，无法定位要中断的会话",
        )
            .into_response();
    };
    if state.manager.interrupt_session(session_id) {
        StatusCode::OK.into_response()
    } else {
        // No instance = nothing running (a run always builds its instance
        // first; a stray interrupt on an idle instance is reset by the agent
        // at the next run start)
        (
            StatusCode::NOT_FOUND,
            "该会话没有运行中的任务（或服务器尚未加载它）",
        )
            .into_response()
    }
}
