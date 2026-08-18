//! SessionManager：work_dir → ChatSession 的惰性注册表。
//!
//! TUI 模式只有一个条目（保持现有行为）；Web 模式每个工作目录一个实例。
//! Storage / Config / MCP 连接全局共享，skills / subagents / commands /
//! system prompt 随 work_dir 独立发现与构建。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use color_eyre::Result;

use crate::config;
use crate::mcp::SharedMcpBackends;
use crate::storage::ChatStorage;

use super::session::{ChatSession, normalize_key};

type SessionEntry = Arc<tokio::sync::Mutex<ChatSession>>;

pub struct SessionManager {
    sessions: RwLock<HashMap<PathBuf, SessionEntry>>,
    /// 每 work_dir 的中断标志（不经过会话锁，立即生效）
    cancel_flags: RwLock<HashMap<PathBuf, Arc<AtomicBool>>>,
    resolved: RwLock<config::ResolvedModel>,
    cfg: RwLock<config::Config>,
    storage: ChatStorage,
    mcp_backends: SharedMcpBackends,
}

impl SessionManager {
    pub fn new(
        resolved: config::ResolvedModel,
        cfg: config::Config,
        storage: ChatStorage,
        mcp_backends: SharedMcpBackends,
    ) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            cancel_flags: RwLock::new(HashMap::new()),
            resolved: RwLock::new(resolved),
            cfg: RwLock::new(cfg),
            storage,
            mcp_backends,
        }
    }

    pub fn resolved(&self) -> config::ResolvedModel {
        self.resolved
            .read()
            .expect("resolved lock poisoned")
            .clone()
    }

    pub fn cfg(&self) -> config::Config {
        self.cfg.read().expect("cfg lock poisoned").clone()
    }

    pub fn set_resolved(&self, resolved: config::ResolvedModel) {
        if let Ok(mut guard) = self.resolved.write() {
            *guard = resolved;
        }
    }

    pub fn set_cfg(&self, cfg: config::Config) {
        if let Ok(mut guard) = self.cfg.write() {
            *guard = cfg;
        }
    }

    /// 已缓存的直接返回；新 work_dir 惰性构建（含目录存在性校验）。
    /// 同一目录的不同写法归一到同一实例（键 canonicalize 归一化）。
    pub async fn get_or_create(&self, work_dir: &Path) -> Result<SessionEntry> {
        let key = normalize_key(work_dir);
        if !key.is_dir() {
            color_eyre::eyre::bail!("工作目录不存在: {}", key.display());
        }

        if let Some(existing) = self.sessions.read().ok().and_then(|m| m.get(&key).cloned()) {
            return Ok(existing);
        }

        // 写入 DB 的 work_dir 统一为 canonicalize 结果（Windows 保留 `\\?\` 前缀），
        // 与 TUI 的 `current_work_dir()` 保持一致，避免两侧会话互相不可见。
        let canonical = work_dir.canonicalize().unwrap_or_else(|_| key.clone());

        let resolved = self.resolved();
        let cfg = self.cfg();
        let mut session = ChatSession::new(
            &resolved,
            &cfg,
            &canonical,
            self.storage.clone(),
            self.mcp_backends.clone(),
            None,
        )?;
        session.register_task_tool(&resolved);
        session.register_mcp_tools(&self.mcp_backends);
        let arc: SessionEntry = Arc::new(tokio::sync::Mutex::new(session));
        let cancel = arc.lock().await.cancel_flag();

        if let Ok(mut map) = self.sessions.write() {
            map.insert(key.clone(), arc.clone());
        }
        if let Ok(mut flags) = self.cancel_flags.write() {
            flags.insert(key, cancel);
        }
        Ok(arc)
    }

    /// 中断指定目录的当前运行（不经过会话锁）。返回 false = 无该目录实例。
    pub fn interrupt(&self, work_dir: &Path) -> bool {
        let key = normalize_key(work_dir);
        self.cancel_flags
            .read()
            .ok()
            .and_then(|m| m.get(&key).cloned())
            .map(|flag| {
                flag.store(true, Ordering::Relaxed);
                true
            })
            .unwrap_or(false)
    }

    /// 所有已构建的 ChatSession（模型切换等广播操作用）
    pub fn all_sessions(&self) -> Vec<SessionEntry> {
        self.sessions
            .read()
            .ok()
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default()
    }

    /// 取指定目录已构建的会话条目（未构建返回 None；键归一化同 `get_or_create`）
    pub fn get(&self, work_dir: &Path) -> Option<SessionEntry> {
        let key = normalize_key(work_dir);
        self.sessions.read().ok().and_then(|m| m.get(&key).cloned())
    }

    /// 使指定目录的会话缓存失效（skills 等磁盘配置变更后调用）。
    /// 运行中的请求持有 Arc 不受影响；下次访问重新发现并构建。
    /// Web 每个请求都携带 session_id 重建上下文（见 sse::ensure_session），可安全调用。
    /// 注意：不清 cancel_flags，避免运行中的请求失去中断能力。
    pub fn invalidate(&self, work_dir: &Path) {
        let key = normalize_key(work_dir);
        if let Ok(mut map) = self.sessions.write() {
            map.remove(&key);
        }
    }

    /// 使全部会话缓存失效（MCP 配置变更等全局影响后调用）。
    /// cancel_flags 保留（理由同 `invalidate`）。
    pub fn invalidate_all(&self) {
        if let Ok(mut map) = self.sessions.write() {
            map.clear();
        }
    }

    /// 列出历史会话中出现过的全部工作目录（sessions 表 DISTINCT）
    pub async fn list_work_dirs(&self) -> Result<Vec<String>> {
        self.storage.list_work_dirs().await
    }

    pub fn storage(&self) -> &ChatStorage {
        &self.storage
    }

    pub fn mcp_backends(&self) -> &SharedMcpBackends {
        &self.mcp_backends
    }
}
