use async_openai::config::OpenAIConfig;
use color_eyre::{Result, eyre::Context, eyre::ContextCompat};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

const CONFIG_DIR_NAME: &str = ".hailux";
const CONFIG_FILE_NAME: &str = "config.toml";

// ── 预定义 Provider ──────────────────────────────────────────

pub(crate) struct ProviderDef {
    pub(crate) id: &'static str,
    pub(crate) name: &'static str,
    pub(crate) base_url: &'static str,
    pub(crate) models: &'static [ModelDef],
}

pub(crate) struct ModelDef {
    pub(crate) id: &'static str,
    pub(crate) name: &'static str,
    /// API 输出上限 (max_completion_tokens)
    pub(crate) max_tokens: u32,
    /// 上下文窗口大小（UI 进度显示用）
    pub(crate) context_window: u32,
}

pub(crate) const PROVIDERS: &[ProviderDef] = &[
    ProviderDef {
        id: "deepseek",
        name: "DeepSeek",
        base_url: "https://api.deepseek.com",
        models: &[
            ModelDef {
                id: "deepseek-v4-flash",
                name: "deepseek-v4-flash",
                max_tokens: 131072,
                context_window: 1000000,
            },
            ModelDef {
                id: "deepseek-v4-pro",
                name: "deepseek-v4-pro",
                max_tokens: 131072,
                context_window: 1000000,
            },
        ],
    },
    ProviderDef {
        id: "zhipu-coding-plan",
        name: "Zhipu AI Coding Plan",
        base_url: "https://open.bigmodel.cn/api/coding/paas/v4",
        models: &[
            ModelDef {
                id: "GLM-5.2",
                name: "GLM-5.2",
                max_tokens: 131072,
                context_window: 1000000,
            },
            ModelDef {
                id: "GLM-5.1",
                name: "GLM-5.1",
                max_tokens: 131072,
                context_window: 204800,
            },
        ],
    },
];

pub(crate) fn find_provider_def(id: &str) -> Option<&'static ProviderDef> {
    PROVIDERS.iter().find(|p| p.id == id)
}

// ── 运行时配置结构 ───────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PermissionConfig {
    /// "ask" (Normal) 或 "yolo" (Yolo)
    #[serde(default)]
    pub mode: String,
    /// bash 权限规则: pattern -> action ("allow"/"deny"/"ask")
    #[serde(default)]
    pub bash: BTreeMap<String, String>,
    /// read 权限规则
    #[serde(default)]
    pub read: BTreeMap<String, String>,
    /// edit 权限规则
    #[serde(default)]
    pub edit: BTreeMap<String, String>,
    /// write 权限规则
    #[serde(default)]
    pub write: BTreeMap<String, String>,
    /// mcp 权限规则
    #[serde(default)]
    pub mcp: BTreeMap<String, String>,
    /// external_directory 权限规则（默认询问；可配置放行特定外部目录）
    #[serde(default)]
    pub external_directory: BTreeMap<String, String>,
}

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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            main_model: String::new(),
            providers: BTreeMap::new(),
            compact_threshold: default_compact_threshold(),
            permission: PermissionConfig::default(),
        }
    }
}

fn default_compact_threshold() -> f32 {
    0.75
}

#[derive(Debug, Deserialize, Clone)]
pub struct ProviderEntry {
    pub api_key: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub models: Option<BTreeMap<String, CustomModelEntry>>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CustomModelEntry {
    /// API 输出上限
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// 上下文窗口大小
    #[serde(default = "default_context_window")]
    pub context_window: u32,
}

fn default_max_tokens() -> u32 {
    131072 // 默认 128K（写入配置）
}

fn default_context_window() -> u32 {
    131072 // 默认 128K 上下文窗口
}

// ── 可选模型条目（供 UI 使用）─────────────────────────────────

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ModelEntry {
    pub provider_id: String,
    pub provider_name: String,
    pub model_id: String,
    pub model_name: String,
    pub display: String,
    pub context_window: u32,
    pub needs_setup: bool,
}

// ── 核心方法 ─────────────────────────────────────────────────

impl Config {
    /// 获取 provider 的显示名称
    fn provider_display_name(&self, pid: &str) -> String {
        if let Some(def) = find_provider_def(pid) {
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
        find_provider_def(pid).map(|d| d.base_url.to_string())
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

            // 优先使用自定义模型
            if let Some(ref custom_models) = entry.models {
                for (mid, custom) in custom_models {
                    result.push(ModelEntry {
                        provider_id: pid.clone(),
                        provider_name: provider_name.clone(),
                        model_id: mid.clone(),
                        model_name: mid.clone(),
                        display: format!("{}/{}", pid, mid),
                        context_window: custom.context_window,
                        needs_setup: false,
                    });
                }
                if !custom_models.is_empty() {
                    continue;
                }
            }

            // 回退到预定义模型
            if let Some(def) = find_provider_def(pid) {
                for m in def.models {
                    result.push(ModelEntry {
                        provider_id: pid.clone(),
                        provider_name: provider_name.clone(),
                        model_id: m.id.to_string(),
                        model_name: m.name.to_string(),
                        display: format!("{}/{}", pid, m.id),
                        context_window: m.context_window,
                        needs_setup: false,
                    });
                }
            }
        }

        // 未配置的预定义 provider
        for def in PROVIDERS {
            if !self.providers.contains_key(def.id) {
                for m in def.models {
                    result.push(ModelEntry {
                        provider_id: def.id.to_string(),
                        provider_name: def.name.to_string(),
                        model_id: m.id.to_string(),
                        model_name: m.name.to_string(),
                        display: format!("{}/{}", def.id, m.id),
                        context_window: m.context_window,
                        needs_setup: true,
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
        let (max_tokens, context_window) = if let Some(ref custom_models) = entry.models {
            if let Some(custom) = custom_models.get(model_id) {
                (custom.max_tokens, custom.context_window)
            } else {
                fallback_model_values(provider_id, model_id)
            }
        } else {
            fallback_model_values(provider_id, model_id)
        };

        let config = OpenAIConfig::new()
            .with_api_key(&entry.api_key)
            .with_api_base(&base_url);

        Ok(ResolvedModel {
            config,
            model_id: model_id.to_string(),
            max_tokens,
            context_window,
            display: format!("{}/{}", provider_id, model_id),
        })
    }

    /// 解析默认模型
    pub fn resolve_default(&self) -> Result<ResolvedModel> {
        if self.main_model.is_empty() {
            return Err(color_eyre::eyre::eyre!("未配置模型，请通过设置添加"));
        }
        self.resolve(&self.main_model)
    }
}

/// 从预定义模型中查找值作为回退
fn fallback_model_values(provider_id: &str, model_id: &str) -> (u32, u32) {
    find_provider_def(provider_id)
        .and_then(|d| d.models.iter().find(|m| m.id == model_id))
        .map(|m| (m.max_tokens, m.context_window))
        .unwrap_or((default_max_tokens(), default_context_window()))
}

/// 从预定义 provider 构造模型表
fn predefined_models_table(provider_id: &str) -> Option<BTreeMap<String, CustomModelEntry>> {
    find_provider_def(provider_id).map(|def| {
        def.models
            .iter()
            .map(|m| {
                (
                    m.id.to_string(),
                    CustomModelEntry {
                        max_tokens: m.max_tokens,
                        context_window: m.context_window,
                    },
                )
            })
            .collect()
    })
}

impl Config {
    /// 添加自定义模型到已有 provider，返回新模型的 display 字符串
    /// 如果 provider 不存在会自动创建（需同时提供 base_url 和 api_key）
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
        models.insert(
            model_id.to_string(),
            CustomModelEntry {
                max_tokens,
                context_window,
            },
        );

        format!("{}/{}", provider_id, model_id)
    }

    /// 确保 provider 的 model 列表已写入配置（从预定义同步过来）
    pub fn ensure_provider_models(&mut self, provider_id: &str) {
        if let Some(entry) = self.providers.get_mut(provider_id) {
            if entry.models.is_some() {
                return;
            }
            entry.models = predefined_models_table(provider_id);
        }
    }

    /// 将预定义 provider 添加到配置（如果还不存在）
    pub fn add_predefined_provider(&mut self, provider_id: &str, api_key: &str) {
        if self.providers.contains_key(provider_id) {
            self.ensure_provider_models(provider_id);
            return;
        }
        if let Some(models) = predefined_models_table(provider_id) {
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
        save_config(self)
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

pub fn save_config(config: &Config) -> Result<()> {
    let dir = config_dir()?;
    if !dir.exists() {
        std::fs::create_dir_all(&dir)
            .wrap_err_with(|| format!("无法创建配置目录: {}", dir.display()))?;
    }

    let mut providers_toml = toml::map::Map::new();
    for (pid, entry) in &config.providers {
        let mut table = toml::map::Map::new();
        table.insert("api_key".into(), toml::Value::String(entry.api_key.clone()));
        if let Some(ref url) = entry.base_url {
            table.insert("base_url".into(), toml::Value::String(url.clone()));
        }
        if let Some(ref models) = entry.models {
            let mut models_table = toml::map::Map::new();
            for (mid, custom) in models {
                let mut m = toml::map::Map::new();
                m.insert(
                    "max_tokens".into(),
                    toml::Value::Integer(custom.max_tokens as i64),
                );
                m.insert(
                    "context_window".into(),
                    toml::Value::Integer(custom.context_window as i64),
                );
                models_table.insert(mid.clone(), toml::Value::Table(m));
            }
            table.insert("models".into(), toml::Value::Table(models_table));
        }
        providers_toml.insert(pid.clone(), toml::Value::Table(table));
    }

    let mut root = toml::map::Map::new();
    root.insert(
        "main_model".into(),
        toml::Value::String(config.main_model.clone()),
    );
    root.insert("providers".into(), toml::Value::Table(providers_toml));
    root.insert(
        "compact_threshold".into(),
        toml::Value::Float(config.compact_threshold as f64),
    );

    // 权限配置
    let mut perm_table = toml::map::Map::new();
    if !config.permission.mode.is_empty() {
        perm_table.insert(
            "mode".into(),
            toml::Value::String(config.permission.mode.clone()),
        );
    }
    for (key, table) in [
        ("bash", &config.permission.bash),
        ("read", &config.permission.read),
        ("edit", &config.permission.edit),
        ("write", &config.permission.write),
        ("mcp", &config.permission.mcp),
        ("external_directory", &config.permission.external_directory),
    ] {
        if !table.is_empty() {
            let mut t = toml::map::Map::new();
            for (k, v) in table {
                t.insert(k.clone(), toml::Value::String(v.clone()));
            }
            perm_table.insert(key.into(), toml::Value::Table(t));
        }
    }
    if !perm_table.is_empty() {
        root.insert("permission".into(), toml::Value::Table(perm_table));
    }

    let path = config_file_path()?;
    let toml_str = toml::to_string_pretty(&toml::Value::Table(root)).wrap_err("无法序列化配置")?;
    // 原子写入：先写临时文件，再重命名
    let tmp_path = path.with_extension("toml.tmp");
    std::fs::write(&tmp_path, &toml_str)
        .wrap_err_with(|| format!("无法写入配置文件: {}", path.display()))?;
    std::fs::rename(&tmp_path, &path)
        .wrap_err_with(|| format!("无法重命名配置文件: {}", path.display()))?;

    Ok(())
}
