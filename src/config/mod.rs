use async_openai::config::OpenAIConfig;
use color_eyre::{Result, eyre::Context, eyre::ContextCompat};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

mod providers;
mod toml_serde;
mod types;

#[cfg(test)]
mod tests;

pub(crate) use providers::PROVIDERS;
#[cfg(feature = "web")]
pub(crate) use providers::find_provider_def;
pub use types::{
    CustomModelEntry, ModelEntry, PermissionConfig, ProviderEntry, RemoveError, RetryConfigToml,
};

const CONFIG_DIR_NAME: &str = ".hailux";
const CONFIG_FILE_NAME: &str = "config.toml";

/// 模型输出上限（max_completion_tokens）
pub const DEFAULT_OUTPUT_TOKENS: u32 = 65536;
/// 模型上下文窗口大小
pub const DEFAULT_CONTEXT_WINDOW: u32 = 131072;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub main_model: String,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderEntry>,
    /// 自动压缩阈值（0.0-1.0），上下文 token 占比超过此值时自动压缩
    #[serde(default = "default_compact_threshold")]
    pub compact_threshold: f32,
    /// 权限配置
    #[serde(default)]
    pub permission: PermissionConfig,
    /// LLM 请求瞬态错误重试策略
    #[serde(default)]
    pub retry: RetryConfigToml,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            main_model: String::new(),
            providers: BTreeMap::new(),
            compact_threshold: default_compact_threshold(),
            permission: PermissionConfig::default(),
            retry: RetryConfigToml::default(),
        }
    }
}

fn default_compact_threshold() -> f32 {
    0.75
}

// ── 核心方法 ─────────────────────────────────────────────────

impl Config {
    /// 获取 provider 的显示名称
    fn provider_display_name(&self, pid: &str) -> String {
        if let Some(def) = providers::find_provider_def(pid) {
            def.name.to_string()
        } else {
            pid.to_string()
        }
    }

    /// 获取 provider 的 base_url
    fn provider_base_url(&self, pid: &str) -> Option<String> {
        let entry = self.providers.get(pid)?;
        if let Some(ref url) = entry.base_url {
            return Some(url.clone());
        }
        providers::find_provider_def(pid).map(|d| d.base_url.to_string())
    }

    /// 返回所有已启用 provider 下的可选模型列表
    pub fn available_models(&self) -> Vec<ModelEntry> {
        let mut result = Vec::new();

        // 已配置的 provider
        for (pid, entry) in &self.providers {
            if entry.api_key.is_empty() {
                continue;
            }
            let provider_name = self.provider_display_name(pid);

            // 自定义模型优先
            if let Some(ref custom_models) = entry.models {
                for mid in custom_models.keys() {
                    // 与预定义模型同 id 的条目来自 setup 同步，删除后会被预定义合并分支复活 → 不可删
                    let deletable = providers::find_provider_def(pid)
                        .and_then(|d| d.models.iter().find(|m| m.id == *mid))
                        .is_none();
                    result.push(ModelEntry {
                        provider_id: pid.clone(),
                        provider_name: provider_name.clone(),
                        model_id: mid.clone(),
                        model_name: mid.clone(),
                        display: format!("{}/{}", pid, mid),
                        needs_setup: false,
                        deletable,
                    });
                }
            }

            // 合并预定义模型（跳过已被自定义覆盖的），确保新增预定义模型对已有配置可见
            if let Some(def) = providers::find_provider_def(pid) {
                for m in def.models {
                    let covered = entry
                        .models
                        .as_ref()
                        .is_some_and(|models| models.contains_key(m.id));
                    if !covered {
                        result.push(ModelEntry {
                            provider_id: pid.clone(),
                            provider_name: provider_name.clone(),
                            model_id: m.id.to_string(),
                            model_name: m.name.to_string(),
                            display: format!("{}/{}", pid, m.id),
                            needs_setup: false,
                            deletable: false,
                        });
                    }
                }
            }
        }

        // 未配置的预定义 provider
        for def in providers::PROVIDERS {
            if !self.providers.contains_key(def.id) {
                for m in def.models {
                    result.push(ModelEntry {
                        provider_id: def.id.to_string(),
                        provider_name: def.name.to_string(),
                        model_id: m.id.to_string(),
                        model_name: m.name.to_string(),
                        display: format!("{}/{}", def.id, m.id),
                        needs_setup: true,
                        deletable: false,
                    });
                }
            }
        }

        result
    }

    /// 根据 "provider/model" 选择器解析出 OpenAIConfig、模型 ID、max_tokens、context_window
    pub fn resolve(&self, selector: &str) -> Result<ResolvedModel> {
        let (provider_id, model_id) = selector.split_once('/').ok_or_else(|| {
            color_eyre::eyre::eyre!("模型格式错误，应为 provider/model: {}", selector)
        })?;

        let entry = self
            .providers
            .get(provider_id)
            .ok_or_else(|| color_eyre::eyre::eyre!("未找到 provider: {}", provider_id))?;

        if providers::REMOVED_PREDEFINED_MODELS
            .iter()
            .any(|(p, m)| *p == provider_id && *m == model_id)
        {
            return Err(color_eyre::eyre::eyre!(
                "模型 {selector} 已从预定义列表移除，请通过 /model 重新选择（config 中残留的旧条目可删除）"
            ));
        }

        if entry.api_key.is_empty() {
            return Err(color_eyre::eyre::eyre!(
                "provider {} 的 api_key 未设置",
                provider_id
            ));
        }

        let base_url = self
            .provider_base_url(provider_id)
            .ok_or_else(|| color_eyre::eyre::eyre!("provider {} 未配置 base_url", provider_id))?;

        // 查找模型的 max_tokens 和 context_window
        let (max_tokens, context_window, supports_vision) =
            if let Some(ref custom_models) = entry.models {
                if let Some(custom) = custom_models.get(model_id) {
                    (
                        custom.max_tokens,
                        custom.context_window,
                        custom.supports_vision,
                    )
                } else {
                    providers::fallback_model_values(provider_id, model_id)
                }
            } else {
                providers::fallback_model_values(provider_id, model_id)
            };
        // supports_vision 回退链：显式配置 > 同 provider 预定义模型声明 > false
        let supports_vision = supports_vision
            .or_else(|| providers::predefined_vision_flag(provider_id, model_id))
            .unwrap_or(false);

        let config = OpenAIConfig::new()
            .with_api_key(&entry.api_key)
            .with_api_base(&base_url);

        Ok(ResolvedModel {
            config,
            model_id: model_id.to_string(),
            max_tokens,
            context_window,
            display: format!("{}/{}", provider_id, model_id),
            supports_vision,
        })
    }

    /// 解析默认模型
    pub fn resolve_default(&self) -> Result<ResolvedModel> {
        if self.main_model.is_empty() {
            return Err(color_eyre::eyre::eyre!("未配置模型，请通过设置添加"));
        }
        self.resolve(&self.main_model)
    }

    /// LLM 请求重试策略（运行时形态）
    pub fn retry_config(&self) -> crate::agent::retry::RetryConfig {
        crate::agent::retry::RetryConfig::from(&self.retry)
    }
}

impl Config {
    /// 添加自定义模型到已有 provider，返回新模型的 display 字符串
    /// 如果 provider 不存在会自动创建（需同时提供 base_url 和 api_key）
    #[allow(clippy::too_many_arguments)]
    pub fn add_custom_model(
        &mut self,
        provider_id: &str,
        base_url: Option<&str>,
        api_key: Option<&str>,
        model_id: &str,
        max_tokens: u32,
        context_window: u32,
    ) -> String {
        let entry = self
            .providers
            .entry(provider_id.to_string())
            .or_insert_with(|| ProviderEntry {
                api_key: String::new(),
                base_url: None,
                models: Some(BTreeMap::new()),
            });

        if let Some(url) = base_url {
            entry.base_url = Some(url.to_string());
        }
        if let Some(key) = api_key {
            entry.api_key = key.to_string();
        }

        let models = entry.models.get_or_insert_with(BTreeMap::new);
        // 保留已有条目的 supports_vision（重复添加不丢用户配置）；
        // 缺省时物化预定义声明，使 config.toml 始终反映生效值
        let supports_vision = models
            .get(model_id)
            .and_then(|m| m.supports_vision)
            .or_else(|| providers::predefined_vision_flag(provider_id, model_id));
        models.insert(
            model_id.to_string(),
            CustomModelEntry {
                max_tokens,
                context_window,
                supports_vision,
            },
        );

        format!("{}/{}", provider_id, model_id)
    }

    /// 删除用户自定义模型（selector = "provider/model"）。
    ///
    /// - 预定义 provider 的预定义模型不可删：即使被 setup 同步写入 models 表，
    ///   删除后也会被 available_models 的预定义合并分支复活
    /// - models 表删空时清理 provider：自定义 provider 整条移除，预定义 provider 重置为 None（回归合并预定义模式）
    /// - 若删除的是当前 main_model：迁移到第一个可用模型；无任何可用模型则拒绝（Err）
    ///
    /// 返回 Ok(Some(新 main_model)) 表示已迁移；Ok(None) 表示 main_model 未受影响。
    pub fn remove_custom_model(&mut self, selector: &str) -> Result<Option<String>, RemoveError> {
        // 在克隆上操作，失败（含拒绝删除场景）时保持 self 不被污染
        let mut next = self.clone();
        let migrated = next.remove_custom_model_inner(selector)?;
        *self = next;
        Ok(migrated)
    }

    fn remove_custom_model_inner(&mut self, selector: &str) -> Result<Option<String>, RemoveError> {
        let (provider_id, model_id) = selector.split_once('/').ok_or_else(|| {
            RemoveError::InvalidSelector(format!("模型格式错误，应为 provider/model: {}", selector))
        })?;

        let is_predefined_model = providers::find_provider_def(provider_id)
            .is_some_and(|d| d.models.iter().any(|m| m.id == model_id));
        if is_predefined_model {
            return Err(RemoveError::Predefined(format!(
                "预定义模型 {}/{} 不可删除",
                provider_id, model_id
            )));
        }

        let entry = self
            .providers
            .get_mut(provider_id)
            .ok_or_else(|| RemoveError::NotFound(format!("未找到 provider: {}", provider_id)))?;
        let in_models = entry
            .models
            .as_ref()
            .is_some_and(|models| models.contains_key(model_id));
        if !in_models {
            return Err(RemoveError::NotFound(format!("未找到模型: {}", selector)));
        }
        if let Some(models) = entry.models.as_mut() {
            models.remove(model_id);
        }

        // models 删空后的 provider 清理
        let entry_empty = entry
            .models
            .as_ref()
            .is_some_and(|models| models.is_empty());
        if entry_empty {
            if providers::find_provider_def(provider_id).is_some() {
                // 预定义 provider：models 重置为 None，回归合并预定义模式
                entry.models = None;
            } else {
                // 自定义 provider：整条移除（else 分支不再使用 entry，NLL 借用已结束）
                self.providers.remove(provider_id);
            }
        }

        // main_model 迁移（仅考虑已配置的模型，未配置的预定义条目无法 resolve）
        if self.main_model == selector {
            match self.available_models().into_iter().find(|m| !m.needs_setup) {
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

    /// 删除自定义 provider（整条含凭据与全部模型）。预定义 provider 不可删。
    ///
    /// main_model 迁移语义同 `remove_custom_model`。
    pub fn remove_provider(&mut self, provider_id: &str) -> Result<Option<String>, RemoveError> {
        // 在克隆上操作，失败时保持 self 不被污染
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

        // main_model 指向该 provider 下的模型 → 迁移（仅考虑已配置的模型）
        if self
            .main_model
            .split_once('/')
            .is_some_and(|(pid, _)| pid == provider_id)
        {
            match self.available_models().into_iter().find(|m| !m.needs_setup) {
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

    /// 确保 provider 的 model 列表已写入配置（从预定义同步过来）
    pub fn ensure_provider_models(&mut self, provider_id: &str) {
        if let Some(entry) = self.providers.get_mut(provider_id) {
            if entry.models.is_some() {
                return;
            }
            entry.models = providers::predefined_models_table(provider_id);
        }
    }

    /// 将预定义 provider 添加到配置（如果还不存在）
    pub fn add_predefined_provider(&mut self, provider_id: &str, api_key: &str) {
        if self.providers.contains_key(provider_id) {
            self.ensure_provider_models(provider_id);
            return;
        }
        if let Some(models) = providers::predefined_models_table(provider_id) {
            self.providers.insert(
                provider_id.to_string(),
                ProviderEntry {
                    api_key: api_key.to_string(),
                    base_url: None,
                    models: Some(models),
                },
            );
        }
    }

    /// 存量配置迁移：将 models 表中 supports_vision 缺省（None）且命中预定义表
    /// 的条目补写预定义声明值。显式配置（含 false）与自定义 provider 不动。
    /// 返回是否有变更（调用方据此决定是否 save）。
    pub fn migrate_predefined_vision_flags(&mut self) -> bool {
        let mut changed = false;
        for (pid, entry) in self.providers.iter_mut() {
            let Some(models) = entry.models.as_mut() else {
                continue;
            };
            for (mid, custom) in models.iter_mut() {
                if custom.supports_vision.is_none()
                    && let Some(sv) = providers::predefined_vision_flag(pid, mid)
                {
                    custom.supports_vision = Some(sv);
                    changed = true;
                }
            }
        }
        changed
    }

    /// 返回当前所有已配置 provider 的列表（用于 UI 中选择目标 provider）
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

    /// 持久化保存到配置文件
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
    /// 模型是否支持视觉（图片）输入；不支持时附件在请求组装层降级为提示文本
    pub supports_vision: bool,
}

// ── 配置文件 I/O ─────────────────────────────────────────────

fn config_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().wrap_err("无法获取用户主目录")?;
    Ok(home.join(CONFIG_DIR_NAME))
}

fn config_file_path() -> Result<PathBuf> {
    Ok(config_dir()?.join(CONFIG_FILE_NAME))
}

/// 加载结果：配置就绪或需要初始化设置
pub enum LoadResult {
    Ready(Box<Config>),
    NeedsSetup,
}

/// 读取配置文件，判断是否需要初始化
pub fn load() -> Result<LoadResult> {
    let path = config_file_path()?;

    if !path.exists() {
        return Ok(LoadResult::NeedsSetup);
    }

    let content = std::fs::read_to_string(&path)
        .wrap_err_with(|| format!("无法读取配置文件: {}", path.display()))?;
    let mut config: Config = toml::from_str(&content)
        .wrap_err_with(|| format!("无法解析配置文件: {}", path.display()))?;

    if config.compact_threshold <= 0.0 || config.compact_threshold >= 1.0 {
        config.compact_threshold = default_compact_threshold();
    }

    let has_valid_provider = config.providers.values().any(|e| !e.api_key.is_empty());
    if !has_valid_provider {
        return Ok(LoadResult::NeedsSetup);
    }

    // 存量配置迁移：supports_vision 缺省且命中预定义表的条目物化声明值。
    // save 失败静默忽略——resolve 的回退链已保证运行时取值正确。
    if config.migrate_predefined_vision_flags() {
        let _ = toml_serde::save_config(&config);
    }

    if config.main_model.is_empty() {
        let available = config.available_models();
        if available.is_empty() {
            return Ok(LoadResult::NeedsSetup);
        }
        // 自动将第一个可用模型设为默认
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
