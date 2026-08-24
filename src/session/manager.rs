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
use crate::permission::{PermissionManager, PermissionMode};
use crate::storage::ChatStorage;

use super::session::{ChatSession, normalize_key};

type SessionEntry = Arc<tokio::sync::Mutex<ChatSession>>;

pub struct SessionManager {
    sessions: RwLock<HashMap<PathBuf, SessionEntry>>,
    /// 每 work_dir 的中断标志（不经过会话锁，立即生效）
    cancel_flags: RwLock<HashMap<PathBuf, Arc<AtomicBool>>>,
    /// 每 work_dir 的权限管理器句柄（YOLO 广播不经过会话锁，立即生效）
    perm_managers: RwLock<HashMap<PathBuf, PermissionManager>>,
    resolved: RwLock<config::ResolvedModel>,
    cfg: RwLock<config::Config>,
    storage: ChatStorage,
    mcp_backends: SharedMcpBackends,
    /// Web 全局 YOLO 模式（所有 work_dir 共享；新建会话继承）
    global_yolo: AtomicBool,
    /// Web 全局规划模式（所有 work_dir 共享；新建会话继承）
    global_plan: AtomicBool,
}

impl SessionManager {
    pub fn new(
        resolved: config::ResolvedModel,
        cfg: config::Config,
        storage: ChatStorage,
        mcp_backends: SharedMcpBackends,
    ) -> Self {
        // 初始 YOLO 取配置默认（对齐 TUI 启动语义），plan 恒为 false
        let global_yolo =
            AtomicBool::new(PermissionMode::from_str(&cfg.permission.mode) == PermissionMode::Yolo);
        Self {
            sessions: RwLock::new(HashMap::new()),
            cancel_flags: RwLock::new(HashMap::new()),
            perm_managers: RwLock::new(HashMap::new()),
            resolved: RwLock::new(resolved),
            cfg: RwLock::new(cfg),
            storage,
            mcp_backends,
            global_yolo,
            global_plan: AtomicBool::new(false),
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
        // 继承全局模式（Web 切换项目后保持 YOLO/Plan 状态）
        session.set_yolo(self.global_yolo.load(Ordering::Relaxed));
        session.set_plan_mode(self.global_plan.load(Ordering::Relaxed));
        let perm = session.permission_manager();
        let arc: SessionEntry = Arc::new(tokio::sync::Mutex::new(session));
        let cancel = arc.lock().await.cancel_flag();

        if let Ok(mut map) = self.sessions.write() {
            map.insert(key.clone(), arc.clone());
        }
        if let Ok(mut flags) = self.cancel_flags.write() {
            flags.insert(key.clone(), cancel);
        }
        if let Ok(mut perms) = self.perm_managers.write() {
            perms.insert(key, perm);
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

    // ── Web 全局模式（YOLO / Plan，跨 work_dir 共享）──────────

    pub fn global_yolo(&self) -> bool {
        self.global_yolo.load(Ordering::Relaxed)
    }

    pub fn global_plan(&self) -> bool {
        self.global_plan.load(Ordering::Relaxed)
    }

    /// 设置全局 YOLO。经权限管理器句柄广播，**不经过会话锁**：
    /// SSE 流长时间持锁（如等待权限弹窗）时也能立即生效，
    /// 下一次工具权限检查即按新模式执行。
    pub fn set_global_yolo(&self, on: bool) {
        self.global_yolo.store(on, Ordering::Relaxed);
        let mode = if on {
            PermissionMode::Yolo
        } else {
            PermissionMode::Normal
        };
        if let Ok(perms) = self.perm_managers.read() {
            for pm in perms.values() {
                pm.set_mode(mode);
            }
        }
    }

    /// 设置全局规划模式。plan_mode 存放在 Agent 上（非权限管理器），
    /// 广播需会话锁；持锁中的会话（SSE 流运行）跳过，由 chat 入口同步兜底。
    pub fn set_global_plan(&self, on: bool) {
        self.global_plan.store(on, Ordering::Relaxed);
        for entry in self.all_sessions() {
            if let Ok(mut session) = entry.try_lock() {
                session.set_plan_mode(on);
            }
        }
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
        if let Ok(mut perms) = self.perm_managers.write() {
            perms.remove(&key);
        }
    }

    /// 使全部会话缓存失效（MCP 配置变更等全局影响后调用）。
    /// cancel_flags 保留（理由同 `invalidate`）。
    pub fn invalidate_all(&self) {
        if let Ok(mut map) = self.sessions.write() {
            map.clear();
        }
        if let Ok(mut perms) = self.perm_managers.write() {
            perms.clear();
        }
    }

    /// 列出历史会话中出现过的全部工作目录（sessions 表 DISTINCT，含会话数/最近活跃）
    pub async fn list_work_dirs(&self) -> Result<Vec<crate::storage::WorkDirInfo>> {
        self.storage.list_work_dirs().await
    }

    pub fn storage(&self) -> &ChatStorage {
        &self.storage
    }

    pub fn mcp_backends(&self) -> &SharedMcpBackends {
        &self.mcp_backends
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    async fn make_manager() -> (SessionManager, PathBuf, PathBuf) {
        let storage = ChatStorage::new_in_memory().await.unwrap();
        let resolved = config::ResolvedModel {
            config: async_openai::config::OpenAIConfig::new(),
            model_id: "test-model".to_string(),
            max_tokens: 1024,
            context_window: 8192,
            display: "test/test-model".to_string(),
        };
        let manager = SessionManager::new(
            resolved,
            config::Config::default(),
            storage,
            Arc::new(Mutex::new(Vec::new())),
        );
        let dir_a = std::env::temp_dir().join(format!("hailux-mgr-test-{}", uuid::Uuid::new_v4()));
        let dir_b = std::env::temp_dir().join(format!("hailux-mgr-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();
        (manager, dir_a, dir_b)
    }

    /// 全局 YOLO：绕会话锁广播（持锁中切换不阻塞不死锁）、新建会话自动继承。
    #[tokio::test]
    async fn global_yolo_broadcasts_and_inherits() {
        let (manager, dir_a, dir_b) = make_manager().await;
        let entry_a = manager.get_or_create(&dir_a).await.unwrap();
        assert_eq!(
            entry_a.lock().await.agent().permission().mode(),
            PermissionMode::Normal
        );

        // 持锁期间设置全局 YOLO（模拟 SSE 流运行中切换）
        {
            let guard = entry_a.lock().await;
            manager.set_global_yolo(true);
            assert!(manager.global_yolo());
            assert_eq!(guard.agent().permission().mode(), PermissionMode::Yolo);
        }

        // 新 work_dir 的会话继承全局模式
        let entry_b = manager.get_or_create(&dir_b).await.unwrap();
        assert_eq!(
            entry_b.lock().await.agent().permission().mode(),
            PermissionMode::Yolo
        );

        manager.set_global_yolo(false);
        assert_eq!(
            entry_a.lock().await.agent().permission().mode(),
            PermissionMode::Normal
        );
    }

    /// 全局 Plan：try_lock 广播，持锁会话跳过（不阻塞），入口同步兜底。
    #[tokio::test]
    async fn global_plan_broadcast_skips_locked_session() {
        let (manager, dir_a, _dir_b) = make_manager().await;
        let entry_a = manager.get_or_create(&dir_a).await.unwrap();

        // 空闲时直接生效
        manager.set_global_plan(true);
        assert!(manager.global_plan());
        assert!(entry_a.lock().await.plan_mode());

        // 持锁时跳过广播（不死锁），标志仍更新，会话旧值由入口同步纠正
        {
            let guard = entry_a.lock().await;
            manager.set_global_plan(false);
            assert!(!manager.global_plan());
            assert!(guard.plan_mode());
        }

        // 锁释放后再次广播生效
        manager.set_global_plan(false);
        assert!(!entry_a.lock().await.plan_mode());
    }

    /// invalidate 后重建的会话继承当前全局模式（skills/MCP 变更场景）。
    #[tokio::test]
    async fn rebuilt_session_inherits_global_modes() {
        let (manager, dir_a, _dir_b) = make_manager().await;
        let _ = manager.get_or_create(&dir_a).await.unwrap();
        manager.set_global_yolo(true);
        manager.set_global_plan(true);

        manager.invalidate(&dir_a);
        let entry = manager.get_or_create(&dir_a).await.unwrap();
        let session = entry.lock().await;
        assert_eq!(session.agent().permission().mode(), PermissionMode::Yolo);
        assert!(session.plan_mode());
    }
}
