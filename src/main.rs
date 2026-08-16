use clap::{Parser, Subcommand};
use color_eyre::Result;
use hailux::agent::skill;
#[cfg(feature = "web")]
use hailux::web;
use hailux::{
    config, rebuild_database, resolve_work_dir, run_non_interactive, run_tui, run_tui_setup,
    updater,
};

// ── CLI 定义 ─────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "hailux", version, about = "终端 AI 编程助手")]
struct Cli {
    /// 设置工作目录
    #[arg(short = 'p', long = "path", global = true)]
    work_dir: Option<String>,
    /// 重建数据库（清空全部历史记录），迁移故障时使用
    #[arg(long = "rebuild-db", global = true)]
    rebuild_db: bool,
    /// 以 YOLO 模式运行（跳过所有权限确认），对 TUI 和非交互模式均生效
    #[arg(long = "yolo", global = true)]
    yolo: bool,
    /// 启动时自动恢复最近的 session
    #[arg(short = 'r', long = "resume", global = true)]
    resume: bool,
    /// 检查并更新到最新版本
    #[arg(long = "update", global = true)]
    update: bool,
    /// 以 Web UI 模式启动（等价于 `hailux web` 子命令，使用默认地址端口）
    #[arg(long = "web", global = true)]
    web: bool,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// 非交互模式：发送单条消息并打印回复
    Run {
        /// 消息内容（省略时从 stdin 读取）
        message: Option<String>,
        /// 覆盖配置中的模型，格式：provider/model
        #[arg(short = 'm', long = "model")]
        model: Option<String>,
        /// 禁用所有工具调用（含 MCP），纯对话模式
        #[arg(long = "no-tools")]
        no_tools: bool,
    },
    /// 启动 Web UI 服务器（默认仅监听本机回环地址）
    #[cfg(feature = "web")]
    Web {
        /// 监听地址
        #[arg(long = "host", default_value = "127.0.0.1")]
        host: String,
        /// 监听端口
        #[arg(long = "port", default_value = "18080")]
        port: u16,
        /// 自动打开浏览器
        #[arg(long)]
        open: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    // Windows: 清理上次更新可能残留的 .old 文件
    #[cfg(windows)]
    updater::cleanup_old_binary();

    let cli = Cli::parse();

    if cli.rebuild_db {
        return rebuild_database().await;
    }

    if cli.update {
        return updater::run_update().await;
    }

    // --web：默认参数启动 Web UI（详细参数用 `hailux web` 子命令）
    #[cfg(feature = "web")]
    if cli.web && !matches!(cli.command, Some(Commands::Web { .. })) {
        let work_dir = resolve_work_dir(cli.work_dir.as_deref())?;
        return web::run_web("127.0.0.1", 18080, false, &work_dir).await;
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
            web::run_web(&host, port, open, &work_dir).await
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
