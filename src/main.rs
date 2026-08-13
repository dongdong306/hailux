mod agent;
mod config;
mod mcp;
mod permission;
mod prompts;
mod storage;
mod tui;
mod updater;

use crate::agent::subagent;
use crate::agent::{Agent, BashTool, EditTool};
use crate::agent::{AskTool, GrepTool, ReadTool, TodoWriteTool};
use crate::agent::{CommandRegistry, SkillTool, skill};
use crate::agent::{GlobTool, WebFetchTool, WriteTool};
use clap::{Parser, Subcommand};
use color_eyre::Result;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tui::App;

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
}

/// 数据库迁移失败时拒绝启动，并提示用户重建
fn ensure_storage_ok(storage: &storage::ChatStorage) -> Result<()> {
    if let Some(err) = storage.migration_error() {
        color_eyre::eyre::bail!(
            "数据库迁移失败：{err}\n请使用 `hailux --rebuild-db` 重建数据库（将清空全部历史记录）。"
        );
    }
    Ok(())
}

/// `hailux --rebuild-db`：备份旧数据库文件后重建数据库
async fn rebuild_database() -> Result<()> {
    let mut storage = storage::ChatStorage::new().await?;
    let backup_path = storage.rebuild().await?;
    println!("数据库已重建（历史记录已清空）。");
    if let Some(path) = backup_path {
        println!("原数据库已备份至：{}", path.display());
    }
    Ok(())
}

fn build_permission_rules(cfg: &config::Config) -> Vec<crate::permission::PermissionRule> {
    use crate::permission::rules_from_config_table;
    let mut rules = Vec::new();
    rules.extend(rules_from_config_table("bash", &cfg.permission.bash));
    rules.extend(rules_from_config_table("read", &cfg.permission.read));
    rules.extend(rules_from_config_table("edit", &cfg.permission.edit));
    rules.extend(rules_from_config_table("write", &cfg.permission.write));
    rules.extend(rules_from_config_table("mcp", &cfg.permission.mcp));
    rules.extend(rules_from_config_table(
        "external_directory",
        &cfg.permission.external_directory,
    ));
    // 注意：不要在此追加「默认 Ask」规则。evaluate 无匹配时本就默认 Ask，
    // 追加默认规则反而会因其在末尾优先匹配，遮蔽用户配置的具体 deny/allow 规则。
    rules
}

// ── Agent 构建 ───────────────────────────────────────────────

fn build_agent_base(
    resolved: &config::ResolvedModel,
    cfg: &config::Config,
    work_dir: &Path,
) -> Result<(
    Agent,
    Vec<skill::SkillInfo>,
    CommandRegistry,
    Vec<subagent::SubagentConfig>,
)> {
    let pm = crate::permission::PermissionManager::new(
        crate::permission::PermissionMode::from_str(&cfg.permission.mode),
        build_permission_rules(cfg),
    );
    let mut agent = Agent::new(
        resolved.config.clone(),
        &resolved.model_id,
        resolved.max_tokens,
        pm,
        &work_dir.display().to_string(),
    );
    agent.register_tool(Box::new(BashTool));
    agent.register_tool(Box::new(ReadTool));
    agent.register_tool(Box::new(EditTool));
    agent.register_tool(Box::new(WriteTool));
    agent.register_tool(Box::new(WebFetchTool::new()));
    agent.register_tool(Box::new(GrepTool));
    agent.register_tool(Box::new(GlobTool));
    agent.register_tool(Box::new(TodoWriteTool));

    let skills = skill::discover_skills(work_dir).unwrap_or_default();
    if !skills.is_empty() {
        agent.register_tool(Box::new(SkillTool::new(skills.clone())));
    }

    let agent_md_entries = agent::agents_md::discover_agent_md(work_dir);

    let subagents = subagent::discover_subagents(work_dir).unwrap_or_default();

    let system_prompt =
        prompts::build_system_prompt(work_dir, &skills, &agent_md_entries, &subagents);
    agent.set_system_prompt(&system_prompt);

    let command_registry = CommandRegistry::discover(work_dir).unwrap_or_default();

    Ok((agent, skills, command_registry, subagents))
}

fn build_agent(
    resolved: &config::ResolvedModel,
    cfg: &config::Config,
    event_tx: tui::event::EventTx,
    work_dir: &Path,
) -> Result<(
    Agent,
    Vec<skill::SkillInfo>,
    CommandRegistry,
    Vec<subagent::SubagentConfig>,
)> {
    let (mut agent, skills, command_registry, mut subagents) =
        build_agent_base(resolved, cfg, work_dir)?;

    agent.register_tool(Box::new(AskTool::new(event_tx)));

    if !subagents.iter().any(|s| s.name == "general") {
        subagents.insert(0, subagent::builtin_general_subagent(&cfg.main_model));
    }

    Ok((agent, skills, command_registry, subagents))
}

// ── 非交互模式 ───────────────────────────────────────────────

async fn run_non_interactive(
    message: Option<String>,
    model: Option<String>,
    no_tools: bool,
    yolo: bool,
    work_dir: &Path,
) -> Result<()> {
    let message = match message {
        Some(m) if !m.is_empty() => m,
        _ => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf.trim().to_string()
        }
    };
    if message.is_empty() {
        color_eyre::eyre::bail!("错误：未提供消息。用法：hailux run \"消息\" 或通过管道输入");
    }

    let cfg = match config::load()? {
        config::LoadResult::Ready(cfg) => *cfg,
        config::LoadResult::NeedsSetup => {
            color_eyre::eyre::bail!("配置未完成，请先运行 hailux 进行初始化设置");
        }
    };

    let resolved = if let Some(ref m) = model {
        cfg.resolve(m)?
    } else {
        cfg.resolve_default()?
    };

    let (event_tx, mut event_rx) = tui::event::create_event_channel();

    let (mut agent, skills, _, mut subagents) = build_agent_base(&resolved, &cfg, work_dir)?;
    if yolo {
        // --yolo：跳过所有权限确认
        agent
            .permission()
            .set_mode(crate::permission::PermissionMode::Yolo);
    } else {
        // 非交互模式没有 TUI 弹窗应答权限请求：需要询问的操作直接拒绝。
        // 工作目录内的读取本就不触发权限检查（默认放行），不受影响。
        agent.permission().set_auto_deny(true);
    }
    if !subagents.iter().any(|s| s.name == "general") {
        subagents.insert(0, subagent::builtin_general_subagent(&cfg.main_model));
    }

    if !no_tools {
        // MCP 连接（并行，静默）
        let mcp_cfg = mcp::config::load()?;
        let connections = if !mcp_cfg.mcp_servers.is_empty() {
            mcp::connect_mcp_servers(&mcp_cfg).await
        } else {
            Vec::new()
        };

        let mcp_backends: Arc<Mutex<Vec<crate::mcp::McpToolBackend>>> =
            Arc::new(Mutex::new(Vec::new()));
        for conn in &connections {
            if let Some(backend) = &conn.backend {
                mcp_backends
                    .lock()
                    .map_err(|e| color_eyre::eyre::eyre!("{e}"))?
                    .push(crate::mcp::McpToolBackend {
                        server_name: conn.status.name.clone(),
                        backend: backend.clone(),
                        tools: conn.tools.clone(),
                    });
                for tool in &conn.tools {
                    agent.register_tool(Box::new(crate::mcp::McpTool::new(
                        &conn.status.name,
                        tool,
                        backend.clone(),
                    )));
                }
            }
        }

        // 注册 TaskTool
        let storage = storage::ChatStorage::new().await?;
        ensure_storage_ok(&storage)?;
        let current_session_shared = Arc::new(Mutex::new(None::<String>));
        let shared_config: Arc<Mutex<config::Config>> = Arc::new(Mutex::new(cfg.clone()));
        let task_tool = subagent::TaskTool::new(
            subagents.clone(),
            skills.clone(),
            storage.clone(),
            resolved.config.clone(),
            resolved.display.clone(),
            resolved.max_tokens,
            work_dir.display().to_string(),
            current_session_shared,
            mcp_backends,
            shared_config,
            Some(event_tx.clone()),
            agent.permission().clone(),
        );
        agent.register_tool(Box::new(task_tool));
    }

    agent
        .chat_stream(&message, event_tx.clone())
        .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;

    let mut in_reasoning = false;

    while let Some(event) = event_rx.recv().await {
        if !matches!(event, tui::AppEvent::AgentReasoningChunk(_)) && in_reasoning {
            println!();
            in_reasoning = false;
        }
        match event {
            tui::AppEvent::AgentChunk(text) => {
                print!("{}", text);
                let _ = std::io::stdout().flush();
            }
            tui::AppEvent::AgentReasoningChunk(text) => {
                if !in_reasoning {
                    print!("\n[thinking] ");
                    in_reasoning = true;
                }
                print!("{}", text);
                let _ = std::io::stdout().flush();
            }
            tui::AppEvent::ToolCallStart {
                name, arguments, ..
            } => {
                println!("\n[tool] {}({})", name, arguments);
                let _ = std::io::stdout().flush();
            }
            tui::AppEvent::ToolResult { name, result, .. } => {
                let preview = if result.chars().count() > 200 {
                    let end = result
                        .char_indices()
                        .nth(200)
                        .map(|(i, _)| i)
                        .unwrap_or(result.len());
                    format!("{}...", &result[..end])
                } else {
                    result
                };
                println!("[tool:result] {} → {}", name, preview);
                let _ = std::io::stdout().flush();
            }
            tui::AppEvent::AgentComplete { status, .. } => {
                println!();
                let _ = std::io::stdout().flush();
                match status {
                    tui::event::TaskStatus::Completed => {}
                    tui::event::TaskStatus::Interrupted => {
                        eprintln!("\n[已中断]");
                    }
                    tui::event::TaskStatus::Error => {
                        eprintln!("\n[错误]");
                    }
                }
                break;
            }
            _ => {}
        }
    }

    Ok(())
}

// ── TUI 启动（共享逻辑） ─────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn launch_tui(
    agent: Agent,
    resolved: config::ResolvedModel,
    cfg: config::Config,
    skills: Vec<skill::SkillInfo>,
    command_registry: CommandRegistry,
    subagents: Vec<subagent::SubagentConfig>,
    storage: storage::ChatStorage,
    shared: tui::app::AppSharedState,
    events: (tui::event::EventTx, tui::event::EventRx),
    enter_setup: bool,
    resume: bool,
) -> Result<()> {
    let (event_tx, _event_rx) = &events;
    let mcp_cfg = mcp::config::load()?;
    let initial_statuses = mcp::create_placeholder_statuses(&mcp_cfg);
    let mcp_event_tx = event_tx.clone();
    let mcp_cfg_clone = mcp_cfg.clone();
    tokio::spawn(async move {
        let connections = mcp::connect_mcp_servers(&mcp_cfg_clone).await;
        let _ = mcp_event_tx
            .send(tui::event::AppEvent::McpReady(connections))
            .await;
    });

    tui::terminal::install_panic_hook();
    let mut terminal = tui::terminal::init()?;

    let mut app = App::new(
        agent,
        resolved,
        storage,
        cfg,
        skills,
        command_registry,
        initial_statuses,
        subagents,
        shared,
        events,
    );
    if enter_setup {
        app.enter_setup();
    } else if resume {
        app.resume_last_session().await?;
    }
    app.run(&mut terminal).await?;

    tui::terminal::restore(&mut terminal)?;
    Ok(())
}

// ── TUI 模式 ─────────────────────────────────────────────────

async fn run_tui(
    cfg: config::Config,
    resolved: config::ResolvedModel,
    work_dir: &Path,
    yolo: bool,
    resume: bool,
) -> Result<()> {
    let (event_tx, event_rx) = tui::event::create_event_channel();
    let (mut agent, skills, command_registry, subagents) =
        build_agent(&resolved, &cfg, event_tx.clone(), work_dir)?;
    if yolo {
        // --yolo：跳过所有权限确认
        agent
            .permission()
            .set_mode(crate::permission::PermissionMode::Yolo);
    }

    let storage = storage::ChatStorage::new().await?;
    ensure_storage_ok(&storage)?;
    // 绑定存储到 PermissionManager，启用 session 级 DB 持久化
    agent.permission().with_storage_ref(storage.clone());

    let shared = tui::app::AppSharedState {
        current_session: Arc::new(Mutex::new(None::<String>)),
        mcp_backends: Arc::new(Mutex::new(Vec::new())),
        config: Arc::new(Mutex::new(cfg.clone())),
    };

    let task_tool = subagent::TaskTool::new(
        subagents.clone(),
        skills.clone(),
        storage.clone(),
        resolved.config.clone(),
        resolved.display.clone(),
        resolved.max_tokens,
        work_dir.display().to_string(),
        shared.current_session.clone(),
        shared.mcp_backends.clone(),
        shared.config.clone(),
        Some(event_tx.clone()),
        agent.permission().clone(),
    );
    agent.register_tool(Box::new(task_tool));

    launch_tui(
        agent,
        resolved,
        cfg,
        skills,
        command_registry,
        subagents,
        storage,
        shared,
        (event_tx, event_rx),
        false,
        resume,
    )
    .await
}

/// 首次运行（未配置）的 TUI 启动
async fn run_tui_setup(work_dir: &Path) -> Result<()> {
    let cfg = config::Config::default();
    let resolved = config::ResolvedModel {
        config: async_openai::config::OpenAIConfig::new(),
        model_id: String::new(),
        max_tokens: 16384,
        context_window: 131072,
        display: String::new(),
    };

    let (event_tx, event_rx) = tui::event::create_event_channel();

    // NeedsSetup 时 build_agent 会因空 model_id 产生问题，
    // 所以只用 build_agent_base + AskTool
    let (mut agent, skills, command_registry, mut subagents) =
        build_agent_base(&resolved, &cfg, work_dir)?;
    agent.register_tool(Box::new(AskTool::new(event_tx.clone())));

    if !subagents.iter().any(|s| s.name == "general") {
        subagents.insert(0, subagent::builtin_general_subagent(&cfg.main_model));
    }

    let storage = storage::ChatStorage::new().await?;
    ensure_storage_ok(&storage)?;
    let shared = tui::app::AppSharedState {
        current_session: Arc::new(Mutex::new(None::<String>)),
        mcp_backends: Arc::new(Mutex::new(Vec::new())),
        config: Arc::new(Mutex::new(cfg.clone())),
    };

    // 不注册 TaskTool（model 未配置）

    launch_tui(
        agent,
        resolved,
        cfg,
        skills,
        command_registry,
        subagents,
        storage,
        shared,
        (event_tx, event_rx),
        true,
        false,
    )
    .await
}

// ── 入口 ─────────────────────────────────────────────────────

fn resolve_work_dir(cli_work_dir: Option<&str>) -> Result<PathBuf> {
    if let Some(d) = cli_work_dir {
        std::env::set_current_dir(d)?;
    }
    Ok(Path::new(".").canonicalize()?)
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

    match cli.command {
        Some(Commands::Run {
            message,
            model,
            no_tools,
        }) => {
            let work_dir = resolve_work_dir(cli.work_dir.as_deref())?;
            run_non_interactive(message, model, no_tools, cli.yolo, &work_dir).await
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
