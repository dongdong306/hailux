//! Background run tasks: owners of the LLM turn / compaction event loops.
//!
//! Unlike the previous design (the loop ran inside the SSE stream and a
//! disconnect killed the task), a runner is an independent `tokio::spawn` task
//! whose lifetime is unrelated to any SSE connection — a disconnect only loses
//! one observer. Events fan out through the `SessionBus` (replay buffer +
//! broadcast).
//!
//! Responsibilities (same event-consumption semantics as the TUI):
//! - Hold the session lock (serialize same-session, parallelize cross-session)
//! - Persist everything: PersistMessage / UsageUpdate / runtime_meta / auto
//!   compaction — even with zero observers
//! - Advance the durable watermark after each DB write completes (see the
//!   bus.rs module docs for the history/replay complement rule)
//! - Register permission/ask requests in the TaskRegistry (run-scoped; a
//!   disconnect no longer auto-denies)
//! - End with a RunComplete sentinel + sweep pending requests; abnormal exits
//!   are covered by `RunFinishGuard::drop` so observer streams can close

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;

use crate::agent::event::{CoreEvent, CoreEventTx, create_core_event_channel};
use crate::agent::media;
use crate::session::ChatSession;
use crate::storage::{MessageRole, StoredMessage};
use crate::web::protocol::{self, ServerEvent};
use crate::web::state::WebServerState;

use super::bus::SessionBus;
use super::task_registry::TaskRegistry;

/// Max length of a tool result pushed to the frontend (in chars)
const TOOL_RESULT_MAX_CHARS: usize = 100_000;

/// Delay before the orphaned tool_calls repair on interruption: give the
/// agent's PersistMessage wrap-up a chance to land first (the repair itself
/// is idempotent)
const INTERRUPT_REPAIR_DELAY: Duration = Duration::from_secs(2);

/// Run finish guard: `finish()` marks a normal end; Drop covers panics and
/// early returns so subscriber streams always receive a sentinel and close.
struct RunFinishGuard {
    bus: Arc<SessionBus>,
    registry: Arc<TaskRegistry>,
    session_id: String,
    done: bool,
}

impl RunFinishGuard {
    fn new(bus: Arc<SessionBus>, registry: Arc<TaskRegistry>, session_id: String) -> Self {
        Self {
            bus,
            registry,
            session_id,
            done: false,
        }
    }

    /// Normal finish: sweep pending requests, publish the sentinel, mark the
    /// bus idle, release the run claim.
    fn finish(&mut self) {
        if self.done {
            return;
        }
        self.registry.cancel_session(&self.session_id);
        self.bus
            .publish(ServerEvent::RunComplete { abnormal: false });
        self.bus.end_run();
        self.bus.release_run_claim();
        self.done = true;
    }
}

impl Drop for RunFinishGuard {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        self.registry.cancel_session(&self.session_id);
        self.bus
            .publish(ServerEvent::RunComplete { abnormal: true });
        self.bus.end_run();
        self.bus.release_run_claim();
    }
}

/// Chat turn: the full event loop (from sending the user message through
/// AgentComplete / auto compaction). The caller (chat handler) has resolved
/// the instance and bus and holds the concurrency permit.
///
/// Ordering rule for persisted events: publish first, write to DB, then
/// `bus.advance_durable(bus.last_seq())` — the watermark may only cross event
/// seqs whose DB effects have fully landed, otherwise clients subscribing with
/// `since = head_seq` after completion would replay already-rendered events.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_chat_task(
    state: Arc<WebServerState>,
    session_arc: Arc<tokio::sync::Mutex<ChatSession>>,
    session_id: String,
    message: String,
    attachments: Vec<media::Attachment>,
    mentions: Vec<protocol::ChatMention>,
    bus: Arc<SessionBus>,
    permit: tokio::sync::OwnedSemaphorePermit,
) {
    let resolved = state.manager.resolved();
    let compact_threshold = state.manager.cfg().compact_threshold;
    let registry = state.registry.clone();
    let _permit = permit;
    let mut guard = RunFinishGuard::new(bus.clone(), registry.clone(), session_id.clone());

    let mut session = session_arc.lock().await;
    // begin_run after taking the lock: a queued runner must not clear the
    // replay buffer of the turn currently holding it
    bus.begin_run();

    // Mode reconciliation: YOLO/Plan/reasoning effort/model are server-level global
    // state; instances holding the lock are skipped during broadcast (see
    // SessionManager::set_global_plan / apply_model_config), so sync as a fallback
    // at each request entry, also covering invalidate-rebuild and similar cases.
    session.set_yolo(state.manager.global_yolo());
    session.set_plan_mode(state.manager.global_plan());
    session.set_reasoning_effort(state.manager.global_effort());
    if session.current_model_display() != resolved.display {
        session.switch_model(&resolved);
    }

    // ── Prompt-style slash command expansion (/init, custom commands)────────
    // UI-style commands (/compact etc.) are intercepted by the frontend and
    // never reach this endpoint.
    // Align with TUI: the fully expanded prompt is used for persistence and sending.
    let message = match session.command_registry.render_prompt_command(&message) {
        Some(rendered) => {
            // /init writes AGENTS.md and plan mode filters out write tools, so reject early (align with TUI guard)
            let cmd_name = crate::agent::parse_slash_input(&message).map(|(n, _)| n);
            if session.plan_mode()
                && cmd_name.as_deref() == Some(crate::agent::command_def::INIT_COMMAND_NAME)
            {
                bus.publish(ServerEvent::Error {
                    message:
                        "当前为只读规划模式，无法创建 AGENTS.md。请先退出规划模式，再运行 /init。"
                            .to_string(),
                });
                session.unbind_event_channel();
                guard.finish();
                return;
            }
            rendered
        }
        None => message,
    };

    // Defensive fallback: the instance was bound to this session at creation
    // (resolve_chat_target); a mismatch means broken instance state — rebind.
    if session.current_session_id().as_deref() != Some(session_id.as_str())
        && let Err(e) = session.switch_session(&session_id).await
    {
        bus.publish(ServerEvent::Error {
            message: e.to_string(),
        });
        session.unbind_event_channel();
        guard.finish();
        return;
    }

    let _ = bus.publish(ServerEvent::SessionStarted {
        session_id: session_id.clone(),
    });

    if let Err(e) =
        persist_user_message(&session, &session_id, &message, &attachments, &mentions).await
    {
        bus.publish(ServerEvent::Error {
            message: format!("持久化用户消息失败: {e}"),
        });
        session.unbind_event_channel();
        guard.finish();
        return;
    }

    // Generate the title from the first user message when empty (align with TUI handle_user_message: take first 50 chars)
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
                bus.advance_durable(bus.last_seq());
            }
        }
    }

    // ── Start streaming ────────────────────────────────────
    let started = Instant::now();
    // Thinking timer (align with TUI finalize_thinking_ms: freeze the segment
    // on thinking→output/tool-call switch, sum into think_ms at PersistMessage)
    let mut thinking_started: Option<Instant> = None;
    let mut think_total_ms: u64 = 0;
    {
        let (tx, mut rx) = create_core_event_channel();
        session.bind_event_channel(tx.clone());
        if let Err(e) = session.send_message(&message, attachments, tx.clone()) {
            bus.publish(ServerEvent::Error {
                message: e.to_string(),
            });
            session.unbind_event_channel();
            guard.finish();
            return;
        }

        let mut last_prompt: u32 = 0;
        let mut last_completion: u32 = 0;
        // Auto-compaction start time (elapsed measured at CompactComplete)
        let mut compact_started = Instant::now();
        let mut interrupted = false;

        while let Some(event) = rx.recv().await {
            match event {
                CoreEvent::AgentChunk(text) => {
                    finalize_thinking(&mut thinking_started, &mut think_total_ms);
                    bus.publish(ServerEvent::AgentChunk { text });
                }
                CoreEvent::AgentReasoningChunk(text) => {
                    thinking_started.get_or_insert_with(Instant::now);
                    bus.publish(ServerEvent::AgentReasoningChunk { text });
                }
                CoreEvent::AgentRetry {
                    attempt,
                    max_retries,
                    delay_ms,
                    reason,
                } => {
                    // Thinking time of failed attempts is discarded along with their
                    // content (align with TUI: pop the AgentThinking cell without
                    // settling think_ms)
                    finalize_thinking(&mut thinking_started, &mut think_total_ms);
                    think_total_ms = 0;
                    bus.publish(ServerEvent::AgentRetry {
                        attempt,
                        max_retries,
                        delay_ms,
                        reason,
                    });
                }
                CoreEvent::UsageUpdate {
                    prompt_tokens,
                    completion_tokens,
                    cached_tokens,
                } => {
                    last_prompt = prompt_tokens;
                    last_completion = completion_tokens;
                    // Write-first (opposite of AgentComplete): the session usage row is
                    // never re-rendered from history, and covering it with the watermark
                    // prevents replayed deltas from double-counting client counters
                    let _ = session
                        .storage()
                        .update_session_usage(
                            &session_id,
                            prompt_tokens as i64,
                            completion_tokens as i64,
                        )
                        .await;
                    bus.advance_durable(bus.last_seq());
                    bus.publish(ServerEvent::UsageUpdate {
                        prompt_tokens,
                        completion_tokens,
                        cached_tokens,
                        context_window: resolved.context_window,
                    });
                }
                CoreEvent::PersistMessage {
                    msg,
                    usage,
                    model,
                    display,
                } => {
                    // Persist (align with TUI PersistMessage handling; not pushed to the frontend)
                    let mut stored = crate::storage::to_stored_message(&msg);
                    if let Some(u) = usage {
                        stored.prompt_tokens = Some(u.prompt_tokens as i64);
                        stored.completion_tokens = Some(u.completion_tokens as i64);
                        stored.cached_tokens = Some(u.cached_tokens as i64);
                    }
                    // Usage stats attributed to the per-message model (streaming snapshot values carried by the event)
                    stored.model = Some(model);
                    if let Some(d) = display {
                        stored.runtime_meta = Some(d);
                    }
                    // Write the summed thinking time (align with TUI: only assistant messages with reasoning)
                    finalize_thinking(&mut thinking_started, &mut think_total_ms);
                    if stored.role == crate::storage::MessageRole::Assistant
                        && stored.reasoning_content.is_some()
                        && think_total_ms > 0
                    {
                        stored.think_ms = Some(think_total_ms as i64);
                    }
                    think_total_ms = 0;
                    let _ = session.storage().append_message(&session_id, &stored).await;
                    bus.advance_durable(bus.last_seq());
                }
                CoreEvent::ToolCallStart {
                    name,
                    arguments,
                    subagent_name,
                    subagent_index,
                } => {
                    finalize_thinking(&mut thinking_started, &mut think_total_ms);
                    bus.publish(ServerEvent::ToolCallStart {
                        name,
                        arguments,
                        subagent: subagent_name,
                        subagent_index,
                    });
                }
                CoreEvent::ToolResult {
                    name,
                    result,
                    display,
                    subagent_name,
                    subagent_index,
                } => {
                    let result = truncate_chars(&result, TOOL_RESULT_MAX_CHARS);
                    bus.publish(ServerEvent::ToolResult {
                        name,
                        result,
                        display,
                        subagent: subagent_name,
                        subagent_index,
                    });
                }
                CoreEvent::PermissionRequest {
                    request,
                    response_tx,
                    subagent_name,
                } => {
                    register_permission_request(
                        &registry,
                        &bus,
                        &session_id,
                        response_tx,
                        request.description,
                        request.always_patterns,
                        subagent_name,
                    );
                }
                CoreEvent::AskUser {
                    questions,
                    response_tx,
                } => {
                    register_ask_request(&registry, &bus, &session_id, response_tx, questions);
                }
                CoreEvent::AgentComplete {
                    messages,
                    status,
                    model,
                    reasoning_effort,
                    ..
                } => {
                    // Sync the in-memory context (align with TUI AgentComplete handling)
                    if !messages.is_empty() {
                        session.sync_messages(messages);
                    }
                    let total_ms = started.elapsed().as_millis() as u64;
                    interrupted = status == crate::agent::event::TaskStatus::Interrupted;
                    // Publish before the runtime_meta write — see the ordering rule
                    // in this function's doc; otherwise a post-completion subscriber
                    // with since=head_seq would replay this and duplicate the done row
                    bus.publish(ServerEvent::AgentComplete {
                        status: protocol::status_str(&status).to_string(),
                        total_ms,
                        model: model.clone(),
                    });
                    // runtime_meta uses the snapshot model/effort carried by the event (mid-turn switches have no effect)
                    let _ = session
                        .storage()
                        .update_last_assistant_runtime_meta(
                            &session_id,
                            &runtime_meta_json(
                                total_ms,
                                &model,
                                reasoning_effort.as_str(),
                                protocol::status_str(&status),
                            ),
                        )
                        .await;
                    let _ = session.storage().touch_session(&session_id).await;
                    bus.advance_durable(bus.last_seq());

                    // Auto-compaction check (align with TUI threshold logic); after starting, keep consuming compaction events
                    if maybe_start_compaction(
                        &mut session,
                        &tx,
                        &session_id,
                        last_prompt + last_completion,
                        resolved.context_window,
                        compact_threshold,
                    )
                    .await
                    {
                        compact_started = Instant::now();
                        bus.publish(ServerEvent::CompactStart {
                            text: format!(
                                "上下文 token 已达 {}%，自动压缩",
                                (compact_threshold * 100.0) as u32
                            ),
                        });
                    } else {
                        break;
                    }
                }
                CoreEvent::CompactChunk(_) => {
                    // Align with TUI: the summary stream only drives the status animation; its content is not displayed
                }
                CoreEvent::CompactComplete {
                    summary,
                    session_id: compact_sid,
                    usage,
                } => {
                    let compacted_count = session
                        .storage()
                        .count_active_messages(&compact_sid)
                        .await
                        .unwrap_or(0) as usize;
                    // Post-compaction context estimate (align with TUI: context_prompt - compact_prompt + compact_completion)
                    let mut context_tokens: Option<u32> = None;
                    if compact_sid == session_id {
                        session.apply_compaction_result(&summary);
                        if let Some(u) = usage {
                            let estimated = last_prompt
                                .saturating_sub(u.prompt_tokens)
                                .saturating_add(u.completion_tokens);
                            context_tokens = Some(estimated);
                        }
                    }
                    // Publish before persisting — same ordering rule as AgentComplete
                    bus.publish(ServerEvent::CompactComplete {
                        compacted_count,
                        total_ms: compact_started.elapsed().as_millis() as u64,
                        context_tokens,
                    });
                    let _ = session
                        .storage()
                        .mark_messages_compacted(&compact_sid)
                        .await;
                    let _ = session
                        .storage()
                        .set_compact_summary(&compact_sid, &summary)
                        .await;
                    if compact_sid == session_id
                        && let Some(ctx) = context_tokens
                    {
                        let _ = session
                            .storage()
                            .update_session_usage(&session_id, ctx as i64, 0)
                            .await;
                    }
                    bus.advance_durable(bus.last_seq());
                    break;
                }
                CoreEvent::CompactError(msg) => {
                    bus.publish(ServerEvent::Error {
                        message: format!("压缩失败: {msg}"),
                    });
                    break;
                }
            }
        }

        session.unbind_event_channel();
        if interrupted {
            // Belt-and-braces: after an interrupt, dangling tool_call stubs have
            // normally already persisted via PersistMessage (the runner survives);
            // the delayed idempotent repair covers any residual gap. Non-blocking.
            let repair_session = session_arc.clone();
            tokio::spawn(async move {
                tokio::time::sleep(INTERRUPT_REPAIR_DELAY).await;
                let s = repair_session.lock().await;
                if let Some(session_id) = s.current_session_id()
                    && let Err(e) = s.storage().repair_orphaned_tool_calls(&session_id).await
                {
                    eprintln!("[warn] repair_orphaned_tool_calls: {e}");
                }
            });
        }
    }

    guard.finish();
}

/// Manual compaction task (POST /api/compact).
pub(super) async fn run_compact_task(
    state: Arc<WebServerState>,
    session_arc: Arc<tokio::sync::Mutex<ChatSession>>,
    session_id: String,
    bus: Arc<SessionBus>,
    permit: tokio::sync::OwnedSemaphorePermit,
) {
    let registry = state.registry.clone();
    let _permit = permit;
    let mut guard = RunFinishGuard::new(bus.clone(), registry.clone(), session_id.clone());

    let mut session = session_arc.lock().await;
    bus.begin_run();

    // Defensive fallback, same as chat: the instance should already be bound
    // to this session.
    if session.current_session_id().as_deref() != Some(session_id.as_str())
        && let Err(e) = session.switch_session(&session_id).await
    {
        bus.publish(ServerEvent::Error {
            message: e.to_string(),
        });
        session.unbind_event_channel();
        guard.finish();
        return;
    }

    if session.messages_excluding_system_count() < 4 {
        bus.publish(ServerEvent::Notice {
            text: "消息太少，无需压缩".to_string(),
        });
        session.unbind_event_channel();
        guard.finish();
        return;
    }

    // Align with TUI compact_conversation: first emit a compaction status line
    // (CompactStreaming); the summary stream is not shown and the whole line is
    // replaced by the completion marker when done
    let compact_started = Instant::now();
    bus.publish(ServerEvent::CompactStart {
        text: "正在压缩上下文...".to_string(),
    });
    let (tx, mut rx) = create_core_event_channel();
    session.bind_event_channel(tx.clone());
    if let Err(e) = session.request_compaction(tx.clone(), session_id.clone()) {
        bus.publish(ServerEvent::Error {
            message: e.to_string(),
        });
        session.unbind_event_channel();
        guard.finish();
        return;
    }

    while let Some(event) = rx.recv().await {
        match event {
            CoreEvent::CompactChunk(_) => {
                // Align with TUI: the summary stream only drives the status animation; its content is not displayed
            }
            CoreEvent::CompactComplete {
                summary,
                session_id: sid,
                usage,
            } => {
                let compacted_count = session
                    .storage()
                    .count_active_messages(&sid)
                    .await
                    .unwrap_or(0) as usize;
                // Post-compaction context estimate (align with TUI: session_prompt - compact_prompt + compact_completion)
                let mut context_tokens: Option<u32> = None;
                if sid == session_id {
                    session.apply_compaction_result(&summary);
                    // Refine the session usage estimate using the compaction request's own usage
                    if let (Some(u), Ok((pt, _))) =
                        (usage, session.storage().get_session_usage(&sid).await)
                    {
                        let estimated = (pt as u32)
                            .saturating_sub(u.prompt_tokens)
                            .saturating_add(u.completion_tokens);
                        context_tokens = Some(estimated);
                    }
                }
                // Publish before persisting — same ordering rule as AgentComplete
                bus.publish(ServerEvent::CompactComplete {
                    compacted_count,
                    total_ms: compact_started.elapsed().as_millis() as u64,
                    context_tokens,
                });
                let _ = session.storage().mark_messages_compacted(&sid).await;
                let _ = session.storage().set_compact_summary(&sid, &summary).await;
                if sid == session_id
                    && let Some(ctx) = context_tokens
                {
                    let _ = session
                        .storage()
                        .update_session_usage(&sid, ctx as i64, 0)
                        .await;
                }
                bus.advance_durable(bus.last_seq());
                break;
            }
            CoreEvent::CompactError(msg) => {
                bus.publish(ServerEvent::Error {
                    message: format!("压缩失败: {msg}"),
                });
                break;
            }
            CoreEvent::PermissionRequest {
                request,
                response_tx,
                subagent_name,
            } => {
                register_permission_request(
                    &registry,
                    &bus,
                    &session_id,
                    response_tx,
                    request.description,
                    request.always_patterns,
                    subagent_name,
                );
            }
            CoreEvent::AskUser {
                questions,
                response_tx,
            } => {
                register_ask_request(&registry, &bus, &session_id, response_tx, questions);
            }
            _ => {}
        }
    }
    session.unbind_event_channel();
    guard.finish();
}

// ── Internal helpers ─────────────────────────────────────────────────

/// Register a permission request in the run-scoped registry and publish it.
/// Pending requests live for the run, not the connection (run end sweeps
/// them; a disconnect no longer auto-denies). Shared by the chat and compact
/// event loops.
fn register_permission_request(
    registry: &TaskRegistry,
    bus: &SessionBus,
    session_id: &str,
    response_tx: tokio::sync::oneshot::Sender<crate::permission::PermissionReply>,
    description: String,
    patterns: Vec<String>,
    subagent: Option<String>,
) {
    let request_id = uuid::Uuid::new_v4().to_string();
    registry.insert_permission(&request_id, response_tx, session_id);
    bus.publish(ServerEvent::PermissionRequest {
        request_id,
        description,
        patterns,
        subagent,
    });
}

/// Ask-user variant of [`register_permission_request`].
fn register_ask_request(
    registry: &TaskRegistry,
    bus: &SessionBus,
    session_id: &str,
    response_tx: tokio::sync::oneshot::Sender<String>,
    questions: Vec<crate::agent::event::QuestionInfo>,
) {
    let request_id = uuid::Uuid::new_v4().to_string();
    registry.insert_ask(&request_id, response_tx, session_id);
    bus.publish(ServerEvent::AskUser {
        request_id,
        questions,
    });
}

async fn persist_user_message(
    session: &ChatSession,
    session_id: &str,
    message: &str,
    attachments: &[crate::agent::media::Attachment],
    mentions: &[protocol::ChatMention],
) -> Result<(), String> {
    let attachments_json = if attachments.is_empty() {
        None
    } else {
        Some(serde_json::to_string(attachments).map_err(|e| e.to_string())?)
    };
    let mentions_json = if mentions.is_empty() {
        None
    } else {
        Some(serde_json::to_string(mentions).map_err(|e| e.to_string())?)
    };
    let stored = StoredMessage {
        role: MessageRole::User,
        content: message.to_string(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        prompt_tokens: None,
        completion_tokens: None,
        cached_tokens: None,
        model: None,
        runtime_meta: None,
        think_ms: None,
        compacted: false,
        attachments: attachments_json,
        mentions: mentions_json,
    };
    session
        .storage()
        .append_message(session_id, &stored)
        .await
        .map_err(|e| e.to_string())
}

fn runtime_meta_json(total_ms: u64, model: &str, reasoning_effort: &str, status: &str) -> String {
    serde_json::json!({
        "total_ms": total_ms,
        "model": model,
        "reasoning_effort": reasoning_effort,
        "status": status,
    })
    .to_string()
}

/// Freeze the current thinking segment duration (align with TUI finalize_thinking_ms: settled on thinking→output/tool-call switches)
fn finalize_thinking(thinking_started: &mut Option<Instant>, think_total_ms: &mut u64) {
    if let Some(t) = thinking_started.take() {
        *think_total_ms += t.elapsed().as_millis() as u64;
    }
}

/// Start compaction when over the threshold (returns true = started; the caller keeps consuming events)
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

/// Validate and convert image attachments uploaded by the frontend: strip the
/// data URL prefix (strict mime match) → base64 decode (also validating charset
/// and padding) → exact size check → magic-number sniff, overriding the mime
/// with the sniff result. Returns Err(reason) on failure; the caller sends it
/// back as an SSE Error event.
pub(crate) fn convert_attachments(
    attachments: &[protocol::ChatAttachment],
) -> Result<Vec<media::Attachment>, String> {
    if attachments.len() > media::MAX_ATTACHMENT_COUNT {
        return Err(format!(
            "图片附件数量超过上限（{} 张）",
            media::MAX_ATTACHMENT_COUNT
        ));
    }
    attachments
        .iter()
        .enumerate()
        .map(|(i, att)| {
            let idx = i + 1;
            if !media::is_supported_image_mime(&att.mime) {
                return Err(format!(
                    "第 {idx} 个附件类型不支持（{}），仅支持 PNG/JPEG/GIF/WebP",
                    att.mime
                ));
            }
            let prefix = format!("data:{};base64,", att.mime);
            let Some(payload) = att.data_url.strip_prefix(&prefix) else {
                return Err(format!("第 {idx} 个附件 data URL 格式非法"));
            };
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(payload.trim())
                .map_err(|_| format!("第 {idx} 个附件 base64 解码失败"))?;
            if bytes.len() > media::MAX_IMAGE_BYTES {
                return Err(format!("第 {idx} 个附件超过大小上限（10 MB）"));
            }
            if bytes.is_empty() {
                return Err(format!("第 {idx} 个附件内容为空"));
            }
            let Some(mime) = media::sniff_image_mime(&bytes) else {
                return Err(format!("第 {idx} 个附件内容不是受支持的图片格式"));
            };
            // Rebuild the attachment from the sniffed mime (normalizes the data_url;
            // identical form for DB storage and sending)
            Ok(media::Attachment::from_bytes(mime, &bytes))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn att(mime: &str, data_url: &str) -> protocol::ChatAttachment {
        protocol::ChatAttachment {
            mime: mime.to_string(),
            data_url: data_url.to_string(),
        }
    }

    fn png_data_url() -> String {
        crate::agent::media::Attachment::from_bytes(
            "image/png",
            &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2],
        )
        .data_url
    }

    #[test]
    fn converts_valid_png() {
        let out = convert_attachments(&[att("image/png", &png_data_url())]).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].mime, "image/png");
        assert!(out[0].data_url.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn rejects_unsupported_mime() {
        let err = convert_attachments(&[att("image/svg+xml", "data:image/svg+xml;base64,AAAA")])
            .unwrap_err();
        assert!(err.contains("类型不支持"));
    }

    #[test]
    fn rejects_malformed_data_url() {
        // prefix does not match the mime
        let err = convert_attachments(&[att("image/png", "data:image/jpeg;base64,iVBORw0KGgo")])
            .unwrap_err();
        assert!(err.contains("data URL 格式非法"));
        // no data URL prefix at all
        let err = convert_attachments(&[att("image/png", "iVBORw0KGgo")]).unwrap_err();
        assert!(err.contains("data URL 格式非法"));
    }

    #[test]
    fn rejects_invalid_base64() {
        let err =
            convert_attachments(&[att("image/png", "data:image/png;base64,!!!!")]).unwrap_err();
        assert!(err.contains("base64 解码失败"));
    }

    #[test]
    fn rejects_empty_payload() {
        let err = convert_attachments(&[att("image/png", "data:image/png;base64,")]).unwrap_err();
        assert!(err.contains("内容为空"));
    }

    #[test]
    fn rejects_non_image_content() {
        // valid base64 but the content is not an image (magic-number sniff fails)
        let err =
            convert_attachments(&[att("image/png", "data:image/png;base64,aGVsbG8=")]).unwrap_err();
        assert!(err.contains("不是受支持的图片格式"));
    }

    #[test]
    fn rejects_count_over_limit() {
        let durl = png_data_url();
        let many: Vec<_> = (0..media::MAX_ATTACHMENT_COUNT + 1)
            .map(|_| att("image/png", &durl))
            .collect();
        let err = convert_attachments(&many).unwrap_err();
        assert!(err.contains("数量超过上限"));
    }
}
