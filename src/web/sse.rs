//! SSE handlers: `POST /api/chat`, `POST /api/compact`, `GET /api/sessions/{id}/events`.
//!
//! Connections and tasks are decoupled: chat/compact spawn a background
//! runner (see `runner.rs`) that owns the LLM turn / compaction event loop,
//! while the connection acts as a SessionBus observer (Snapshot replay +
//! live fanout). A disconnect only drops the observer — the task keeps
//! running and persisting; the only interrupt entry is `POST /api/interrupt`
//! (precise, by session_id).
//!
//! Attach protocol (no loss, no dup, see `bus.rs`): subscribe first, then
//! replay buffered events after `since` as the first frame, dedupe live
//! events by seq, close on the `RunComplete` sentinel. Single flight per
//! session: `SessionBus::try_claim_run` CAS-guarantees one run (including
//! the queued window); duplicate requests get 409.

use std::convert::Infallible;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::agent::media;
use crate::session::ChatSession;
use tokio::sync::broadcast;

use super::bus::SessionBus;
use super::protocol::{self, Envelope, ServerEvent};
use super::runner;
use super::state::WebServerState;

/// Body limit for the chat endpoint. The axum `Json` extractor defaults to a
/// 2MB body limit; with the 4/3 base64 inflation an original image >~1.4MB
/// already triggers 413, making the 10MB cap useless — the router layer must
/// explicitly raise it with this constant (see web/mod.rs).
/// Sized for a full attachment set (count cap × per-attachment base64 size)
/// plus JSON structure headroom.
pub(super) const CHAT_BODY_LIMIT: usize =
    media::MAX_ATTACHMENT_COUNT * (media::MAX_IMAGE_BYTES / 3 * 4) + 64 * 1024;

pub async fn chat_handler(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<protocol::ChatRequest>,
) -> Response {
    // Resolve the target before spawning: lazily build/reuse the per-session
    // instance; without a session_id, persist a new session row first.
    // Failures return plain 4xx instead of SSE Error events.
    let (session_arc, session_id) =
        match resolve_chat_target(&state, req.session_id.as_deref(), req.work_dir.as_deref()).await
        {
            Ok(t) => t,
            Err(e) => return error_response(e),
        };
    let attachments = match runner::convert_attachments(&req.attachments) {
        Ok(a) => a,
        Err(e) => return error_response(e),
    };

    let bus = state.buses.get_or_create(&session_id, state.epoch);
    // Single flight per session (incl. the queued window): duplicates get 409.
    // The frontend has its own single-flight guard; this covers multi-tab and
    // concurrent scripts.
    if !bus.try_claim_run() {
        return (
            StatusCode::CONFLICT,
            "该会话已有任务在运行，请等待完成或先中断",
        )
            .into_response();
    }
    // Global concurrency cap on top-level runs (across sessions).
    let permit = match state.run_permits.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            bus.release_run_claim();
            return (
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "并发任务数已达全局上限（{}），请等待其他任务完成后再试",
                    state.run_limit
                ),
            )
                .into_response();
        }
    };

    // Observer cursor: capture last_seq BEFORE the spawn. The sender is the
    // current view — everything published so far is already rendered there, so
    // only new events are wanted. Never replay from 0: the replay buffer is
    // cleared by the runner's begin_run, and attaching earlier than that would
    // re-deliver the whole previous round after the new user message (always
    // reproducible on the second message of a session).
    let cursor = bus.last_seq();
    tokio::spawn(runner::run_chat_task(
        state.clone(),
        session_arc,
        session_id.clone(),
        req.message,
        attachments,
        req.mentions,
        bus.clone(),
        permit,
    ));

    let rx = bus.subscribe();
    observer_stream(bus, rx, cursor, true).into_response()
}

/// Manual compaction (POST /api/compact)
pub async fn compact_handler(
    State(state): State<Arc<WebServerState>>,
    Json(req): Json<protocol::CompactRequest>,
) -> Response {
    let session_arc = match state
        .manager
        .get_or_create_for_session(&req.session_id)
        .await
    {
        Ok(s) => s,
        Err(e) => return error_response(e.to_string()),
    };

    let bus = state.buses.get_or_create(&req.session_id, state.epoch);
    if !bus.try_claim_run() {
        return (
            StatusCode::CONFLICT,
            "该会话已有任务在运行，请等待完成或先中断",
        )
            .into_response();
    }
    let permit = match state.run_permits.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            bus.release_run_claim();
            return (
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "并发任务数已达全局上限（{}），请等待其他任务完成后再试",
                    state.run_limit
                ),
            )
                .into_response();
        }
    };

    // Same as chat_handler: capture last_seq before the spawn so the previous
    // round's buffer is never replayed to the sender.
    let cursor = bus.last_seq();
    tokio::spawn(runner::run_compact_task(
        state.clone(),
        session_arc,
        req.session_id,
        bus.clone(),
        permit,
    ));

    let rx = bus.subscribe();
    observer_stream(bus, rx, cursor, true).into_response()
}

/// Subscribe to a session's event stream
/// (GET /api/sessions/{id}/events?since=<seq>&epoch=<ts>).
///
/// For reconnects, live resume after session switches, and multi-tab
/// observation. Never touches ChatSession (registry lookup only, zero lock
/// contention). Events after `since` are replayed in the Snapshot first frame,
/// then live fanout. An `epoch` mismatch (server restart) invalidates the
/// cursor and falls back to full replay; the client rebuilds from history.
pub async fn events_subscribe_handler(
    State(state): State<Arc<WebServerState>>,
    Path(id): Path<String>,
    Query(q): Query<EventsQuery>,
) -> Response {
    // get_or_create: viewing creates the bus (idle long connection; the next
    // run's events arrive immediately). No ChatSession involvement.
    let bus = state.buses.get_or_create(&id, state.epoch);
    let since = if q.epoch == Some(bus.epoch()) {
        q.since.unwrap_or(0)
    } else {
        0
    };
    let rx = bus.subscribe();
    observer_stream(bus, rx, since, false).into_response()
}

#[derive(Debug, Deserialize)]
pub(crate) struct EventsQuery {
    /// Highest seq the client has consumed (events with seq > since are replayed)
    since: Option<u64>,
    /// Client's recorded server epoch (mismatch = server restarted, cursor invalid)
    epoch: Option<u64>,
}

// ── Internal helpers ─────────────────────────────────────────────────

/// Resolve the chat target: returns (instance, session id).
/// - With session_id: lazily build/reuse the per-session instance (bound to
///   the session and context-rebuilt at creation)
/// - Without: persist a new session row in the work dir, then build the bound
///   instance (no ChatSession needed just to create a row — storage handles it)
async fn resolve_chat_target(
    state: &WebServerState,
    session_id: Option<&str>,
    work_dir: Option<&str>,
) -> Result<(Arc<tokio::sync::Mutex<ChatSession>>, String), String> {
    match session_id {
        Some(id) => {
            let arc = state
                .manager
                .get_or_create_for_session(id)
                .await
                .map_err(|e| e.to_string())?;
            Ok((arc, id.to_string()))
        }
        None => {
            let dir = match work_dir {
                Some(d) if !d.is_empty() => std::path::PathBuf::from(d),
                _ => state.default_work_dir.clone(),
            };
            let canonical = dir
                .canonicalize()
                .map_err(|e| format!("工作目录无效 {}: {e}", dir.display()))?;
            let display = state.manager.resolved().display;
            let id = state
                .manager
                .storage()
                .create_session(&display, &canonical.display().to_string())
                .await
                .map_err(|e| e.to_string())?;
            let arc = state
                .manager
                .get_or_create_for_session(&id)
                .await
                .map_err(|e| e.to_string())?;
            Ok((arc, id))
        }
    }
}

/// Observer stream: Snapshot replay first frame + live fanout.
///
/// Attach order (no loss, no dup): the caller must `bus.subscribe()` FIRST and
/// pass the receiver in; this fn then takes the replay snapshot. The two
/// sources may overlap — live events are deduped by seq.
///
/// `close_on_run_complete`: POST observer streams close at the end of their
/// own run (the turn-completed signal); GET subscriptions pass false — they
/// stay open while idle and keep delivering across runs (multi-tab, switch
/// away and back), terminating only on Lagged / channel close.
fn observer_stream(
    bus: Arc<SessionBus>,
    rx: broadcast::Receiver<Envelope>,
    since: u64,
    close_on_run_complete: bool,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let stream = async_stream::stream! {
        let replay = bus.replay_since(since);
        let running_at_attach = bus.is_running();
        let mut last_sent: u64 = replay.last().map(|e| e.seq).unwrap_or(since);
        yield sse_event(&ServerEvent::Snapshot {
            epoch: bus.epoch(),
            running: running_at_attach,
            events: replay,
        });
        // No "idle -> close" early return: the runner calls begin_run only
        // after its spawn, so a POST observer always attaches while is_running
        // is still false; GET subscriptions intentionally idle-wait for the
        // next run.
        let mut rx = rx;
        loop {
            match rx.recv().await {
                Ok(env) => {
                    if env.seq <= last_sent {
                        continue;
                    }
                    last_sent = env.seq;
                    let is_run_complete =
                        matches!(env.event, ServerEvent::RunComplete { .. });
                    yield envelope_frame(&env);
                    if is_run_complete && close_on_run_complete {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    // Live channel overflow: the gap cannot be patched inline.
                    // Terminate; the client reconnects and replays from the buffer.
                    yield sse_event(&ServerEvent::Error {
                        message: format!("事件流落后 {n} 条，请刷新页面重新订阅"),
                    });
                    break;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn sse_event(event: &ServerEvent) -> Result<Event, Infallible> {
    let json = serde_json::to_string(event).unwrap_or_else(|_| {
        serde_json::json!({"type": "Error", "message": "serialize failed"}).to_string()
    });
    Ok(Event::default().data(json))
}

fn envelope_frame(env: &Envelope) -> Result<Event, Infallible> {
    let json = serde_json::to_string(env).unwrap_or_else(|_| {
        "{\"seq\":0,\"event\":{\"type\":\"Error\",\"message\":\"serialize failed\"}}".to_string()
    });
    Ok(Event::default().data(json))
}

fn error_response(msg: String) -> Response {
    (StatusCode::BAD_REQUEST, msg).into_response()
}
