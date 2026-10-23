use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;

use crate::permission::PermissionReply;

use super::super::protocol::{AskReplyBody, PermissionReplyBody};
use super::super::state::WebServerState;

pub(super) async fn permission_reply(
    State(state): State<Arc<WebServerState>>,
    Path(request_id): Path<String>,
    Json(reply): Json<PermissionReplyBody>,
) -> StatusCode {
    let decision = if reply.allow {
        if reply.always {
            PermissionReply::Always
        } else {
            PermissionReply::Once
        }
    } else {
        PermissionReply::Deny
    };
    match state.registry.take_permission(&request_id) {
        Some(tx) => {
            let _ = tx.send(decision);
            StatusCode::OK
        }
        None => StatusCode::NOT_FOUND,
    }
}

pub(super) async fn ask_reply(
    State(state): State<Arc<WebServerState>>,
    Path(request_id): Path<String>,
    Json(reply): Json<AskReplyBody>,
) -> StatusCode {
    match state.registry.take_ask(&request_id) {
        Some(tx) => {
            let _ = tx.send(reply.answer);
            StatusCode::OK
        }
        None => StatusCode::NOT_FOUND,
    }
}
