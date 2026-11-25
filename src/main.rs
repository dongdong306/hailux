use clap::{Parser, Subcommand};
use color_eyre::Result;
use hailux::agent::skill;
#[cfg(feature = "web")]
use hailux::web;
use hailux::{
    config, rebuild_database, resolve_work_dir, run_non_interactive, run_tui, run_tui_setup,
    updater,
};

// ── CLI definition ─────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "hailux", version, about = "终端 AI 编程助手")]
struct Cli {
    /// Set the working directory
    #[arg(short = 'p', long = "path", global = true)]
    work_dir: Option<String>,
    /// Rebuild the database (wipes all history); for migration failures
    #[arg(long = "rebuild-db", global = true)]
    rebuild_db: bool,
    /// Run in YOLO mode (skip all permission confirmations), for TUI and non-interactive
    #[arg(long = "yolo", global = true)]
    yolo: bool,
    /// Auto-resume the most recent session at startup
    #[arg(short = 'r', long = "resume", global = true)]
    resume: bool,
    /// Check for and install the latest version
    #[arg(long = "update", global = true)]
    update: bool,
    /// Start in Web UI mode (same as the `hailux web` subcommand, default host/port)
    #[arg(long = "web", global = true)]
    web: bool,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Non-interactive mode: send a single message and print the reply
    Run {
        /// Message content (read from stdin when omitted)
        message: Option<String>,
        /// Override the configured model; format: provider/model
        #[arg(short = 'm', long = "model")]
        model: Option<String>,
        /// Disable all tool calls (incl. MCP); pure chat mode
        #[arg(long = "no-tools")]
        no_tools: bool,
    },
    /// Start the Web UI server (listens on loopback only by default)
    #[cfg(feature = "web")]
    Web {
        /// Listen address
        #[arg(long = "host", default_value = "127.0.0.1")]
        host: String,
        /// Listen port (default 18080; if set explicitly and occupied, error out with no fallback)
        #[arg(long = "port")]
        port: Option<u16>,
        /// Open the browser automatically
        #[arg(long)]
        open: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    // Windows: clean up .old files possibly left over from the last update
    #[cfg(windows)]
    updater::cleanup_old_binary();

    let cli = Cli::parse();

    if cli.rebuild_db {
        return rebuild_database().await;
    }

    if cli.update {
        return updater::run_update().await;
    }

    // --web: start the Web UI with defaults (use the `hailux web` subcommand for options)
    #[cfg(feature = "web")]
    if cli.web && !matches!(cli.command, Some(Commands::Web { .. })) {
        let work_dir = resolve_work_dir(cli.work_dir.as_deref())?;
        return web::run_web(web::WebOptions {
            host: "127.0.0.1".to_string(),
            port: 18080,
            open: false,
            auto_fallback: true,
            work_dir,
        })
        .await;
    }
    #[cfg(not(feature = "web"))]
    if cli.web {
        color_eyre::eyre::bail!(
            "此构建未启用 Web UI（feature = \"web\"）。请使用默认 feature 重新构建。"
        );
    }

    match cli.command {
        Some(Commands::Run {
            message,
            model,
            no_tools,
        }) => {
            let work_dir = resolve_work_dir(cli.work_dir.as_deref())?;
            run_non_interactive(message, model, no_tools, cli.yolo, &work_dir).await
        }
        #[cfg(feature = "web")]
        Some(Commands::Web { host, port, open }) => {
            let work_dir = resolve_work_dir(cli.work_dir.as_deref())?;
            let auto_fallback = port.is_none();
            web::run_web(web::WebOptions {
                host,
                port: port.unwrap_or(18080),
                open,
                auto_fallback,
                work_dir,
            })
            .await
        }
        None => {
            let work_dir = resolve_work_dir(cli.work_dir.as_deref())?;

            let load_result = config::load()?;
            let needs_setup = matches!(load_result, config::LoadResult::NeedsSetup);
            let cfg = match load_result {
                config::LoadResult::Ready(cfg) => *cfg,
                config::LoadResult::NeedsSetup => config::Config::default(),
            };
            let resolved = if needs_setup {
                config::ResolvedModel {
                    config: async_openai::config::OpenAIConfig::new(),
                    model_id: String::new(),
                    max_tokens: 16384,
                    context_window: 131072,
                    display: String::new(),
                    supports_vision: false,
                    native_reasoning: false,
                }
            } else {
                cfg.resolve_default()?
            };

            if needs_setup {
                skill::ensure_default_skills();
                run_tui_setup(&work_dir).await
            } else {
                run_tui(cfg, resolved, &work_dir, cli.yolo, cli.resume).await
            }
        }
    }
}
