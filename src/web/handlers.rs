//! REST handlers：会话管理、工作目录、权限/提问回复、模型、技能、MCP、模式。

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::permission::PermissionReply;

use super::protocol::{
    AskReplyBody, CommandInfoDto, CreateMcpServerRequest, CreateSessionRequest, CreateSkillRequest,
    DeleteMcpServerRequest, DeleteSkillRequest, FsEntry, InterruptRequest, McpServerInfo,
    ModelInfo, PermissionReplyBody, PlanModeRequest, SessionInfo, SkillInfoDto, SwitchModelRequest,
    UpdateMcpServerRequest, UpdateSkillRequest, ValidateWorkdirRequest, WorkdirInfo, YoloRequest,
};
use super::sse;
use super::state::WebServerState;

pub fn api_router() -> Router<Arc<WebServerState>> {
    Router::new()
        .route("/api/chat", post(sse::chat_handler))
        .route("/api/compact", post(sse::compact_handler))
        .route("/api/sessions", get(list_sessions).post(create_session))
        .route(
            "/api/sessions/{id}",
            get(get_session).delete(delete_session),
        )
        .route("/api/workdirs", get(list_workdirs))
        .route("/api/default-workdir", get(get_default_workdir))
        .route("/api/workdirs/validate", post(validate_workdir))
        .route("/api/fs", get(list_fs))
        .route("/api/files", get(search_files))
        .route("/api/permission/{request_id}/reply", post(permission_reply))
        .route("/api/ask/{request_id}/reply", post(ask_reply))
        .route("/api/interrupt", post(interrupt))
        .route("/api/models", get(list_models).post(switch_model))
        .route(
            "/api/skills",
            get(list_skills)
                .post(create_skill)
                .put(update_skill)
                .delete(delete_skill),
        )
        .route("/api/skills/file", get(read_skill_file))
        .route(
            "/api/mcp",
            get(mcp_status)
                .post(create_mcp_server)
                .put(update_mcp_server)
                .delete(delete_mcp_server),
        )
        .route("/api/plan-mode", post(set_plan_mode))
        .route("/api/yolo", post(set_yolo))
        .route("/api/commands", get(list_commands))
}

// ── 会话管理 ─────────────────────────────────────────────────

#[derive(Deserialize)]
struct SessionsQuery {
    work_dir: Option<String>,
    /// all=true：跨工作目录列出全部顶层会话（侧边栏按目录分组用）
    #[serde(default)]
    all: bool,
}

async fn list_sessions(
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

async fn create_session(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<CreateSessionRequest>,
) -> Response {
    let dir = resolve_dir(&state, req.work_dir.as_deref());
    let resolved = state.manager.resolved();
    let session_arc = match state.manager.get_or_create(&dir).await {
        Ok(s) => s,
        Err(e) => return err500(e),
    };
    let session = session_arc.lock().await;
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

async fn get_session(State(state): State<Arc<WebServerState>>, Path(id): Path<String>) -> Response {
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

async fn delete_session(
    State(state): State<Arc<WebServerState>>,
    Path(id): Path<String>,
) -> Response {
    match state.manager.storage().delete_session(&id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err500(e),
    }
}

// ── 工作目录 ─────────────────────────────────────────────────

async fn list_workdirs(State(state): State<Arc<WebServerState>>) -> Response {
    match state.manager.list_work_dirs().await {
        Ok(dirs) => Json(
            dirs.into_iter()
                .map(|path| WorkdirInfo {
                    path: strip_verbatim(&path),
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => err500(e),
    }
}

/// 剥离 Windows verbatim 前缀，前端展示用
pub(crate) fn strip_verbatim(path: &str) -> String {
    path.strip_prefix(r"\\?\")
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.to_string())
}

/// 服务器启动目录（canonical，剥离 verbatim 前缀）—— 前端初始项目目录
async fn get_default_workdir(State(state): State<Arc<WebServerState>>) -> Response {
    Json(WorkdirInfo {
        path: strip_verbatim(&state.default_work_dir.display().to_string()),
    })
    .into_response()
}

async fn validate_workdir(Json(req): Json<ValidateWorkdirRequest>) -> Response {
    let path = std::path::Path::new(&req.path);
    if !path.is_dir() {
        return (StatusCode::NOT_FOUND, format!("目录不存在: {}", req.path)).into_response();
    }
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let s = canonical.to_string_lossy().to_string();
    let s = s.strip_prefix(r"\\?\").map(|x| x.to_string()).unwrap_or(s);
    Json(WorkdirInfo { path: s }).into_response()
}

#[derive(Deserialize)]
struct FsQuery {
    path: Option<String>,
    #[serde(default)]
    dirs_only: bool,
}

async fn list_fs(Query(q): Query<FsQuery>) -> Response {
    let base = q.path.unwrap_or_else(|| ".".to_string());
    let path = std::path::PathBuf::from(&base)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(&base));
    let entries = match std::fs::read_dir(&path) {
        Ok(iter) => iter,
        Err(e) => return (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    };
    let mut result = Vec::new();
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            result.push(FsEntry {
                name: entry.file_name().to_string_lossy().to_string(),
                path: entry.path().to_string_lossy().to_string(),
                is_dir: true,
            });
        } else if !q.dirs_only {
            result.push(FsEntry {
                name: entry.file_name().to_string_lossy().to_string(),
                path: entry.path().to_string_lossy().to_string(),
                is_dir: false,
            });
        }
    }
    result.sort_by_key(|e| e.name.to_lowercase());
    Json(result).into_response()
}

// ── 文件提及搜索（@ 补全）────────────────────────────────────

#[derive(Deserialize)]
struct FilesQuery {
    q: String,
    work_dir: Option<String>,
}

async fn search_files(
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

// ── 权限 / 提问回复 ──────────────────────────────────────────

async fn permission_reply(
    State(state): State<Arc<WebServerState>>,
    Path(request_id): Path<String>,
    Json(reply): Json<PermissionReplyBody>,
) -> StatusCode {
    let decision = if reply.allow {
        if reply.always {
            PermissionReply::Always
        } else {
            PermissionReply::Once
        }
    } else {
        PermissionReply::Deny
    };
    match state.registry.take_permission(&request_id) {
        Some(tx) => {
            let _ = tx.send(decision);
            StatusCode::OK
        }
        None => StatusCode::NOT_FOUND,
    }
}

async fn ask_reply(
    State(state): State<Arc<WebServerState>>,
    Path(request_id): Path<String>,
    Json(reply): Json<AskReplyBody>,
) -> StatusCode {
    match state.registry.take_ask(&request_id) {
        Some(tx) => {
            let _ = tx.send(reply.answer);
            StatusCode::OK
        }
        None => StatusCode::NOT_FOUND,
    }
}

// ── 运行时控制 ───────────────────────────────────────────────

async fn interrupt(
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

// ── 模型 ─────────────────────────────────────────────────────

async fn list_models(State(state): State<Arc<WebServerState>>) -> Response {
    let cfg = state.manager.cfg();
    let entries = cfg.available_models();
    Json(
        entries
            .into_iter()
            .map(|m| {
                let active = m.display == cfg.main_model;
                let context_window = cfg.resolve(&m.display).ok().map(|r| r.context_window);
                ModelInfo {
                    provider_id: m.provider_id,
                    provider_name: m.provider_name,
                    model_id: m.model_id,
                    display: m.display,
                    active,
                    context_window,
                }
            })
            .collect::<Vec<_>>(),
    )
    .into_response()
}

async fn switch_model(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<SwitchModelRequest>,
) -> Response {
    let cfg = state.manager.cfg();
    // 1. 解析新模型
    let resolved = match cfg.resolve(&req.selector) {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    // 2. 持久化到 config.toml
    let mut new_cfg = cfg.clone();
    new_cfg.main_model = req.selector.clone();
    if let Err(e) = new_cfg.save() {
        return err500(e);
    }

    // 3. 切换所有已构建 ChatSession 的模型（后续新会话用新 resolved）
    let sessions = state.manager.all_sessions();
    for session_arc in sessions {
        let mut session = session_arc.lock().await;
        session.switch_model(&resolved);
    }
    state.manager.set_resolved(resolved);
    state.manager.set_cfg(new_cfg);

    StatusCode::OK.into_response()
}

// ── Skills / MCP ─────────────────────────────────────────────

#[derive(Deserialize)]
struct SkillsQuery {
    work_dir: Option<String>,
}

/// 全局 skills 根：~/.hailux/skills
fn global_skills_root() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".hailux").join("skills"))
}

/// 项目级 skills 根：<work_dir>/.hailux/skills
fn project_skills_root(work_dir: &std::path::Path) -> std::path::PathBuf {
    work_dir.join(".hailux").join("skills")
}

/// 路径比较键：统一分隔符，Windows 下大小写不敏感
fn norm_cmp_key(path: &std::path::Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) { s.to_lowercase() } else { s }
}

/// path 是否位于 root 之下（含相等；内部各自 canonicalize，消除 verbatim 前缀差异）
fn path_is_under(path: &std::path::Path, root: &std::path::Path) -> bool {
    let canon = |p: &std::path::Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let p = norm_cmp_key(&canon(path));
    let r = norm_cmp_key(&canon(root));
    p.starts_with(&format!("{r}/")) || p == r
}

/// 判定 skill 来源作用域（依据 SKILL.md 路径前缀）
fn skill_scope(location: &std::path::Path, work_dir: &std::path::Path) -> &'static str {
    if global_skills_root().is_some_and(|root| path_is_under(location, &root)) {
        return "global";
    }
    if path_is_under(location, &project_skills_root(work_dir)) {
        return "project";
    }
    "global"
}

/// 技能名约束：字母数字开头，仅含字母数字 / `-` / `_`，≤ 64 字符（防路径穿越）
fn valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 渲染 SKILL.md 内容（frontmatter + 正文）。description 压成单行以适配行级 frontmatter 解析。
fn render_skill_md(name: &str, description: &str, content: &str) -> String {
    let desc = description.replace(['\r', '\n'], " ");
    format!(
        "---\nname: {name}\ndescription: {desc}\n---\n\n{}\n",
        content.trim_end()
    )
}

/// 安全校验 location：canonicalize 后必须位于两个合法 skills 根之下且文件名为 SKILL.md，
/// 返回 canonicalize 结果（防任意文件读写删除）
fn validate_skill_location(
    location: &str,
    work_dir: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let canonical = std::path::PathBuf::from(location).canonicalize().ok()?;
    if canonical.file_name()?.to_str()? != "SKILL.md" {
        return None;
    }
    let global_ok = global_skills_root().is_some_and(|root| path_is_under(&canonical, &root));
    let project_ok = path_is_under(&canonical, &project_skills_root(work_dir));
    (global_ok || project_ok).then_some(canonical)
}

/// 技能目录内文件列表上限（防止巨型目录拖垮响应）
const SKILL_FILES_MAX: usize = 500;
/// 单文件内容读取上限（1 MiB，防误读大二进制）
const SKILL_FILE_CONTENT_MAX: u64 = 1024 * 1024;

/// 递归列出技能目录内全部文件（相对路径 + 字节大小）。
/// 跳过隐藏文件/目录；超上限即截断。
fn list_skill_files(base_dir: &std::path::Path) -> Vec<super::protocol::SkillFileDto> {
    let mut out = Vec::new();
    let mut stack = vec![(base_dir.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if out.len() >= SKILL_FILES_MAX {
                break;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                stack.push((entry.path(), rel));
            } else {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                out.push(super::protocol::SkillFileDto { path: rel, size });
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

async fn list_skills(
    State(state): State<Arc<WebServerState>>,
    Query(q): Query<SkillsQuery>,
) -> Response {
    let dir = resolve_dir(&state, q.work_dir.as_deref());
    // 直接磁盘发现（不构建 ChatSession）：目录失效时仅跳过项目级，全局技能仍可列出
    let skills = crate::agent::skill::discover_skills(&dir).unwrap_or_default();
    Json(
        skills
            .iter()
            .map(|s| SkillInfoDto {
                name: s.name.clone(),
                description: s.description.clone(),
                location: strip_verbatim(&s.location.display().to_string()),
                scope: skill_scope(&s.location, &dir).to_string(),
                content: s.content.clone(),
                files: list_skill_files(&s.base_dir()),
            })
            .collect::<Vec<_>>(),
    )
    .into_response()
}

/// 读取技能目录内单个文件内容（UI 查看附属脚本/参考文件用）。
#[derive(Deserialize)]
struct SkillFileQuery {
    work_dir: Option<String>,
    /// 技能 SKILL.md 绝对路径（定位技能目录）
    location: String,
    /// 技能目录内的相对路径（禁止绝对路径与 `..`）
    file: String,
}

async fn read_skill_file(
    State(state): State<Arc<WebServerState>>,
    Query(q): Query<SkillFileQuery>,
) -> Response {
    let dir = resolve_dir(&state, q.work_dir.as_deref());
    let Some(skill_md) = validate_skill_location(&q.location, &dir) else {
        return (
            StatusCode::BAD_REQUEST,
            "非法的技能路径：必须位于 skills 目录下且文件名为 SKILL.md",
        )
            .into_response();
    };
    let Some(base) = skill_md.parent() else {
        return (StatusCode::BAD_REQUEST, "无法定位技能目录").into_response();
    };

    // 相对路径安全校验：拒绝绝对路径与父目录引用
    let rel = std::path::Path::new(&q.file);
    if rel.is_absolute()
        || rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return (StatusCode::BAD_REQUEST, "非法的文件路径").into_response();
    }
    let full = base.join(rel);
    let Ok(canonical) = full.canonicalize() else {
        return (StatusCode::NOT_FOUND, format!("文件不存在: {}", q.file)).into_response();
    };
    if !path_is_under(&canonical, base) || !canonical.is_file() {
        return (StatusCode::BAD_REQUEST, "非法的文件路径").into_response();
    }
    let size = canonical.metadata().map(|m| m.len()).unwrap_or(u64::MAX);
    if size > SKILL_FILE_CONTENT_MAX {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("文件过大（{} 字节，上限 1 MiB）", size),
        )
            .into_response();
    }
    match std::fs::read(&canonical) {
        Ok(bytes) => Json(super::protocol::SkillFileContentDto {
            path: q.file,
            content: String::from_utf8_lossy(&bytes).into_owned(),
        })
        .into_response(),
        Err(e) => err500(color_eyre::eyre::eyre!(e)),
    }
}

async fn create_skill(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<CreateSkillRequest>,
) -> Response {
    if !valid_skill_name(&req.name) {
        return (
            StatusCode::BAD_REQUEST,
            "技能名只能包含字母、数字、-、_，且以字母或数字开头（最长 64 字符）",
        )
            .into_response();
    }
    let dir = resolve_dir(&state, req.work_dir.as_deref());
    let root = if req.scope == "project" {
        project_skills_root(&dir)
    } else {
        match global_skills_root() {
            Some(r) => r,
            None => {
                return (StatusCode::INTERNAL_SERVER_ERROR, "无法定位用户主目录").into_response();
            }
        }
    };

    // 发现层面的名称冲突（同名会互相覆盖）
    let existing = crate::agent::skill::discover_skills(&dir).unwrap_or_default();
    if existing.iter().any(|s| s.name == req.name) {
        return (
            StatusCode::CONFLICT,
            format!(
                "技能 \"{}\" 已存在（同名时项目级与全局级互相覆盖）",
                req.name
            ),
        )
            .into_response();
    }
    let skill_dir = root.join(&req.name);
    if skill_dir.exists() {
        return (
            StatusCode::CONFLICT,
            format!(
                "目录已存在: {}",
                strip_verbatim(&skill_dir.display().to_string())
            ),
        )
            .into_response();
    }

    if let Err(e) = std::fs::create_dir_all(&skill_dir) {
        return err500(color_eyre::eyre::eyre!(e));
    }
    let skill_file = skill_dir.join("SKILL.md");
    if let Err(e) = std::fs::write(
        &skill_file,
        render_skill_md(&req.name, &req.description, &req.content),
    ) {
        return err500(color_eyre::eyre::eyre!(e));
    }

    state.manager.invalidate(&dir);
    let canonical = skill_file.canonicalize().unwrap_or(skill_file);
    let base_dir = canonical.parent().map(|p| p.to_path_buf());
    (
        StatusCode::CREATED,
        Json(SkillInfoDto {
            name: req.name,
            description: req.description,
            location: strip_verbatim(&canonical.display().to_string()),
            scope: skill_scope(&canonical, &dir).to_string(),
            content: req.content,
            files: base_dir
                .as_deref()
                .map(list_skill_files)
                .unwrap_or_default(),
        }),
    )
        .into_response()
}

async fn update_skill(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<UpdateSkillRequest>,
) -> Response {
    if !valid_skill_name(&req.name) {
        return (
            StatusCode::BAD_REQUEST,
            "技能名只能包含字母、数字、-、_，且以字母或数字开头（最长 64 字符）",
        )
            .into_response();
    }
    let dir = resolve_dir(&state, req.work_dir.as_deref());
    let canonical = match validate_skill_location(&req.location, &dir) {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "非法的技能路径：必须位于 skills 目录下且文件名为 SKILL.md",
            )
                .into_response();
        }
    };

    // 改名冲突：新名称已被其他技能占用
    let existing = crate::agent::skill::discover_skills(&dir).unwrap_or_default();
    let self_key = norm_cmp_key(&canonical);
    if existing
        .iter()
        .any(|s| s.name == req.name && norm_cmp_key(&s.location) != self_key)
    {
        return (
            StatusCode::CONFLICT,
            format!(
                "技能 \"{}\" 已存在（同名时项目级与全局级互相覆盖）",
                req.name
            ),
        )
            .into_response();
    }

    if let Err(e) = std::fs::write(
        &canonical,
        render_skill_md(&req.name, &req.description, &req.content),
    ) {
        return err500(color_eyre::eyre::eyre!(e));
    }
    state.manager.invalidate(&dir);
    StatusCode::OK.into_response()
}

async fn delete_skill(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<DeleteSkillRequest>,
) -> Response {
    let dir = resolve_dir(&state, req.work_dir.as_deref());
    let canonical = match validate_skill_location(&req.location, &dir) {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "非法的技能路径：必须位于 skills 目录下且文件名为 SKILL.md",
            )
                .into_response();
        }
    };
    // 删除整个技能目录（含 scripts/ 等附属文件）
    let base_dir = match canonical.parent() {
        Some(p) => p.to_path_buf(),
        None => return (StatusCode::BAD_REQUEST, "无法定位技能目录").into_response(),
    };
    if !base_dir.is_dir() {
        return (StatusCode::NOT_FOUND, "技能目录不存在").into_response();
    }
    if let Err(e) = std::fs::remove_dir_all(&base_dir) {
        return err500(color_eyre::eyre::eyre!(e));
    }
    state.manager.invalidate(&dir);
    StatusCode::NO_CONTENT.into_response()
}

/// MCP 服务器名约束：字母数字开头，仅含字母数字 / `-` / `_` / `.`，≤ 64 字符。
/// 名称作为 toml key 与 `mcp__<name>__<tool>` 注册名的一部分，需严格白名单。
fn valid_mcp_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// 由请求字段构造 ServerConfig；transport 与必填字段不匹配时返回错误。
fn build_server_config(
    transport: &str,
    command: Option<String>,
    args: Vec<String>,
    env: std::collections::BTreeMap<String, String>,
    url: Option<String>,
    headers: std::collections::BTreeMap<String, String>,
) -> Result<crate::mcp::config::ServerConfig, String> {
    match transport {
        "stdio" => {
            let command = command.filter(|c| !c.trim().is_empty()).ok_or(
                "stdio 服务器必须提供 command（可执行程序，如 npx / uvx / node）".to_string(),
            )?;
            Ok(crate::mcp::config::ServerConfig::Stdio { command, args, env })
        }
        "http" => {
            let url = url
                .filter(|u| !u.trim().is_empty())
                .ok_or_else(|| "http 服务器必须提供 url".to_string())?;
            Ok(crate::mcp::config::ServerConfig::Http { url, headers })
        }
        other => Err(format!("不支持的传输方式: {other}（仅 stdio / http）")),
    }
}

/// 重连全部 MCP 服务器并替换共享 backends，随后使会话缓存失效
/// （下次请求时按新 backends 重新注册 MCP 工具）。
/// 运行中的请求持有旧连接的 Arc 不受影响；被移除的旧 stdio 子进程随 Arc 释放自动终止。
fn schedule_mcp_reconnect(state: &Arc<WebServerState>) {
    let backends = state.manager.mcp_backends().clone();
    let state = state.clone();
    tokio::spawn(async move {
        let mcp_cfg = crate::mcp::config::load().unwrap_or_default();
        let connections = crate::mcp::connect_mcp_servers(&mcp_cfg).await;
        if let Ok(mut guard) = backends.lock() {
            guard.clear();
            for conn in &connections {
                if let Some(backend) = &conn.backend {
                    guard.push(crate::mcp::McpToolBackend {
                        server_name: conn.status.name.clone(),
                        backend: backend.clone(),
                        tools: conn.tools.clone(),
                    });
                }
            }
        }
        state.manager.invalidate_all();
    });
}

/// 合并配置文件（权威来源）与运行时连接状态为前端展示结构。
fn merge_mcp_status(
    config: &crate::mcp::config::McpConfig,
    backends: &crate::mcp::SharedMcpBackends,
) -> Vec<McpServerInfo> {
    let guard = backends.lock().ok();
    config
        .mcp_servers
        .iter()
        .map(|(name, server_config)| {
            // 名称匹配（backends 记录的是配置 key，连接失败的服务器不在其中）
            let backend = guard
                .as_ref()
                .and_then(|g| g.iter().find(|b| b.server_name == *name));
            let (connected, tools, tool_details) = match backend {
                Some(b) => (
                    true,
                    b.tools.len(),
                    b.tools
                        .iter()
                        .map(|t| super::protocol::McpToolInfo {
                            name: t.name.to_string(),
                            description: t.description.as_deref().unwrap_or("").to_string(),
                            schema: t.schema_as_json_value(),
                        })
                        .collect(),
                ),
                None => (false, 0, Vec::new()),
            };
            let transport = server_config.transport_label().to_string();
            match server_config {
                crate::mcp::config::ServerConfig::Stdio { command, args, env } => McpServerInfo {
                    name: name.clone(),
                    connected,
                    tools,
                    transport,
                    command: Some(command.clone()),
                    args: args.clone(),
                    env: env.clone(),
                    url: None,
                    headers: Default::default(),
                    error: (!connected).then(|| "未连接或连接失败".to_string()),
                    tool_details,
                },
                crate::mcp::config::ServerConfig::Http { url, headers } => McpServerInfo {
                    name: name.clone(),
                    connected,
                    tools,
                    transport,
                    command: None,
                    args: Vec::new(),
                    env: Default::default(),
                    url: Some(url.clone()),
                    headers: headers.clone(),
                    error: (!connected).then(|| "未连接或连接失败".to_string()),
                    tool_details,
                },
            }
        })
        .collect()
}

async fn mcp_status(State(state): State<Arc<WebServerState>>) -> Response {
    let config = match crate::mcp::config::load() {
        Ok(c) => c,
        Err(e) => return err500(e),
    };
    Json(merge_mcp_status(&config, state.manager.mcp_backends())).into_response()
}

async fn create_mcp_server(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<CreateMcpServerRequest>,
) -> Response {
    if !valid_mcp_name(&req.name) {
        return (
            StatusCode::BAD_REQUEST,
            "服务器名只能包含字母、数字、-、_、.，且以字母或数字开头（最长 64 字符）",
        )
            .into_response();
    }
    let server_config = match build_server_config(
        &req.transport,
        req.command,
        req.args,
        req.env,
        req.url,
        req.headers,
    ) {
        Ok(c) => c,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let mut config = match crate::mcp::config::load() {
        Ok(c) => c,
        Err(e) => return err500(e),
    };
    if config.mcp_servers.contains_key(&req.name) {
        return (
            StatusCode::CONFLICT,
            format!("MCP 服务器 \"{}\" 已存在", req.name),
        )
            .into_response();
    }
    config.mcp_servers.insert(req.name.clone(), server_config);
    if let Err(e) = crate::mcp::config::save(&config) {
        return err500(e);
    }
    schedule_mcp_reconnect(&state);
    (
        StatusCode::CREATED,
        Json(
            merge_mcp_status(&config, state.manager.mcp_backends())
                .into_iter()
                .find(|s| s.name == req.name)
                .map(|mut s| {
                    // 重连在后台进行，此时连接状态尚未就绪
                    s.connected = false;
                    s.tools = 0;
                    s.tool_details = Vec::new();
                    s.error = Some("已保存，正在连接…".to_string());
                    s
                }),
        ),
    )
        .into_response()
}

async fn update_mcp_server(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<UpdateMcpServerRequest>,
) -> Response {
    let new_name = req.new_name.filter(|n| !n.trim().is_empty());
    if let Some(n) = &new_name
        && !valid_mcp_name(n)
    {
        return (
            StatusCode::BAD_REQUEST,
            "服务器名只能包含字母、数字、-、_、.，且以字母或数字开头（最长 64 字符）",
        )
            .into_response();
    }
    let server_config = match build_server_config(
        &req.transport,
        req.command,
        req.args,
        req.env,
        req.url,
        req.headers,
    ) {
        Ok(c) => c,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let mut config = match crate::mcp::config::load() {
        Ok(c) => c,
        Err(e) => return err500(e),
    };
    let Some(_old) = config.mcp_servers.remove(&req.name) else {
        return (
            StatusCode::NOT_FOUND,
            format!("MCP 服务器 \"{}\" 不存在", req.name),
        )
            .into_response();
    };
    let target = new_name.unwrap_or_else(|| req.name.clone());
    if target != req.name && config.mcp_servers.contains_key(&target) {
        // 改名冲突：本地副本尚未保存，直接报错即可（磁盘配置不变）
        return (
            StatusCode::CONFLICT,
            format!("MCP 服务器 \"{target}\" 已存在"),
        )
            .into_response();
    }
    config.mcp_servers.insert(target.clone(), server_config);
    if let Err(e) = crate::mcp::config::save(&config) {
        return err500(e);
    }
    schedule_mcp_reconnect(&state);
    StatusCode::OK.into_response()
}

async fn delete_mcp_server(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<DeleteMcpServerRequest>,
) -> Response {
    let mut config = match crate::mcp::config::load() {
        Ok(c) => c,
        Err(e) => return err500(e),
    };
    if config.mcp_servers.remove(&req.name).is_none() {
        return (
            StatusCode::NOT_FOUND,
            format!("MCP 服务器 \"{}\" 不存在", req.name),
        )
            .into_response();
    }
    if let Err(e) = crate::mcp::config::save(&config) {
        return err500(e);
    }
    schedule_mcp_reconnect(&state);
    StatusCode::NO_CONTENT.into_response()
}

// ── 模式 ─────────────────────────────────────────────────────

async fn set_plan_mode(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<PlanModeRequest>,
) -> Response {
    let dir = resolve_dir(&state, req.work_dir.as_deref());
    let session_arc = match state.manager.get_or_create(&dir).await {
        Ok(s) => s,
        Err(e) => return err500(e),
    };
    let mut session = session_arc.lock().await;
    session.set_plan_mode(req.enabled);
    StatusCode::OK.into_response()
}

async fn set_yolo(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<YoloRequest>,
) -> Response {
    let dir = resolve_dir(&state, req.work_dir.as_deref());
    let session_arc = match state.manager.get_or_create(&dir).await {
        Ok(s) => s,
        Err(e) => return err500(e),
    };
    let session = session_arc.lock().await;
    let mode = session.toggle_yolo();
    // toggle 语义：请求 enabled=false 且当前已是非 YOLO → 再切一次回到 Normal
    if (mode == crate::permission::PermissionMode::Yolo) != req.enabled {
        session.toggle_yolo();
    }
    StatusCode::OK.into_response()
}

// ── 斜杠命令 ─────────────────────────────────────────────────

#[derive(Deserialize)]
struct CommandsQuery {
    work_dir: Option<String>,
}

/// Web 端可用的斜杠命令列表（输入框 `/` 补全用）。
///
/// - prompt 型：`/init` 与自定义命令，`POST /api/chat` 时由后端展开为完整提示词
/// - ui 型：`/compact`（前端本地处理，走 `POST /api/compact`）。
///   其余 TUI UI-action 命令（/new /sessions /plan /models /skills /mcp /yolo /tasks /exit）
///   在 Web 端均有对应的界面入口或无意义，不列出。
async fn list_commands(
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

// ── 辅助 ─────────────────────────────────────────────────────

fn resolve_dir(state: &WebServerState, work_dir: Option<&str>) -> std::path::PathBuf {
    match work_dir {
        Some(d) if !d.is_empty() => std::path::PathBuf::from(d),
        _ => state.default_work_dir.clone(),
    }
}

fn err500(e: color_eyre::eyre::Report) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_name_validation() {
        assert!(valid_skill_name("code-review"));
        assert!(valid_skill_name("a"));
        assert!(valid_skill_name("My_Skill2"));
        assert!(!valid_skill_name(""));
        assert!(!valid_skill_name("-x"));
        assert!(!valid_skill_name("a b"));
        assert!(!valid_skill_name("a/b"));
        assert!(!valid_skill_name("../escape"));
        assert!(!valid_skill_name("中文"));
        assert!(!valid_skill_name(&"x".repeat(65)));
    }

    #[test]
    fn rejects_location_outside_skills_roots() {
        let tmp = std::env::temp_dir().join(format!("hailux-skill-loc-{}", uuid::Uuid::new_v4()));
        let skill_dir = tmp.join(".hailux").join("skills").join("demo");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let skill_file = skill_dir.join("SKILL.md");
        std::fs::write(&skill_file, "---\nname: demo\n---\nbody").unwrap();
        let outside = tmp.join("SKILL.md");
        std::fs::write(&outside, "---\nname: evil\n---\nbody").unwrap();

        // skills 根下的 SKILL.md 合法
        assert!(validate_skill_location(&skill_file.display().to_string(), &tmp).is_some());
        // 项目根下（非 skills 子目录）的同名文件非法
        assert!(validate_skill_location(&outside.display().to_string(), &tmp).is_none());
        // 不存在的路径非法
        assert!(
            validate_skill_location(
                &tmp.join("nope").join("SKILL.md").display().to_string(),
                &tmp
            )
            .is_none()
        );

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn render_and_discover_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("hailux-skill-rt-{}", uuid::Uuid::new_v4()));
        let skill_dir = tmp.join(".hailux").join("skills").join("demo");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            render_skill_md(
                "demo",
                "a demo with: colon\nsecond line",
                "# Body\n\ncontent",
            ),
        )
        .unwrap();

        let skills = crate::agent::skill::discover_skills(&tmp).unwrap();
        let demo = skills.iter().find(|s| s.name == "demo").unwrap();
        assert_eq!(demo.description, "a demo with: colon second line");
        assert!(demo.content.contains("# Body"));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn scope_detection() {
        let tmp = std::env::temp_dir().join(format!("hailux-skill-scope-{}", uuid::Uuid::new_v4()));
        let skill_dir = tmp.join(".hailux").join("skills").join("p1");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let loc = skill_dir.join("SKILL.md");
        std::fs::write(&loc, "---\nname: p1\n---\nbody").unwrap();
        assert_eq!(skill_scope(&loc, &tmp), "project");
        assert_eq!(skill_scope(&loc, &tmp.join("other")), "global");

        std::fs::remove_dir_all(&tmp).ok();
    }
}
