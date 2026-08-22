use async_openai::config::OpenAIConfig;
use color_eyre::{Result, eyre::Context, eyre::ContextCompat};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

const CONFIG_DIR_NAME: &str = ".hailux";
const CONFIG_FILE_NAME: &str = "config.toml";

/// 模型输出上限（max_completion_tokens）
pub const DEFAULT_OUTPUT_TOKENS: u32 = 65536;
/// 模型上下文窗口大小
pub const DEFAULT_CONTEXT_WINDOW: u32 = 131072;

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
                id: "GLM-5.3",
                name: "GLM-5.3",
                max_tokens: 131072,
                context_window: 1000000,
            },
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
    DEFAULT_CONTEXT_WINDOW
}

// ── 可选模型条目（供 UI 使用）─────────────────────────────────

#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub provider_id: String,
    pub provider_name: String,
    pub model_id: String,
    pub model_name: String,
    pub display: String,
    pub needs_setup: bool,
    /// 可删除（用户自定义添加的模型；预定义 provider 的预定义模型不可删，会被自动合并回来）
    pub deletable: bool,
}

/// 删除自定义模型/provider 的失败原因（Web 层据此映射 HTTP 状态码）
#[derive(Debug)]
pub enum RemoveError {
    /// selector 格式错误，应为 provider/model
    InvalidSelector(String),
    /// 预定义模型/provider 不可删除
    Predefined(String),
    /// 未找到 provider 或模型
    NotFound(String),
    /// 删除后没有任何可用模型，拒绝删除
    NoModelsLeft(String),
}

impl std::fmt::Display for RemoveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RemoveError::InvalidSelector(msg)
            | RemoveError::Predefined(msg)
            | RemoveError::NotFound(msg)
            | RemoveError::NoModelsLeft(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for RemoveError {}

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

            // 自定义模型优先
            if let Some(ref custom_models) = entry.models {
                for mid in custom_models.keys() {
                    // 与预定义模型同 id 的条目来自 setup 同步，删除后会被预定义合并分支复活 → 不可删
                    let deletable = find_provider_def(pid)
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
            if let Some(def) = find_provider_def(pid) {
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
        for def in PROVIDERS {
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

        let is_predefined_model = find_provider_def(provider_id)
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
            if find_provider_def(provider_id).is_some() {
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
        if find_provider_def(provider_id).is_some() {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造测试配置：deepseek（预定义，含一个自定义模型）+ ollama（自定义 provider，两个模型）
    fn test_config() -> Config {
        let mut cfg = Config::default();
        cfg.add_predefined_provider("deepseek", "sk-test");
        // deepseek 下挂一个自定义模型
        cfg.add_custom_model("deepseek", None, None, "custom-model", 65536, 131072);
        // 自定义 provider
        cfg.add_custom_model(
            "ollama",
            Some("http://localhost:11434/v1"),
            Some("sk-ollama"),
            "qwen3",
            65536,
            131072,
        );
        cfg.add_custom_model("ollama", None, None, "llama3", 65536, 131072);
        cfg
    }

    #[test]
    fn remove_custom_model_deletes_entry() {
        let mut cfg = test_config();
        // main_model 指向别的模型 → 不迁移
        cfg.main_model = "ollama/qwen3".to_string();
        assert_eq!(cfg.remove_custom_model("ollama/llama3").unwrap(), None);
        let displays: Vec<String> = cfg
            .available_models()
            .into_iter()
            .map(|m| m.display)
            .collect();
        assert!(!displays.contains(&"ollama/llama3".to_string()));
        assert!(displays.contains(&"ollama/qwen3".to_string()));
    }

    #[test]
    fn remove_custom_model_migrates_main_model() {
        let mut cfg = test_config();
        cfg.main_model = "ollama/qwen3".to_string();
        let migrated = cfg.remove_custom_model("ollama/qwen3").unwrap();
        // deepseek 的自定义模型排序在前（BTreeMap 字母序，自定义模型优先列出）
        assert_eq!(migrated.as_deref(), Some("deepseek/custom-model"));
        assert_eq!(cfg.main_model, "deepseek/custom-model");
        assert!(cfg.resolve_default().is_ok());
    }

    #[test]
    fn remove_rejects_predefined_model() {
        let mut cfg = test_config();
        cfg.main_model = "ollama/qwen3".to_string();
        // 未写入 models 表的预定义模型（合并分支）
        assert!(
            cfg.remove_custom_model("deepseek/deepseek-v4-flash")
                .is_err()
        );
        // setup 同步写入 models 表的预定义模型同样不可删
        cfg.ensure_provider_models("deepseek");
        assert!(
            cfg.remove_custom_model("deepseek/deepseek-v4-flash")
                .is_err()
        );
        let displays: Vec<String> = cfg
            .available_models()
            .into_iter()
            .map(|m| m.display)
            .collect();
        assert!(displays.contains(&"deepseek/deepseek-v4-flash".to_string()));
    }

    #[test]
    fn remove_rejects_unknown_model() {
        let mut cfg = test_config();
        cfg.main_model = "ollama/qwen3".to_string();
        assert!(cfg.remove_custom_model("ollama/nonexistent").is_err());
        assert!(cfg.remove_custom_model("nobody/model").is_err());
        assert!(cfg.remove_custom_model("bad-format").is_err());
    }

    #[test]
    fn remove_last_model_of_custom_provider_removes_provider() {
        let mut cfg = test_config();
        cfg.main_model = "deepseek/custom-model".to_string();
        assert!(cfg.remove_custom_model("ollama/qwen3").is_ok());
        assert!(cfg.remove_custom_model("ollama/llama3").is_ok());
        // provider 整条移除
        assert!(!cfg.providers.contains_key("ollama"));
    }

    #[test]
    fn remove_last_custom_model_of_predefined_provider_keeps_provider() {
        // 手工构造：预定义 provider 的 models 表仅含一个自定义模型（hand-edited 配置场景）
        let mut cfg = Config::default();
        let mut models = BTreeMap::new();
        models.insert(
            "custom-model".to_string(),
            CustomModelEntry {
                max_tokens: 65536,
                context_window: 131072,
            },
        );
        cfg.providers.insert(
            "deepseek".to_string(),
            ProviderEntry {
                api_key: "sk".to_string(),
                base_url: None,
                models: Some(models),
            },
        );
        cfg.main_model = "deepseek/custom-model".to_string();
        let migrated = cfg.remove_custom_model("deepseek/custom-model").unwrap();
        // provider 保留，models 重置为 None（回归合并预定义）；main_model 迁移到预定义模型
        assert_eq!(migrated.as_deref(), Some("deepseek/deepseek-v4-flash"));
        let entry = cfg.providers.get("deepseek").unwrap();
        assert!(entry.models.is_none());
        // 预定义模型仍列出可用
        let displays: Vec<String> = cfg
            .available_models()
            .into_iter()
            .map(|m| m.display)
            .collect();
        assert!(displays.contains(&"deepseek/deepseek-v4-flash".to_string()));
    }

    #[test]
    fn remove_last_available_model_rejected() {
        // 仅剩一个自定义模型时删除 → 拒绝
        let mut cfg = Config::default();
        cfg.add_custom_model(
            "ollama",
            Some("http://localhost:11434/v1"),
            Some("sk"),
            "only-model",
            65536,
            131072,
        );
        cfg.main_model = "ollama/only-model".to_string();
        assert!(cfg.remove_custom_model("ollama/only-model").is_err());
        // 配置未被污染：模型仍在
        let displays: Vec<String> = cfg
            .available_models()
            .into_iter()
            .map(|m| m.display)
            .collect();
        assert!(displays.contains(&"ollama/only-model".to_string()));
        assert!(cfg.resolve_default().is_ok());
    }

    #[test]
    fn remove_provider_removes_all_models() {
        let mut cfg = test_config();
        cfg.main_model = "ollama/qwen3".to_string();
        // 迁移到 deepseek 的第一个可用模型（自定义模型排序在前）
        let migrated = cfg.remove_provider("ollama").unwrap();
        assert_eq!(migrated.as_deref(), Some("deepseek/custom-model"));
        assert!(!cfg.providers.contains_key("ollama"));
        let displays: Vec<String> = cfg
            .available_models()
            .into_iter()
            .map(|m| m.display)
            .collect();
        assert!(!displays.iter().any(|d| d.starts_with("ollama/")));
        assert!(cfg.resolve_default().is_ok());
    }

    #[test]
    fn remove_provider_rejects_predefined_and_unknown() {
        let mut cfg = test_config();
        cfg.main_model = "ollama/qwen3".to_string();
        assert!(cfg.remove_provider("deepseek").is_err());
        assert!(cfg.remove_provider("nobody").is_err());
        assert!(cfg.providers.contains_key("deepseek"));
    }

    #[test]
    fn available_models_marks_deletable() {
        let cfg = test_config();
        let entries = cfg.available_models();
        // 自定义模型可删
        assert!(
            entries
                .iter()
                .any(|m| m.display == "ollama/qwen3" && m.deletable)
        );
        // setup 同步进 models 表的预定义模型不可删（deletable=false，来自第一分支）
        let ds = entries
            .iter()
            .find(|m| m.display == "deepseek/deepseek-v4-flash")
            .unwrap();
        assert!(!ds.deletable);
        // 深度验证：ensure 后仍在表中但 deletable=false
        let mut cfg2 = test_config();
        cfg2.ensure_provider_models("deepseek");
        let ds2 = cfg2
            .available_models()
            .into_iter()
            .find(|m| m.display == "deepseek/deepseek-v4-flash")
            .unwrap();
        assert!(!ds2.deletable);
    }
}
