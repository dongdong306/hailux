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

/// Global skills root: ~/.hailux/skills
fn global_skills_root() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".hailux").join("skills"))
}

/// Project-level skills root: <work_dir>/.hailux/skills
fn project_skills_root(work_dir: &std::path::Path) -> std::path::PathBuf {
    work_dir.join(".hailux").join("skills")
}

/// Path comparison key: unified separators, case-insensitive on Windows
fn norm_cmp_key(path: &std::path::Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) { s.to_lowercase() } else { s }
}

/// Whether `path` sits under `root` (inclusive; each side is canonicalized internally
/// to remove verbatim prefix differences)
fn path_is_under(path: &std::path::Path, root: &std::path::Path) -> bool {
    let canon = |p: &std::path::Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let p = norm_cmp_key(&canon(path));
    let r = norm_cmp_key(&canon(root));
    p.starts_with(&format!("{r}/")) || p == r
}

/// Determine a skill's source scope (based on the SKILL.md path prefix)
pub(super) fn skill_scope(location: &std::path::Path, work_dir: &std::path::Path) -> &'static str {
    if global_skills_root().is_some_and(|root| path_is_under(location, &root)) {
        return "global";
    }
    if path_is_under(location, &project_skills_root(work_dir)) {
        return "project";
    }
    "global"
}

/// Skill name constraint: starts with an alphanumeric, contains only alphanumerics /
/// `-` / `_`, at most 64 chars (guards against path traversal)
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

/// Render SKILL.md content (frontmatter + body). description is collapsed to a single
/// line to fit line-based frontmatter parsing.
pub(super) fn render_skill_md(name: &str, description: &str, content: &str) -> String {
    let desc = description.replace(['\r', '\n'], " ");
    format!(
        "---\nname: {name}\ndescription: {desc}\n---\n\n{}\n",
        content.trim_end()
    )
}

/// Safety-validate location: after canonicalization it must sit at least two levels
/// under a skills root (`<skills root>/<skill dir>/SKILL.md`) with the file name
/// SKILL.md; returns the canonicalized path (guards arbitrary file read/write/delete).
/// The two-level minimum is a hard requirement: otherwise the parent of
/// `<root>/SKILL.md` is the root itself and deletion would wipe the whole skills root.
pub(super) fn validate_skill_location(
    location: &str,
    work_dir: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let canonical = std::path::PathBuf::from(location).canonicalize().ok()?;
    if canonical.file_name()?.to_str()? != "SKILL.md" {
        return None;
    }
    let nested_under = |root: &std::path::Path| {
        // Canonicalize both sides to remove verbatim (`\\?\`) prefix differences before comparing
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

/// Max number of files listed per skill directory (keeps huge directories from stalling responses)
const SKILL_FILES_MAX: usize = 500;
/// Per-file content read cap (1 MiB, guards against accidentally reading large binaries)
const SKILL_FILE_CONTENT_MAX: u64 = 1024 * 1024;

/// Recursively list all files in a skill directory (relative path + byte size).
/// Skips hidden files/dirs; truncates once past the cap.
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
    // Direct on-disk discovery (no ChatSession built): if a directory is invalid only
    // the project level is skipped; global skills still list
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

/// Read a single file inside a skill directory (used by the UI to view companion scripts/reference files).
#[derive(Deserialize)]
pub(super) struct SkillFileQuery {
    work_dir: Option<String>,
    /// Absolute path of the skill's SKILL.md (locates the skill directory)
    location: String,
    /// Relative path inside the skill directory (absolute paths and `..` are rejected)
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

    // Relative path safety check: reject absolute paths and parent-directory references
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

    // Name conflict at the discovery level (same names overwrite each other)
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

    // Rename conflict: the new name is already taken by another skill
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
    // Delete the entire skill directory (including companion files like scripts/)
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
