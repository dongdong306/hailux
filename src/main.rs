mod agent;
mod config;
mod mcp;
mod prompts;
mod storage;
mod tui;

use crate::agent::subagent;
use crate::agent::{Agent, BashTool, EditTool};
use crate::agent::{AskTool, GrepTool, ReadTool, TodoWriteTool};
use crate::agent::{CommandRegistry, SkillTool, skill};
use crate::agent::{GlobTool, WebFetchTool, WriteTool};
use color_eyre::Result;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tui::App;

fn build_agent(
    resolved: &config::ResolvedModel,
    cfg: &config::Config,
    event_tx: tui::event::EventTx,
) -> Result<(
    Agent,
    Vec<skill::SkillInfo>,
    CommandRegistry,
    Vec<subagent::SubagentConfig>,
)> {
    let mut agent = Agent::new(
        resolved.config.clone(),
        &resolved.model_id,
        resolved.max_tokens,
    );
    agent.register_tool(Box::new(BashTool));
    agent.register_tool(Box::new(ReadTool));
    agent.register_tool(Box::new(EditTool));
    agent.register_tool(Box::new(WriteTool));
    agent.register_tool(Box::new(WebFetchTool::new()));
    agent.register_tool(Box::new(GrepTool));
    agent.register_tool(Box::new(GlobTool));
    agent.register_tool(Box::new(TodoWriteTool));
    agent.register_tool(Box::new(AskTool::new(event_tx)));

    let work_dir_path = Path::new(".").canonicalize()?;

    let skills = skill::discover_skills(&work_dir_path).unwrap_or_default();
    if !skills.is_empty() {
        agent.register_tool(Box::new(SkillTool::new(skills.clone())));
    }

    let agent_md_entries = agent::agents_md::discover_agent_md(&work_dir_path);

    let mut subagents = subagent::discover_subagents(&work_dir_path).unwrap_or_default();
    if !subagents.iter().any(|s| s.name == "general") {
        subagents.insert(0, subagent::builtin_general_subagent(&cfg.main_model));
    }

    let system_prompt =
        prompts::build_system_prompt(&work_dir_path, &skills, &agent_md_entries, &subagents);
    agent.set_system_prompt(&system_prompt);

    let command_registry = CommandRegistry::discover(&work_dir_path).unwrap_or_default();

    Ok((agent, skills, command_registry, subagents))
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let load_result = config::load()?;
    let needs_setup = matches!(load_result, config::LoadResult::NeedsSetup);
    let cfg = match load_result {
        config::LoadResult::Ready(cfg) => cfg,
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

    let (event_tx, event_rx) = tui::event::create_event_channel();
    let (agent, skills, command_registry, subagents) =
        build_agent(&resolved, &cfg, event_tx.clone())?;
    let model_display = if needs_setup {
        String::new()
    } else {
        resolved.display.clone()
    };

    // 加载 MCP 配置
    let mcp_cfg = mcp::config::load()?;
    // 立即生成占位状态，UI 先行显示"连接中..."
    let initial_statuses = mcp::create_placeholder_statuses(&mcp_cfg);
    // 在后台异步连接 MCP 服务器
    let mcp_event_tx = event_tx.clone();
    let mcp_cfg_clone = mcp_cfg.clone();
    tokio::spawn(async move {
        let connections = mcp::connect_mcp_servers(&mcp_cfg_clone).await;
        let _ = mcp_event_tx
            .send(tui::event::AppEvent::McpReady(connections))
            .await;
    });

    let storage = storage::ChatStorage::new().await?;

    // 创建共享状态用于 TaskTool
    let current_session_shared = Arc::new(Mutex::new(None::<String>));
    let mcp_backends: Arc<Mutex<Vec<crate::mcp::McpToolBackend>>> =
        Arc::new(Mutex::new(Vec::new()));
    let shared_config: Arc<Mutex<config::Config>> = Arc::new(Mutex::new(cfg.clone()));

    // 注册 TaskTool（需要 subagent 配置和共享状态）
    // 注意：TaskTool 内部会 clone 这些共享状态，注册后 MCP backends 会在
    // handle_mcp_ready 中更新到同一个 Arc<Mutex<...>>
    let task_tool = subagent::TaskTool::new(
        subagents.clone(),
        skills.clone(),
        storage.clone(),
        resolved.config.clone(),
        resolved.display.clone(),
        resolved.max_tokens,
        std::env::current_dir()
            .map(|p| p.canonicalize().unwrap_or(p).display().to_string())
            .unwrap_or_default(),
        current_session_shared.clone(),
        mcp_backends.clone(),
        shared_config.clone(),
        Some(event_tx.clone()),
    );
    // agent 需要可变引用来注册工具
    let mut agent = agent;
    agent.register_tool(Box::new(task_tool));

    tui::terminal::install_panic_hook();
    let mut terminal = tui::terminal::init()?;

    let mut app = App::new(
        agent,
        model_display,
        resolved.context_window,
        storage,
        cfg,
        skills,
        command_registry,
        initial_statuses,
        event_tx,
        event_rx,
        subagents,
        resolved.config.clone(),
        resolved.display.clone(),
        resolved.max_tokens,
        current_session_shared,
        mcp_backends,
        shared_config,
    );
    if needs_setup {
        skill::ensure_default_skills();
        app.enter_setup();
    }
    app.run(&mut terminal).await?;

    tui::terminal::restore(&mut terminal)?;
    Ok(())
}
