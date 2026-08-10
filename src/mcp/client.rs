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

/// 单个 MCP 服务器的连接句柄类型（stdio 与 http 均使用 ClientInfo 处理器，故类型统一）。
pub type McpClient = RunningService<RoleClient, ClientInfo>;

/// MCP 服务器连接成功后的状态快照（供 UI 渲染，不含连接句柄）。
#[derive(Debug, Clone)]
pub struct McpServerStatus {
    /// 配置文件中的服务器名称（key）
    pub name: String,
    pub connected: bool,
    /// 服务器自报名称
    pub server_name: String,
    pub server_version: String,
    /// "stdio" 或 "http"
    pub transport: String,
    pub error: Option<String>,
    /// 工具名列表
    pub tools: Vec<String>,
    pub resource_count: usize,
    pub prompt_count: usize,
    /// 资源详情（URI + 描述），供详情页展示
    pub resources: Vec<(String, String)>,
    /// 提示词详情（名称 + 描述），供详情页展示
    pub prompts: Vec<(String, String)>,
}

/// 单台服务器的连接结果：状态 + （成功时）连接句柄与工具定义。
#[derive(Debug)]
pub struct McpConnection {
    pub status: McpServerStatus,
    pub backend: Option<Arc<McpClient>>,
    pub tools: Vec<McpToolDef>,
}

/// MCP 工具后端：服务器名称 + 连接句柄 + 工具定义列表，
/// 供 subagent 共享 MCP 工具使用。
#[derive(Debug, Clone)]
pub struct McpToolBackend {
    pub server_name: String,
    pub backend: Arc<McpClient>,
    pub tools: Vec<McpToolDef>,
}

/// 共享 MCP 后端列表，使用 Arc<Mutex> 包装以在主 agent 和 subagent 间共享。
pub type SharedMcpBackends = Arc<std::sync::Mutex<Vec<McpToolBackend>>>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// 根据配置生成初始占位状态（全部标记为"连接中..."），
/// 用于 UI 先行渲染，避免等待实际连接。
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

/// 遍历配置中的所有 MCP 服务器，并发尝试连接（带超时与容错）。
/// 单台失败不影响其他服务器，失败信息记录在 `status.error` 中。
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

/// 连接单台服务器并收集 tools/resources/prompts 与服务器信息。
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

/// 根据传输类型建立连接，完成 MCP 初始化握手。
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
            // 在后台读取 stderr 以防止管道缓冲区满导致子进程阻塞
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

/// 解析可执行程序路径。
///
/// Windows 下裸命令（如 `npx`）对应的是 `.cmd`/`.bat` 脚本，
/// 而 `CreateProcess` 不会自动加后缀，导致 "program not found"。
/// 这里手动遍历 `PATH` + `PATHEXT` 找到真实文件路径；
/// 一旦解析到 `.cmd`/`.bat`，Rust std 会自动识别并用 `cmd.exe` 运行。
///
/// **安全**：仅搜索 `PATH`，不检查当前目录，以防止二进制植入攻击。
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
        // 仅搜索 PATH，不检查当前目录，以防止二进制植入
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
    // 仅搜索 PATH，不检查当前目录，以防止二进制植入
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

/// 将配置中的 header 名/值映射转为 http 类型。
/// 失败（非法 header 名或含非法字节的值）时返回可读错误。
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

/// 将 MCP 服务器的工具适配为 hailux 的异步 `Tool` trait。
///
/// 工具调用是异步的（走传输层），`execute_async` 直接 await
/// `McpClient::call_tool` 完成调用。
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
    ) -> Option<crate::permission::PermissionRequest> {
        Some(crate::permission::PermissionRequest {
            permission: "mcp".to_string(),
            patterns: vec![self.registered_name.clone()],
            always_patterns: vec![self.registered_name.clone()],
            description: format!("MCP tool: {}", self.registered_name),
        })
    }

    fn execute_async<'a>(
        &'a self,
        arguments: &'a str,
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

/// 将 `CallToolResult` 的内容序列转为纯文本；`is_error` 为真时转为错误。
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
