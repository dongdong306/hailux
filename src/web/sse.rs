//! SSE handler：`POST /api/chat`、`POST /api/compact`。
//!
//! 每个请求创建独立的 `CoreEvent` 通道；事件循环把领域事件映射为
//! `ServerEvent` JSON 推送，同时完成 TUI 侧等价的持久化工作
//! （PersistMessage 落库、AgentComplete 写 runtime_meta、自动压缩）。
//!
//! 断连语义：客户端断开 → 流被 drop → `ConnectionGuard` 兜底拒绝
//! pending 权限/提问；`DropInterrupt` 中断仍在运行的 agent，防止
//! 上下文与 DB 脱钩（正常结束时 `completed = true`，不触发中断）。

use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};

use crate::agent::event::{CoreEvent, CoreEventTx, create_core_event_channel};
use crate::session::ChatSession;
use crate::storage::{MessageRole, StoredMessage};

use super::protocol::{self, ServerEvent};
use super::state::WebServerState;
use super::task_registry::ConnectionGuard;

/// 工具结果推送前端的长度上限（字符）
const TOOL_RESULT_MAX_CHARS: usize = 100_000;

pub async fn chat_handler(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<super::protocol::ChatRequest>,
) -> Response {
    let session_arc =
        match resolve_session(&state, req.session_id.as_deref(), req.work_dir.as_deref()).await {
            Ok(s) => s,
            Err(e) => return error_response(e),
        };

    let (core_tx, mut core_rx) = create_core_event_channel();
    let guard = ConnectionGuard::new(state.registry.clone());
    let message = req.message;
    let requested_session_id = req.session_id;
    let resolved = state.manager.resolved();
    let compact_threshold = state.manager.cfg().compact_threshold;

    let stream = async_stream::stream! {
        // per-session 串行化：同一会话的并发请求在此排队
        let mut session = session_arc.lock().await;
        session.bind_event_channel(core_tx.clone());

        // ── prompt 型斜杠命令展开（/init、自定义命令）────────
        // UI 型命令（/compact 等）由前端拦截，不经过此接口。
        // 对齐 TUI：展开后的完整提示词用于持久化与发送。
        let message = match session.command_registry.render_prompt_command(&message) {
            Some(rendered) => {
                // /init 需要写 AGENTS.md，规划模式会过滤写工具，提前拒绝（对齐 TUI 守卫）
                let cmd_name = crate::agent::parse_slash_input(&message).map(|(n, _)| n);
                if session.plan_mode()
                    && cmd_name.as_deref() == Some(crate::agent::command_def::INIT_COMMAND_NAME)
                {
                    yield sse_event(&ServerEvent::Error { message: "当前为只读规划模式，无法创建 AGENTS.md。请先退出规划模式，再运行 /init。".to_string() });
                    session.unbind_event_channel();
                    return;
                }
                rendered
            }
            None => message,
        };

        // ── 会话准备（对齐 TUI handle_user_message）──────────
        let session_id = match ensure_session(&mut session, &resolved.display, requested_session_id).await {
            Ok(id) => id,
            Err(e) => {
                yield sse_event(&ServerEvent::Error { message: e });
                return;
            }
        };

        if let Err(e) = persist_user_message(&session, &session_id, &message).await {
            yield sse_event(&ServerEvent::Error { message: format!("持久化用户消息失败: {e}") });
            return;
        }

        // 标题为空时用首条用户消息生成（对齐 TUI handle_user_message：截取前 50 字符）
        match session.storage().get_session_title(&session_id).await {
            Ok(Some(title)) if !title.is_empty() => {}
            _ => {
                let title: String = if message.chars().count() > 50 {
                    format!("{}...", message.chars().take(50).collect::<String>())
                } else {
                    message.chars().take(50).collect()
                };
                if !title.is_empty() {
                    let _ = session
                        .storage()
                        .update_session_title(&session_id, &title)
                        .await;
                }
            }
        }

        // ── 启动流式处理 ────────────────────────────────────
        let started = Instant::now();
        // 思考计时（对齐 TUI finalize_thinking_ms：思考→输出/工具调用切换时冻结分段，
        // PersistMessage 时求和写入 think_ms）
        let mut thinking_started: Option<Instant> = None;
        let mut think_total_ms: u64 = 0;
        if let Err(e) = session.send_message(&message, core_tx.clone()) {
            yield sse_event(&ServerEvent::Error { message: e.to_string() });
            return;
        }

        // 断连时中断 agent（正常结束置 completed = true 跳过）
        let mut interrupt_on_drop = DropInterrupt { session: session_arc.clone(), completed: false };

        let mut last_prompt: u32 = 0;
        let mut last_completion: u32 = 0;

        while let Some(event) = core_rx.recv().await {
            match event {
                CoreEvent::AgentChunk(text) => {
                    finalize_thinking(&mut thinking_started, &mut think_total_ms);
                    yield sse_event(&ServerEvent::AgentChunk { text });
                }
                CoreEvent::AgentReasoningChunk(text) => {
                    thinking_started.get_or_insert_with(Instant::now);
                    yield sse_event(&ServerEvent::AgentReasoningChunk { text });
                }
                CoreEvent::UsageUpdate { prompt_tokens, completion_tokens } => {
                    last_prompt = prompt_tokens;
                    last_completion = completion_tokens;
                    let _ = session.storage()
                        .update_session_usage(&session_id, prompt_tokens as i64, completion_tokens as i64)
                        .await;
                    yield sse_event(&ServerEvent::UsageUpdate {
                        prompt_tokens,
                        completion_tokens,
                        context_window: resolved.context_window,
                    });
                }
                CoreEvent::PersistMessage { msg, usage, display } => {
                    // 持久化（对齐 TUI PersistMessage 处理；不推送给前端）
                    let mut stored = crate::storage::to_stored_message(&msg);
                    if let Some((pt, ct)) = usage {
                        stored.prompt_tokens = Some(pt as i64);
                        stored.completion_tokens = Some(ct as i64);
                    }
                    if let Some(d) = display {
                        stored.runtime_meta = Some(d);
                    }
                    // 思考耗时求和写入（对齐 TUI：仅带 reasoning 的 assistant 消息）
                    finalize_thinking(&mut thinking_started, &mut think_total_ms);
                    if stored.role == crate::storage::MessageRole::Assistant
                        && stored.reasoning_content.is_some()
                        && think_total_ms > 0
                    {
                        stored.think_ms = Some(think_total_ms as i64);
                    }
                    think_total_ms = 0;
                    let _ = session.storage().append_message(&session_id, &stored).await;
                }
                CoreEvent::ToolCallStart { name, arguments, subagent_name } => {
                    finalize_thinking(&mut thinking_started, &mut think_total_ms);
                    yield sse_event(&ServerEvent::ToolCallStart {
                        name, arguments,
                        subagent: subagent_name,
                    });
                }
                CoreEvent::ToolResult { name, result, display, subagent_name } => {
                    let result = truncate_chars(&result, TOOL_RESULT_MAX_CHARS);
                    yield sse_event(&ServerEvent::ToolResult {
                        name, result, display,
                        subagent: subagent_name,
                    });
                }
                CoreEvent::PermissionRequest { request, response_tx, subagent_name } => {
                    let request_id = uuid::Uuid::new_v4().to_string();
                    guard.track_permission(request_id.clone(), response_tx);
                    yield sse_event(&ServerEvent::PermissionRequest {
                        request_id,
                        description: request.description,
                        patterns: request.always_patterns,
                        subagent: subagent_name,
                    });
                }
                CoreEvent::AskUser { questions, response_tx } => {
                    let request_id = uuid::Uuid::new_v4().to_string();
                    guard.track_ask(request_id.clone(), response_tx);
                    yield sse_event(&ServerEvent::AskUser { request_id, questions });
                }
                CoreEvent::AgentComplete { messages, status, .. } => {
                    // 同步 in-memory 上下文（对齐 TUI AgentComplete 处理）
                    if !messages.is_empty() {
                        session.sync_messages(messages);
                    }
                    let total_ms = started.elapsed().as_millis() as u64;
                    let _ = session.storage()
                        .update_last_assistant_runtime_meta(
                            &session_id,
                            &runtime_meta_json(total_ms, &resolved.display, protocol::status_str(&status)),
                        )
                        .await;
                    let _ = session.storage().touch_session(&session_id).await;
                    yield sse_event(&ServerEvent::AgentComplete {
                        status: protocol::status_str(&status).to_string(),
                        total_ms,
                        model: resolved.display.clone(),
                    });

                    // 自动压缩检查（对齐 TUI 阈值逻辑）；启动后继续消费压缩事件
                    if maybe_start_compaction(
                        &mut session, &core_tx, &session_id,
                        last_prompt + last_completion, resolved.context_window, compact_threshold,
                    ).await {
                        yield sse_event(&ServerEvent::Notice {
                            text: format!(
                                "上下文 token 已达 {}%，自动压缩",
                                (compact_threshold * 100.0) as u32
                            ),
                        });
                    } else {
                        break;
                    }
                }
                CoreEvent::CompactChunk(text) => {
                    yield sse_event(&ServerEvent::CompactChunk { text });
                }
                CoreEvent::CompactComplete { summary, session_id: compact_sid, usage } => {
                    let compacted_count = session.storage()
                        .count_active_messages(&compact_sid).await.unwrap_or(0) as usize;
                    let _ = session.storage().mark_messages_compacted(&compact_sid).await;
                    let _ = session.storage().set_compact_summary(&compact_sid, &summary).await;
                    if compact_sid == session_id {
                        session.apply_compaction_result(&summary);
                        if let Some(u) = usage {
                            let estimated = last_prompt
                                .saturating_sub(u.prompt_tokens)
                                .saturating_add(u.completion_tokens);
                            let _ = session.storage()
                                .update_session_usage(&session_id, estimated as i64, 0)
                                .await;
                        }
                    }
                    yield sse_event(&ServerEvent::CompactComplete {
                        summary_chars: summary.chars().count(),
                        compacted_count,
                    });
                    break;
                }
                CoreEvent::CompactError(msg) => {
                    yield sse_event(&ServerEvent::Error { message: format!("压缩失败: {msg}") });
                    break;
                }
            }
        }

        // 正常走完事件循环（或前置于 send 的 early return 不经过此处）
        interrupt_on_drop.completed = true;
        // 请求结束：解绑通道（下一次请求重新绑定）
        session.unbind_event_channel();
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// 手动压缩（POST /api/compact，SSE 响应）
pub async fn compact_handler(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<super::protocol::CompactRequest>,
) -> Response {
    let session_arc =
        match resolve_session(&state, Some(&req.session_id), req.work_dir.as_deref()).await {
            Ok(s) => s,
            Err(e) => return error_response(e),
        };

    let (core_tx, mut core_rx) = create_core_event_channel();
    let guard = ConnectionGuard::new(state.registry.clone());
    let requested_session_id = req.session_id;

    let stream = async_stream::stream! {
        let mut session = session_arc.lock().await;
        session.bind_event_channel(core_tx.clone());

        if session.current_session_id().as_deref() != Some(requested_session_id.as_str())
            && let Err(e) = session.switch_session(&requested_session_id).await
        {
            yield sse_event(&ServerEvent::Error { message: e.to_string() });
            return;
        }

        if session.messages_excluding_system_count() < 4 {
            yield sse_event(&ServerEvent::Notice { text: "消息太少，无需压缩".to_string() });
            return;
        }

        yield sse_event(&ServerEvent::Notice { text: "正在压缩上下文...".to_string() });
        if let Err(e) = session.request_compaction(core_tx.clone(), requested_session_id.clone()) {
            yield sse_event(&ServerEvent::Error { message: e.to_string() });
            return;
        }

        while let Some(event) = core_rx.recv().await {
            match event {
                CoreEvent::CompactChunk(text) => {
                    yield sse_event(&ServerEvent::CompactChunk { text });
                }
                CoreEvent::CompactComplete { summary, session_id: sid, usage } => {
                    let compacted_count = session.storage()
                        .count_active_messages(&sid).await.unwrap_or(0) as usize;
                    let _ = session.storage().mark_messages_compacted(&sid).await;
                    let _ = session.storage().set_compact_summary(&sid, &summary).await;
                    if sid == requested_session_id {
                        session.apply_compaction_result(&summary);
                        // 依据压缩请求自身的 usage 修正会话用量估算
                        if let (Some(u), Ok((pt, _))) = (
                            usage,
                            session.storage().get_session_usage(&sid).await,
                        ) {
                            let estimated = (pt as u32)
                                .saturating_sub(u.prompt_tokens)
                                .saturating_add(u.completion_tokens);
                            let _ = session
                                .storage()
                                .update_session_usage(&sid, estimated as i64, 0)
                                .await;
                        }
                    }
                    yield sse_event(&ServerEvent::CompactComplete {
                        summary_chars: summary.chars().count(),
                        compacted_count,
                    });
                    break;
                }
                CoreEvent::CompactError(msg) => {
                    yield sse_event(&ServerEvent::Error { message: format!("压缩失败: {msg}") });
                    break;
                }
                CoreEvent::PermissionRequest { request, response_tx, subagent_name } => {
                    let request_id = uuid::Uuid::new_v4().to_string();
                    guard.track_permission(request_id.clone(), response_tx);
                    yield sse_event(&ServerEvent::PermissionRequest {
                        request_id,
                        description: request.description,
                        patterns: request.always_patterns,
                        subagent: subagent_name,
                    });
                }
                CoreEvent::AskUser { questions, response_tx } => {
                    let request_id = uuid::Uuid::new_v4().to_string();
                    guard.track_ask(request_id.clone(), response_tx);
                    yield sse_event(&ServerEvent::AskUser { request_id, questions });
                }
                _ => {}
            }
        }
        session.unbind_event_channel();
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

// ── 内部辅助 ─────────────────────────────────────────────────

/// 流被 drop 时中断 agent（客户端断开且事件循环未正常结束）。
/// 中断在后台 task 中执行（Drop 不能等待异步锁）。
struct DropInterrupt {
    session: Arc<tokio::sync::Mutex<ChatSession>>,
    completed: bool,
}

impl Drop for DropInterrupt {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let session = self.session.clone();
        tokio::spawn(async move {
            let s = session.lock().await;
            s.interrupt();
            s.unbind_event_channel();
        });
    }
}

pub(crate) async fn resolve_session(
    state: &WebServerState,
    session_id: Option<&str>,
    work_dir: Option<&str>,
) -> Result<Arc<tokio::sync::Mutex<ChatSession>>, String> {
    // 优先级：显式 work_dir > session 归属目录 > 服务器默认目录
    let dir = if let Some(d) = work_dir {
        std::path::PathBuf::from(d)
    } else if let Some(sid) = session_id
        && let Ok(Some(dir)) = state.manager.storage().get_session_work_dir(sid).await
    {
        std::path::PathBuf::from(dir)
    } else {
        state.default_work_dir.clone()
    };

    state
        .manager
        .get_or_create(&dir)
        .await
        .map_err(|e| e.to_string())
}

/// 确保存在当前会话 ID：请求带 session_id → 切换；否则沿用当前或新建
async fn ensure_session(
    session: &mut ChatSession,
    model_display: &str,
    requested: Option<String>,
) -> Result<String, String> {
    if let Some(id) = requested {
        if session.current_session_id().as_deref() != Some(id.as_str()) {
            session
                .switch_session(&id)
                .await
                .map_err(|e| e.to_string())?;
        }
        return Ok(id);
    }
    if let Some(current) = session.current_session_id() {
        return Ok(current);
    }
    session
        .create_session(model_display)
        .await
        .map_err(|e| e.to_string())
}

async fn persist_user_message(
    session: &ChatSession,
    session_id: &str,
    message: &str,
) -> Result<(), String> {
    let stored = StoredMessage {
        role: MessageRole::User,
        content: message.to_string(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        prompt_tokens: None,
        completion_tokens: None,
        runtime_meta: None,
        think_ms: None,
        compacted: false,
    };
    session
        .storage()
        .append_message(session_id, &stored)
        .await
        .map_err(|e| e.to_string())
}

fn runtime_meta_json(total_ms: u64, model: &str, status: &str) -> String {
    serde_json::json!({
        "total_ms": total_ms,
        "model": model,
        "status": status,
    })
    .to_string()
}

/// 冻结当前思考分段耗时（对齐 TUI finalize_thinking_ms：思考→输出/工具调用切换时结算）
fn finalize_thinking(thinking_started: &mut Option<Instant>, think_total_ms: &mut u64) {
    if let Some(t) = thinking_started.take() {
        *think_total_ms += t.elapsed().as_millis() as u64;
    }
}

/// 超过阈值时启动压缩（返回 true = 已启动，调用方继续消费事件）
async fn maybe_start_compaction(
    session: &mut ChatSession,
    core_tx: &CoreEventTx,
    session_id: &str,
    total_context: u32,
    context_window: u32,
    compact_threshold: f32,
) -> bool {
    let threshold = (context_window as f32 * compact_threshold) as u32;
    if total_context <= threshold {
        return false;
    }
    session
        .request_compaction(core_tx.clone(), session_id.to_string())
        .is_ok()
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let safe: String = s.chars().take(max).collect();
        format!("{safe}...(truncated)")
    } else {
        s.to_string()
    }
}

fn sse_event(event: &ServerEvent) -> Result<Event, std::convert::Infallible> {
    let json = serde_json::to_string(event).unwrap_or_else(|_| {
        serde_json::json!({"type": "Error", "message": "serialize failed"}).to_string()
    });
    Ok(Event::default().data(json))
}

fn error_response(msg: String) -> Response {
    (StatusCode::BAD_REQUEST, msg).into_response()
}
