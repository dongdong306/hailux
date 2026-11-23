use async_openai::config::OpenAIConfig;
use color_eyre::{Result, eyre::Context, eyre::ContextCompat};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub(crate) mod model_discovery;
mod providers;
mod toml_serde;
mod types;

#[cfg(test)]
mod tests;

pub(crate) use providers::{PROVIDERS, find_provider_def};
pub use types::{
    CustomModelEntry, ModelEntry, PermissionConfig, ProviderEntry, RemoveError, RetryConfigToml,
    UpdateError,
};

use crate::agent::ReasoningEffort;

const CONFIG_DIR_NAME: &str = ".hailux";
const CONFIG_FILE_NAME: &str = "config.toml";

/// Max output tokens (max_completion_tokens)
pub const DEFAULT_OUTPUT_TOKENS: u32 = 65536;
/// Context window assumed for newly added models (256K)
pub const DEFAULT_CONTEXT_WINDOW: u32 = 262144;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub main_model: String,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderEntry>,
    /// Auto-compaction threshold (0.0-1.0); compaction triggers once context token usage exceeds this ratio
    #[serde(default = "default_compact_threshold")]
    pub compact_threshold: f32,
    /// Permission rules
    #[serde(default)]
    pub permission: PermissionConfig,
    /// Retry policy for transient LLM request errors
    #[serde(default)]
    pub retry: RetryConfigToml,
    /// Reasoning effort (top-level `reasoning_effort` in the request body: low/high/max; default high)
    #[serde(default)]
    pub reasoning_effort: ReasoningEffort,
    /// Model lists fetched from each provider's `/models` endpoint (a disposable hint layer, never saved to config.toml)
    #[serde(skip)]
    pub(crate) discovered: model_discovery::DiscoveryCache,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            main_model: String::new(),
            providers: BTreeMap::new(),
            compact_threshold: default_compact_threshold(),
            permission: PermissionConfig::default(),
            retry: RetryConfigToml::default(),
            reasoning_effort: ReasoningEffort::default(),
            discovered: model_discovery::DiscoveryCache::new(),
        }
    }
}

fn default_compact_threshold() -> f32 {
    0.75
}

// ── Core methods ─────────────────────────────────────────────

impl Config {
    /// Display name of a provider
    fn provider_display_name(&self, pid: &str) -> String {
        if let Some(def) = providers::find_provider_def(pid) {
            def.name.to_string()
        } else {
            pid.to_string()
        }
    }

    /// Base URL of a provider
    pub fn provider_base_url(&self, pid: &str) -> Option<String> {
        let entry = self.providers.get(pid)?;
        if let Some(ref url) = entry.base_url {
            return Some(url.clone());
        }
        providers::find_provider_def(pid).map(|d| d.base_url.to_string())
    }

    /// Model metadata fetched from the provider, when the cache holds it
    pub fn remote_model(&self, pid: &str, model_id: &str) -> Option<&model_discovery::RemoteModel> {
        self.discovered
            .get(pid)?
            .models
            .iter()
            .find(|m| m.id.eq_ignore_ascii_case(model_id))
    }

    /// Replace one provider's discovered list and refresh its fetch timestamp
    pub(crate) fn set_discovered_models(
        &mut self,
        pid: &str,
        models: Vec<model_discovery::RemoteModel>,
    ) {
        self.discovered.insert(
            pid.to_string(),
            model_discovery::ProviderDiscovery {
                fetched_at: model_discovery::now_unix(),
                models,
            },
        );
    }

    /// Available models: exactly the provider's model table. The `/models` cache only feeds
    /// the fetch dialog; a reported model becomes visible once the user picks it.
    pub fn available_models(&self) -> Vec<ModelEntry> {
        let mut result = Vec::new();

        for (pid, entry) in &self.providers {
            if entry.api_key.is_empty() {
                continue;
            }
            let provider_name = self.provider_display_name(pid);

            for mid in entry.models.keys() {
                result.push(ModelEntry {
                    provider_id: pid.clone(),
                    provider_name: provider_name.clone(),
                    model_id: mid.clone(),
                    model_name: mid.clone(),
                    display: format!("{}/{}", pid, mid),
                });
            }
        }

        result
    }

    /// Resolve a "provider/model" selector into OpenAIConfig, model id, max_tokens and context_window
    pub fn resolve(&self, selector: &str) -> Result<ResolvedModel> {
        let (provider_id, model_id) = selector.split_once('/').ok_or_else(|| {
            color_eyre::eyre::eyre!("模型格式错误，应为 provider/model: {}", selector)
        })?;

        let entry = self
            .providers
            .get(provider_id)
            .ok_or_else(|| color_eyre::eyre::eyre!("未找到 provider: {}", provider_id))?;

        if entry.api_key.is_empty() {
            return Err(color_eyre::eyre::eyre!(
                "provider {} 的 api_key 未设置",
                provider_id
            ));
        }

        let base_url = self
            .provider_base_url(provider_id)
            .ok_or_else(|| color_eyre::eyre::eyre!("provider {} 未配置 base_url", provider_id))?;

        // Table-driven: a model that was never picked from the fetch dialog cannot be used.
        // Case-insensitive: the table key is the canonical spelling (see find_model_key).
        let actual_id = entry.find_model_key(model_id).ok_or_else(|| {
            color_eyre::eyre::eyre!("模型 {} 未配置，请先在模型列表中添加", selector)
        })?;
        let custom = &entry.models[&actual_id];
        let supports_vision = custom.supports_vision.unwrap_or(false);

        // Inference API split: predefined providers follow their declaration (DeepSeek/GLM native, send the
        // private `thinking` switch); custom providers always use the standard OpenAI API (no thinking)
        let native_reasoning = providers::find_provider_def(provider_id)
            .map(|d| d.native_reasoning)
            .unwrap_or(false);

        let config = OpenAIConfig::new()
            .with_api_key(&entry.api_key)
            .with_api_base(&base_url);

        Ok(ResolvedModel {
            config,
            model_id: actual_id.clone(),
            max_tokens: custom.max_tokens,
            context_window: custom.context_window,
            display: format!("{}/{}", provider_id, actual_id),
            supports_vision,
            native_reasoning,
        })
    }

    /// Resolve the default model
    pub fn resolve_default(&self) -> Result<ResolvedModel> {
        if self.main_model.is_empty() {
            return Err(color_eyre::eyre::eyre!("未配置模型，请通过设置添加"));
        }
        self.resolve(&self.main_model)
    }

    /// Retry policy for LLM requests (runtime form)
    pub fn retry_config(&self) -> crate::agent::retry::RetryConfig {
        crate::agent::retry::RetryConfig::from(&self.retry)
    }
}

impl Config {
    /// Add a model to an existing provider with explicit parameters; returns the display string.
    /// A missing provider is created automatically (requires both base_url and api_key unless predefined).
    pub fn add_custom_model(
        &mut self,
        provider_id: &str,
        base_url: Option<&str>,
        api_key: Option<&str>,
        model_id: &str,
        max_tokens: u32,
        context_window: u32,
    ) -> String {
        self.add_model_entry(
            provider_id,
            base_url,
            api_key,
            model_id,
            CustomModelEntry {
                max_tokens,
                context_window,
                // Keep supports_vision from an existing entry so re-adding does not drop user config
                supports_vision: self.providers.get(provider_id).and_then(|e| {
                    e.find_model_key(model_id)
                        .and_then(|key| e.models.get(&key))
                        .and_then(|m| m.supports_vision)
                }),
            },
        )
    }

    /// Add a model with a full entry (the setup / add-model flow passes `/models` metadata here).
    /// A missing provider is created automatically; `base_url`/`api_key` override when given.
    pub fn add_model_entry(
        &mut self,
        provider_id: &str,
        base_url: Option<&str>,
        api_key: Option<&str>,
        model_id: &str,
        entry: CustomModelEntry,
    ) -> String {
        let provider = self
            .providers
            .entry(provider_id.to_string())
            .or_insert_with(|| ProviderEntry {
                api_key: String::new(),
                base_url: None,
                models: BTreeMap::new(),
            });

        if let Some(url) = base_url {
            provider.base_url = Some(url.to_string());
        }
        if let Some(key) = api_key {
            provider.api_key = key.to_string();
        }

        // One row per model regardless of case (find_model_key contract): the inserted
        // spelling replaces a differently-cased pre-existing row instead of duplicating it
        provider
            .models
            .retain(|k, _| !k.eq_ignore_ascii_case(model_id));
        provider.models.insert(model_id.to_string(), entry);

        format!("{}/{}", provider_id, model_id)
    }

    /// Add the picked model ids of one provider from its cached `/models` list (the fetch
    /// dialog's confirm action). Ids already in the table are skipped — the existing row wins.
    /// An id missing from the cache lands on the default parameters.
    ///
    /// Requires the provider to be configured and to have a cached list. Returns the ids that
    /// were actually added, in request order.
    pub fn apply_discovered_models(
        &mut self,
        provider_id: &str,
        model_ids: &[String],
    ) -> Result<Vec<String>, UpdateError> {
        let entry = self
            .providers
            .get(provider_id)
            .ok_or_else(|| UpdateError::NotFound(format!("未找到 provider: {provider_id}")))?;
        if entry.api_key.is_empty() {
            return Err(UpdateError::Conflict(format!(
                "provider {provider_id} 未配置 API Key，请先完成配置"
            )));
        }
        let discovery = self.discovered.get(provider_id).cloned().ok_or_else(|| {
            UpdateError::NotFound(format!(
                "provider {provider_id} 没有已获取的模型列表，请先获取模型"
            ))
        })?;

        let mut added = Vec::new();
        for id in model_ids {
            let id = id.trim();
            if id.is_empty() {
                return Err(UpdateError::Invalid("模型名称不能为空".into()));
            }
            if id.contains('/') {
                return Err(UpdateError::Invalid("模型名称不能包含 /".into()));
            }
            let already = self
                .providers
                .get(provider_id)
                .is_some_and(|e| e.find_model_key(id).is_some());
            if already {
                continue;
            }
            let entry_meta = discovery
                .models
                .iter()
                .find(|m| m.id.eq_ignore_ascii_case(id))
                .map(model_discovery::RemoteModel::to_model_entry)
                .unwrap_or_else(|| CustomModelEntry {
                    max_tokens: types::default_max_tokens(),
                    context_window: types::default_context_window(),
                    supports_vision: None,
                });
            self.add_model_entry(provider_id, None, None, id, entry_meta);
            added.push(id.to_string());
        }
        Ok(added)
    }

    /// Delete a model (selector = "provider/model").
    ///
    /// - When the table empties, clean up the provider: custom providers are removed entirely,
    ///   predefined providers stay (credentials kept, back to discovered-only mode)
    ///
    /// - Deleting the current main_model migrates to the first available model; Err when none is available.
    pub fn remove_custom_model(&mut self, selector: &str) -> Result<Option<String>, RemoveError> {
        // Operate on a clone: a failed deletion must not leave self half-mutated
        let mut next = self.clone();
        let migrated = next.remove_custom_model_inner(selector)?;
        *self = next;
        Ok(migrated)
    }

    fn remove_custom_model_inner(&mut self, selector: &str) -> Result<Option<String>, RemoveError> {
        let (provider_id, model_id) = selector.split_once('/').ok_or_else(|| {
            RemoveError::InvalidSelector(format!("模型格式错误，应为 provider/model: {}", selector))
        })?;

        let entry = self
            .providers
            .get_mut(provider_id)
            .ok_or_else(|| RemoveError::NotFound(format!("未找到 provider: {}", provider_id)))?;
        // Normalize to the table's canonical key so a differently-cased selector still
        // removes the one row (see find_model_key)
        let Some(actual_id) = entry.find_model_key(model_id) else {
            return Err(RemoveError::NotFound(format!("未找到模型: {}", selector)));
        };
        entry
            .models
            .remove(&actual_id)
            .ok_or_else(|| RemoveError::NotFound(format!("未找到模型: {}", selector)))?;

        if entry.models.is_empty() && providers::find_provider_def(provider_id).is_none() {
            // Custom provider: remove the whole entry (predefined ones keep their credentials)
            self.providers.remove(provider_id);
            // Same as remove_provider: no stale discovery entry lingering in models.json
            self.discovered.remove(provider_id);
        }

        // main_model migration (only configured models count)
        let main_is_removed = self
            .main_model
            .split_once('/')
            .is_some_and(|(pid, mid)| pid == provider_id && mid == actual_id);
        if main_is_removed {
            match self.available_models().into_iter().next() {
                Some(next_model) => {
                    let display = next_model.display.clone();
                    self.main_model = display.clone();
                    Ok(Some(display))
                }
                None => Err(RemoveError::NoModelsLeft(format!(
                    "删除 {} 后没有任何可用模型，请先配置其他模型",
                    selector
                ))),
            }
        } else {
            Ok(None)
        }
    }

    /// Delete a custom provider (the whole entry, credentials and all models). Predefined providers cannot be deleted.
    ///
    /// main_model migration semantics match `remove_custom_model`.
    pub fn remove_provider(&mut self, provider_id: &str) -> Result<Option<String>, RemoveError> {
        // Operate on a clone: a failed deletion must not leave self half-mutated
        let mut next = self.clone();
        let migrated = next.remove_provider_inner(provider_id)?;
        *self = next;
        Ok(migrated)
    }

    fn remove_provider_inner(&mut self, provider_id: &str) -> Result<Option<String>, RemoveError> {
        if !self.providers.contains_key(provider_id) {
            return Err(RemoveError::NotFound(format!(
                "未找到 provider: {}",
                provider_id
            )));
        }
        if providers::find_provider_def(provider_id).is_some() {
            return Err(RemoveError::Predefined(format!(
                "预定义 provider {} 不可删除",
                provider_id
            )));
        }
        self.providers.remove(provider_id);
        // Drop the stale discovery cache entry so it does not linger in models.json forever
        self.discovered.remove(provider_id);
        // main_model points at a model of this provider -> migrate (only configured models count)
        if self
            .main_model
            .split_once('/')
            .is_some_and(|(pid, _)| pid == provider_id)
        {
            match self.available_models().into_iter().next() {
                Some(next_model) => {
                    let display = next_model.display.clone();
                    self.main_model = display.clone();
                    Ok(Some(display))
                }
                None => Err(RemoveError::NoModelsLeft(format!(
                    "删除 provider {} 后没有任何可用模型，请先配置其他模型",
                    provider_id
                ))),
            }
        } else {
            Ok(None)
        }
    }

    /// Update an existing model in place (the settings page "edit model" action).
    ///
    /// - Omitted fields keep the configured value
    /// - `model_id` renames when given
    ///
    /// Returns the resulting selector (identical to the input when nothing was renamed).
    pub fn update_model(
        &mut self,
        selector: &str,
        model_id: Option<&str>,
        context_window: Option<u32>,
        max_tokens: Option<u32>,
        supports_vision: Option<bool>,
    ) -> Result<String, UpdateError> {
        // Clone-and-commit: a rejected edit must not leave self half-mutated
        let mut next = self.clone();
        let updated = next.update_model_inner(
            selector,
            model_id,
            context_window,
            max_tokens,
            supports_vision,
        )?;
        *self = next;
        Ok(updated)
    }

    fn update_model_inner(
        &mut self,
        selector: &str,
        model_id: Option<&str>,
        context_window: Option<u32>,
        max_tokens: Option<u32>,
        supports_vision: Option<bool>,
    ) -> Result<String, UpdateError> {
        let (provider_id, current_id) = selector.split_once('/').ok_or_else(|| {
            UpdateError::InvalidSelector(format!("模型格式错误，应为 provider/model: {selector}"))
        })?;

        if context_window == Some(0) {
            return Err(UpdateError::Invalid("上下文窗口必须为正整数".into()));
        }
        if max_tokens == Some(0) {
            return Err(UpdateError::Invalid("输出上限必须为正整数".into()));
        }

        let entry = self
            .providers
            .get_mut(provider_id)
            .ok_or_else(|| UpdateError::NotFound(format!("未找到 provider: {provider_id}")))?;
        if entry.api_key.is_empty() {
            return Err(UpdateError::Conflict(format!(
                "provider {provider_id} 未配置 API Key，请先完成配置"
            )));
        }
        // Only table rows are editable, so a PUT can never conjure a new one.
        // Normalize to the table's canonical key so lookups/removals below stay consistent
        // when the selector's casing differs (see find_model_key).
        let Some(actual_id) = entry.find_model_key(current_id) else {
            return Err(UpdateError::NotFound(format!("未找到模型: {selector}")));
        };
        let Some(current) = entry.models.get(&actual_id).cloned() else {
            return Err(UpdateError::NotFound(format!("未找到模型: {selector}")));
        };

        let target_id = match model_id.map(str::trim) {
            Some("") => return Err(UpdateError::Invalid("模型名称不能为空".into())),
            Some(id) => id,
            None => &actual_id,
        };
        if target_id.contains('/') {
            return Err(UpdateError::Invalid("模型名称不能包含 /".into()));
        }

        let renamed = target_id != actual_id;
        if renamed
            && entry
                .models
                .keys()
                .any(|k| *k != actual_id && k.eq_ignore_ascii_case(target_id))
        {
            return Err(UpdateError::Conflict(format!(
                "模型 {target_id} 已存在于 {provider_id}"
            )));
        }

        let updated = CustomModelEntry {
            max_tokens: max_tokens.unwrap_or(current.max_tokens),
            context_window: context_window.unwrap_or(current.context_window),
            // `or` keeps the configured value when the field is omitted; None ≈ false, so
            // there is no "unset" back to None — an explicit Some(false) covers that case
            supports_vision: supports_vision.or(current.supports_vision),
        };

        let models = &mut entry.models;
        if renamed {
            models.remove(&actual_id);
        }
        models.insert(target_id.to_string(), updated);

        let new_selector = format!("{provider_id}/{target_id}");
        let main_is_current = self
            .main_model
            .split_once('/')
            .is_some_and(|(pid, mid)| pid == provider_id && mid == actual_id);
        if renamed && main_is_current {
            self.main_model = new_selector.clone();
        }
        Ok(new_selector)
    }

    /// Update an existing provider's credentials and endpoint (the settings page "edit provider" action).
    ///
    /// - `api_key`: overrides when given (empty is rejected)
    /// - `base_url`: `Some("")` clears the override and falls back to the predefined endpoint;
    ///   rejected for custom providers, which would be left without an endpoint
    /// - `new_id`: renames when given; predefined providers reject renames
    /// - An unconfigured predefined provider is enabled by supplying `api_key` (same as the setup flow)
    ///
    /// Returns the resulting provider id.
    pub fn update_provider(
        &mut self,
        provider_id: &str,
        new_id: Option<&str>,
        api_key: Option<&str>,
        base_url: Option<&str>,
    ) -> Result<String, UpdateError> {
        let mut next = self.clone();
        let updated = next.update_provider_inner(provider_id, new_id, api_key, base_url)?;
        *self = next;
        Ok(updated)
    }

    fn update_provider_inner(
        &mut self,
        provider_id: &str,
        new_id: Option<&str>,
        api_key: Option<&str>,
        base_url: Option<&str>,
    ) -> Result<String, UpdateError> {
        let def = providers::find_provider_def(provider_id);
        let mut entry = match self.providers.get(provider_id).cloned() {
            Some(entry) => entry,
            // Unconfigured predefined provider: fill in the key to enable it (same as the setup flow)
            None if def.is_some() => ProviderEntry {
                api_key: String::new(),
                base_url: None,
                models: BTreeMap::new(),
            },
            None => {
                return Err(UpdateError::NotFound(format!(
                    "未找到 provider: {provider_id}"
                )));
            }
        };

        if let Some(key) = api_key.map(str::trim) {
            if key.is_empty() {
                return Err(UpdateError::Invalid("API Key 不能为空".into()));
            }
            entry.api_key = key.to_string();
        }
        if entry.api_key.is_empty() {
            return Err(UpdateError::Conflict(format!(
                "provider {provider_id} 尚未配置 API Key，请填写后保存"
            )));
        }

        if let Some(url) = base_url {
            let url = url.trim();
            if url.is_empty() {
                if def.is_none() {
                    return Err(UpdateError::Invalid(format!(
                        "自定义服务商 {provider_id} 必须提供 API 地址"
                    )));
                }
                entry.base_url = None;
            } else if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err(UpdateError::Invalid(
                    "API 地址必须以 http:// 或 https:// 开头".into(),
                ));
            } else {
                entry.base_url = Some(url.to_string());
            }
        }

        let target_id = match new_id.map(str::trim) {
            Some("") => return Err(UpdateError::Invalid("服务商标识不能为空".into())),
            Some(id) => id,
            None => provider_id,
        };

        if target_id != provider_id {
            if def.is_some() {
                return Err(UpdateError::Conflict(format!(
                    "预定义服务商 {provider_id} 不可重命名"
                )));
            }
            if target_id.contains('/') {
                return Err(UpdateError::Invalid("服务商标识不能包含 /".into()));
            }
            if providers::find_provider_def(target_id).is_some()
                || self.providers.contains_key(target_id)
            {
                return Err(UpdateError::Conflict(format!("服务商 {target_id} 已存在")));
            }
            self.providers.remove(provider_id);
            // Keep the discovery cache reachable: resolve/available_models look it up by provider id
            if let Some(disc) = self.discovered.remove(provider_id) {
                self.discovered.insert(target_id.to_string(), disc);
            }
            // main_model carries the provider id as its prefix, so it follows the rename
            let main_model = self.main_model.clone();
            if let Some((pid, mid)) = main_model.split_once('/')
                && pid == provider_id
            {
                self.main_model = format!("{target_id}/{mid}");
            }
        }
        self.providers.insert(target_id.to_string(), entry);
        Ok(target_id.to_string())
    }

    /// All configured providers (used by the UI to pick a target provider)
    pub fn configured_providers(&self) -> Vec<ProviderInfo> {
        let mut result = Vec::new();
        for (pid, entry) in &self.providers {
            if entry.api_key.is_empty() {
                continue;
            }
            let name = self.provider_display_name(pid);
            let base_url = self.provider_base_url(pid).unwrap_or_default();
            result.push(ProviderInfo {
                id: pid.clone(),
                name,
                base_url,
            });
        }
        result
    }

    /// Persist to the config file
    pub fn save(&self) -> Result<()> {
        toml_serde::save_config(self)
    }
}

#[derive(Debug, Clone)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub base_url: String,
}

#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub config: OpenAIConfig,
    pub model_id: String,
    pub max_tokens: u32,
    pub context_window: u32,
    pub display: String,
    /// Whether the model accepts image input; when not, attachments are downgraded to hint text at request assembly
    pub supports_vision: bool,
    /// Whether this uses the native DeepSeek/GLM reasoning API (decides whether to send the private
    /// `thinking` switch; the three reasoning_effort levels are common to both ecosystems and always passed through)
    pub native_reasoning: bool,
}

// ── Config file I/O ──────────────────────────────────────────

fn config_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().wrap_err("无法获取用户主目录")?;
    Ok(home.join(CONFIG_DIR_NAME))
}

fn config_file_path() -> Result<PathBuf> {
    Ok(config_dir()?.join(CONFIG_FILE_NAME))
}

/// Load result: config is ready, or initial setup is required
pub enum LoadResult {
    Ready(Box<Config>),
    NeedsSetup,
}

/// Read the config file and decide whether initial setup is needed
pub fn load() -> Result<LoadResult> {
    let path = config_file_path()?;

    if !path.exists() {
        return Ok(LoadResult::NeedsSetup);
    }

    let content = std::fs::read_to_string(&path)
        .wrap_err_with(|| format!("无法读取配置文件: {}", path.display()))?;
    let mut config: Config = toml::from_str(&content)
        .wrap_err_with(|| format!("无法解析配置文件: {}", path.display()))?;
    config.discovered = model_discovery::load_cache();

    if config.compact_threshold <= 0.0 || config.compact_threshold >= 1.0 {
        config.compact_threshold = default_compact_threshold();
    }

    let has_valid_provider = config.providers.values().any(|e| !e.api_key.is_empty());
    if !has_valid_provider {
        return Ok(LoadResult::NeedsSetup);
    }

    if config.main_model.is_empty() {
        let available = config.available_models();
        if available.is_empty() {
            return Ok(LoadResult::NeedsSetup);
        }
        config.main_model = available[0].display.clone();
    }

    if config.resolve_default().is_err() {
        let available = config.available_models();
        if !available.is_empty() {
            let models_str: Vec<&str> = available.iter().map(|m| m.display.as_str()).collect();
            return Err(color_eyre::eyre::eyre!(
                "当前 models 不可用，可用的模型有: {}，请编辑 {}",
                models_str.join(", "),
                path.display()
            ));
        }
    }

    Ok(LoadResult::Ready(Box::new(config)))
}
