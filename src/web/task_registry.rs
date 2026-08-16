//! request_id → oneshot Sender 注册表 + 连接级 drop guard。
//!
//! 权限确认 / 用户提问通过独立 POST 回复，用 `request_id` 关联。
//! **断连防泄漏**：SSE 流随时可能断开（关标签页、刷新、网络抖动），
//! 前端死后无人回复会让 agent task 永久阻塞在 `oneshot::Receiver` 上。
//! 因此每个待回复条目记录所属连接 ID；SSE 流被 drop 时（axum 感知
//! 客户端断开），`ConnectionGuard` 自动将 pending 请求以 Deny / 取消
//! 文本终结，agent task 正常唤醒。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

use crate::permission::PermissionReply;

enum PendingReply {
    Permission(oneshot::Sender<PermissionReply>),
    Ask(oneshot::Sender<String>),
}

#[derive(Default)]
pub struct TaskRegistry {
    inner: Mutex<HashMap<String, PendingReply>>,
}

impl TaskRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_permission(&self, id: &str, tx: oneshot::Sender<PermissionReply>) {
        if let Ok(mut map) = self.inner.lock() {
            map.insert(id.to_string(), PendingReply::Permission(tx));
        }
    }

    pub fn take_permission(&self, id: &str) -> Option<oneshot::Sender<PermissionReply>> {
        if let Ok(mut map) = self.inner.lock()
            && let Some(PendingReply::Permission(tx)) = map.remove(id)
        {
            return Some(tx);
        }
        None
    }

    pub fn insert_ask(&self, id: &str, tx: oneshot::Sender<String>) {
        if let Ok(mut map) = self.inner.lock() {
            map.insert(id.to_string(), PendingReply::Ask(tx));
        }
    }

    pub fn take_ask(&self, id: &str) -> Option<oneshot::Sender<String>> {
        if let Ok(mut map) = self.inner.lock()
            && let Some(PendingReply::Ask(tx)) = map.remove(id)
        {
            return Some(tx);
        }
        None
    }
}

/// SSE 连接的取消守卫：持有期间产生的 pending 回复记录其归属；
/// 守卫 drop（流结束或客户端断开）时全部以兜底值终结。
///
/// 注意 oneshot sender drop 本身就会让 receiver 立即 Err——
/// 这里显式发送兜底值，是为了让 agent 收到语义明确的「拒绝/取消」
/// 而非 channel 错误，两条路径都不会挂起。
pub struct ConnectionGuard {
    registry: Arc<TaskRegistry>,
    /// 归属本连接的 pending request_id 列表
    pending: Mutex<Vec<String>>,
}

impl ConnectionGuard {
    pub fn new(registry: Arc<TaskRegistry>) -> Self {
        Self {
            registry,
            pending: Mutex::new(Vec::new()),
        }
    }

    /// 登记权限请求，返回 request_id
    pub fn track_permission(&self, request_id: String, tx: oneshot::Sender<PermissionReply>) {
        self.registry.insert_permission(&request_id, tx);
        if let Ok(mut p) = self.pending.lock() {
            p.push(request_id);
        }
    }

    /// 登记提问，返回 request_id
    pub fn track_ask(&self, request_id: String, tx: oneshot::Sender<String>) {
        self.registry.insert_ask(&request_id, tx);
        if let Ok(mut p) = self.pending.lock() {
            p.push(request_id);
        }
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        // 发送兜底回复（sender 已被 take 时 take_* 内部为 None，静默跳过）
        if let Ok(ids) = self.pending.lock() {
            for id in ids.iter() {
                if let Some(tx) = self.registry.take_permission(id) {
                    let _ = tx.send(PermissionReply::Deny);
                } else if let Some(tx) = self.registry.take_ask(id) {
                    let _ = tx.send("[User Cancelled]".to_string());
                }
            }
        }
    }
}
