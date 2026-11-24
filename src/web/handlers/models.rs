use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::config::{DEFAULT_CONTEXT_WINDOW, DEFAULT_OUTPUT_TOKENS};

use super::super::protocol::{
    ApplyDiscoveredRequest, ApplyDiscoveredResponse, CreateModelRequest, DeleteModelRequest,
    DeleteProviderRequest, DiscoverModelsRequest, DiscoverModelsResponse, DiscoveredModelDto,
    ModelInfo, ProviderInfoDto, SwitchModelRequest, UpdateModelRequest, UpdateProviderRequest,
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
                let resolved = cfg.resolve(&m.display).ok();
                ModelInfo {
                    provider_id: m.provider_id.clone(),
                    provider_name: m.provider_name,
                    model_id: m.model_id,
                    display: m.display,
                    active,
                    context_window: resolved.as_ref().map(|r| r.context_window),
                    max_tokens: resolved.as_ref().map(|r| r.max_tokens),
                    supports_vision: resolved.as_ref().map(|r| r.supports_vision),
                    provider_predefined: crate::config::find_provider_def(&m.provider_id).is_some(),
                }
            })
            .collect::<Vec<_>>(),
    )
    .into_response()
}

/// Shared flow to make a model effective: write back the config (including main_model) -> save ->
/// switch all built ChatSessions -> update the manager's resolved/cfg. Used by switch_model and add/delete.
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
        // try_lock: locked instances (in-flight runs) are skipped so the
        // broadcast never blocks; drift is reconciled by the chat entry
        if let Ok(mut session) = session_arc.try_lock() {
            session.switch_model(&resolved);
        }
    }
    state.manager.set_resolved(resolved);
    state.manager.set_cfg(new_cfg);
    Ok(())
}

pub(super) async fn switch_model(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<SwitchModelRequest>,
) -> Response {
    let _guard = state.config_write_lock.lock().await;
    // Resolution failure (unconfigured provider etc.) -> BAD_REQUEST (handled in apply_model_config, validated before saving)
    let cfg = state.manager.cfg();
    match apply_model_config(&state, cfg, &req.selector).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(resp) => resp,
    }
}

/// Provider list (target provider when adding a model): configured ones first,
/// unconfigured predefined providers after (supplying an API key enables them, mirroring the TUI needs_setup flow)
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

/// Add a model (mirrors the TUI setup flow) and switch to it on success
pub(super) async fn create_model(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<CreateModelRequest>,
) -> Response {
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
    if req.context_window == Some(0) {
        return (StatusCode::BAD_REQUEST, "context_window 必须为正整数").into_response();
    }

    let mut new_cfg = state.manager.cfg();
    let existing = new_cfg.providers.get(&provider_id).cloned();

    match existing {
        // Configured provider: reuse its credentials (mirrors the TUI: base_url/api_key untouched)
        Some(entry) if !entry.api_key.is_empty() => {
            new_cfg.add_custom_model(
                &provider_id,
                None,
                None,
                &model_id,
                DEFAULT_OUTPUT_TOKENS,
                req.context_window.unwrap_or(DEFAULT_CONTEXT_WINDOW),
            );
        }
        Some(_) => {
            // Present but without credentials: add the key
            let Some(api_key) = req
                .api_key
                .as_deref()
                .map(str::trim)
                .filter(|k| !k.is_empty())
            else {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("provider {} 未配置 API Key，请提供 api_key", provider_id),
                )
                    .into_response();
            };
            let base_url = req
                .base_url
                .as_deref()
                .map(str::trim)
                .filter(|u| !u.is_empty());
            add_model_with_discovery(
                &mut new_cfg,
                &provider_id,
                base_url,
                Some(api_key),
                &model_id,
                req.context_window,
            );
        }
        None => {
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
            let base_url = req
                .base_url
                .as_deref()
                .map(str::trim)
                .filter(|u| !u.is_empty());
            if crate::config::find_provider_def(&provider_id).is_none() {
                // Custom provider: the endpoint is mandatory
                let Some(base_url) = base_url else {
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
            }
            add_model_with_discovery(
                &mut new_cfg,
                &provider_id,
                base_url,
                Some(api_key),
                &model_id,
                req.context_window,
            );
        }
    }

    let selector = format!("{}/{}", provider_id, model_id);
    match apply_model_config(&state, new_cfg, &selector).await {
        Ok(()) => (StatusCode::CREATED, Json(&selector)).into_response(),
        Err(resp) => resp,
    }
}

/// Add a model, taking its parameters from the `/models` cache (see `discover_models`) when the
/// request leaves `context_window` open; otherwise the explicit value or the default applies.
fn add_model_with_discovery(
    cfg: &mut crate::config::Config,
    provider_id: &str,
    base_url: Option<&str>,
    api_key: Option<&str>,
    model_id: &str,
    context_window: Option<u32>,
) {
    match (context_window, cfg.remote_model(provider_id, model_id)) {
        (None, Some(remote)) => cfg.add_model_entry(
            provider_id,
            base_url,
            api_key,
            model_id,
            remote.to_model_entry(),
        ),
        (cw, _) => cfg.add_custom_model(
            provider_id,
            base_url,
            api_key,
            model_id,
            DEFAULT_OUTPUT_TOKENS,
            cw.unwrap_or(DEFAULT_CONTEXT_WINDOW),
        ),
    };
}

/// Fetch a provider's model list (the fetch dialog). With an api_key the not-yet-saved
/// credentials of the add-model dialog apply; without one the saved credentials of the
/// configured provider are used. The list is cached under provider_id, so the following
/// apply-discovered picks the metadata up without the frontend echoing it back.
pub(super) async fn discover_models(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<DiscoverModelsRequest>,
) -> Response {
    // Read credentials under the write lock, then release it for the network fetch: a slow
    // provider must not stall every other config write for up to the fetch timeout
    let provider_id = req.provider_id.unwrap_or_default();
    let (saved_key, saved_base_url) = {
        let _guard = state.config_write_lock.lock().await;
        let cfg = state.manager.cfg();
        let saved_key = cfg
            .providers
            .get(&provider_id)
            .map(|e| e.api_key.clone())
            .unwrap_or_default();
        let saved_base_url = cfg.provider_base_url(&provider_id);
        (saved_key, saved_base_url)
    };

    let api_key = match req
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
    {
        Some(k) => k.to_string(),
        None if !saved_key.is_empty() && !provider_id.is_empty() => saved_key,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "API Key 不能为空（未配置的 provider 需要提供 api_key）",
            )
                .into_response();
        }
    };

    let base_url = match req
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
    {
        Some(url) => url.to_string(),
        None => match crate::config::find_provider_def(&provider_id) {
            Some(def) => def.base_url.to_string(),
            None => match saved_base_url {
                Some(url) => url,
                None => {
                    return (
                        StatusCode::BAD_REQUEST,
                        "API 地址必填（或 provider_id 为内置服务商）",
                    )
                        .into_response();
                }
            },
        },
    };

    let models =
        match crate::config::model_discovery::fetch_provider_models(&base_url, &api_key).await {
            Ok(m) => m,
            Err(e) => return (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
        };

    // Re-take the lock and rebase onto the latest config: writes made while the fetch was
    // in flight must not be clobbered by set_cfg
    let _guard = state.config_write_lock.lock().await;
    let mut new_cfg = state.manager.cfg();
    let already_added = |id: &str| {
        new_cfg
            .providers
            .get(&provider_id)
            .is_some_and(|e| e.find_model_key(id).is_some())
    };
    let resp = DiscoverModelsResponse {
        provider_id: provider_id.clone(),
        models: models
            .iter()
            .map(|m| DiscoveredModelDto {
                id: m.id.clone(),
                name: m.name.clone(),
                context_window: m.context_window,
                max_tokens: m.max_output_tokens,
                supports_vision: m.supports_vision,
                already_added: already_added(&m.id),
            })
            .collect(),
    };

    if !provider_id.is_empty() {
        new_cfg.set_discovered_models(&provider_id, models);
        if let Err(e) = crate::config::model_discovery::save_cache(&new_cfg.discovered) {
            eprintln!("[warn] 无法写入模型列表缓存: {e}");
        }
        state.manager.set_cfg(new_cfg);
    }
    Json(resp).into_response()
}

/// Add the checked models of a fetched list to the provider's model table and persist.
/// Rows already in the table win (they are skipped), and the main model is left untouched.
pub(super) async fn apply_discovered(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<ApplyDiscoveredRequest>,
) -> Response {
    let _guard = state.config_write_lock.lock().await;
    let mut new_cfg = state.manager.cfg();
    let added = match new_cfg.apply_discovered_models(&req.provider_id, &req.model_ids) {
        Ok(a) => a,
        Err(e) => return update_error_response(e),
    };
    if let Err(e) = new_cfg.save() {
        return err500(e);
    }
    state.manager.set_cfg(new_cfg);
    Json(ApplyDiscoveredResponse { added }).into_response()
}

/// RemoveError -> HTTP status: malformed 400, missing 404, predefined/no-model-left 409
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

/// Delete a custom model; auto-migrate when it is the current model (handled in remove_custom_model)
pub(super) async fn delete_model(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<DeleteModelRequest>,
) -> Response {
    let _guard = state.config_write_lock.lock().await;
    let mut new_cfg = state.manager.cfg();
    let migrated = match new_cfg.remove_custom_model(&req.selector) {
        Ok(m) => m,
        Err(e) => return remove_error_response(e),
    };
    // Migrated -> the new main_model must go through apply_model_config to take full effect; otherwise just save
    let target = migrated.unwrap_or_else(|| new_cfg.main_model.clone());
    match apply_model_config(&state, new_cfg, &target).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(resp) => resp,
    }
}

/// Delete a custom provider (the whole entry, credentials and all models)
pub(super) async fn delete_provider(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<DeleteProviderRequest>,
) -> Response {
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

/// UpdateError → HTTP status: bad input 400, missing entry 404, invariant violation 409
fn update_error_response(e: crate::config::UpdateError) -> Response {
    let status = match &e {
        crate::config::UpdateError::InvalidSelector(_) | crate::config::UpdateError::Invalid(_) => {
            StatusCode::BAD_REQUEST
        }
        crate::config::UpdateError::NotFound(_) => StatusCode::NOT_FOUND,
        crate::config::UpdateError::Conflict(_) => StatusCode::CONFLICT,
    };
    (status, e.to_string()).into_response()
}

/// Persist an edited config. When the edited entry backs the current main model, re-resolve it
/// and push the new ResolvedModel to every built session (window / credentials may have changed).
async fn commit_config_update(
    state: &Arc<WebServerState>,
    new_cfg: crate::config::Config,
    affected_provider: &str,
) -> Response {
    let active_affected = new_cfg
        .main_model
        .split_once('/')
        .is_some_and(|(pid, _)| pid == affected_provider);
    if active_affected {
        let selector = new_cfg.main_model.clone();
        return match apply_model_config(state, new_cfg, &selector).await {
            Ok(()) => StatusCode::OK.into_response(),
            Err(resp) => resp,
        };
    }
    if let Err(e) = new_cfg.save() {
        return err500(e);
    }
    state.manager.set_cfg(new_cfg);
    StatusCode::OK.into_response()
}

/// Edit an existing model (PUT /api/models/custom): parameter overrides + optional rename
pub(super) async fn update_model(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<UpdateModelRequest>,
) -> Response {
    let _guard = state.config_write_lock.lock().await;
    let mut new_cfg = state.manager.cfg();
    let selector = match new_cfg.update_model(
        &req.selector,
        req.model_id.as_deref(),
        req.context_window,
        req.max_tokens,
        req.supports_vision,
    ) {
        Ok(selector) => selector,
        Err(e) => return update_error_response(e),
    };
    let provider_id = selector
        .split_once('/')
        .map(|(pid, _)| pid.to_string())
        .unwrap_or_default();
    let resp = commit_config_update(&state, new_cfg, &provider_id).await;
    if resp.status() == StatusCode::OK {
        return (StatusCode::OK, Json(selector)).into_response();
    }
    resp
}

/// Edit an existing provider (PUT /api/providers): credential/endpoint overrides + optional rename
pub(super) async fn update_provider(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<UpdateProviderRequest>,
) -> Response {
    let _guard = state.config_write_lock.lock().await;
    let mut new_cfg = state.manager.cfg();
    let provider_id = match new_cfg.update_provider(
        &req.provider_id,
        req.new_id.as_deref(),
        req.api_key.as_deref(),
        req.base_url.as_deref(),
    ) {
        Ok(id) => id,
        Err(e) => return update_error_response(e),
    };
    commit_config_update(&state, new_cfg, &provider_id).await
}
