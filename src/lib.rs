//! hailux library entry: module declarations and startup orchestration.
//!
//! `main.rs` is a thin CLI entry point; all reusable logic lives here,
//! shared by the binary entry and the `tests/` integration tests.

pub mod agent;
pub mod config;
pub mod mcp;
pub mod permission;
pub mod prompts;
pub mod session;
pub mod share;
pub mod storage;
pub mod tui;
pub mod updater;

#[cfg(feature = "web")]
pub mod web;

use crate::agent::subagent;
use crate::agent::{Agent, EditTool, ShellTool};
use crate::agent::{CommandRegistry, SkillTool, skill};
use crate::agent::{GlobTool, WebFetchTool, WriteTool};
use crate::agent::{GrepTool, QuestionTool, ReadTool, TodoWriteTool};
use color_eyre::Result;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tui::App;

// ── Storage startup check ─────────────────────────────────────────────

/// Refuses startup when DB migration fails and prompts the user to rebuild
pub fn ensure_storage_ok(storage: &storage::ChatStorage) -> Result<()> {
    if let Some(err) = storage.migration_error() {
        color_eyre::eyre::bail!(
            "数据库迁移失败：{err}\n请使用 `hailux --rebuild-db` 重建数据库（将清空全部历史记录）。"
        );
    }
    Ok(())
}

/// `hailux --rebuild-db`: backs up the old DB file, then rebuilds the database
pub async fn rebuild_database() -> Result<()> {
    let mut storage = storage::ChatStorage::new().await?;
    let backup_path = storage.rebuild().await?;
    println!("数据库已重建（历史记录已清空）。");
    if let Some(path) = backup_path {
        println!("原数据库已备份至：{}", path.display());
    }
    Ok(())
}

// ── Agent construction ───────────────────────────────────────────────

fn build_permission_rules(cfg: &config::Config) -> Vec<crate::permission::PermissionRule> {
    use crate::permission::rules_from_config_table;
    let mut rules = Vec::new();
    rules.extend(rules_from_config_table("shell", &cfg.permission.shell));
    rules.extend(rules_from_config_table("read", &cfg.permission.read));
    rules.extend(rules_from_config_table("edit", &cfg.permission.edit));
    rules.extend(rules_from_config_table("write", &cfg.permission.write));
    rules.extend(rules_from_config_table("mcp", &cfg.permission.mcp));
    rules.extend(rules_from_config_table(
        "external_directory",
        &cfg.permission.external_directory,
    ));
    // Note: do not append a "default Ask" rule here: `evaluate` already defaults to Ask on no
    // match; an appended rule would win by being last and shadow specific deny/allow rules.
    rules
}

pub fn build_agent_base(
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
        &resolved.display,
        resolved.max_tokens,
        resolved.supports_vision,
        resolved.native_reasoning,
        cfg.reasoning_effort,
        pm,
        &work_dir.display().to_string(),
        cfg.retry_config(),
    );
    #[cfg(windows)]
    crate::agent::prewarm_shell_description();
    agent.register_tool(Box::new(ShellTool));
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

pub fn build_agent(
    resolved: &config::ResolvedModel,
    cfg: &config::Config,
    work_dir: &Path,
) -> Result<(
    Agent,
    Vec<skill::SkillInfo>,
    CommandRegistry,
    Vec<subagent::SubagentConfig>,
)> {
    let (mut agent, skills, command_registry, mut subagents) =
        build_agent_base(resolved, cfg, work_dir)?;

    agent.register_tool(Box::new(QuestionTool::new(agent.event_hub().clone())));

    if !subagents.iter().any(|s| s.name == "general") {
        subagents.insert(0, subagent::builtin_general_subagent(&cfg.main_model));
    }

    Ok((agent, skills, command_registry, subagents))
}

// ── Non-interactive mode ───────────────────────────────────────────────

pub async fn run_non_interactive(
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

    let (event_tx, mut event_rx) = agent::event::create_core_event_channel();

    let (mut agent, skills, _, mut subagents) = build_agent_base(&resolved, &cfg, work_dir)?;
    // TaskTool forwards events into this request's channel (no consumer here; ignore silently)
    agent.event_hub().set(event_tx.clone());
    if yolo {
        // --yolo: skip all permission confirmations
        agent
            .permission()
            .set_mode(crate::permission::PermissionMode::Yolo);
    } else {
        // Non-interactive mode has no TUI dialog to answer permission prompts:
        // operations that need asking are denied outright. Reads inside the
        // work dir never trigger permission checks (allowed by default) and
        // are unaffected.
        agent.permission().set_auto_deny(true);
    }
    if !subagents.iter().any(|s| s.name == "general") {
        subagents.insert(0, subagent::builtin_general_subagent(&cfg.main_model));
    }

    if !no_tools {
        // MCP connections (parallel, silent)
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

        // Register TaskTool
        let storage = storage::ChatStorage::new().await?;
        ensure_storage_ok(&storage)?;
        let current_session_shared = Arc::new(Mutex::new(None::<String>));
        let shared_config: Arc<Mutex<config::Config>> = Arc::new(Mutex::new(cfg.clone()));
        let task_tool = subagent::TaskTool::new(
            subagents.clone(),
            skills.clone(),
            storage.clone(),
            resolved.display.clone(),
            work_dir.display().to_string(),
            current_session_shared,
            mcp_backends,
            shared_config,
            agent.event_hub().clone(),
            agent.permission().clone(),
        );
        agent.register_tool(Box::new(task_tool));
    }

    // No session: non-interactive runs never persist (the reminder event
    // carries an empty session_id and no consumer acts on it here)
    agent
        .chat_stream("", &message, Vec::new(), event_tx.clone())
        .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;

    let mut in_reasoning = false;

    while let Some(event) = event_rx.recv().await {
        if !matches!(event, agent::event::CoreEvent::AgentReasoningChunk(_)) && in_reasoning {
            println!();
            in_reasoning = false;
        }
        match event {
            agent::event::CoreEvent::AgentChunk(text) => {
                print!("{}", text);
                let _ = std::io::stdout().flush();
            }
            agent::event::CoreEvent::AgentRetry {
                attempt,
                max_retries,
                delay_ms,
                reason,
            } => {
                // stdout cannot retract already-printed output; note the retry on stderr only
                eprintln!(
                    "\n[请求失败，{delay_ms}ms 后进行第 {attempt}/{max_retries} 次重试：{reason}]"
                );
            }
            agent::event::CoreEvent::AgentReasoningChunk(text) => {
                if !in_reasoning {
                    print!("\n[thinking] ");
                    in_reasoning = true;
                }
                print!("{}", text);
                let _ = std::io::stdout().flush();
            }
            agent::event::CoreEvent::ToolCallStart {
                name, arguments, ..
            } => {
                println!("\n[tool] {}({})", name, arguments);
                let _ = std::io::stdout().flush();
            }
            agent::event::CoreEvent::ToolResult { name, result, .. } => {
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
            agent::event::CoreEvent::AgentComplete { status, .. } => {
                println!();
                let _ = std::io::stdout().flush();
                match status {
                    agent::event::TaskStatus::Completed => {}
                    agent::event::TaskStatus::Interrupted => {
                        eprintln!("\n[已中断]");
                    }
                    agent::event::TaskStatus::Error => {
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

// ── TUI launch (shared logic) ─────────────────────────────────────

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
    core_tx: agent::event::CoreEventTx,
    core_rx: agent::event::CoreEventRx,
    enter_setup: bool,
    resume: bool,
) -> Result<()> {
    let (event_tx, _event_rx) = &events;
    // Forward domain events → AppEvent::Core into the terminal event loop
    tui::event::spawn_core_forwarder(core_rx, event_tx.clone());

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
        core_tx,
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

// ── TUI mode ─────────────────────────────────────────────────

pub async fn run_tui(
    cfg: config::Config,
    resolved: config::ResolvedModel,
    work_dir: &Path,
    yolo: bool,
    resume: bool,
) -> Result<()> {
    let (event_tx, event_rx) = tui::event::create_event_channel();
    let (core_tx, core_rx) = agent::event::create_core_event_channel();
    let (mut agent, skills, command_registry, subagents) = build_agent(&resolved, &cfg, work_dir)?;
    // The TUI channel lives as long as the process, so binding once is enough
    agent.event_hub().set(core_tx.clone());
    if yolo {
        // --yolo: skip all permission confirmations
        agent
            .permission()
            .set_mode(crate::permission::PermissionMode::Yolo);
    }

    let storage = storage::ChatStorage::new().await?;
    ensure_storage_ok(&storage)?;
    // Bind storage to the PermissionManager to enable session-level DB persistence
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
        resolved.display.clone(),
        work_dir.display().to_string(),
        shared.current_session.clone(),
        shared.mcp_backends.clone(),
        shared.config.clone(),
        agent.event_hub().clone(),
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
        core_tx,
        core_rx,
        false,
        resume,
    )
    .await
}

/// TUI launch for a first run (unconfigured)
pub async fn run_tui_setup(work_dir: &Path) -> Result<()> {
    let cfg = config::Config::default();
    let resolved = config::ResolvedModel {
        config: async_openai::config::OpenAIConfig::new(),
        model_id: String::new(),
        max_tokens: 16384,
        context_window: 131072,
        display: String::new(),
        supports_vision: false,
        native_reasoning: false,
    };

    let (event_tx, event_rx) = tui::event::create_event_channel();
    let (core_tx, core_rx) = agent::event::create_core_event_channel();

    // During NeedsSetup, build_agent would break on the empty model_id,
    // so only build_agent_base + QuestionTool is used here.
    let (mut agent, skills, command_registry, mut subagents) =
        build_agent_base(&resolved, &cfg, work_dir)?;
    agent.register_tool(Box::new(QuestionTool::new(agent.event_hub().clone())));
    agent.event_hub().set(core_tx.clone());

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

    // Do not register TaskTool (model not configured)

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
        core_tx,
        core_rx,
        true,
        false,
    )
    .await
}

// ── Work dir resolution ─────────────────────────────────────────────

pub fn resolve_work_dir(cli_work_dir: Option<&str>) -> Result<PathBuf> {
    if let Some(d) = cli_work_dir {
        std::env::set_current_dir(d)?;
    }
    Ok(Path::new(".").canonicalize()?)
}
