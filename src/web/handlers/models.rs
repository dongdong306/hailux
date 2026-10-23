use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::config::{DEFAULT_CONTEXT_WINDOW, DEFAULT_OUTPUT_TOKENS};

use super::super::protocol::{
    CreateModelRequest, DeleteModelRequest, DeleteProviderRequest, ModelInfo, ProviderInfoDto,
    SwitchModelRequest,
};
use super::super::state::WebServerState;
use super::err500;

pub(super) async fn list_models(State(state): State<Arc<WebServerState>>) -> Response {
    let cfg = state.manager.cfg();
    let entries = cfg.available_models();
    Json(
        entries
            .into_iter()
            .map(|m| {
                let active = m.display == cfg.main_model;
                let context_window = cfg.resolve(&m.display).ok().map(|r| r.context_window);
                ModelInfo {
                    provider_id: m.provider_id.clone(),
                    provider_name: m.provider_name,
                    model_id: m.model_id,
                    display: m.display,
                    active,
                    context_window,
                    deletable: m.deletable,
                    provider_predefined: crate::config::find_provider_def(&m.provider_id).is_some(),
                    needs_setup: m.needs_setup,
                }
            })
            .collect::<Vec<_>>(),
    )
    .into_response()
}

/// 模型生效的公共流程：写回 config（含 main_model）→ 保存 → 切换所有已构建
/// ChatSession → 更新 manager 的 resolved/cfg。switch_model 与添加/删除模型共用。
async fn apply_model_config(
    state: &Arc<WebServerState>,
    mut new_cfg: crate::config::Config,
    selector: &str,
) -> Result<(), Response> {
    let resolved = match new_cfg.resolve(selector) {
        Ok(r) => r,
        Err(e) => return Err((StatusCode::BAD_REQUEST, e.to_string()).into_response()),
    };
    new_cfg.main_model = selector.to_string();
    if let Err(e) = new_cfg.save() {
        return Err(err500(e));
    }
    let sessions = state.manager.all_sessions();
    for session_arc in sessions {
        let mut session = session_arc.lock().await;
        session.switch_model(&resolved);
    }
    state.manager.set_resolved(resolved);
    state.manager.set_cfg(new_cfg);
    Ok(())
}

pub(super) async fn switch_model(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<SwitchModelRequest>,
) -> Response {
    // 配置写锁：cfg 读-改-写全程串行化，防止并发请求互相覆盖
    let _guard = state.config_write_lock.lock().await;
    // 解析失败（未配置 provider 等）→ BAD_REQUEST（apply_model_config 内处理，保存前校验）
    let cfg = state.manager.cfg();
    match apply_model_config(&state, cfg, &req.selector).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(resp) => resp,
    }
}

/// provider 列表（添加模型时选择目标 provider）：已配置的在前，
/// 未配置的预定义 provider 在后（可补 API Key 启用，即 TUI 的 needs_setup 引导）
pub(super) async fn list_providers(State(state): State<Arc<WebServerState>>) -> Response {
    let cfg = state.manager.cfg();
    let mut result: Vec<ProviderInfoDto> = cfg
        .configured_providers()
        .into_iter()
        .map(|p| ProviderInfoDto {
            predefined: crate::config::find_provider_def(&p.id).is_some(),
            needs_setup: false,
            id: p.id,
            name: p.name,
            base_url: p.base_url,
        })
        .collect();
    for def in crate::config::PROVIDERS {
        let unconfigured = cfg
            .providers
            .get(def.id)
            .is_none_or(|e| e.api_key.is_empty());
        if unconfigured {
            result.push(ProviderInfoDto {
                predefined: true,
                needs_setup: true,
                id: def.id.to_string(),
                name: def.name.to_string(),
                base_url: def.base_url.to_string(),
            });
        }
    }
    Json(result).into_response()
}

/// 添加自定义模型（对齐 TUI AddModelForm 流程），成功后自动切换到新模型
pub(super) async fn create_model(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<CreateModelRequest>,
) -> Response {
    // 配置写锁：cfg 读-改-写全程串行化，防止并发请求互相覆盖
    let _guard = state.config_write_lock.lock().await;
    let provider_id = req.provider_id.trim().to_string();
    let model_id = req.model_id.trim().to_string();
    if provider_id.is_empty() {
        return (StatusCode::BAD_REQUEST, "服务商名称不能为空").into_response();
    }
    if model_id.is_empty() {
        return (StatusCode::BAD_REQUEST, "模型名称不能为空").into_response();
    }
    if model_id.contains('/') || provider_id.contains('/') {
        return (StatusCode::BAD_REQUEST, "名称不能包含 /").into_response();
    }
    let context_window = req.context_window.unwrap_or(DEFAULT_CONTEXT_WINDOW);
    if context_window == 0 {
        return (StatusCode::BAD_REQUEST, "context_window 必须为正整数").into_response();
    }

    let mut new_cfg = state.manager.cfg();
    let existing = new_cfg.providers.get(&provider_id).cloned();
    let predefined = crate::config::find_provider_def(&provider_id).is_some();

    match existing {
        Some(entry) => {
            if entry.api_key.is_empty() {
                // 已存在但未配置凭据（理论上不会出现：configured_providers 过滤空 key）→ 补 key
                let api_key = req.api_key.filter(|k| !k.trim().is_empty());
                match api_key {
                    Some(k) => {
                        let base_url = req
                            .base_url
                            .as_deref()
                            .map(str::trim)
                            .filter(|u| !u.is_empty());
                        new_cfg.add_custom_model(
                            &provider_id,
                            base_url,
                            Some(k.trim()),
                            &model_id,
                            DEFAULT_OUTPUT_TOKENS,
                            context_window,
                        );
                    }
                    None => {
                        return (
                            StatusCode::BAD_REQUEST,
                            format!("provider {} 未配置 API Key，请提供 api_key", provider_id),
                        )
                            .into_response();
                    }
                }
            } else {
                // 已配置 provider：复用凭据（对齐 TUI：不修改 base_url/api_key）
                new_cfg.add_custom_model(
                    &provider_id,
                    None,
                    None,
                    &model_id,
                    DEFAULT_OUTPUT_TOKENS,
                    context_window,
                );
            }
        }
        None => {
            // 新建 provider：api_key 必填
            let Some(api_key) = req
                .api_key
                .as_deref()
                .map(str::trim)
                .filter(|k| !k.is_empty())
            else {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("新建 provider {} 必须提供 api_key", provider_id),
                )
                    .into_response();
            };
            if predefined {
                // 预定义 provider：启用即写入全部预定义模型
                // （add_predefined_provider 保留各模型预定义的 max_tokens/context_window；
                //   对齐 TUI 从模型选择器进入 setup 的 append_only 流程）
                new_cfg.add_predefined_provider(&provider_id, api_key);
                // 指定模型不在预定义表中（自定义 id）→ 追加
                let model_predefined = crate::config::find_provider_def(&provider_id)
                    .is_some_and(|d| d.models.iter().any(|m| m.id == model_id));
                if !model_predefined {
                    new_cfg.add_custom_model(
                        &provider_id,
                        None,
                        None,
                        &model_id,
                        DEFAULT_OUTPUT_TOKENS,
                        context_window,
                    );
                }
            } else {
                // 自定义 provider：base_url 必填，且须为 http(s) 端点（提前拦截格式错误）
                let Some(base_url) = req
                    .base_url
                    .as_deref()
                    .map(str::trim)
                    .filter(|u| !u.is_empty())
                else {
                    return (
                        StatusCode::BAD_REQUEST,
                        format!("自定义 provider {} 必须提供 base_url", provider_id),
                    )
                        .into_response();
                };
                if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
                    return (
                        StatusCode::BAD_REQUEST,
                        "base_url 必须以 http:// 或 https:// 开头",
                    )
                        .into_response();
                }
                new_cfg.add_custom_model(
                    &provider_id,
                    Some(base_url),
                    Some(api_key),
                    &model_id,
                    DEFAULT_OUTPUT_TOKENS,
                    context_window,
                );
            }
        }
    }

    let selector = format!("{}/{}", provider_id, model_id);
    match apply_model_config(&state, new_cfg, &selector).await {
        Ok(()) => (StatusCode::CREATED, Json(&selector)).into_response(),
        Err(resp) => resp,
    }
}

/// RemoveError → HTTP 状态码：格式错误 400，未找到 404，预定义/删后无模型 409
fn remove_error_response(e: crate::config::RemoveError) -> Response {
    let status = match &e {
        crate::config::RemoveError::InvalidSelector(_) => StatusCode::BAD_REQUEST,
        crate::config::RemoveError::NotFound(_) => StatusCode::NOT_FOUND,
        crate::config::RemoveError::Predefined(_) | crate::config::RemoveError::NoModelsLeft(_) => {
            StatusCode::CONFLICT
        }
    };
    (status, e.to_string()).into_response()
}

/// 删除自定义模型；若删除的是当前模型则自动迁移（remove_custom_model 内处理）
pub(super) async fn delete_model(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<DeleteModelRequest>,
) -> Response {
    // 配置写锁：cfg 读-改-写全程串行化，防止并发请求互相覆盖
    let _guard = state.config_write_lock.lock().await;
    let mut new_cfg = state.manager.cfg();
    let migrated = match new_cfg.remove_custom_model(&req.selector) {
        Ok(m) => m,
        Err(e) => return remove_error_response(e),
    };
    // 有迁移 → 新 main_model 须经 apply_model_config 全量生效；无迁移 → 仅保存
    let target = migrated.unwrap_or_else(|| new_cfg.main_model.clone());
    match apply_model_config(&state, new_cfg, &target).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(resp) => resp,
    }
}

/// 删除自定义 provider（整条含凭据与全部模型）
pub(super) async fn delete_provider(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<DeleteProviderRequest>,
) -> Response {
    // 配置写锁：cfg 读-改-写全程串行化，防止并发请求互相覆盖
    let _guard = state.config_write_lock.lock().await;
    let mut new_cfg = state.manager.cfg();
    let migrated = match new_cfg.remove_provider(&req.provider_id) {
        Ok(m) => m,
        Err(e) => return remove_error_response(e),
    };
    let target = migrated.unwrap_or_else(|| new_cfg.main_model.clone());
    match apply_model_config(&state, new_cfg, &target).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(resp) => resp,
    }
}
