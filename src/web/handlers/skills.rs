use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use super::super::protocol::{
    CreateSkillRequest, DeleteSkillRequest, SkillInfoDto, UpdateSkillRequest,
};
use super::super::state::WebServerState;
use super::{err500, resolve_dir, strip_verbatim};

#[derive(Deserialize)]
pub(super) struct SkillsQuery {
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
pub(super) fn skill_scope(location: &std::path::Path, work_dir: &std::path::Path) -> &'static str {
    if global_skills_root().is_some_and(|root| path_is_under(location, &root)) {
        return "global";
    }
    if path_is_under(location, &project_skills_root(work_dir)) {
        return "project";
    }
    "global"
}

/// 技能名约束：字母数字开头，仅含字母数字 / `-` / `_`，≤ 64 字符（防路径穿越）
pub(super) fn valid_skill_name(name: &str) -> bool {
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
pub(super) fn render_skill_md(name: &str, description: &str, content: &str) -> String {
    let desc = description.replace(['\r', '\n'], " ");
    format!(
        "---\nname: {name}\ndescription: {desc}\n---\n\n{}\n",
        content.trim_end()
    )
}

/// 安全校验 location：canonicalize 后必须位于某个 skills 根之下至少两层
/// （`<skills 根>/<技能目录>/SKILL.md`）且文件名为 SKILL.md，
/// 返回 canonicalize 结果（防任意文件读写删除）。
/// 至少两层是硬性要求：否则 `<根>/SKILL.md` 的 parent 即根目录，删除时会抹掉整个 skills 根。
pub(super) fn validate_skill_location(
    location: &str,
    work_dir: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let canonical = std::path::PathBuf::from(location).canonicalize().ok()?;
    if canonical.file_name()?.to_str()? != "SKILL.md" {
        return None;
    }
    let nested_under = |root: &std::path::Path| {
        // 两边都 canonicalize 消除 verbatim（`\\?\`）前缀差异后才能做相等比较
        let root_canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        path_is_under(&canonical, root)
            && canonical
                .parent()
                .is_some_and(|p| norm_cmp_key(p) != norm_cmp_key(&root_canon))
    };
    let global_ok = global_skills_root().is_some_and(|root| nested_under(&root));
    let project_ok = nested_under(&project_skills_root(work_dir));
    (global_ok || project_ok).then_some(canonical)
}

/// 技能目录内文件列表上限（防止巨型目录拖垮响应）
const SKILL_FILES_MAX: usize = 500;
/// 单文件内容读取上限（1 MiB，防误读大二进制）
const SKILL_FILE_CONTENT_MAX: u64 = 1024 * 1024;

/// 递归列出技能目录内全部文件（相对路径 + 字节大小）。
/// 跳过隐藏文件/目录；超上限即截断。
fn list_skill_files(base_dir: &std::path::Path) -> Vec<super::super::protocol::SkillFileDto> {
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
                out.push(super::super::protocol::SkillFileDto { path: rel, size });
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

pub(super) async fn list_skills(
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
pub(super) struct SkillFileQuery {
    work_dir: Option<String>,
    /// 技能 SKILL.md 绝对路径（定位技能目录）
    location: String,
    /// 技能目录内的相对路径（禁止绝对路径与 `..`）
    file: String,
}

pub(super) async fn read_skill_file(
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
        Ok(bytes) => Json(super::super::protocol::SkillFileContentDto {
            path: q.file,
            content: String::from_utf8_lossy(&bytes).into_owned(),
        })
        .into_response(),
        Err(e) => err500(color_eyre::eyre::eyre!(e)),
    }
}

pub(super) async fn create_skill(
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

pub(super) async fn update_skill(
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

pub(super) async fn delete_skill(
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
