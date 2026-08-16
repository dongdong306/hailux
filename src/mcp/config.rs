use color_eyre::{Result, eyre::Context, eyre::ContextCompat};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const CONFIG_DIR_NAME: &str = ".hailux";
const MCP_CONFIG_FILE_NAME: &str = "mcp.toml";

/// 单个 MCP 服务器的配置。通过 untagged enum 区分传输方式：
/// - 包含 `command` 字段 → stdio 本地子进程
/// - 包含 `url` 字段 → 远程 streamable-http / SSE
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ServerConfig {
    Stdio {
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
    },
    Http {
        url: String,
        /// 附加到每个 HTTP 请求的自定义 header（可用于鉴权，如 Authorization / X-API-Key）
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
    },
}

impl ServerConfig {
    pub fn transport_label(&self) -> &'static str {
        match self {
            ServerConfig::Stdio { .. } => "stdio",
            ServerConfig::Http { .. } => "http",
        }
    }
}

/// `~/.hailux/mcp.toml` 的根结构。
///
/// ```toml
/// [mcp_servers.context7]
/// command = "npx"
/// args = ["-y", "@upstash/context7-mcp"]
/// env = { API_KEY = "..." }
///
/// [mcp_servers.remote]
/// url = "https://example.com/mcp"
/// ```
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    #[serde(default)]
    pub mcp_servers: BTreeMap<String, ServerConfig>,
}

fn mcp_config_file_path() -> Result<PathBuf> {
    let home = dirs::home_dir().wrap_err("无法获取用户主目录")?;
    Ok(home.join(CONFIG_DIR_NAME).join(MCP_CONFIG_FILE_NAME))
}

/// 首次运行时写入的示例模板（全部注释，取消注释即可启用对应服务器）。
const SAMPLE_TEMPLATE: &str = r#"# ──────────────────────────────────────────────────────────────
# hailux MCP 服务器配置
# ──────────────────────────────────────────────────────────────
# 取消注释下面的任意一段即可启用对应 MCP 服务器。
# 保存后重启 hailux 生效；用 /mcp 命令可查看各服务器连接状态。
# 成功连接的工具会自动注册给 agent，LLM 可直接调用。
#
# 两种传输方式：
#   1) stdio —— 本地子进程（最常见），提供 command / args / env
#   2) http  —— 远程 streamable-http / SSE，提供 url
# ──────────────────────────────────────────────────────────────

# ── 示例 1：stdio 本地子进程 ──────────────────────────────────
# 通过 npx 运行 filesystem 服务器，暴露指定目录的文件操作工具
# [mcp_servers.filesystem]
# command = "npx"
# args = ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/allowed/dir"]

# ── 示例 2：带环境变量的 stdio 服务器 ────────────────────────
# Context7 文档检索（需要 API Key）
# [mcp_servers.context7]
# command = "npx"
# args = ["-y", "@upstash/context7-mcp"]
# env = { API_KEY = "your-api-key-here" }

# ── 示例 3：Python 实现的 stdio 服务器 ───────────────────────
# 通过 uvx 运行
# [mcp_servers.git]
# command = "uvx"
# args = ["mcp-server-git", "--repository", "."]

# ── 示例 4：远程 HTTP / SSE 服务器（无鉴权）──────────────────
# 直接提供 URL，无需本地子进程
# [mcp_servers.remote]
# url = "https://example.com/mcp"

# ── 示例 5：需要 header 鉴权的远程服务器 ─────────────────────
# 通过 headers 传递鉴权信息（支持任意 header）：
#   - Bearer Token : Authorization = "Bearer <token>"
#   - Basic Auth   : Authorization = "Basic <base64>"
#   - API Key      : "X-API-Key" = "<key>"
# [mcp_servers.secure]
# url = "https://example.com/mcp"
# headers = { Authorization = "Bearer your-token-here" }

# ──────────────────────────────────────────────────────────────
# 字段说明：
#   command : 可执行程序（需在 PATH 中，如 npx / uvx / node / python）
#   args    : 传给程序的参数列表
#   env     : 注入子进程的环境变量（可选）
#   url     : 远程 MCP 端点（与 command 二选一）
#   headers : 远程服务器的自定义 HTTP header（可选，常用于鉴权）
# ──────────────────────────────────────────────────────────────
"#;

/// 读取 MCP 配置文件。
///
/// 文件不存在时，自动在配置目录生成示例模板（全部注释），便于用户编辑；
/// 生成失败不阻断启动，仍返回空配置。
pub fn load() -> Result<McpConfig> {
    let path = mcp_config_file_path()?;
    if !path.exists() {
        create_sample_file(&path);
        return Ok(McpConfig::default());
    }
    let content = std::fs::read_to_string(&path)
        .wrap_err_with(|| format!("无法读取 MCP 配置文件: {}", path.display()))?;
    let config: McpConfig = toml::from_str(&content)
        .wrap_err_with(|| format!("无法解析 MCP 配置文件: {}", path.display()))?;
    Ok(config)
}

/// 将配置写回 `~/.hailux/mcp.toml`（全量序列化，文件内注释不保留）。
/// 必要时创建父目录。
pub fn save(config: &McpConfig) -> Result<()> {
    let path = mcp_config_file_path()?;
    if let Some(parent) = path.parent()
        && !parent.exists()
    {
        std::fs::create_dir_all(parent)
            .wrap_err_with(|| format!("无法创建 MCP 配置目录: {}", parent.display()))?;
    }
    let content = toml::to_string_pretty(config).wrap_err("序列化 MCP 配置失败")?;
    std::fs::write(&path, content)
        .wrap_err_with(|| format!("无法写入 MCP 配置文件: {}", path.display()))?;
    Ok(())
}

/// 将示例模板写入指定路径，必要时创建父目录。失败时打印警告但不阻断启动。
fn create_sample_file(path: &Path) {
    if let Some(parent) = path.parent()
        && !parent.exists()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        eprintln!(
            "[hailux] 警告：无法创建 MCP 配置目录 {}: {}",
            parent.display(),
            e
        );
        return;
    }
    if let Err(e) = std::fs::write(path, SAMPLE_TEMPLATE) {
        eprintln!(
            "[hailux] 警告：无法写入示例 MCP 配置文件 {}: {}",
            path.display(),
            e
        );
    }
}

/// 返回配置文件路径的字符串表示（供 UI 提示使用）。
pub fn config_path_display() -> String {
    match mcp_config_file_path() {
        Ok(p) => p.display().to_string(),
        Err(_) => "~/.hailux/mcp.toml".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stdio_server() {
        let toml_str = r#"
[mcp_servers.context7]
command = "npx"
args = ["-y", "@upstash/context7-mcp"]
"#;
        let config: McpConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.mcp_servers.len(), 1);
        match &config.mcp_servers["context7"] {
            ServerConfig::Stdio { command, args, env } => {
                assert_eq!(command, "npx");
                assert_eq!(args, &["-y", "@upstash/context7-mcp"]);
                assert!(env.is_empty());
            }
            ServerConfig::Http { .. } => panic!("应为 stdio"),
        }
    }

    #[test]
    fn parses_http_server() {
        let toml_str = r#"
[mcp_servers.remote]
url = "https://example.com/mcp"
"#;
        let config: McpConfig = toml::from_str(toml_str).unwrap();
        match &config.mcp_servers["remote"] {
            ServerConfig::Stdio { .. } => panic!("应为 http"),
            ServerConfig::Http { url, headers } => {
                assert_eq!(url, "https://example.com/mcp");
                assert!(headers.is_empty());
            }
        }
    }

    #[test]
    fn parses_http_server_with_headers() {
        let toml_str = r#"
[mcp_servers.remote]
url = "https://example.com/mcp"
headers = { Authorization = "Bearer token123", "X-API-Key" = "abc" }
"#;
        let config: McpConfig = toml::from_str(toml_str).unwrap();
        match &config.mcp_servers["remote"] {
            ServerConfig::Http { url, headers } => {
                assert_eq!(url, "https://example.com/mcp");
                assert_eq!(headers.get("Authorization").unwrap(), "Bearer token123");
                assert_eq!(headers.get("X-API-Key").unwrap(), "abc");
            }
            ServerConfig::Stdio { .. } => panic!("应为 http"),
        }
    }

    #[test]
    fn parses_mixed_servers() {
        let toml_str = r#"
[mcp_servers.local]
command = "node"
args = ["server.js"]
env = { TOKEN = "abc" }

[mcp_servers.remote]
url = "https://example.com/mcp"
"#;
        let config: McpConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.mcp_servers.len(), 2);
    }

    #[test]
    fn empty_config_is_default() {
        let config: McpConfig = toml::from_str("").unwrap();
        assert!(config.mcp_servers.is_empty());
    }

    #[test]
    fn serializes_roundtrip() {
        let mut config = McpConfig::default();
        config.mcp_servers.insert(
            "context7".to_string(),
            ServerConfig::Stdio {
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "@upstash/context7-mcp".to_string()],
                env: BTreeMap::new(),
            },
        );
        config.mcp_servers.insert(
            "remote".to_string(),
            ServerConfig::Http {
                url: "https://example.com/mcp".to_string(),
                headers: BTreeMap::new(),
            },
        );
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: McpConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.mcp_servers.len(), 2);
        assert!(matches!(
            &parsed.mcp_servers["context7"],
            ServerConfig::Stdio { command, args, .. }
            if command == "npx" && args.len() == 2
        ));
        assert!(matches!(
            &parsed.mcp_servers["remote"],
            ServerConfig::Http { url, .. } if url == "https://example.com/mcp"
        ));
    }
}
