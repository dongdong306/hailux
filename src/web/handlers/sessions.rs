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
    if q.all {
        return match state.manager.storage().list_all_top_level_sessions().await {
            Ok(list) => Json(
                list.into_iter()
                    .map(|(s, dir)| SessionInfo {
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
    let session_arc = match state.manager.get_or_create(&dir).await {
        Ok(s) => s,
        Err(e) => return err500(e),
    };
    let mut session = session_arc.lock().await;
    // 最新会话仍是空白新对话（标题为空 = 从未发过消息）时直接复用，避免堆叠空会话
    let reuse = match session
        .storage()
        .list_top_level_sessions(&session.work_dir.display().to_string())
        .await
    {
        Ok(list) => list.first().filter(|s| s.title.is_empty()).cloned(),
        Err(_) => None,
    };
    if let Some(latest) = reuse {
        // 仅切换 session ID 不足以清掉 Agent 内存中的上一会话消息，需重置上下文
        session.reset_context();
        session.set_current_session(Some(latest.id.clone()));
        return Json(SessionInfo {
            id: latest.id,
            title: String::new(),
            model: latest.model,
            updated_at: latest.updated_at,
            work_dir: strip_verbatim(&session.work_dir.display().to_string()),
        })
        .into_response();
    }
    match session.create_session(&resolved.display).await {
        Ok(id) => Json(SessionInfo {
            id,
            title: String::new(),
            model: resolved.display.clone(),
            updated_at: String::new(),
            work_dir: strip_verbatim(&session.work_dir.display().to_string()),
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
            #[derive(serde::Serialize)]
            struct SessionDetail {
                messages: Vec<crate::storage::StoredMessage>,
                #[serde(skip_serializing_if = "Option::is_none")]
                compact_summary: Option<String>,
            }
            Json(SessionDetail {
                messages,
                compact_summary: summary,
            })
            .into_response()
        }
        Err(e) => err500(e),
    }
}

pub(super) async fn delete_session(
    State(state): State<Arc<WebServerState>>,
    Path(id): Path<String>,
) -> Response {
    // 删除前先查归属目录（删除后行已不存在）；只锁该目录的会话实例，
    // 避免遍历锁定其他目录、被无关目录的活跃流式请求阻塞
    let owner_dir = state
        .manager
        .storage()
        .get_session_work_dir(&id)
        .await
        .ok()
        .flatten();
    match state.manager.storage().delete_session(&id).await {
        Ok(()) => {
            // 删除的是该目录当前会话时，同步清掉内存中残留的消息与 session ID
            if let Some(dir) = owner_dir
                && let Some(session_arc) = state.manager.get(std::path::Path::new(&dir))
            {
                let mut session = session_arc.lock().await;
                if session.current_session_id().as_deref() == Some(id.as_str()) {
                    session.reset_session();
                }
            }
            StatusCode::NO_CONTENT.into_response()
        }
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
            .hidden(true)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy().to_lowercase();
                name != "node_modules" && name != "target" && name != ".git"
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
            if keyword.is_empty() || rel.to_lowercase().contains(&keyword) {
                hits.push(rel);
                if hits.len() >= 50 {
                    break;
                }
            }
        }
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
    // 不经会话锁：直接置 cancel 标志，立即生效
    let dir = resolve_dir(&state, req.work_dir.as_deref());
    let _ = req.session_id;
    if state.manager.interrupt(&dir) {
        StatusCode::OK.into_response()
    } else {
        (StatusCode::NOT_FOUND, "没有该工作目录的活动会话").into_response()
    }
}
