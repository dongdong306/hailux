//! SSE handler: `POST /api/chat`, `POST /api/compact`.
//!
//! Each request creates an independent `CoreEvent` channel; the event loop maps
//! domain events into `ServerEvent` JSON pushes while performing persistence
//! equivalent to the TUI side (PersistMessage to DB, AgentComplete writes
//! runtime_meta, auto-compaction).
//!
//! Disconnect semantics: client disconnects → stream is dropped → `ConnectionGuard`
//! rejects pending permission/question requests as a fallback; `DropInterrupt`
//! interrupts a still-running agent, preventing context from diverging from the
//! DB (on normal completion `completed = true`, no interrupt is triggered).

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};

use base64::Engine as _;

use crate::agent::event::{CoreEvent, CoreEventTx, create_core_event_channel};
use crate::agent::media;
use crate::session::ChatSession;
use crate::storage::{MessageRole, StoredMessage};

use super::protocol::{self, ServerEvent};
use super::state::WebServerState;
use super::task_registry::ConnectionGuard;

/// Max length of a tool result pushed to the frontend (in chars)
const TOOL_RESULT_MAX_CHARS: usize = 100_000;

/// Body limit for the chat endpoint. The axum `Json` extractor defaults to a
/// 2MB body limit; with the 4/3 base64 inflation an original image >~1.4MB
/// already triggers 413, making the 10MB cap useless — the router layer must
/// explicitly raise it with this constant (see web/mod.rs).
/// Sized for a full attachment set (count cap × per-attachment base64 size)
/// plus JSON structure headroom.
pub(super) const CHAT_BODY_LIMIT: usize =
    media::MAX_ATTACHMENT_COUNT * (media::MAX_IMAGE_BYTES / 3 * 4) + 64 * 1024;

/// Validate and convert image attachments uploaded by the frontend: strip the
/// data URL prefix (strict mime match) → base64 decode (also validating charset
/// and padding) → exact size check → magic-number sniff, overriding the mime
/// with the sniff result. Returns Err(reason) on failure; the caller sends it
/// back as an SSE Error event.
fn convert_attachments(
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
    let attachments = match convert_attachments(&req.attachments) {
        Ok(a) => a,
        Err(e) => return error_response(e),
    };
    let requested_session_id = req.session_id;
    let resolved = state.manager.resolved();
    let compact_threshold = state.manager.cfg().compact_threshold;

    let stream = async_stream::stream! {
        // Per-session serialization: concurrent requests for the same session queue here
        let mut session = session_arc.lock().await;
        session.bind_event_channel(core_tx.clone());

        // Mode reconciliation: YOLO/Plan/reasoning effort are server-level global
        // state; sessions holding the lock are skipped during broadcast (see
        // SessionManager::set_global_plan), so sync as a fallback at each request
        // entry, also covering invalidate-rebuild, history drift, and similar cases.
        session.set_yolo(state.manager.global_yolo());
        session.set_plan_mode(state.manager.global_plan());
        session.set_reasoning_effort(state.manager.global_effort());

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
                    yield sse_event(&ServerEvent::Error { message: "当前为只读规划模式，无法创建 AGENTS.md。请先退出规划模式，再运行 /init。".to_string() });
                    session.unbind_event_channel();
                    return;
                }
                rendered
            }
            None => message,
        };

        // ── Session preparation (align with TUI handle_user_message)──────────
        let session_id = match ensure_session(&mut session, &resolved.display, requested_session_id).await {
            Ok(id) => id,
            Err(e) => {
                yield sse_event(&ServerEvent::Error { message: e });
                return;
            }
        };

        yield sse_event(&ServerEvent::SessionStarted { session_id: session_id.clone() });

        if let Err(e) =
            persist_user_message(&session, &session_id, &message, &attachments, &req.mentions).await
        {
            yield sse_event(&ServerEvent::Error { message: format!("持久化用户消息失败: {e}") });
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
                }
            }
        }

        // ── Start streaming ────────────────────────────────────
        let started = Instant::now();
        // Thinking timer (align with TUI finalize_thinking_ms: freeze the segment
        // on thinking→output/tool-call switch, sum into think_ms at PersistMessage)
        let mut thinking_started: Option<Instant> = None;
        let mut think_total_ms: u64 = 0;
        if let Err(e) = session.send_message(&message, attachments, core_tx.clone()) {
            yield sse_event(&ServerEvent::Error { message: e.to_string() });
            return;
        }

        // Interrupt the agent on disconnect (skipped by setting completed = true on normal completion)
        let mut interrupt_on_drop = DropInterrupt { session: session_arc.clone(), completed: false };

        let mut last_prompt: u32 = 0;
        let mut last_completion: u32 = 0;
        // Auto-compaction start time (elapsed measured at CompactComplete)
        let mut compact_started = Instant::now();

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
                    yield sse_event(&ServerEvent::AgentRetry {
                        attempt,
                        max_retries,
                        delay_ms,
                        reason,
                    });
                }
                CoreEvent::UsageUpdate { prompt_tokens, completion_tokens, cached_tokens } => {
                    last_prompt = prompt_tokens;
                    last_completion = completion_tokens;
                    let _ = session.storage()
                        .update_session_usage(&session_id, prompt_tokens as i64, completion_tokens as i64)
                        .await;
                    yield sse_event(&ServerEvent::UsageUpdate {
                        prompt_tokens,
                        completion_tokens,
                        cached_tokens,
                        context_window: resolved.context_window,
                    });
                }
                CoreEvent::PersistMessage { msg, usage, model, display } => {
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
                }
                CoreEvent::ToolCallStart { name, arguments, subagent_name, subagent_index } => {
                    finalize_thinking(&mut thinking_started, &mut think_total_ms);
                    yield sse_event(&ServerEvent::ToolCallStart {
                        name, arguments,
                        subagent: subagent_name,
                        subagent_index,
                    });
                }
                CoreEvent::ToolResult { name, result, display, subagent_name, subagent_index } => {
                    let result = truncate_chars(&result, TOOL_RESULT_MAX_CHARS);
                    yield sse_event(&ServerEvent::ToolResult {
                        name, result, display,
                        subagent: subagent_name,
                        subagent_index,
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
                CoreEvent::AgentComplete { messages, status, model, reasoning_effort, .. } => {
                    // Sync the in-memory context (align with TUI AgentComplete handling)
                    if !messages.is_empty() {
                        session.sync_messages(messages);
                    }
                    let total_ms = started.elapsed().as_millis() as u64;
                    // runtime_meta uses the snapshot model/effort carried by the event (mid-turn switches have no effect)
                    let _ = session.storage()
                        .update_last_assistant_runtime_meta(
                            &session_id,
                            &runtime_meta_json(total_ms, &model, reasoning_effort.as_str(), protocol::status_str(&status)),
                        )
                        .await;
                    let _ = session.storage().touch_session(&session_id).await;
                    yield sse_event(&ServerEvent::AgentComplete {
                        status: protocol::status_str(&status).to_string(),
                        total_ms,
                        model,
                    });

                    // Auto-compaction check (align with TUI threshold logic); after starting, keep consuming compaction events
                    if maybe_start_compaction(
                        &mut session, &core_tx, &session_id,
                        last_prompt + last_completion, resolved.context_window, compact_threshold,
                    ).await {
                        compact_started = Instant::now();
                        yield sse_event(&ServerEvent::CompactStart {
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
                CoreEvent::CompactComplete { summary, session_id: compact_sid, usage } => {
                    let compacted_count = session.storage()
                        .count_active_messages(&compact_sid).await.unwrap_or(0) as usize;
                    let _ = session.storage().mark_messages_compacted(&compact_sid).await;
                    let _ = session.storage().set_compact_summary(&compact_sid, &summary).await;
                    // Post-compaction context estimate (align with TUI: context_prompt - compact_prompt + compact_completion)
                    let mut context_tokens: Option<u32> = None;
                    if compact_sid == session_id {
                        session.apply_compaction_result(&summary);
                        if let Some(u) = usage {
                            let estimated = last_prompt
                                .saturating_sub(u.prompt_tokens)
                                .saturating_add(u.completion_tokens);
                            context_tokens = Some(estimated);
                            let _ = session.storage()
                                .update_session_usage(&session_id, estimated as i64, 0)
                                .await;
                        }
                    }
                    yield sse_event(&ServerEvent::CompactComplete {
                        compacted_count,
                        total_ms: compact_started.elapsed().as_millis() as u64,
                        context_tokens,
                    });
                    break;
                }
                CoreEvent::CompactError(msg) => {
                    yield sse_event(&ServerEvent::Error { message: format!("压缩失败: {msg}") });
                    break;
                }
            }
        }

        // Event loop finished normally (early returns before send do not reach here)
        interrupt_on_drop.completed = true;
        // Request finished: unbind the channel (rebound by the next request)
        session.unbind_event_channel();
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Manual compaction (POST /api/compact, SSE response)
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

        // Align with TUI compact_conversation: first emit a compaction status line
        // (CompactStreaming); the summary stream is not shown and the whole line is
        // replaced by the completion marker when done
        let compact_started = Instant::now();
        yield sse_event(&ServerEvent::CompactStart { text: "正在压缩上下文...".to_string() });
        if let Err(e) = session.request_compaction(core_tx.clone(), requested_session_id.clone()) {
            yield sse_event(&ServerEvent::Error { message: e.to_string() });
            return;
        }

        while let Some(event) = core_rx.recv().await {
            match event {
                CoreEvent::CompactChunk(_) => {
                    // Align with TUI: the summary stream only drives the status animation; its content is not displayed
                }
                CoreEvent::CompactComplete { summary, session_id: sid, usage } => {
                    let compacted_count = session.storage()
                        .count_active_messages(&sid).await.unwrap_or(0) as usize;
                    let _ = session.storage().mark_messages_compacted(&sid).await;
                    let _ = session.storage().set_compact_summary(&sid, &summary).await;
                    // Post-compaction context estimate (align with TUI: session_prompt - compact_prompt + compact_completion)
                    let mut context_tokens: Option<u32> = None;
                    if sid == requested_session_id {
                        session.apply_compaction_result(&summary);
                        // Refine the session usage estimate using the compaction request's own usage
                        if let (Some(u), Ok((pt, _))) = (
                            usage,
                            session.storage().get_session_usage(&sid).await,
                        ) {
                            let estimated = (pt as u32)
                                .saturating_sub(u.prompt_tokens)
                                .saturating_add(u.completion_tokens);
                            context_tokens = Some(estimated);
                            let _ = session
                                .storage()
                                .update_session_usage(&sid, estimated as i64, 0)
                                .await;
                        }
                    }
                    yield sse_event(&ServerEvent::CompactComplete {
                        compacted_count,
                        total_ms: compact_started.elapsed().as_millis() as u64,
                        context_tokens,
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

// ── Internal helpers ─────────────────────────────────────────────────

/// Interrupt the agent when the stream is dropped (client disconnected and the
/// event loop did not finish normally).
/// The interrupt runs in a background task (Drop cannot wait on async locks).
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
            {
                let s = session.lock().await;
                s.interrupt();
                s.unbind_event_channel();
            }
            // Align with TUI cleanup_on_quit's 2s wrap-up window: after the
            // interrupt the agent emits stubs for dangling tool_calls, but the
            // event channel was closed on disconnect, so PersistMessage cannot
            // reach the DB — hence this delayed idempotent repair to avoid a
            // Provider 400 when resuming the session.
            // Safety precondition: after unbind the agent no longer writes the DB
            // directly (persisting only goes through the event consumer), and new
            // requests in the meantime hold the session lock, serializing the
            // repair to run after that round ends.
            tokio::time::sleep(Duration::from_secs(2)).await;
            let s = session.lock().await;
            if let Some(session_id) = s.current_session_id()
                && let Err(e) = s.storage().repair_orphaned_tool_calls(&session_id).await
            {
                eprintln!("[warn] repair_orphaned_tool_calls: {e}");
            }
        });
    }
}

pub(crate) async fn resolve_session(
    state: &WebServerState,
    session_id: Option<&str>,
    work_dir: Option<&str>,
) -> Result<Arc<tokio::sync::Mutex<ChatSession>>, String> {
    // Priority: explicit work_dir > the session's owning directory > server default directory
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

/// Ensure a current session ID exists: switch if the request carries session_id; otherwise reuse the current one or create a new session
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

fn sse_event(event: &ServerEvent) -> Result<Event, std::convert::Infallible> {
    let json = serde_json::to_string(event).unwrap_or_else(|_| {
        serde_json::json!({"type": "Error", "message": "serialize failed"}).to_string()
    });
    Ok(Event::default().data(json))
}

fn error_response(msg: String) -> Response {
    (StatusCode::BAD_REQUEST, msg).into_response()
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
