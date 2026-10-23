// ── 预定义 Provider ──────────────────────────────────────────

use std::collections::BTreeMap;

use super::types::{CustomModelEntry, default_context_window, default_max_tokens};

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
    /// 预定义模型是否支持视觉（图片）输入
    pub(crate) supports_vision: bool,
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
                supports_vision: false,
            },
            ModelDef {
                id: "deepseek-v4-flash-vision-exp",
                name: "deepseek-v4-flash-vision-exp",
                max_tokens: 131072,
                context_window: 1000000,
                supports_vision: true,
            },
            ModelDef {
                id: "deepseek-v4-pro",
                name: "deepseek-v4-pro",
                max_tokens: 131072,
                context_window: 1000000,
                supports_vision: false,
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
                supports_vision: false,
            },
            ModelDef {
                id: "GLM-5.3-Flash",
                name: "GLM-5.3-Flash",
                max_tokens: 131072,
                context_window: 1000000,
                supports_vision: true,
            },
        ],
    },
];

pub(crate) fn find_provider_def(id: &str) -> Option<&'static ProviderDef> {
    PROVIDERS.iter().find(|p| p.id == id)
}

/// 曾在预定义列表中、后被下架的模型。老用户 config.toml 里自动同步时代写入的
/// 残留条目会让 resolve 静默命中（ensure_provider_models 只增不删），直到请求时
/// 才收到 provider 侧「模型不存在」。这里显式拦截，给出重新选择的升级指引。
pub(super) const REMOVED_PREDEFINED_MODELS: &[(&str, &str)] = &[
    ("zhipu-coding-plan", "GLM-5.1"),
    ("zhipu-coding-plan", "GLM-5.2"),
];

/// 从预定义模型中查找值作为回退。
/// 预定义模型返回其硬编码声明 `Some(supports_vision)`，非预定义模型返回 `None`。
pub(super) fn fallback_model_values(provider_id: &str, model_id: &str) -> (u32, u32, Option<bool>) {
    match find_provider_def(provider_id).and_then(|d| d.models.iter().find(|m| m.id == model_id)) {
        Some(m) => (m.max_tokens, m.context_window, Some(m.supports_vision)),
        None => (default_max_tokens(), default_context_window(), None),
    }
}

/// 查询预定义模型的 supports_vision 声明（仅用于 supports_vision 缺省时的回退）。
pub(super) fn predefined_vision_flag(provider_id: &str, model_id: &str) -> Option<bool> {
    find_provider_def(provider_id)?
        .models
        .iter()
        .find(|m| m.id == model_id)
        .map(|m| m.supports_vision)
}

/// 从预定义 provider 构造模型表
pub(super) fn predefined_models_table(
    provider_id: &str,
) -> Option<BTreeMap<String, CustomModelEntry>> {
    find_provider_def(provider_id).map(|def| {
        def.models
            .iter()
            .map(|m| {
                (
                    m.id.to_string(),
                    CustomModelEntry {
                        max_tokens: m.max_tokens,
                        context_window: m.context_window,
                        supports_vision: Some(m.supports_vision),
                    },
                )
            })
            .collect()
    })
}
