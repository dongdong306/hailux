//! Web backend: axum SSE + REST + embedded static assets.
//!
//! Startup entry point `run_web()` (invoked by the `hailux web` subcommand). By
//! default it only listens on `127.0.0.1` (same trust level as the TUI: the local
//! user). The ability to pick a working directory means any local directory is
//! reachable — evaluate the risk yourself before exposing on `0.0.0.0`.

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

mod bus;
mod handlers;
mod protocol;
mod runner;
mod sse;
mod state;
mod task_registry;

use state::WebServerState;
use task_registry::TaskRegistry;

/// Static assets (web/dist, embedded via rust-embed; read from the filesystem in debug mode)
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
            // index.html must not be cached (it references hashed asset names, so freshness
            // must be validated on every request); hashed assets can be cached long-term
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
        // SPA fallback: unknown paths fall back to index.html (frontend routing)
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

/// Max number of ports tried during automatic port fallback (including the original port)
const PORT_FALLBACK_ATTEMPTS: u16 = 10;

/// Marker value in the `/api/instance` probe endpoint response (the full response is
/// `{"app":"hailux"}`; the field name is fixed as `app`). Shared by the handlers'
/// `instance_info` and [`detect_hailux_instance`]; keep them in sync when changing.
pub(crate) const INSTANCE_MARKER_APP: &str = "hailux";

/// Port binding outcome.
#[derive(Debug)]
enum BindOutcome {
    /// Bound successfully: listener, actual port, and the list of ports found busy during fallback
    Bound {
        listener: tokio::net::TcpListener,
        port: u16,
        occupied: Vec<u16>,
    },
    /// Explicitly requested port is occupied (no automatic fallback)
    Busy(u16),
}

/// Bind the web listening port. When `auto_fallback` is true and the port is busy,
/// retry with +1 increments (up to [`PORT_FALLBACK_ATTEMPTS`] attempts); when false
/// and the port is busy, return [`BindOutcome::Busy`] and let the caller report the error.
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

/// Whether an `/api/instance` response comes from a hailux Web UI instance.
fn is_hailux_instance_response(value: &serde_json::Value) -> bool {
    value.get("app").and_then(|v| v.as_str()) == Some(INSTANCE_MARKER_APP)
}

/// Probe whether `{host}:{port}` runs another hailux Web UI instance.
/// Sends `GET /api/instance` and matches the JSON marker exactly; only local
/// addresses are probed (`0.0.0.0`/`::` treated as loopback). Best-effort:
/// network errors, timeouts, or content mismatches all return None.
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
    // Root URL for display, not the probe endpoint itself
    is_hailux_instance_response(&value).then(|| format!("http://{http_host}:{port}"))
}

/// Build the local URLs reachable after startup (show `127.0.0.1` only, not `localhost`).
fn access_urls(host: &str, port: u16) -> Vec<String> {
    match host {
        "localhost" | "127.0.0.1" | "0.0.0.0" => vec![format!("http://127.0.0.1:{port}")],
        "::" | "::1" => vec![format!("http://[::1]:{port}")],
        other => vec![format!("http://{other}:{port}")],
    }
}

/// Web UI server startup options (`hailux web` subcommand / `--web` global flag).
pub struct WebOptions {
    /// Listen address
    pub host: String,
    /// Listen port
    pub port: u16,
    /// Open the browser automatically after startup
    pub open: bool,
    /// Whether to fall back with +1 automatically when the port is busy; false when
    /// `--port` is given explicitly, in which case a busy port errors out directly
    pub auto_fallback: bool,
    /// Working directory
    pub work_dir: PathBuf,
}

/// Start the Web UI server (`hailux web`). When `auto_fallback` is true, a busy
/// default port retries with +1; when `--port` is given explicitly, a busy port
/// errors out directly.
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

    // MCP connections (in parallel, in the background; once connected they are registered
    // into the shared backends so sessions can lazily register tools)
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

    let max_concurrent_runs = cfg.session.max_concurrent_runs;
    let manager = SessionManager::new(resolved, cfg, storage, mcp_backends);
    let epoch = WebServerState::new_epoch();
    let state = Arc::new(WebServerState {
        manager,
        registry: Arc::new(TaskRegistry::new()),
        buses: bus::BusRegistry::default(),
        epoch,
        run_permits: WebServerState::run_permits_from_limit(max_concurrent_runs),
        run_limit: max_concurrent_runs,
        default_work_dir: work_dir,
        config_write_lock: tokio::sync::Mutex::new(()),
    });

    let app = Router::new()
        .merge(handlers::api_router())
        .route("/", get(static_handler))
        .route("/{*path}", get(static_handler))
        // Attachment base64 inflation far exceeds axum's default 2MB body limit; raise it
        // explicitly (the limit derives from the attachment constant in sse.rs, see CHAT_BODY_LIMIT)
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

    /// Serialize bind-related tests: the kernel allocates ephemeral `:0` ports in
    /// increasing order, so parallel tests get adjacent ports and step into each
    /// other's fallback windows
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
        // Pre-occupy busy+1 so the fallback target becomes busy+2; if that port happens
        // to be taken by another process (AddrInUse), treat it as busy too
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
        // Occupy busy+1 .. busy+9; if an individual port happens to be taken by the
        // system (AddrInUse), treat it as busy too — the test still holds
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
