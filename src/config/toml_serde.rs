// ── 配置文件 I/O ─────────────────────────────────────────────

use color_eyre::{Result, eyre::Context};

use super::{Config, config_dir, config_file_path};

/// 将配置序列化为 TOML 字符串（不落盘，供 save_config 与单测复用）
pub(super) fn config_to_toml(config: &Config) -> Result<String> {
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
                if let Some(vision) = custom.supports_vision {
                    m.insert("supports_vision".into(), toml::Value::Boolean(vision));
                }
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

    // 重试策略：与默认值一致时不写入，保持配置文件最小化
    if !config.retry.is_default() {
        let mut retry_table = toml::map::Map::new();
        retry_table.insert(
            "max_retries".into(),
            toml::Value::Integer(config.retry.max_retries as i64),
        );
        retry_table.insert(
            "initial_delay_ms".into(),
            toml::Value::Integer(config.retry.initial_delay_ms as i64),
        );
        retry_table.insert(
            "max_delay_ms".into(),
            toml::Value::Integer(config.retry.max_delay_ms as i64),
        );
        retry_table.insert(
            "multiplier".into(),
            toml::Value::Float(config.retry.multiplier),
        );
        root.insert("retry".into(), toml::Value::Table(retry_table));
    }

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

    toml::to_string_pretty(&toml::Value::Table(root)).wrap_err("无法序列化配置")
}

pub fn save_config(config: &Config) -> Result<()> {
    let dir = config_dir()?;
    if !dir.exists() {
        std::fs::create_dir_all(&dir)
            .wrap_err_with(|| format!("无法创建配置目录: {}", dir.display()))?;
    }

    let toml_str = config_to_toml(config)?;
    let path = config_file_path()?;
    // 原子写入：先写临时文件，再重命名
    let tmp_path = path.with_extension("toml.tmp");
    std::fs::write(&tmp_path, &toml_str)
        .wrap_err_with(|| format!("无法写入配置文件: {}", path.display()))?;
    std::fs::rename(&tmp_path, &path)
        .wrap_err_with(|| format!("无法重命名配置文件: {}", path.display()))?;

    Ok(())
}
