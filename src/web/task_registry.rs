//! request_id -> oneshot sender registry for pending permission confirmations
//! and user questions.
//!
//! Unlike the previous design (ConnectionGuard auto-denied on disconnect),
//! pending requests live for the **run**, not the connection — a closed tab no
//! longer denies; reconnecting clients see the PermissionRequest/AskUser event
//! replayed, re-surface the dialog, and answer (same as the TUI: no answer,
//! no progress).
//!
//! Lifecycle is owned by the runner: when a run ends (including interrupts)
//! `cancel_session` sweeps all entries of that run with fallback replies.
//! Sending to an already-dead receiver is a harmless no-op.

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::oneshot;

use crate::permission::PermissionReply;

enum PendingReply {
    Permission(oneshot::Sender<PermissionReply>),
    Ask(oneshot::Sender<String>),
}

#[derive(Default)]
pub struct TaskRegistry {
    /// request_id -> (pending entry, owning session_id)
    inner: Mutex<HashMap<String, (PendingReply, String)>>,
}

impl TaskRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_permission(
        &self,
        id: &str,
        tx: oneshot::Sender<PermissionReply>,
        session_id: &str,
    ) {
        if let Ok(mut map) = self.inner.lock() {
            map.insert(
                id.to_string(),
                (PendingReply::Permission(tx), session_id.to_string()),
            );
        }
    }

    pub fn take_permission(&self, id: &str) -> Option<oneshot::Sender<PermissionReply>> {
        let mut map = self.inner.lock().ok()?;
        // Verify the kind before removing: deleting an Ask entry inside
        // take_permission would silently swallow its fallback reply
        if !matches!(map.get(id), Some((PendingReply::Permission(_), _))) {
            return None;
        }
        match map.remove(id) {
            Some((PendingReply::Permission(tx), _)) => Some(tx),
            _ => None,
        }
    }

    pub fn insert_ask(&self, id: &str, tx: oneshot::Sender<String>, session_id: &str) {
        if let Ok(mut map) = self.inner.lock() {
            map.insert(
                id.to_string(),
                (PendingReply::Ask(tx), session_id.to_string()),
            );
        }
    }

    pub fn take_ask(&self, id: &str) -> Option<oneshot::Sender<String>> {
        let mut map = self.inner.lock().ok()?;
        if !matches!(map.get(id), Some((PendingReply::Ask(_), _))) {
            return None;
        }
        match map.remove(id) {
            Some((PendingReply::Ask(tx), _)) => Some(tx),
            _ => None,
        }
    }

    /// Sweep all pending requests of a session at run end: permissions -> Deny,
    /// asks -> "[User Cancelled]". Entries already taken are skipped; sending
    /// to a dead receiver is harmless. An explicit fallback (instead of
    /// dropping the sender) keeps both paths unambiguous for the agent.
    pub fn cancel_session(&self, session_id: &str) {
        let ids: Vec<String> = if let Ok(map) = self.inner.lock() {
            map.iter()
                .filter(|(_, (_, sid))| sid == session_id)
                .map(|(id, _)| id.clone())
                .collect()
        } else {
            return;
        };
        for id in ids {
            if let Some(tx) = self.take_permission(&id) {
                let _ = tx.send(PermissionReply::Deny);
            } else if let Some(tx) = self.take_ask(&id) {
                let _ = tx.send("[User Cancelled]".to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::PermissionReply;

    #[test]
    fn cancel_session_sweeps_only_target_session() {
        let registry = TaskRegistry::new();
        let (ptx, mut prx) = oneshot::channel::<PermissionReply>();
        let (atx, mut arx) = oneshot::channel::<String>();
        let (other_tx, mut other_rx) = oneshot::channel::<String>();
        registry.insert_permission("p1", ptx, "s1");
        registry.insert_ask("a1", atx, "s1");
        registry.insert_ask("a2", other_tx, "s2");

        registry.cancel_session("s1");

        // s1's permission got the Deny fallback; the ask got the cancel text
        assert_eq!(prx.try_recv(), Ok(PermissionReply::Deny));
        assert_eq!(arx.try_recv(), Ok("[User Cancelled]".to_string()));
        // s2 untouched
        assert!(other_rx.try_recv().is_err());
        assert!(registry.take_ask("a2").is_some());
        // s1 entries swept
        assert!(registry.take_permission("p1").is_none());
        assert!(registry.take_ask("a1").is_none());
    }
}
