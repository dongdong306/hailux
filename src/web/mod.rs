//! Web 后端：axum SSE + REST + 嵌入式静态资源。
//!
//! 启动入口 `run_web()`（由 `hailux web` 子命令调用）。默认只监听
//! `127.0.0.1`（与 TUI 同等信任级别：本机用户）。工作目录选择能力
//! 意味着可访问本机任意目录，暴露到 `0.0.0.0` 前请自行评估风险。

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use color_eyre::Result;
use tower_http::cors::CorsLayer;

use crate::config;
use crate::ensure_storage_ok;
use crate::mcp;
use crate::session::SessionManager;
use crate::storage::ChatStorage;

mod handlers;
mod protocol;
mod sse;
mod state;
mod task_registry;

use state::WebServerState;
use task_registry::TaskRegistry;

/// 静态资源（web/dist，由 rust-embed 嵌入；debug 模式运行时读取文件系统）
#[derive(rust_embed::RustEmbed)]
#[folder = "web/dist"]
struct WebAssets;

static INDEX_HTML: &str = "index.html";

async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { INDEX_HTML } else { path };

    match WebAssets::get(path) {
        Some(content) => {
            let mime = content.metadata.mimetype();
            // index.html 禁止缓存（内容引用带 hash 的资源名，必须每次校验新鲜度）；
            // 带 hash 的资源文件可长缓存
            let cache = if path == INDEX_HTML {
                "no-cache"
            } else {
                "public, max-age=31536000, immutable"
            };
            (
                [(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, cache)],
                content.data,
            )
                .into_response()
        }
        // SPA fallback：未知路径回落到 index.html（前端路由）
        None => match WebAssets::get(INDEX_HTML) {
            Some(content) => (
                [
                    (header::CONTENT_TYPE, "text/html"),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
                content.data,
            )
                .into_response(),
            None => (StatusCode::NOT_FOUND, "frontend not built").into_response(),
        },
    }
}

/// 启动 Web UI 服务器（`hailux web`）。
pub async fn run_web(host: &str, port: u16, open: bool, work_dir: &Path) -> Result<()> {
    let load_result = config::load()?;
    let cfg = match load_result {
        config::LoadResult::Ready(cfg) => *cfg,
        config::LoadResult::NeedsSetup => {
            color_eyre::eyre::bail!("配置未完成，请先运行 hailux 进行初始化设置");
        }
    };
    let resolved = cfg.resolve_default()?;

    let storage = ChatStorage::new().await?;
    ensure_storage_ok(&storage)?;

    // MCP 连接（后台并行；连接完成后注册进共享 backends 供会话懒注册工具）
    let mcp_backends: mcp::SharedMcpBackends = Arc::new(std::sync::Mutex::new(Vec::new()));
    let backends_for_connect = mcp_backends.clone();
    tokio::spawn(async move {
        let mcp_cfg = mcp::config::load().unwrap_or_default();
        if mcp_cfg.mcp_servers.is_empty() {
            return;
        }
        let connections = mcp::connect_mcp_servers(&mcp_cfg).await;
        if let Ok(mut guard) = backends_for_connect.lock() {
            for conn in &connections {
                if let Some(backend) = &conn.backend {
                    guard.push(mcp::McpToolBackend {
                        server_name: conn.status.name.clone(),
                        backend: backend.clone(),
                        tools: conn.tools.clone(),
                    });
                }
            }
        }
    });

    let manager = SessionManager::new(resolved, cfg, storage, mcp_backends);
    let state = Arc::new(WebServerState {
        manager,
        registry: Arc::new(TaskRegistry::new()),
        default_work_dir: work_dir.to_path_buf(),
    });

    let app = Router::new()
        .merge(handlers::api_router())
        .route("/", get(static_handler))
        .route("/{*path}", get(static_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .map_err(|e| color_eyre::eyre::eyre!("监听地址无效 {host}:{port}: {e}"))?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("hailux Web UI: http://{addr}");

    if open {
        let url = format!("http://{addr}");
        tokio::spawn(async move {
            let _ = open_browser(&url).await;
        });
    }

    axum::serve(listener, app).await?;
    Ok(())
}

async fn open_browser(url: &str) -> Result<()> {
    #[cfg(windows)]
    {
        tokio::process::Command::new("cmd")
            .args(["/c", "start", "", url])
            .spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        tokio::process::Command::new("open").arg(url).spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        tokio::process::Command::new("xdg-open").arg(url).spawn()?;
    }
    Ok(())
}
