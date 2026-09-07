//! Web 后端：axum SSE + REST + 嵌入式静态资源。
//!
//! 启动入口 `run_web()`（由 `hailux web` 子命令调用）。默认只监听
//! `127.0.0.1`（与 TUI 同等信任级别：本机用户）。工作目录选择能力
//! 意味着可访问本机任意目录，暴露到 `0.0.0.0` 前请自行评估风险。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::DefaultBodyLimit;
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

/// 端口自动回退时最多尝试的端口数（含原始端口）
const PORT_FALLBACK_ATTEMPTS: u16 = 10;

/// 实例探测端点 `/api/instance` 响应中的标记值（完整响应为
/// `{"app":"hailux"}`，字段名固定为 `app`）。由 handlers 的
/// `instance_info` 与 [`detect_hailux_instance`] 共享，修改必须同步。
pub(crate) const INSTANCE_MARKER_APP: &str = "hailux";

/// 端口绑定结果。
#[derive(Debug)]
enum BindOutcome {
    /// 绑定成功：监听器、实际端口、回退过程中发现被占用的端口列表
    Bound {
        listener: tokio::net::TcpListener,
        port: u16,
        occupied: Vec<u16>,
    },
    /// 显式指定端口且被占用（不做自动回退）
    Busy(u16),
}

/// 绑定 Web 监听端口。`auto_fallback` 为 true 时端口被占用则依次 +1 重试
/// （最多 [`PORT_FALLBACK_ATTEMPTS`] 次）；为 false 时被占用返回
/// [`BindOutcome::Busy`]，由调用方决定报错信息。
async fn bind_listener(host: &str, port: u16, auto_fallback: bool) -> Result<BindOutcome> {
    let mut occupied = Vec::new();
    let mut candidate = port;
    for _ in 0..PORT_FALLBACK_ATTEMPTS {
        let addr: SocketAddr = format!("{host}:{candidate}")
            .parse()
            .map_err(|e| color_eyre::eyre::eyre!("监听地址无效 {host}:{candidate}: {e}"))?;
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                return Ok(BindOutcome::Bound {
                    listener,
                    port: candidate,
                    occupied,
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                occupied.push(candidate);
                if !auto_fallback {
                    return Ok(BindOutcome::Busy(candidate));
                }
                let Some(next) = candidate.checked_add(1) else {
                    break;
                };
                candidate = next;
            }
            Err(e) => return Err(e.into()),
        }
    }
    color_eyre::eyre::bail!("端口 {port}..{candidate} 均被占用，无可用端口");
}

/// 判断 `/api/instance` 响应是否来自 hailux Web UI 实例。
fn is_hailux_instance_response(value: &serde_json::Value) -> bool {
    value.get("app").and_then(|v| v.as_str()) == Some(INSTANCE_MARKER_APP)
}

/// 探测 `{host}:{port}` 是否运行着另一个 hailux Web UI 实例。
/// 请求 `GET /api/instance` 并精确匹配 JSON 标识；仅探测本机地址
/// （`0.0.0.0`/`::` 视为回环），best-effort：网络错误、超时或内容
/// 不匹配均返回 None。
async fn detect_hailux_instance(host: &str, port: u16) -> Option<String> {
    let http_host = match host {
        "0.0.0.0" | "::" => "127.0.0.1",
        other => other,
    };
    let probe_url = format!("http://{http_host}:{port}/api/instance");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(800))
        .build()
        .ok()?;
    let body = client
        .get(&probe_url)
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    let value: serde_json::Value = serde_json::from_str(&body).ok()?;
    // 展示用根地址，而非探测端点本身
    is_hailux_instance_response(&value).then(|| format!("http://{http_host}:{port}"))
}

/// 构建启动后可访问的本机 URL 列表（仅展示 `127.0.0.1`，不展示 `localhost`）。
fn access_urls(host: &str, port: u16) -> Vec<String> {
    match host {
        "localhost" | "127.0.0.1" | "0.0.0.0" => vec![format!("http://127.0.0.1:{port}")],
        "::" | "::1" => vec![format!("http://[::1]:{port}")],
        other => vec![format!("http://{other}:{port}")],
    }
}

/// Web UI 服务器启动参数（`hailux web` 子命令 / `--web` 全局标志）。
pub struct WebOptions {
    /// 监听地址
    pub host: String,
    /// 监听端口
    pub port: u16,
    /// 启动后自动打开浏览器
    pub open: bool,
    /// 端口被占用时是否自动 +1 回退；显式指定 `--port` 时为 false，
    /// 被占用直接报错
    pub auto_fallback: bool,
    /// 工作目录
    pub work_dir: PathBuf,
}

/// 启动 Web UI 服务器（`hailux web`）。`auto_fallback` 为 true 时默认端口
/// 被占用会自动 +1 重试；显式指定 `--port` 时被占用直接报错。
pub async fn run_web(options: WebOptions) -> Result<()> {
    let WebOptions {
        host,
        port,
        open,
        auto_fallback,
        work_dir,
    } = options;
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
        default_work_dir: work_dir,
        config_write_lock: tokio::sync::Mutex::new(()),
    });

    let app = Router::new()
        .merge(handlers::api_router())
        .route("/", get(static_handler))
        .route("/{*path}", get(static_handler))
        // 附件 base64 膨胀后远超 axum 默认 2MB body limit，显式放开
        // （上限与 sse.rs 的附件常量同源推导，见 CHAT_BODY_LIMIT）
        .layer(DefaultBodyLimit::max(sse::CHAT_BODY_LIMIT))
        .layer(CorsLayer::permissive())
        .with_state(state);

    match bind_listener(&host, port, auto_fallback).await? {
        BindOutcome::Busy(busy) => {
            let hint = match detect_hailux_instance(&host, busy).await {
                Some(url) => format!("（检测到其他 hailux Web UI 实例: {url}）"),
                None => String::new(),
            };
            color_eyre::eyre::bail!("端口 {busy} 已被占用{hint}，请更换端口或去掉 --port 参数");
        }
        BindOutcome::Bound {
            listener,
            port,
            occupied,
        } => {
            if let Some(original) = occupied.first() {
                println!("端口 {original} 已被占用，已自动改用 {port}");
            }

            let mut instances = Vec::new();
            for p in &occupied {
                if let Some(url) = detect_hailux_instance(&host, *p).await {
                    instances.push(url);
                }
            }
            if !instances.is_empty() {
                println!("检测到其他 hailux Web UI 实例：");
                for url in &instances {
                    println!("  - {url}");
                }
            }

            let urls = access_urls(&host, port);
            println!("hailux Web UI 已启动，可访问：");
            for url in &urls {
                println!("  - {url}");
            }

            if open {
                let url = urls[0].clone();
                tokio::spawn(async move {
                    let _ = open_browser(&url).await;
                });
            }

            axum::serve(listener, app).await?;
            Ok(())
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 串行化绑定类测试：内核对 `:0` 的临时端口分配是递增的，并行运行时
    /// 各测试拿到的端口相邻，会互相踩进对方的回退窗口
    static BIND_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn bind_test_lock() -> tokio::sync::MutexGuard<'static, ()> {
        BIND_TEST_LOCK.lock().await
    }

    #[test]
    fn is_hailux_instance_response_matches_marker() {
        let ok: serde_json::Value = serde_json::from_str(r#"{"app":"hailux"}"#).unwrap();
        assert!(is_hailux_instance_response(&ok));
        let spaced: serde_json::Value = serde_json::from_str(r#"{ "app" : "hailux" }"#).unwrap();
        assert!(is_hailux_instance_response(&spaced));
        let other: serde_json::Value = serde_json::from_str(r#"{"app":"other-app"}"#).unwrap();
        assert!(!is_hailux_instance_response(&other));
        let missing: serde_json::Value = serde_json::from_str("{}").unwrap();
        assert!(!is_hailux_instance_response(&missing));
    }

    #[test]
    fn access_urls_lists_local_hosts() {
        for host in ["127.0.0.1", "localhost", "0.0.0.0"] {
            assert_eq!(
                access_urls(host, 18080),
                vec!["http://127.0.0.1:18080"],
                "host = {host}"
            );
        }
        assert_eq!(access_urls("::", 18080), vec!["http://[::1]:18080"]);
        assert_eq!(
            access_urls("192.168.1.5", 18080),
            vec!["http://192.168.1.5:18080"]
        );
    }

    #[tokio::test]
    async fn bind_listener_falls_back_when_port_busy() {
        let _guard = bind_test_lock().await;
        let first = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let busy = first.local_addr().unwrap().port();
        // 预先占住 busy+1，使回退目标确定为 busy+2；若该端口恰已被其他
        // 进程占用（AddrInUse）同样视为忙
        let second = match tokio::net::TcpListener::bind(("127.0.0.1", busy + 1)).await {
            Ok(l) => Some(l),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => None,
            Err(e) => panic!("预占端口 {} 失败: {e}", busy + 1),
        };
        let outcome = bind_listener("127.0.0.1", busy, true).await.unwrap();
        let BindOutcome::Bound {
            port,
            occupied,
            listener,
        } = outcome
        else {
            panic!("expected fallback binding");
        };
        assert_eq!(port, busy + 2);
        assert_eq!(occupied, vec![busy, busy + 1]);
        drop(listener);
        drop(first);
        drop(second);
    }

    #[tokio::test]
    async fn bind_listener_reports_busy_without_fallback() {
        let _guard = bind_test_lock().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let busy = listener.local_addr().unwrap().port();
        let outcome = bind_listener("127.0.0.1", busy, false).await.unwrap();
        assert!(matches!(outcome, BindOutcome::Busy(p) if p == busy));
    }

    #[tokio::test]
    async fn bind_listener_errors_when_all_attempts_busy() {
        let _guard = bind_test_lock().await;
        let first = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let busy = first.local_addr().unwrap().port();
        // 占满 busy+1 .. busy+9；个别端口若恰被系统占用（AddrInUse）
        // 同样视为忙，不影响用例成立
        let mut extra = Vec::new();
        for offset in 1..PORT_FALLBACK_ATTEMPTS {
            match tokio::net::TcpListener::bind(("127.0.0.1", busy + offset)).await {
                Ok(l) => extra.push(l),
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {}
                Err(e) => panic!("预占端口 {} 失败: {e}", busy + offset),
            }
        }
        let err = bind_listener("127.0.0.1", busy, true).await.unwrap_err();
        assert!(err.to_string().contains("均被占用"), "got: {err}");
    }
}
