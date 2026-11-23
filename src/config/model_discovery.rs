//! Model discovery through the OpenAI-compatible `GET /models` endpoint.
//!
//! Response shapes differ between providers: some (DeepSeek) return metadata such as
//! `context_window` and `input_modalities`, others (GLM) return ids only. The cached list is
//! a hint layer feeding the fetch dialog — a model only enters the config when the user picks
//! it there. Caching keeps the picker free of network calls.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use color_eyre::eyre::{Result, WrapErr, eyre};
use serde::{Deserialize, Serialize};

use super::types::{CustomModelEntry, default_context_window, default_max_tokens};

const CACHE_FILE_NAME: &str = "models.json";
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// One model as reported by the provider; metadata is absent when the provider omits it
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_vision: Option<bool>,
}

impl RemoteModel {
    /// A config table row for this model: reported metadata, defaults where the provider said
    /// nothing. Used when the user picks the model in the fetch dialog.
    pub fn to_model_entry(&self) -> CustomModelEntry {
        CustomModelEntry {
            max_tokens: self.max_output_tokens.unwrap_or_else(default_max_tokens),
            context_window: self.context_window.unwrap_or_else(default_context_window),
            supports_vision: self.supports_vision,
        }
    }
}

/// Last successful fetch for one provider
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderDiscovery {
    /// Unix seconds of the fetch
    pub fetched_at: u64,
    pub models: Vec<RemoteModel>,
}

pub type DiscoveryCache = BTreeMap<String, ProviderDiscovery>;

#[derive(Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<RawModel>,
}

/// One entry of the `/models` payload. Field names differ between providers (vLLM reports
/// `max_model_len`, some gateways report `context_length`), so aliases are accepted.
#[derive(Deserialize)]
struct RawModel {
    id: String,
    #[serde(default, alias = "display_name")]
    name: Option<String>,
    #[serde(
        default,
        alias = "context_length",
        alias = "max_context_length",
        alias = "max_model_len"
    )]
    context_window: Option<u32>,
    #[serde(default, alias = "max_tokens", alias = "max_completion_tokens")]
    max_output_tokens: Option<u32>,
    #[serde(default)]
    input_modalities: Option<Vec<String>>,
    #[serde(default)]
    supports_vision: Option<bool>,
    #[serde(default)]
    capabilities: Option<RawCapabilities>,
}

#[derive(Deserialize)]
struct RawCapabilities {
    #[serde(default)]
    vision: Option<bool>,
}

/// Parse a `/models` response body. Kept separate from the request so it can be unit tested.
pub fn parse_models_response(body: &str) -> Result<Vec<RemoteModel>> {
    let parsed: ModelsResponse =
        serde_json::from_str(body).map_err(|e| eyre!("模型列表响应无法解析: {e}"))?;
    Ok(parsed
        .data
        .into_iter()
        .map(|m| RemoteModel {
            id: m.id,
            name: m.name,
            context_window: m.context_window,
            max_output_tokens: m.max_output_tokens,
            // Explicit flags win over the modality list; a provider that reports neither stays unknown
            supports_vision: m
                .supports_vision
                .or(m.capabilities.and_then(|c| c.vision))
                .or_else(|| {
                    m.input_modalities
                        .map(|modalities| modalities.iter().any(|m| m == "image"))
                }),
        })
        .collect())
}

/// Fetch the provider's model list. Server order is preserved (it is the provider's own ranking).
pub async fn fetch_provider_models(base_url: &str, api_key: &str) -> Result<Vec<RemoteModel>> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .wrap_err("无法创建 HTTP 客户端")?;
    let resp = client
        .get(&url)
        .bearer_auth(api_key)
        .send()
        .await
        .wrap_err_with(|| format!("请求 {url} 失败，请检查网络与 API 地址"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(eyre!(
            "服务商返回 HTTP {}（模型列表不可用，请检查 API Key 与 API 地址）",
            status.as_u16()
        ));
    }
    parse_models_response(&body)
}

/// Validate the provider is refreshable, returning (api_key, base_url).
/// Shared pre-flight of the TUI model picker and the web fetch flows.
pub(crate) fn check_refreshable(
    cfg: &super::Config,
    provider_id: &str,
) -> Result<(String, String)> {
    let entry = cfg
        .providers
        .get(provider_id)
        .ok_or_else(|| eyre!("未找到 provider: {provider_id}"))?;
    if entry.api_key.is_empty() {
        return Err(eyre!(
            "provider {provider_id} 未配置 API Key，无法获取模型列表"
        ));
    }
    let base_url = cfg
        .provider_base_url(provider_id)
        .ok_or_else(|| eyre!("provider {provider_id} 未配置 API 地址"))?;
    Ok((entry.api_key.clone(), base_url))
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn cache_path() -> Result<PathBuf> {
    Ok(super::config_dir()?.join(CACHE_FILE_NAME))
}

/// Read the cache; a missing or corrupt file is treated as empty (it is a disposable hint layer)
pub fn load_cache() -> DiscoveryCache {
    let Ok(path) = cache_path() else {
        return DiscoveryCache::new();
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return DiscoveryCache::new();
    };
    serde_json::from_str(&content).unwrap_or_default()
}

pub fn save_cache(cache: &DiscoveryCache) -> Result<()> {
    let path = cache_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .wrap_err_with(|| format!("无法创建目录: {}", dir.display()))?;
    }
    let body = serde_json::to_string_pretty(cache)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body).wrap_err_with(|| format!("无法写入 {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).wrap_err_with(|| format!("无法写入 {}", path.display()))?;
    Ok(())
}
