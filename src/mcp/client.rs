use super::config::{McpConfig, ServerConfig};
use crate::agent::{Tool, ToolExecuteError};
use http::{HeaderName, HeaderValue};
use rmcp::{
    RoleClient, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResult, ClientCapabilities, ClientInfo, ContentBlock,
        Implementation, Tool as McpToolDef,
    },
    service::RunningService,
    transport::streamable_http_client::StreamableHttpClientTransportConfig,
    transport::{StreamableHttpClientTransport, TokioChildProcess},
};
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;

/// Connection handle type for a single MCP server (both stdio and http use
/// a `ClientInfo` handler, so the type is unified).
pub type McpClient = RunningService<RoleClient, ClientInfo>;

/// Status snapshot after an MCP server connects successfully (for UI
/// rendering; excludes the connection handle).
#[derive(Debug, Clone)]
pub struct McpServerStatus {
    /// Server name (key) from the config file
    pub name: String,
    pub connected: bool,
    /// Self-reported server name
    pub server_name: String,
    pub server_version: String,
    /// "stdio" or "http"
    pub transport: String,
    pub error: Option<String>,
    /// List of tool names
    pub tools: Vec<String>,
    pub resource_count: usize,
    pub prompt_count: usize,
    /// Resource details (URI + description), for the detail page
    pub resources: Vec<(String, String)>,
    /// Prompt details (name + description), for the detail page
    pub prompts: Vec<(String, String)>,
}

/// Connection result for a single server: status plus, on success, the
/// connection handle and tool definitions.
#[derive(Debug)]
pub struct McpConnection {
    pub status: McpServerStatus,
    pub backend: Option<Arc<McpClient>>,
    pub tools: Vec<McpToolDef>,
}

/// MCP tool backend: server name + connection handle + tool definition list,
/// shared with subagents for MCP tool use.
#[derive(Debug, Clone)]
pub struct McpToolBackend {
    pub server_name: String,
    pub backend: Arc<McpClient>,
    pub tools: Vec<McpToolDef>,
}

/// Shared list of MCP backends, wrapped in Arc<Mutex> for sharing between
/// the main agent and subagents.
pub type SharedMcpBackends = Arc<std::sync::Mutex<Vec<McpToolBackend>>>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// Builds initial placeholder statuses from the config (all marked as
/// "connecting...") so the UI can render immediately without waiting for
/// actual connections.
pub fn create_placeholder_statuses(config: &McpConfig) -> Vec<McpServerStatus> {
    config
        .mcp_servers
        .iter()
        .map(|(name, server_config)| {
            let transport = server_config.transport_label().to_string();
            McpServerStatus {
                name: name.clone(),
                connected: false,
                server_name: String::new(),
                server_version: String::new(),
                transport,
                error: Some("连接中...".to_string()),
                tools: Vec::new(),
                resource_count: 0,
                prompt_count: 0,
                resources: Vec::new(),
                prompts: Vec::new(),
            }
        })
        .collect()
}

/// Iterates over all MCP servers in the config and attempts to connect
/// concurrently (with timeout and fault tolerance). One failure does not
/// affect other servers; the error is recorded in `status.error`.
pub async fn connect_mcp_servers(config: &McpConfig) -> Vec<McpConnection> {
    use futures_util::future::join_all;

    let futs: Vec<_> = config
        .mcp_servers
        .iter()
        .map(|(name, server_config)| {
            let name = name.clone();
            let transport = server_config.transport_label().to_string();
            async move {
                match connect_and_gather(server_config).await {
                    Ok((client, tools, resources, prompts, peer_name, peer_version)) => {
                        let resource_count = resources.len();
                        let prompt_count = prompts.len();
                        let status = McpServerStatus {
                            name,
                            connected: true,
                            server_name: peer_name,
                            server_version: peer_version,
                            transport,
                            error: None,
                            tools: tools.iter().map(|t| t.name.to_string()).collect(),
                            resource_count,
                            prompt_count,
                            resources,
                            prompts,
                        };
                        McpConnection {
                            status,
                            backend: Some(Arc::new(client)),
                            tools,
                        }
                    }
                    Err(e) => {
                        let status = McpServerStatus {
                            name,
                            connected: false,
                            server_name: String::new(),
                            server_version: String::new(),
                            transport,
                            error: Some(e),
                            tools: Vec::new(),
                            resource_count: 0,
                            prompt_count: 0,
                            resources: Vec::new(),
                            prompts: Vec::new(),
                        };
                        McpConnection {
                            status,
                            backend: None,
                            tools: Vec::new(),
                        }
                    }
                }
            }
        })
        .collect();

    join_all(futs).await
}

/// Connects to a single server and collects tools/resources/prompts plus
/// server info.
async fn connect_and_gather(
    config: &ServerConfig,
) -> Result<
    (
        McpClient,
        Vec<McpToolDef>,
        Vec<(String, String)>,
        Vec<(String, String)>,
        String,
        String,
    ),
    String,
> {
    let connect_fut = async {
        let client = connect_one(config).await?;

        let (peer_name, peer_version) = match client.peer_info() {
            Some(info) => (
                info.server_info.name.clone(),
                info.server_info.version.clone(),
            ),
            None => (String::new(), String::new()),
        };

        let tools = client
            .list_all_tools()
            .await
            .map_err(|e| format!("列出工具失败: {e}"))?;

        let resources = client
            .list_all_resources()
            .await
            .map(|r| {
                r.into_iter()
                    .map(|res| {
                        let desc = res
                            .description
                            .clone()
                            .or(res.title.clone())
                            .unwrap_or_default();
                        (res.name.clone(), desc)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let prompts = client
            .list_all_prompts()
            .await
            .map(|p| {
                p.into_iter()
                    .map(|pr| {
                        let desc = pr
                            .description
                            .clone()
                            .or(pr.title.clone())
                            .unwrap_or_default();
                        (pr.name.clone(), desc)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        Ok::<_, String>((client, tools, resources, prompts, peer_name, peer_version))
    };

    tokio::time::timeout(CONNECT_TIMEOUT, connect_fut)
        .await
        .map_err(|_| format!("连接超时（超过 {} 秒）", CONNECT_TIMEOUT.as_secs()))?
}

/// Establishes a connection based on the transport type and completes the
/// MCP initialization handshake.
async fn connect_one(config: &ServerConfig) -> Result<McpClient, String> {
    let client_info = build_client_info();
    match config {
        ServerConfig::Stdio { command, args, env } => {
            let resolved = resolve_program(command);
            let mut cmd = Command::new(&resolved);
            cmd.kill_on_drop(true);
            cmd.args(args);
            for (k, v) in env {
                cmd.env(k, v);
            }
            let (transport, stderr_opt) = TokioChildProcess::builder(cmd)
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| format!("启动子进程失败: {e}"))?;
            // Drain stderr in the background so a full pipe buffer cannot
            // block the child process
            if let Some(stderr) = stderr_opt {
                tokio::spawn(async move {
                    use tokio::io::AsyncReadExt;
                    let mut reader = tokio::io::BufReader::new(stderr);
                    let mut buf = Vec::new();
                    let _ = reader.read_to_end(&mut buf).await;
                });
            }
            client_info
                .serve(transport)
                .await
                .map_err(|e| format!("连接失败: {e}"))
        }
        ServerConfig::Http { url, headers } => {
            let mut config = StreamableHttpClientTransportConfig::with_uri(url.as_str());
            if !headers.is_empty() {
                let map = build_header_map(headers)?;
                config = config.custom_headers(map);
            }
            let transport = StreamableHttpClientTransport::from_config(config);
            client_info
                .serve(transport)
                .await
                .map_err(|e| format!("连接失败: {e}"))
        }
    }
}

/// Resolves the path of an executable program.
///
/// On Windows, a bare command (e.g. `npx`) maps to a `.cmd`/`.bat` script,
/// while `CreateProcess` does not append extensions automatically, leading
/// to "program not found". This manually walks `PATH` + `PATHEXT` to locate
/// the real file path; once resolved to a `.cmd`/`.bat`, Rust std recognizes
/// it and runs it via `cmd.exe`.
///
/// **Security**: searches only `PATH`, never the current directory, to
/// prevent binary planting attacks.
#[cfg(windows)]
fn resolve_program(command: &str) -> String {
    let p = std::path::Path::new(command);
    if p.extension().is_some() {
        return command.to_string();
    }
    let (Some(pathext), Some(path)) = (std::env::var_os("PATHEXT"), std::env::var_os("PATH"))
    else {
        return command.to_string();
    };
    for ext in std::env::split_paths(&pathext) {
        let ext_str = ext.to_string_lossy();
        let with_ext = format!("{command}{ext_str}");
        // Search only PATH, never the current directory, to prevent binary
        // planting
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(&with_ext);
            if candidate.is_file() {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }
    command.to_string()
}

#[cfg(not(windows))]
fn resolve_program(command: &str) -> String {
    if std::path::Path::new(command).is_absolute() {
        return command.to_string();
    }
    // Search only PATH, never the current directory, to prevent binary
    // planting
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(command);
            if candidate.is_file() || candidate.is_symlink() {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }
    command.to_string()
}

fn build_client_info() -> ClientInfo {
    let implementation =
        Implementation::new("hailux", env!("CARGO_PKG_VERSION")).with_title("hailux");
    ClientInfo::new(ClientCapabilities::default(), implementation)
}

/// Converts the config's header name/value map into http types.
/// On failure (invalid header name or value containing illegal bytes),
/// returns a readable error.
fn build_header_map(
    headers: &std::collections::BTreeMap<String, String>,
) -> Result<HashMap<HeaderName, HeaderValue>, String> {
    let mut map = HashMap::new();
    for (name, value) in headers {
        let n = HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| format!("无效的 header 名 '{name}': {e}"))?;
        let v =
            HeaderValue::from_str(value).map_err(|e| format!("header '{name}' 的值无效: {e}"))?;
        map.insert(n, v);
    }
    Ok(map)
}

/// Adapts an MCP server tool to hailux's async `Tool` trait.
///
/// Tool invocation is asynchronous (via the transport layer); `execute_async`
/// directly awaits `McpClient::call_tool` to complete the call.
pub struct McpTool {
    registered_name: String,
    tool_name: String,
    description: String,
    schema: Value,
    backend: Arc<McpClient>,
}

impl McpTool {
    pub fn new(server_name: &str, tool: &McpToolDef, backend: Arc<McpClient>) -> Self {
        let tool_name = tool.name.to_string();
        Self {
            registered_name: format!("mcp__{server_name}__{tool_name}"),
            description: format!(
                "[MCP / {}] {}",
                server_name,
                tool.description.as_deref().unwrap_or("（无描述）")
            ),
            tool_name,
            schema: tool.schema_as_json_value(),
            backend,
        }
    }
}

impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.registered_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        self.schema.clone()
    }

    fn permission_category(&self) -> Option<&str> {
        Some("mcp")
    }

    fn extract_permission(
        &self,
        _arguments: &str,
        _work_dir: &str,
    ) -> Vec<crate::permission::PermissionRequest> {
        vec![crate::permission::PermissionRequest {
            permission: "mcp".to_string(),
            patterns: vec![self.registered_name.clone()],
            always_patterns: vec![self.registered_name.clone()],
            description: format!("MCP tool: {}", self.registered_name),
            force_ask: false,
        }]
    }

    fn execute_async<'a>(
        &'a self,
        arguments: &'a str,
        _work_dir: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolExecuteError>> + Send + 'a>> {
        let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
        let mut params = CallToolRequestParams::new(self.tool_name.clone());
        if let Some(obj) = args.as_object() {
            params = params.with_arguments(obj.clone());
        }
        let backend = self.backend.clone();
        Box::pin(async move {
            let result = backend
                .call_tool(params)
                .await
                .map_err(|e| ToolExecuteError {
                    message: format!("MCP 调用失败: {e}"),
                })?;
            convert_call_tool_result(result)
        })
    }
}

/// Converts the content sequence of a `CallToolResult` into plain text;
/// converts it to an error when `is_error` is true.
fn convert_call_tool_result(result: CallToolResult) -> Result<String, ToolExecuteError> {
    let mut out = String::new();
    for block in result.content {
        push_content(&mut out, block);
        out.push('\n');
    }
    if let Some(structured) = result.structured_content {
        let s = if structured.is_string() {
            structured.as_str().unwrap_or("").to_string()
        } else {
            structured.to_string()
        };
        if !s.is_empty() {
            out.push_str(&s);
        }
    }
    let out = out.trim().to_string();
    if result.is_error.unwrap_or(false) {
        Err(ToolExecuteError { message: out })
    } else {
        Ok(out)
    }
}

fn push_content(out: &mut String, block: ContentBlock) {
    match block {
        ContentBlock::Text(t) => out.push_str(&t.text),
        ContentBlock::Image(i) => {
            out.push_str(&format!("[图片内容: {}]", i.mime_type));
        }
        ContentBlock::Resource(r) => out.push_str(&r.get_text()),
        ContentBlock::Audio(a) => {
            out.push_str(&format!("[音频内容: {}]", a.mime_type));
        }
        ContentBlock::ResourceLink(rl) => {
            out.push_str(&format!("[资源链接: {}]", rl.uri));
        }
        _ => {}
    }
}
