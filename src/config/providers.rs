// ── Predefined providers (endpoints only; model lists always come from /models) ──

pub(crate) struct ProviderDef {
    pub(crate) id: &'static str,
    pub(crate) name: &'static str,
    pub(crate) base_url: &'static str,
    /// Whether this is a DeepSeek/GLM native reasoning endpoint: true sends the private
    /// `thinking` switch; false (custom OpenAI-compatible providers) follows the standard
    /// OpenAI interface and only sends reasoning_effort (low/high/max passed through as
    /// is, all within the OpenAI value range).
    /// This is a protocol convention that /models cannot report, so it lives in code
    /// rather than in the config.
    pub(crate) native_reasoning: bool,
}

pub(crate) const PROVIDERS: &[ProviderDef] = &[
    ProviderDef {
        id: "deepseek",
        name: "DeepSeek",
        base_url: "https://api.deepseek.com",
        native_reasoning: true,
    },
    ProviderDef {
        id: "zhipu-coding-plan",
        name: "Zhipu AI Coding Plan",
        base_url: "https://open.bigmodel.cn/api/coding/paas/v4",
        native_reasoning: true,
    },
];

pub(crate) fn find_provider_def(id: &str) -> Option<&'static ProviderDef> {
    PROVIDERS.iter().find(|p| p.id == id)
}
