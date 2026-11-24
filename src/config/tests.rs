use super::toml_serde::config_to_toml;
use super::{Config, CustomModelEntry, DEFAULT_CONTEXT_WINDOW, DEFAULT_OUTPUT_TOKENS};

/// Build a test config: deepseek (predefined provider, endpoint from the built-in table, two
/// models in the table) + ollama (custom provider, two models)
fn test_config() -> Config {
    let mut cfg = Config::default();
    cfg.add_custom_model(
        "deepseek",
        None,
        Some("sk-test"),
        "deepseek-flash",
        393216,
        1048576,
    );
    cfg.add_custom_model("deepseek", None, None, "custom-model", 65536, 131072);
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

// ── Predefined providers: endpoints only ─────────────

#[test]
fn predefined_provider_supplies_base_url_and_native_reasoning() {
    let cfg = test_config();
    // No base_url in the entry: the built-in endpoint applies
    assert!(cfg.providers["deepseek"].base_url.is_none());
    assert_eq!(
        cfg.provider_base_url("deepseek").as_deref(),
        Some("https://api.deepseek.com")
    );
    assert!(
        cfg.resolve("deepseek/deepseek-flash")
            .unwrap()
            .native_reasoning
    );
    // Custom providers need their own endpoint and use the standard OpenAI API
    assert!(!cfg.resolve("ollama/qwen3").unwrap().native_reasoning);
    // Unconfigured built-in providers are not listed as models (their list needs a key)
    let displays: Vec<String> = cfg
        .available_models()
        .into_iter()
        .map(|m| m.display)
        .collect();
    assert!(!displays.iter().any(|d| d.starts_with("zhipu-coding-plan/")));
}

#[test]
fn empty_models_table_roundtrips_without_a_toml_section() {
    // A freshly enabled provider has no models yet: nothing is written and parsing gives
    // the empty table back
    let mut cfg = Config::default();
    cfg.update_provider("deepseek", None, Some("sk-test"), None)
        .unwrap();
    let toml = config_to_toml(&cfg).unwrap();
    assert!(!toml.contains("models"), "toml:\n{toml}");
    let parsed: Config = toml::from_str(&toml).unwrap();
    assert!(parsed.providers["deepseek"].models.is_empty());
    assert_eq!(parsed.providers["deepseek"].api_key, "sk-test");
}

// ── Serialization ──────────────────────────────────

#[test]
fn permission_mode_serialization() {
    let mut cfg = test_config();
    cfg.permission.mode = "yolo".to_string();
    let toml = config_to_toml(&cfg).unwrap();
    assert!(toml.contains("mode = \"yolo\""));

    // YOLO off: an empty mode omits the field (the whole empty [permission] table is omitted)
    cfg.permission.mode.clear();
    let toml = config_to_toml(&cfg).unwrap();
    assert!(!toml.contains("mode ="));
    assert!(!toml.contains("[permission]"));
}

// ── Deleting models / providers ─────────────────────

#[test]
fn remove_custom_model_deletes_entry() {
    let mut cfg = test_config();
    // main_model points at another model -> no migration
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
    // deepseek's custom-model sorts first (BTreeMap alphabetical order)
    assert_eq!(migrated.as_deref(), Some("deepseek/custom-model"));
    assert_eq!(cfg.main_model, "deepseek/custom-model");
    assert!(cfg.resolve_default().is_ok());
}

#[test]
fn remove_rejects_unknown_model() {
    let mut cfg = test_config();
    cfg.main_model = "ollama/qwen3".to_string();
    assert!(cfg.remove_custom_model("ollama/nonexistent").is_err());
    assert!(cfg.remove_custom_model("nobody/model").is_err());
    assert!(cfg.remove_custom_model("bad-format").is_err());
    // A model known only from the /models cache has no table row to delete
    cfg.set_discovered_models("ollama", vec![remote_model("vendor-distilled")]);
    assert!(cfg.remove_custom_model("ollama/vendor-distilled").is_err());
}

#[test]
fn remove_last_model_of_custom_provider_removes_provider() {
    let mut cfg = test_config();
    cfg.main_model = "deepseek/custom-model".to_string();
    assert!(cfg.remove_custom_model("ollama/qwen3").is_ok());
    assert!(cfg.remove_custom_model("ollama/llama3").is_ok());
    assert!(!cfg.providers.contains_key("ollama"));
}

#[test]
fn remove_last_model_of_predefined_provider_keeps_provider() {
    let mut cfg = Config::default();
    cfg.add_custom_model("deepseek", None, Some("sk"), "only-model", 65536, 131072);
    cfg.add_custom_model(
        "ollama",
        Some("http://localhost:11434/v1"),
        Some("sk-ollama"),
        "qwen3",
        65536,
        131072,
    );
    cfg.main_model = "deepseek/only-model".to_string();
    let migrated = cfg.remove_custom_model("deepseek/only-model").unwrap();
    assert_eq!(migrated.as_deref(), Some("ollama/qwen3"));
    // Credentials kept, table empty: the provider stays available for the add-model flow
    let entry = cfg.providers.get("deepseek").unwrap();
    assert_eq!(entry.api_key, "sk");
    assert!(entry.models.is_empty());
    assert!(
        cfg.configured_providers()
            .iter()
            .any(|p| p.id == "deepseek")
    );
}

#[test]
fn remove_last_available_model_rejected() {
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
    // Config untouched: the model is still there
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
    // Migrates to deepseek's first available model (custom-model sorts first)
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
fn available_models_lists_only_table_rows() {
    let mut cfg = test_config();
    cfg.set_discovered_models(
        "deepseek",
        vec![
            remote_model("deepseek-flash"),
            remote_model("remote-only-v5"),
        ],
    );
    // The list is the table: table rows are present, cache-only models are not
    let entries = cfg.available_models();
    for display in ["ollama/qwen3", "deepseek/deepseek-flash"] {
        assert!(
            entries.iter().any(|m| m.display == display),
            "missing {display}"
        );
    }
    assert!(
        !entries
            .iter()
            .any(|m| m.display == "deepseek/remote-only-v5")
    );
}

// ── Adding models ───────────────────────────────────

#[test]
fn add_model_entry_creates_provider_and_carries_metadata() {
    let mut cfg = Config::default();
    let mut remote = remote_model("GLM-5.3");
    remote.context_window = Some(1_000_000);
    remote.max_output_tokens = Some(131_072);
    remote.supports_vision = Some(true);
    let display = cfg.add_model_entry(
        "zhipu-coding-plan",
        None,
        Some("sk-test"),
        &remote.id,
        remote.to_model_entry(),
    );
    assert_eq!(display, "zhipu-coding-plan/GLM-5.3");
    let entry = &cfg.providers["zhipu-coding-plan"];
    assert_eq!(entry.api_key, "sk-test");
    assert!(entry.base_url.is_none());
    let model = &entry.models["GLM-5.3"];
    assert_eq!(model.context_window, 1_000_000);
    let resolved = cfg.resolve(&display).unwrap();
    assert_eq!(resolved.context_window, 1_000_000);
    assert!(resolved.supports_vision);
    assert!(resolved.native_reasoning);
}

#[test]
fn remote_model_to_model_entry_defaults_missing_metadata() {
    // Providers reporting ids only (GLM) land on the defaults
    let entry = remote_model("glm-4.5").to_model_entry();
    assert_eq!(entry.max_tokens, 131072);
    assert_eq!(entry.context_window, DEFAULT_CONTEXT_WINDOW);
    assert_eq!(entry.supports_vision, None);
}

#[test]
fn add_custom_model_re_adding_keeps_vision_flag() {
    let mut cfg = Config::default();
    cfg.add_custom_model(
        "ollama",
        Some("http://localhost:11434/v1"),
        Some("sk"),
        "qwen3",
        32768,
        131072,
    );
    let model = &cfg.providers["ollama"].models["qwen3"];
    assert_eq!(model.supports_vision, None);
    // Re-adding keeps an existing supports_vision flag but resets the numbers
    let entry = CustomModelEntry {
        max_tokens: 4096,
        context_window: 8192,
        supports_vision: Some(true),
    };
    cfg.add_model_entry("ollama", None, None, "qwen3", entry);
    cfg.add_custom_model("ollama", None, None, "qwen3", 65536, 262144);
    let model = &cfg.providers["ollama"].models["qwen3"];
    assert_eq!(model.supports_vision, Some(true));
    assert_eq!(model.context_window, 262144);
}

// ── apply_discovered_models (the fetch dialog's confirm) ──

#[test]
fn apply_discovered_models_adds_picked_rows_and_skips_existing() {
    let mut cfg = test_config();
    let mut reported = remote_model("deepseek-v5");
    reported.context_window = Some(1_000_000);
    reported.max_output_tokens = Some(64_000);
    let plain = remote_model("id-only");
    cfg.set_discovered_models("deepseek", vec![reported, plain]);

    // deepseek-flash is already configured: skipped and untouched
    let added = cfg
        .apply_discovered_models(
            "deepseek",
            &[
                "deepseek-flash".to_string(),
                "deepseek-v5".to_string(),
                "id-only".to_string(),
            ],
        )
        .unwrap();
    assert_eq!(
        added,
        vec!["deepseek-v5".to_string(), "id-only".to_string()]
    );

    // Picked rows carry the provider-reported metadata
    let v5 = &cfg.providers["deepseek"].models["deepseek-v5"];
    assert_eq!(v5.context_window, 1_000_000);
    assert_eq!(v5.max_tokens, 64_000);
    // Ids missing from the cache land on the defaults
    let fallback = &cfg.providers["deepseek"].models["id-only"];
    assert_eq!(fallback.context_window, DEFAULT_CONTEXT_WINDOW);
    assert_eq!(fallback.max_tokens, 131072);
    // The already-configured row keeps its own values
    let flash = &cfg.providers["deepseek"].models["deepseek-flash"];
    assert_eq!(flash.context_window, 1_048_576);
    // The rows are usable and listed
    assert!(cfg.resolve("deepseek/deepseek-v5").is_ok());
    assert!(
        cfg.available_models()
            .iter()
            .any(|m| m.display == "deepseek/id-only")
    );
}

#[test]
fn apply_discovered_models_requires_configured_provider_and_cache() {
    let mut cfg = Config::default();
    // Unknown provider
    assert!(
        cfg.apply_discovered_models("ollama", &["m".to_string()])
            .is_err()
    );
    // Provider without a key
    cfg.update_provider("deepseek", None, Some("sk"), None)
        .unwrap();
    cfg.providers.get_mut("deepseek").unwrap().api_key.clear();
    assert!(
        cfg.apply_discovered_models("deepseek", &["m".to_string()])
            .is_err()
    );
    // No cached list yet
    cfg.providers.get_mut("deepseek").unwrap().api_key = "sk".into();
    assert!(
        cfg.apply_discovered_models("deepseek", &["m".to_string()])
            .is_err()
    );
}

// ── Vision capability resolution ─────────────────────

#[test]
fn supports_vision_explicit_true_respected() {
    let mut cfg = Config::default();
    cfg.add_custom_model("deepseek", None, Some("sk-test"), "text-model", 4096, 8192);
    let entry = cfg.providers.get_mut("deepseek").unwrap();
    entry.models.get_mut("text-model").unwrap().supports_vision = Some(true);

    let resolved = cfg.resolve("deepseek/text-model").unwrap();
    assert!(resolved.supports_vision);
}

#[test]
fn supports_vision_defaults_false_and_editable() {
    let mut cfg = Config::default();
    cfg.add_custom_model("deepseek", None, Some("sk-test"), "deepseek-v5", 4096, 8192);
    // Unset vision flag resolves to false
    assert!(!cfg.resolve("deepseek/deepseek-v5").unwrap().supports_vision);

    cfg.update_model("deepseek/deepseek-v5", None, None, None, Some(false))
        .unwrap();
    assert!(!cfg.resolve("deepseek/deepseek-v5").unwrap().supports_vision);
    cfg.update_model("deepseek/deepseek-v5", None, None, None, Some(true))
        .unwrap();
    assert!(cfg.resolve("deepseek/deepseek-v5").unwrap().supports_vision);
}

#[test]
fn supports_vision_unknown_model_defaults_false() {
    let cfg = test_config();
    let resolved = cfg.resolve("deepseek/custom-model").unwrap();
    assert!(!resolved.supports_vision);
    let resolved = cfg.resolve("deepseek/deepseek-flash").unwrap();
    assert!(!resolved.supports_vision);
}

// ── Retry config ────────────────────────────────────

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
main_model = "deepseek/deepseek-flash"

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
    // Only some fields overridden, the rest defaulted
    let content = r#"
[providers.deepseek]
api_key = "sk-test"

[retry]
max_retries = 0
"#;
    let cfg: Config = toml::from_str(content).unwrap();
    assert_eq!(cfg.retry.max_retries, 0);
    assert_eq!(cfg.retry.initial_delay_ms, 1000);
    assert_eq!(cfg.retry_config().max_retries, 0);
}

#[test]
fn retry_default_detection_controls_save_section() {
    // is_default decides whether save_config writes the [retry] section
    // (save_config writes ~/.hailux/config.toml directly; only the predicate is tested here)
    let mut cfg = Config::default();
    assert!(
        cfg.retry.is_default(),
        "default config must not write the [retry] section"
    );
    cfg.retry.max_retries = 7;
    assert!(
        !cfg.retry.is_default(),
        "non-default values must write the [retry] section"
    );
}

// ── Session guardrails ──────────────────────────────

#[test]
fn session_defaults_applied() {
    let content = r#"
[providers.deepseek]
api_key = "sk-test"
"#;
    let cfg: Config = toml::from_str(content).unwrap();
    assert_eq!(cfg.session.max_concurrent_runs, 4);
}

#[test]
fn session_section_parsed() {
    let content = r#"
[providers.deepseek]
api_key = "sk-test"

[session]
max_concurrent_runs = 8
"#;
    let cfg: Config = toml::from_str(content).unwrap();
    assert_eq!(cfg.session.max_concurrent_runs, 8);
}

#[test]
fn session_default_detection_controls_save_section() {
    let mut cfg = Config::default();
    assert!(
        cfg.session.is_default(),
        "default config must not write the [session] section"
    );
    let toml = config_to_toml(&cfg).unwrap();
    assert!(!toml.contains("[session]"));

    cfg.session.max_concurrent_runs = 0; // 0 = unlimited, still non-default → written
    assert!(!cfg.session.is_default());
    let toml = config_to_toml(&cfg).unwrap();
    assert!(toml.contains("[session]"));
    assert!(toml.contains("max_concurrent_runs = 0"));
}

// ── Reasoning effort ────────────────────────────────

#[test]
fn native_reasoning_predefined_true_custom_false() {
    let cfg = test_config();
    let resolved = cfg.resolve("deepseek/deepseek-flash").unwrap();
    assert!(resolved.native_reasoning);
    let resolved = cfg.resolve("ollama/qwen3").unwrap();
    assert!(!resolved.native_reasoning);
}

#[test]
fn reasoning_effort_default_and_toml_minimization() {
    let cfg = test_config();
    assert_eq!(cfg.reasoning_effort, crate::agent::ReasoningEffort::High);
    let toml = config_to_toml(&cfg).unwrap();
    assert!(!toml.contains("reasoning_effort"));

    let mut cfg = test_config();
    cfg.reasoning_effort = crate::agent::ReasoningEffort::Max;
    let toml = config_to_toml(&cfg).unwrap();
    assert!(toml.contains("reasoning_effort = \"max\""));
}

#[test]
fn reasoning_effort_toml_roundtrip() {
    // Root keys must precede any table header
    let content = r#"
reasoning_effort = "low"

[providers.deepseek]
api_key = "sk-test"
"#;
    let cfg: Config = toml::from_str(content).unwrap();
    assert_eq!(cfg.reasoning_effort, crate::agent::ReasoningEffort::Low);

    let cfg: Config = toml::from_str(
        r#"[providers.deepseek]
api_key = "sk-test""#,
    )
    .unwrap();
    assert_eq!(cfg.reasoning_effort, crate::agent::ReasoningEffort::High);
}

// Reference the default constants to avoid unused warnings (they are used in types/providers)
#[test]
fn default_constants() {
    assert_eq!(DEFAULT_OUTPUT_TOKENS, 65536);
    assert_eq!(DEFAULT_CONTEXT_WINDOW, 262144);
}

// ── Editing existing models / providers ─────────────

#[test]
fn update_model_overrides_effective_values() {
    let mut cfg = test_config();
    cfg.main_model = "ollama/qwen3".to_string();

    // Window only: the other fields keep their effective values
    let selector = cfg
        .update_model("ollama/qwen3", None, Some(200_000), None, None)
        .unwrap();
    assert_eq!(selector, "ollama/qwen3");
    let resolved = cfg.resolve(&selector).unwrap();
    assert_eq!(resolved.context_window, 200_000);
    assert_eq!(resolved.max_tokens, 65536);

    cfg.update_model("ollama/qwen3", None, None, None, Some(true))
        .unwrap();
    assert!(cfg.resolve("ollama/qwen3").unwrap().supports_vision);
    cfg.update_model("ollama/qwen3", None, None, None, Some(false))
        .unwrap();
    assert!(!cfg.resolve("ollama/qwen3").unwrap().supports_vision);
}

#[test]
fn update_model_rename_migrates_main_model() {
    let mut cfg = test_config();
    cfg.main_model = "ollama/llama3".to_string();
    let selector = cfg
        .update_model("ollama/llama3", Some("llama3-70b"), None, None, None)
        .unwrap();
    assert_eq!(selector, "ollama/llama3-70b");
    assert_eq!(cfg.main_model, "ollama/llama3-70b");
    let displays: Vec<String> = cfg
        .available_models()
        .into_iter()
        .map(|m| m.display)
        .collect();
    assert!(displays.contains(&"ollama/llama3-70b".to_string()));
    assert!(!displays.contains(&"ollama/llama3".to_string()));
}

#[test]
fn update_model_rejects_invalid_edits() {
    let mut cfg = test_config();
    // Renaming onto an existing sibling
    assert!(
        cfg.update_model("ollama/qwen3", Some("llama3"), None, None, None)
            .is_err()
    );
    assert!(
        cfg.update_model("ollama/qwen3", None, Some(0), None, None)
            .is_err()
    );
    assert!(
        cfg.update_model("ollama/nope", None, Some(1), None, None)
            .is_err()
    );
    // A rejected edit must not mutate the config
    assert_eq!(cfg.resolve("ollama/qwen3").unwrap().context_window, 131072);
}

#[test]
fn model_ids_match_case_insensitively_with_one_canonical_row() {
    let mut cfg = test_config();

    // Lookup paths (resolve / remove / update) hit the table's canonical key
    let resolved = cfg.resolve("ollama/QWEN3").unwrap();
    assert_eq!(resolved.model_id, "qwen3");
    assert_eq!(resolved.display, "ollama/qwen3");
    assert_eq!(
        cfg.remove_custom_model("ollama/QWEN3").unwrap(),
        None,
        "removing via a differently-cased selector still deletes the row"
    );
    assert!(cfg.resolve("ollama/qwen3").is_err());

    // Re-adding with a different casing replaces the row instead of duplicating it
    cfg.add_custom_model("ollama", None, None, "Qwen3", 8192, 16384);
    let rows: Vec<&String> = cfg.providers["ollama"].models.keys().collect();
    assert_eq!(rows, [&"Qwen3".to_string(), &"llama3".to_string()]);
    assert!(!cfg.providers["ollama"].models.contains_key("qwen3"));
    assert_eq!(cfg.resolve("ollama/qwen3").unwrap().max_tokens, 8192);

    // A pure-casing rename is allowed (it targets one row) and migrates main_model
    cfg.main_model = "ollama/llama3".to_string();
    let selector = cfg
        .update_model("ollama/LLAMA3", Some("Llama3"), None, None, None)
        .unwrap();
    assert_eq!(selector, "ollama/Llama3");
    assert_eq!(cfg.main_model, "ollama/Llama3");
    assert!(cfg.resolve("ollama/llama3").is_ok());
}

#[test]
fn remove_last_custom_model_drops_discovered_cache() {
    let mut cfg = test_config();
    cfg.set_discovered_models("ollama", vec![remote_model("vendor-distilled")]);
    cfg.main_model = "deepseek/deepseek-flash".to_string();

    // Removing ollama's last model removes the whole custom provider; the discovery
    // entry must not linger in the cache (same contract as remove_provider)
    cfg.remove_custom_model("ollama/qwen3").unwrap();
    cfg.remove_custom_model("ollama/llama3").unwrap();
    assert!(!cfg.providers.contains_key("ollama"));
    assert!(!cfg.discovered.contains_key("ollama"));

    // Predefined providers keep credentials and their discovery cache on deletion
    cfg.set_discovered_models("deepseek", vec![remote_model("deepseek-x")]);
    cfg.remove_custom_model("deepseek/deepseek-flash").unwrap();
    assert!(cfg.providers.contains_key("deepseek"));
    assert_eq!(cfg.providers["deepseek"].api_key, "sk-test");
    assert!(cfg.discovered.contains_key("deepseek"));
}

#[test]
fn update_provider_credentials_and_rename() {
    let mut cfg = test_config();
    cfg.main_model = "ollama/qwen3".to_string();

    let id = cfg
        .update_provider("ollama", None, None, Some("http://localhost:1234/v1"))
        .unwrap();
    assert_eq!(id, "ollama");
    assert_eq!(
        cfg.providers["ollama"].base_url.as_deref(),
        Some("http://localhost:1234/v1")
    );

    // Rename: the main_model prefix follows
    let id = cfg
        .update_provider("ollama", Some("local"), None, None)
        .unwrap();
    assert_eq!(id, "local");
    assert!(!cfg.providers.contains_key("ollama"));
    assert_eq!(cfg.main_model, "local/qwen3");
    assert!(cfg.resolve("local/qwen3").is_ok());
}

#[test]
fn update_provider_predefined_endpoint_reset() {
    let mut cfg = Config::default();
    cfg.add_custom_model(
        "deepseek",
        None,
        Some("sk-test"),
        "deepseek-flash",
        393216,
        1048576,
    );
    cfg.update_provider("deepseek", None, None, Some("https://proxy.local/v1"))
        .unwrap();
    assert_eq!(
        cfg.provider_base_url("deepseek").as_deref(),
        Some("https://proxy.local/v1")
    );
    // Empty string clears the override → built-in endpoint comes back
    cfg.update_provider("deepseek", None, None, Some(""))
        .unwrap();
    assert!(cfg.providers["deepseek"].base_url.is_none());
    assert_eq!(
        cfg.provider_base_url("deepseek").as_deref(),
        Some("https://api.deepseek.com")
    );
}

#[test]
fn update_provider_rejects_invalid_edits() {
    let mut cfg = test_config();
    assert!(
        cfg.update_provider("deepseek", Some("ds"), None, None)
            .is_err()
    );
    // Clearing the endpoint of a custom provider
    assert!(
        cfg.update_provider("ollama", None, None, Some(" "))
            .is_err()
    );
    // Empty key / non-http endpoint
    assert!(cfg.update_provider("ollama", None, Some(""), None).is_err());
    assert!(
        cfg.update_provider("ollama", None, None, Some("localhost:11434"))
            .is_err()
    );
    assert!(cfg.update_provider("nope", None, None, None).is_err());
}

#[test]
fn update_provider_enables_unconfigured_predefined() {
    // An unconfigured predefined provider has no config entry: supplying a key enables it
    // (empty model table, endpoint from the built-in table)
    let mut cfg = Config::default();
    assert!(!cfg.providers.contains_key("deepseek"));
    assert!(cfg.update_provider("deepseek", None, None, None).is_err());
    let id = cfg
        .update_provider("deepseek", None, Some("sk-test"), None)
        .unwrap();
    assert_eq!(id, "deepseek");
    assert!(cfg.providers["deepseek"].models.is_empty());
    assert!(
        cfg.configured_providers()
            .iter()
            .any(|p| p.id == "deepseek")
    );
    // An unconfigured custom provider still counts as missing
    assert!(
        cfg.update_provider("ollama", None, Some("sk-x"), None)
            .is_err()
    );
}

// ── Provider model discovery ───────────────────────

use super::model_discovery::RemoteModel;

fn remote_model(id: &str) -> RemoteModel {
    RemoteModel {
        id: id.to_string(),
        name: None,
        context_window: None,
        max_output_tokens: None,
        supports_vision: None,
    }
}

#[test]
fn parse_models_response_reads_optional_metadata() {
    let body = r#"{"object":"list","data":[
        {"id":"deepseek-flash","name":"DeepSeek-V4.1-Flash","context_window":1048576,
         "max_output_tokens":393216,"input_modalities":["text","image"]},
        {"id":"glm-5.3","object":"model","owned_by":"z-ai"}
    ]}"#;
    let models = super::model_discovery::parse_models_response(body).unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "deepseek-flash");
    assert_eq!(models[0].name.as_deref(), Some("DeepSeek-V4.1-Flash"));
    assert_eq!(models[0].context_window, Some(1_048_576));
    assert_eq!(models[0].max_output_tokens, Some(393_216));
    assert_eq!(models[0].supports_vision, Some(true));
    // Providers that only report ids leave the metadata empty
    assert_eq!(models[1].context_window, None);
    assert_eq!(models[1].max_output_tokens, None);
    assert_eq!(models[1].supports_vision, None);
}

#[test]
fn resolve_uses_only_the_configured_row() {
    // Cache-only models cannot be resolved and never show up in the list
    let mut cfg = Config::default();
    cfg.add_custom_model("deepseek", None, Some("sk-test"), "other", 4096, 8192);
    let mut discovered = remote_model("deepseek-v4-pro");
    discovered.context_window = Some(2_000_000);
    discovered.max_output_tokens = Some(64_000);
    discovered.supports_vision = Some(true);
    cfg.set_discovered_models("deepseek", vec![discovered]);

    assert!(cfg.resolve("deepseek/deepseek-v4-pro").is_err());
    assert!(
        !cfg.available_models()
            .iter()
            .any(|m| m.display == "deepseek/deepseek-v4-pro")
    );

    // Picking it materializes the reported metadata, which resolve then serves
    cfg.apply_discovered_models("deepseek", &["deepseek-v4-pro".to_string()])
        .unwrap();
    let resolved = cfg.resolve("deepseek/deepseek-v4-pro").unwrap();
    assert_eq!(resolved.context_window, 2_000_000);
    assert_eq!(resolved.max_tokens, 64_000);
    assert!(resolved.supports_vision);
}

#[test]
fn resolve_unknown_model_is_rejected() {
    // A selector that was never picked cannot be used (no silent defaults)
    let cfg = test_config();
    assert!(cfg.resolve("deepseek/never-seen").is_err());
}

// ── Discovery cache bookkeeping ─────────────────────

#[test]
fn provider_rename_keeps_discovered_cache_reachable() {
    // The discovery cache is keyed by provider id; a rename must re-key it or the
    // fetch dialog silently loses the cached list
    let mut cfg = test_config();
    let mut remote_only = remote_model("vendor-distilled");
    remote_only.context_window = Some(200_000);
    cfg.set_discovered_models("ollama", vec![remote_only]);

    cfg.update_provider("ollama", Some("local"), None, None)
        .unwrap();
    assert!(cfg.remote_model("local", "vendor-distilled").is_some());
    assert!(cfg.remote_model("ollama", "vendor-distilled").is_none());
}

#[test]
fn provider_removal_drops_discovered_cache() {
    let mut cfg = test_config();
    cfg.set_discovered_models("ollama", vec![remote_model("vendor-distilled")]);
    cfg.main_model = "deepseek/custom-model".to_string();

    cfg.remove_provider("ollama").unwrap();
    assert!(!cfg.discovered.contains_key("ollama"));
    let displays: Vec<String> = cfg
        .available_models()
        .into_iter()
        .map(|m| m.display)
        .collect();
    assert!(!displays.contains(&"ollama/vendor-distilled".to_string()));
}

#[test]
fn legacy_config_with_missing_models_table_parses() {
    // Older configs could carry a provider without a models table (None); it reads as empty
    let content = r#"
main_model = "deepseek/deepseek-flash"

[providers.deepseek]
api_key = "sk-test"

[providers.deepseek.models.deepseek-flash]
max_tokens = 393216
context_window = 1048576
supports_vision = true

[providers.ollama]
api_key = "sk-ollama"
base_url = "http://localhost:11434/v1"
"#;
    let cfg: Config = toml::from_str(content).unwrap();
    assert_eq!(cfg.providers["deepseek"].models.len(), 1);
    assert!(cfg.providers["ollama"].models.is_empty());
    assert_eq!(
        cfg.providers["deepseek"].models["deepseek-flash"].supports_vision,
        Some(true)
    );
}

#[test]
fn parse_models_response_accepts_vendor_field_aliases() {
    let body = r#"{"data":[
        {"id":"qwen3","max_model_len":262144,"max_tokens":32768,"capabilities":{"vision":true}},
        {"id":"llama4","context_length":131072,"supports_vision":false},
        {"id":"plain"}
    ]}"#;
    let models = super::model_discovery::parse_models_response(body).unwrap();
    assert_eq!(models[0].context_window, Some(262_144));
    assert_eq!(models[0].max_output_tokens, Some(32_768));
    assert_eq!(models[0].supports_vision, Some(true));
    assert_eq!(models[1].context_window, Some(131_072));
    assert_eq!(models[1].supports_vision, Some(false));
    // Nothing reported stays unknown so the type defaults apply
    assert_eq!(models[2].context_window, None);
    assert_eq!(models[2].supports_vision, None);
}
