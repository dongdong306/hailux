use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use super::super::protocol::{
    CreateMcpServerRequest, DeleteMcpServerRequest, McpServerInfo, UpdateMcpServerRequest,
};
use super::super::state::WebServerState;
use super::err500;

/// MCP 服务器名约束：字母数字开头，仅含字母数字 / `-` / `_` / `.`，≤ 64 字符。
/// 名称作为 toml key 与 `mcp__<name>__<tool>` 注册名的一部分，需严格白名单。
fn valid_mcp_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// 由请求字段构造 ServerConfig；transport 与必填字段不匹配时返回错误。
fn build_server_config(
    transport: &str,
    command: Option<String>,
    args: Vec<String>,
    env: std::collections::BTreeMap<String, String>,
    url: Option<String>,
    headers: std::collections::BTreeMap<String, String>,
) -> Result<crate::mcp::config::ServerConfig, String> {
    match transport {
        "stdio" => {
            let command = command.filter(|c| !c.trim().is_empty()).ok_or(
                "stdio 服务器必须提供 command（可执行程序，如 npx / uvx / node）".to_string(),
            )?;
            Ok(crate::mcp::config::ServerConfig::Stdio { command, args, env })
        }
        "http" => {
            let url = url
                .filter(|u| !u.trim().is_empty())
                .ok_or_else(|| "http 服务器必须提供 url".to_string())?;
            Ok(crate::mcp::config::ServerConfig::Http { url, headers })
        }
        other => Err(format!("不支持的传输方式: {other}（仅 stdio / http）")),
    }
}

/// 重连全部 MCP 服务器并替换共享 backends，随后使会话缓存失效
/// （下次请求时按新 backends 重新注册 MCP 工具）。
/// 运行中的请求持有旧连接的 Arc 不受影响；被移除的旧 stdio 子进程随 Arc 释放自动终止。
fn schedule_mcp_reconnect(state: &Arc<WebServerState>) {
    let backends = state.manager.mcp_backends().clone();
    let state = state.clone();
    tokio::spawn(async move {
        let mcp_cfg = crate::mcp::config::load().unwrap_or_default();
        let connections = crate::mcp::connect_mcp_servers(&mcp_cfg).await;
        if let Ok(mut guard) = backends.lock() {
            guard.clear();
            for conn in &connections {
                if let Some(backend) = &conn.backend {
                    guard.push(crate::mcp::McpToolBackend {
                        server_name: conn.status.name.clone(),
                        backend: backend.clone(),
                        tools: conn.tools.clone(),
                    });
                }
            }
        }
        state.manager.invalidate_all();
    });
}

/// 合并配置文件（权威来源）与运行时连接状态为前端展示结构。
fn merge_mcp_status(
    config: &crate::mcp::config::McpConfig,
    backends: &crate::mcp::SharedMcpBackends,
) -> Vec<McpServerInfo> {
    let guard = backends.lock().ok();
    config
        .mcp_servers
        .iter()
        .map(|(name, server_config)| {
            // 名称匹配（backends 记录的是配置 key，连接失败的服务器不在其中）
            let backend = guard
                .as_ref()
                .and_then(|g| g.iter().find(|b| b.server_name == *name));
            let (connected, tools, tool_details) = match backend {
                Some(b) => (
                    true,
                    b.tools.len(),
                    b.tools
                        .iter()
                        .map(|t| super::super::protocol::McpToolInfo {
                            name: t.name.to_string(),
                            description: t.description.as_deref().unwrap_or("").to_string(),
                            schema: t.schema_as_json_value(),
                        })
                        .collect(),
                ),
                None => (false, 0, Vec::new()),
            };
            let transport = server_config.transport_label().to_string();
            match server_config {
                crate::mcp::config::ServerConfig::Stdio { command, args, env } => McpServerInfo {
                    name: name.clone(),
                    connected,
                    tools,
                    transport,
                    command: Some(command.clone()),
                    args: args.clone(),
                    env: env.clone(),
                    url: None,
                    headers: Default::default(),
                    error: (!connected).then(|| "未连接或连接失败".to_string()),
                    tool_details,
                },
                crate::mcp::config::ServerConfig::Http { url, headers } => McpServerInfo {
                    name: name.clone(),
                    connected,
                    tools,
                    transport,
                    command: None,
                    args: Vec::new(),
                    env: Default::default(),
                    url: Some(url.clone()),
                    headers: headers.clone(),
                    error: (!connected).then(|| "未连接或连接失败".to_string()),
                    tool_details,
                },
            }
        })
        .collect()
}

pub(super) async fn mcp_status(State(state): State<Arc<WebServerState>>) -> Response {
    let config = match crate::mcp::config::load() {
        Ok(c) => c,
        Err(e) => return err500(e),
    };
    Json(merge_mcp_status(&config, state.manager.mcp_backends())).into_response()
}

pub(super) async fn create_mcp_server(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<CreateMcpServerRequest>,
) -> Response {
    if !valid_mcp_name(&req.name) {
        return (
            StatusCode::BAD_REQUEST,
            "服务器名只能包含字母、数字、-、_、.，且以字母或数字开头（最长 64 字符）",
        )
            .into_response();
    }
    let server_config = match build_server_config(
        &req.transport,
        req.command,
        req.args,
        req.env,
        req.url,
        req.headers,
    ) {
        Ok(c) => c,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let mut config = match crate::mcp::config::load() {
        Ok(c) => c,
        Err(e) => return err500(e),
    };
    if config.mcp_servers.contains_key(&req.name) {
        return (
            StatusCode::CONFLICT,
            format!("MCP 服务器 \"{}\" 已存在", req.name),
        )
            .into_response();
    }
    config.mcp_servers.insert(req.name.clone(), server_config);
    if let Err(e) = crate::mcp::config::save(&config) {
        return err500(e);
    }
    schedule_mcp_reconnect(&state);
    (
        StatusCode::CREATED,
        Json(
            merge_mcp_status(&config, state.manager.mcp_backends())
                .into_iter()
                .find(|s| s.name == req.name)
                .map(|mut s| {
                    // 重连在后台进行，此时连接状态尚未就绪
                    s.connected = false;
                    s.tools = 0;
                    s.tool_details = Vec::new();
                    s.error = Some("已保存，正在连接…".to_string());
                    s
                }),
        ),
    )
        .into_response()
}

pub(super) async fn update_mcp_server(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<UpdateMcpServerRequest>,
) -> Response {
    let new_name = req.new_name.filter(|n| !n.trim().is_empty());
    if let Some(n) = &new_name
        && !valid_mcp_name(n)
    {
        return (
            StatusCode::BAD_REQUEST,
            "服务器名只能包含字母、数字、-、_、.，且以字母或数字开头（最长 64 字符）",
        )
            .into_response();
    }
    let server_config = match build_server_config(
        &req.transport,
        req.command,
        req.args,
        req.env,
        req.url,
        req.headers,
    ) {
        Ok(c) => c,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let mut config = match crate::mcp::config::load() {
        Ok(c) => c,
        Err(e) => return err500(e),
    };
    let Some(_old) = config.mcp_servers.remove(&req.name) else {
        return (
            StatusCode::NOT_FOUND,
            format!("MCP 服务器 \"{}\" 不存在", req.name),
        )
            .into_response();
    };
    let target = new_name.unwrap_or_else(|| req.name.clone());
    if target != req.name && config.mcp_servers.contains_key(&target) {
        // 改名冲突：本地副本尚未保存，直接报错即可（磁盘配置不变）
        return (
            StatusCode::CONFLICT,
            format!("MCP 服务器 \"{target}\" 已存在"),
        )
            .into_response();
    }
    config.mcp_servers.insert(target.clone(), server_config);
    if let Err(e) = crate::mcp::config::save(&config) {
        return err500(e);
    }
    schedule_mcp_reconnect(&state);
    StatusCode::OK.into_response()
}

pub(super) async fn delete_mcp_server(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<DeleteMcpServerRequest>,
) -> Response {
    let mut config = match crate::mcp::config::load() {
        Ok(c) => c,
        Err(e) => return err500(e),
    };
    if config.mcp_servers.remove(&req.name).is_none() {
        return (
            StatusCode::NOT_FOUND,
            format!("MCP 服务器 \"{}\" 不存在", req.name),
        )
            .into_response();
    }
    if let Err(e) = crate::mcp::config::save(&config) {
        return err500(e);
    }
    schedule_mcp_reconnect(&state);
    StatusCode::NO_CONTENT.into_response()
}
