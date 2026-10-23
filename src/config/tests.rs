use super::toml_serde::config_to_toml;
use super::{
    Config, CustomModelEntry, DEFAULT_CONTEXT_WINDOW, DEFAULT_OUTPUT_TOKENS, ProviderEntry,
};

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
fn resolve_removed_predefined_model_gives_clear_error() {
    // 模拟老用户残留：自动同步时代写入、后被下架的智谱模型条目
    let mut cfg = Config::default();
    cfg.add_predefined_provider("zhipu-coding-plan", "sk-test");
    cfg.add_custom_model("zhipu-coding-plan", None, None, "GLM-5.1", 131072, 1000000);
    let err = cfg.resolve("zhipu-coding-plan/GLM-5.1").unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("已从预定义列表移除"), "实际报错: {msg}");
    assert!(msg.contains("/model"), "实际报错: {msg}");
}

#[test]
fn permission_mode_serialization() {
    // YOLO 持久化：mode = "yolo" 写入序列化结果
    let mut cfg = test_config();
    cfg.permission.mode = "yolo".to_string();
    let toml = config_to_toml(&cfg).unwrap();
    assert!(toml.contains("mode = \"yolo\""));

    // 关闭 YOLO：mode 清空则不写该字段（空 [permission] 表整体省略）
    cfg.permission.mode.clear();
    let toml = config_to_toml(&cfg).unwrap();
    assert!(!toml.contains("mode ="));
    assert!(!toml.contains("[permission]"));
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
    let mut models = std::collections::BTreeMap::new();
    models.insert(
        "custom-model".to_string(),
        CustomModelEntry {
            max_tokens: 65536,
            context_window: 131072,
            supports_vision: None,
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

// ── 视觉能力解析 ─────────────────────────────────────

#[test]
fn supports_vision_explicit_true_respected() {
    // 非视觉模型显式 true 生效
    let mut cfg = Config::default();
    cfg.add_predefined_provider("deepseek", "sk-test");
    cfg.add_custom_model("deepseek", None, None, "text-model", 4096, 8192);
    let entry = cfg.providers.get_mut("deepseek").unwrap();
    let custom = entry
        .models
        .as_mut()
        .unwrap()
        .get_mut("text-model")
        .unwrap();
    custom.supports_vision = Some(true);

    let resolved = cfg.resolve("deepseek/text-model").unwrap();
    assert!(resolved.supports_vision);
}

#[test]
fn supports_vision_explicit_false_respected() {
    // 预定义声明为 true 的模型（GLM-5.3-Flash），显式 false 覆盖声明
    let mut cfg = Config::default();
    cfg.add_predefined_provider("zhipu-coding-plan", "sk-test");
    let entry = cfg.providers.get_mut("zhipu-coding-plan").unwrap();
    let custom = entry
        .models
        .as_mut()
        .unwrap()
        .get_mut("GLM-5.3-Flash")
        .unwrap();
    assert_eq!(custom.supports_vision, Some(true));
    custom.supports_vision = Some(false);

    let resolved = cfg.resolve("zhipu-coding-plan/GLM-5.3-Flash").unwrap();
    assert!(!resolved.supports_vision);
}

#[test]
fn supports_vision_predefined_declaration_fallback() {
    // 存量场景：手写 config 中 GLM-5.3-Flash 条目 supports_vision 缺省（None），
    // resolve 回退到预定义声明 true
    let mut cfg = Config::default();
    cfg.providers.insert(
        "zhipu-coding-plan".to_string(),
        ProviderEntry {
            api_key: "sk-test".to_string(),
            base_url: None,
            models: Some(std::collections::BTreeMap::from([(
                "GLM-5.3-Flash".to_string(),
                CustomModelEntry {
                    max_tokens: 131072,
                    context_window: 1000000,
                    supports_vision: None,
                },
            )])),
        },
    );

    let resolved = cfg.resolve("zhipu-coding-plan/GLM-5.3-Flash").unwrap();
    assert!(resolved.supports_vision);
}

#[test]
fn supports_vision_unknown_model_defaults_false() {
    let cfg = test_config();
    // 未命中预定义表且未配置 → false
    let resolved = cfg.resolve("deepseek/custom-model").unwrap();
    assert!(!resolved.supports_vision);
    // 预定义声明 false → false
    let resolved = cfg.resolve("deepseek/deepseek-v4-flash").unwrap();
    assert!(!resolved.supports_vision);
}

#[test]
fn predefined_model_vision_flag_propagates() {
    // 预定义模型的 supports_vision 声明直通 resolve（GLM-5.3-Flash 支持图片输入）
    let mut cfg = Config::default();
    cfg.add_predefined_provider("zhipu-coding-plan", "sk-test");

    let resolved = cfg.resolve("zhipu-coding-plan/GLM-5.3-Flash").unwrap();
    assert!(resolved.supports_vision);
    let resolved = cfg.resolve("zhipu-coding-plan/GLM-5.3").unwrap();
    assert!(!resolved.supports_vision);
}

#[test]
fn migrate_writes_predefined_vision_flag() {
    // 模拟手写 config：缺省 None 且命中预定义表 → 补写声明值；
    // 显式 false 与自定义 provider 下的模型不动
    let mut cfg = Config::default();
    cfg.providers.insert(
        "zhipu-coding-plan".to_string(),
        ProviderEntry {
            api_key: "sk-test".to_string(),
            base_url: None,
            models: Some(std::collections::BTreeMap::from([
                (
                    "GLM-5.3".to_string(),
                    CustomModelEntry {
                        max_tokens: 131072,
                        context_window: 1000000,
                        supports_vision: None,
                    },
                ),
                (
                    "GLM-5.3-Flash".to_string(),
                    CustomModelEntry {
                        max_tokens: 131072,
                        context_window: 1000000,
                        supports_vision: Some(false),
                    },
                ),
            ])),
        },
    );
    cfg.add_custom_model("deepseek", None, None, "text-model", 4096, 8192);

    assert!(cfg.migrate_predefined_vision_flags());

    let models = cfg
        .providers
        .get("zhipu-coding-plan")
        .unwrap()
        .models
        .as_ref()
        .unwrap();
    // GLM-5.3（声明 false）：None → Some(false)
    assert_eq!(models.get("GLM-5.3").unwrap().supports_vision, Some(false));
    // 显式 false 未被改写
    assert_eq!(
        models.get("GLM-5.3-Flash").unwrap().supports_vision,
        Some(false)
    );
    // 自定义 provider 下的模型不命中预定义表，保持 None
    let custom = cfg
        .providers
        .get("deepseek")
        .unwrap()
        .models
        .as_ref()
        .unwrap()
        .get("text-model")
        .unwrap();
    assert_eq!(custom.supports_vision, None);

    // 幂等：再次迁移无变更
    assert!(!cfg.migrate_predefined_vision_flags());
}

#[test]
fn migrate_resolves_none_to_declared_true() {
    // GLM-5.3-Flash（声明 true）缺省 None → 迁移为 Some(true)
    let mut cfg = Config::default();
    cfg.providers.insert(
        "zhipu-coding-plan".to_string(),
        ProviderEntry {
            api_key: "sk-test".to_string(),
            base_url: None,
            models: Some(std::collections::BTreeMap::from([(
                "GLM-5.3-Flash".to_string(),
                CustomModelEntry {
                    max_tokens: 131072,
                    context_window: 1000000,
                    supports_vision: None,
                },
            )])),
        },
    );

    assert!(cfg.migrate_predefined_vision_flags());
    let models = cfg
        .providers
        .get("zhipu-coding-plan")
        .unwrap()
        .models
        .as_ref()
        .unwrap();
    assert_eq!(
        models.get("GLM-5.3-Flash").unwrap().supports_vision,
        Some(true)
    );
}

#[test]
fn migrate_no_change_when_all_explicit() {
    // 全部条目显式配置 → 无变更（不触发 save）
    let mut cfg = Config::default();
    cfg.add_predefined_provider("deepseek", "sk-test");
    assert!(!cfg.migrate_predefined_vision_flags());
}

// ── 重试配置 ─────────────────────────────────────────

#[test]
fn retry_defaults_applied() {
    let cfg = Config::default();
    let retry = cfg.retry_config();
    assert_eq!(retry.max_retries, 3);
    assert_eq!(retry.initial_delay, std::time::Duration::from_millis(1000));
    assert_eq!(retry.max_delay, std::time::Duration::from_millis(20_000));
    assert_eq!(retry.multiplier, 2.0);
}

#[test]
fn retry_section_parsed_and_converted() {
    let content = r#"
main_model = "deepseek/deepseek-v4-flash"

[providers.deepseek]
api_key = "sk-test"

[retry]
max_retries = 5
initial_delay_ms = 500
max_delay_ms = 30000
multiplier = 1.5
"#;
    let cfg: Config = toml::from_str(content).unwrap();
    let retry = cfg.retry_config();
    assert_eq!(retry.max_retries, 5);
    assert_eq!(retry.initial_delay, std::time::Duration::from_millis(500));
    assert_eq!(retry.max_delay, std::time::Duration::from_millis(30_000));
    assert_eq!(retry.multiplier, 1.5);
}

#[test]
fn retry_partial_section_uses_field_defaults() {
    // 只覆盖部分字段，其余取默认
    let content = r#"
[providers.deepseek]
api_key = "sk-test"

[retry]
max_retries = 0
"#;
    let cfg: Config = toml::from_str(content).unwrap();
    assert_eq!(cfg.retry.max_retries, 0);
    assert_eq!(cfg.retry.initial_delay_ms, 1000);
    // max_retries=0 = 禁用重试，转换后直接生效
    assert_eq!(cfg.retry_config().max_retries, 0);
}

#[test]
fn retry_default_detection_controls_save_section() {
    // is_default 决定 save_config 是否写入 [retry] 段
    // （save_config 直接写 ~/.hailux/config.toml，此处只测判定逻辑）
    let mut cfg = Config::default();
    assert!(cfg.retry.is_default(), "默认配置不应写入 [retry]");
    cfg.retry.max_retries = 7;
    assert!(!cfg.retry.is_default(), "非默认值应写入 [retry]");
}

// 引用默认常量，防止未使用告警（它们在 types/providers 中被引用）
#[test]
fn default_constants() {
    assert_eq!(DEFAULT_OUTPUT_TOKENS, 65536);
    assert_eq!(DEFAULT_CONTEXT_WINDOW, 131072);
}
