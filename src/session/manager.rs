//! SessionManager: lazy session_id -> ChatSession registry, one instance
//! per session (not per work_dir), so runs in the same work_dir parallelize;
//! same-session requests serialize on the instance lock.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use color_eyre::Result;

use crate::config;
use crate::mcp::SharedMcpBackends;
use crate::permission::{PermissionManager, PermissionMode};
use crate::storage::ChatStorage;

use super::session::ChatSession;

type SessionEntry = Arc<tokio::sync::Mutex<ChatSession>>;

struct EntryRecord {
    session: SessionEntry,
    work_dir: PathBuf,
    cancel: Arc<AtomicBool>,
    perm: PermissionManager,
    interject: crate::agent::event::InterjectSlot,
}

pub struct SessionManager {
    sessions: RwLock<HashMap<String, EntryRecord>>,
    resolved: RwLock<config::ResolvedModel>,
    cfg: RwLock<config::Config>,
    storage: ChatStorage,
    mcp_backends: SharedMcpBackends,
    /// Web-global modes; new instances inherit them
    global_yolo: AtomicBool,
    global_plan: AtomicBool,
    global_effort: RwLock<crate::agent::ReasoningEffort>,
}

impl SessionManager {
    pub fn new(
        resolved: config::ResolvedModel,
        cfg: config::Config,
        storage: ChatStorage,
        mcp_backends: SharedMcpBackends,
    ) -> Self {
        // Initial YOLO from the config default (mirrors TUI startup); plan is
        // always false at boot.
        let global_yolo =
            AtomicBool::new(PermissionMode::from_str(&cfg.permission.mode) == PermissionMode::Yolo);
        let initial_effort = cfg.reasoning_effort;
        Self {
            sessions: RwLock::new(HashMap::new()),
            resolved: RwLock::new(resolved),
            cfg: RwLock::new(cfg),
            storage,
            mcp_backends,
            global_yolo,
            global_plan: AtomicBool::new(false),
            global_effort: RwLock::new(initial_effort),
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

    /// A session_id always maps to the same instance; different session_ids
    /// (even in one work_dir) hold different locks and run in parallel
    pub async fn get_or_create_for_session(&self, session_id: &str) -> Result<SessionEntry> {
        if let Some(existing) = self
            .sessions
            .read()
            .ok()
            .and_then(|m| m.get(session_id).map(|r| r.session.clone()))
        {
            return Ok(existing);
        }

        let dir = self
            .storage
            .get_session_work_dir(session_id)
            .await?
            .ok_or_else(|| color_eyre::eyre::eyre!("会话不存在: {session_id}"))?;
        let record = self.build_entry(session_id, PathBuf::from(&dir)).await?;

        // Spawn race: the existing instance wins
        let entry = match self.sessions.write() {
            Ok(mut map) => match map.get(session_id) {
                Some(existing) => existing.session.clone(),
                None => {
                    map.insert(
                        session_id.to_string(),
                        EntryRecord {
                            session: record.session.clone(),
                            work_dir: record.work_dir.clone(),
                            cancel: record.cancel.clone(),
                            perm: record.perm.clone(),
                            interject: record.interject.clone(),
                        },
                    );
                    record.session.clone()
                }
            },
            Err(_) => record.session.clone(),
        };
        Ok(entry)
    }

    async fn build_entry(&self, session_id: &str, dir: PathBuf) -> Result<EntryRecord> {
        let canonical = dir.canonicalize().unwrap_or_else(|_| dir.clone());

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
        // Session-scoped "always" rules must survive instance rebuilds
        session
            .permission_manager()
            .with_storage_ref(self.storage.clone());
        session.set_yolo(self.global_yolo.load(Ordering::Relaxed));
        session.set_plan_mode(self.global_plan.load(Ordering::Relaxed));
        session.set_reasoning_effort(self.global_effort());
        session.switch_session(session_id).await?;

        let cancel = session.cancel_flag();
        let perm = session.permission_manager();
        let interject = session.interject_slot();
        Ok(EntryRecord {
            session: Arc::new(tokio::sync::Mutex::new(session)),
            work_dir: canonical,
            cancel,
            perm,
            interject,
        })
    }

    /// No session lock taken. False = no instance.
    pub fn interrupt_session(&self, session_id: &str) -> bool {
        self.sessions
            .read()
            .ok()
            .and_then(|m| {
                m.get(session_id)
                    .map(|r| r.cancel.store(true, Ordering::Relaxed))
                    .map(|_| true)
            })
            .unwrap_or(false)
    }

    /// No session lock taken (the runner holds it for the whole run).
    /// False = no instance / no active run / run cancelling / queue full.
    pub fn try_interject(
        &self,
        session_id: &str,
        interjection: crate::agent::event::Interjection,
    ) -> bool {
        let tx = self
            .sessions
            .read()
            .ok()
            .and_then(|m| m.get(session_id).map(|r| r.interject.clone()));
        let Some(slot) = tx else {
            return false;
        };
        crate::agent::event::deliver_interjection(&slot, interjection)
    }

    pub fn all_sessions(&self) -> Vec<SessionEntry> {
        self.sessions
            .read()
            .ok()
            .map(|m| m.values().map(|r| r.session.clone()).collect())
            .unwrap_or_default()
    }

    pub fn global_yolo(&self) -> bool {
        self.global_yolo.load(Ordering::Relaxed)
    }

    pub fn global_plan(&self) -> bool {
        self.global_plan.load(Ordering::Relaxed)
    }

    /// Via permission handles (never the session lock): locked instances
    /// pick it up on the next permission check
    pub fn set_global_yolo(&self, on: bool) {
        self.global_yolo.store(on, Ordering::Relaxed);
        let mode = if on {
            PermissionMode::Yolo
        } else {
            PermissionMode::Normal
        };
        if let Ok(map) = self.sessions.read() {
            for record in map.values() {
                record.perm.set_mode(mode);
            }
        }
    }

    /// plan_mode lives on the Agent, so this try_locks; locked instances are
    /// skipped and re-synced at the chat entry
    pub fn set_global_plan(&self, on: bool) {
        self.global_plan.store(on, Ordering::Relaxed);
        for entry in self.all_sessions() {
            if let Ok(mut session) = entry.try_lock() {
                session.set_plan_mode(on);
            }
        }
    }

    pub fn global_effort(&self) -> crate::agent::ReasoningEffort {
        *self
            .global_effort
            .read()
            .expect("global_effort lock poisoned")
    }

    /// Same try_lock semantics as `set_global_plan`
    pub fn set_global_effort(&self, effort: crate::agent::ReasoningEffort) {
        if let Ok(mut guard) = self.global_effort.write() {
            *guard = effort;
        }
        for entry in self.all_sessions() {
            if let Ok(mut session) = entry.try_lock() {
                session.set_reasoning_effort(effort);
            }
        }
    }

    /// In-flight requests keep their Arc but lose interruptability (their
    /// cancel flag dies with the evicted instance); the next access rebuilds
    pub fn invalidate(&self, work_dir: &Path) {
        let key = super::session::normalize_key(work_dir);
        if let Ok(mut map) = self.sessions.write() {
            map.retain(|_, r| super::session::normalize_key(&r.work_dir) != key);
        }
    }

    pub fn remove_session(&self, session_id: &str) -> bool {
        let record = self
            .sessions
            .write()
            .ok()
            .and_then(|mut map| map.remove(session_id));
        match record {
            Some(r) => {
                r.cancel.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    pub fn invalidate_all(&self) {
        if let Ok(mut map) = self.sessions.write() {
            map.clear();
        }
    }

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
            supports_vision: false,
            native_reasoning: false,
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

    async fn create_session_in(manager: &SessionManager, dir: &Path) -> String {
        manager
            .storage()
            .create_session("test-model", &dir.display().to_string())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn manager_initializes_yolo_from_config() {
        let storage = ChatStorage::new_in_memory().await.unwrap();
        let resolved = config::ResolvedModel {
            config: async_openai::config::OpenAIConfig::new(),
            model_id: "test-model".to_string(),
            max_tokens: 1024,
            context_window: 8192,
            display: "test/test-model".to_string(),
            supports_vision: false,
            native_reasoning: false,
        };
        let mut cfg = config::Config::default();
        cfg.permission.mode = "yolo".to_string();
        let manager = SessionManager::new(resolved, cfg, storage, Arc::new(Mutex::new(Vec::new())));
        assert!(manager.global_yolo());
    }

    #[tokio::test]
    async fn session_id_maps_to_stable_instance() {
        let (manager, dir_a, _dir_b) = make_manager().await;
        let s1 = create_session_in(&manager, &dir_a).await;
        let s2 = create_session_in(&manager, &dir_a).await;

        let e1 = manager.get_or_create_for_session(&s1).await.unwrap();
        let e1_again = manager.get_or_create_for_session(&s1).await.unwrap();
        assert!(
            Arc::ptr_eq(&e1, &e1_again),
            "same session_id reuses the instance"
        );

        let e2 = manager.get_or_create_for_session(&s2).await.unwrap();
        assert!(
            !Arc::ptr_eq(&e1, &e2),
            "different session_ids get different instances"
        );

        // Each instance is bound to its own session context
        assert_eq!(
            e1.lock().await.current_session_id().as_deref(),
            Some(s1.as_str())
        );
        assert_eq!(
            e2.lock().await.current_session_id().as_deref(),
            Some(s2.as_str())
        );
    }

    #[tokio::test]
    async fn same_dir_sessions_lock_independently() {
        let (manager, dir_a, _dir_b) = make_manager().await;
        let s1 = create_session_in(&manager, &dir_a).await;
        let s2 = create_session_in(&manager, &dir_a).await;
        let e1 = manager.get_or_create_for_session(&s1).await.unwrap();
        let e2 = manager.get_or_create_for_session(&s2).await.unwrap();

        // e1's lock must not block e2
        let g1 = e1.lock().await;
        let g2 = e2
            .try_lock()
            .expect("the other instance's lock must not be blocked");
        assert_eq!(g1.current_session_id().as_deref(), Some(s1.as_str()));
        assert_eq!(g2.current_session_id().as_deref(), Some(s2.as_str()));
    }

    #[tokio::test]
    async fn interrupt_session_is_precise() {
        let (manager, dir_a, _dir_b) = make_manager().await;
        let s1 = create_session_in(&manager, &dir_a).await;
        let s2 = create_session_in(&manager, &dir_a).await;
        let e1 = manager.get_or_create_for_session(&s1).await.unwrap();
        let e2 = manager.get_or_create_for_session(&s2).await.unwrap();

        assert!(manager.interrupt_session(&s1));
        assert!(
            e1.lock().await.cancel_flag().load(Ordering::Relaxed),
            "target session's cancel flag should be set"
        );
        assert!(
            !e2.lock().await.cancel_flag().load(Ordering::Relaxed),
            "same-dir siblings must not be affected"
        );
        // Unknown session: no instance -> false
        assert!(!manager.interrupt_session("nonexistent"));
    }

    #[tokio::test]
    async fn try_interject_requires_active_run() {
        let (manager, dir_a, _dir_b) = make_manager().await;
        let s1 = create_session_in(&manager, &dir_a).await;
        let _entry = manager.get_or_create_for_session(&s1).await.unwrap();

        let interjection = crate::agent::event::Interjection {
            client_id: "c1".to_string(),
            content: "hello".to_string(),
            attachments: vec![],
            mentions: Vec::new(),
        };
        // Unknown session: no registry entry
        assert!(!manager.try_interject("nonexistent", interjection.clone()));
        // Known session but no active chat run (slot still None)
        assert!(!manager.try_interject(&s1, interjection));
    }

    #[tokio::test]
    async fn global_yolo_broadcasts_and_inherits() {
        let (manager, dir_a, dir_b) = make_manager().await;
        let s1 = create_session_in(&manager, &dir_a).await;
        let s2 = create_session_in(&manager, &dir_b).await;
        let entry_a = manager.get_or_create_for_session(&s1).await.unwrap();
        assert_eq!(
            entry_a.lock().await.agent().permission().mode(),
            PermissionMode::Normal
        );

        // Set while the instance is locked (simulating a switch mid-run):
        // the perm-handle broadcast lands immediately
        {
            let guard = entry_a.lock().await;
            manager.set_global_yolo(true);
            assert!(manager.global_yolo());
            assert_eq!(guard.agent().permission().mode(), PermissionMode::Yolo);
        }

        // A new work_dir's instance inherits the global mode
        let entry_b = manager.get_or_create_for_session(&s2).await.unwrap();
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

    #[tokio::test]
    async fn global_plan_broadcast_skips_locked_session() {
        let (manager, dir_a, _dir_b) = make_manager().await;
        let s1 = create_session_in(&manager, &dir_a).await;
        let entry_a = manager.get_or_create_for_session(&s1).await.unwrap();

        // Applies directly while idle
        manager.set_global_plan(true);
        assert!(manager.global_plan());
        assert!(entry_a.lock().await.plan_mode());

        // Locked: broadcast skipped (no deadlock), the flag still updates,
        // the stale instance value gets corrected by the entry sync
        {
            let guard = entry_a.lock().await;
            manager.set_global_plan(false);
            assert!(!manager.global_plan());
            assert!(guard.plan_mode());
        }

        // Broadcast again after the lock is released
        manager.set_global_plan(false);
        assert!(!entry_a.lock().await.plan_mode());
    }

    #[tokio::test]
    async fn rebuilt_session_inherits_global_modes() {
        let (manager, dir_a, _dir_b) = make_manager().await;
        let s1 = create_session_in(&manager, &dir_a).await;
        let _ = manager.get_or_create_for_session(&s1).await.unwrap();
        manager.set_global_yolo(true);
        manager.set_global_plan(true);

        manager.invalidate(&dir_a);
        assert!(
            manager.all_sessions().is_empty(),
            "invalidate removes the dir's instances"
        );
        let entry = manager.get_or_create_for_session(&s1).await.unwrap();
        let session = entry.lock().await;
        assert_eq!(session.agent().permission().mode(), PermissionMode::Yolo);
        assert!(session.plan_mode());
    }

    #[tokio::test]
    async fn invalidate_is_scoped_to_dir() {
        let (manager, dir_a, dir_b) = make_manager().await;
        let s1 = create_session_in(&manager, &dir_a).await;
        let s2 = create_session_in(&manager, &dir_b).await;
        manager.get_or_create_for_session(&s1).await.unwrap();
        manager.get_or_create_for_session(&s2).await.unwrap();

        manager.invalidate(&dir_a);
        let remaining = manager.all_sessions().len();
        assert_eq!(remaining, 1, "only dir_a's instance is removed");

        // s2's instance survives; s1 rebuilds on demand
        let e2 = manager.get_or_create_for_session(&s2).await.unwrap();
        assert_eq!(
            e2.lock().await.current_session_id().as_deref(),
            Some(s2.as_str())
        );
    }

    #[tokio::test]
    async fn remove_session_cancels_and_evicts() {
        let (manager, dir_a, _dir_b) = make_manager().await;
        let s1 = create_session_in(&manager, &dir_a).await;
        let entry = manager.get_or_create_for_session(&s1).await.unwrap();

        assert!(manager.remove_session(&s1));
        assert!(manager.all_sessions().is_empty());
        assert!(
            entry.lock().await.cancel_flag().load(Ordering::Relaxed),
            "the evicted instance's cancel flag should be set"
        );
        assert!(!manager.remove_session(&s1), "double removal returns false");

        // A fresh instance rebuilds and binds to the same session (DB row kept)
        let e1 = manager.get_or_create_for_session(&s1).await.unwrap();
        assert_eq!(
            e1.lock().await.current_session_id().as_deref(),
            Some(s1.as_str())
        );
    }

    #[tokio::test]
    async fn unknown_session_id_is_rejected() {
        let (manager, _dir_a, _dir_b) = make_manager().await;
        let err = manager
            .get_or_create_for_session("no-such-session")
            .await
            .map(|_| ())
            .unwrap_err();
        assert!(err.to_string().contains("会话不存在"));
    }

    #[tokio::test]
    async fn session_permission_rules_persist_across_rebuild() {
        let (manager, dir_a, _dir_b) = make_manager().await;
        let s1 = create_session_in(&manager, &dir_a).await;
        let entry = manager.get_or_create_for_session(&s1).await.unwrap();

        // User picks "always" -> the rule lands in the DB
        entry
            .lock()
            .await
            .permission_manager()
            .add_session_rules_async("shell", &["git *".to_string()])
            .await;
        assert_eq!(
            entry
                .lock()
                .await
                .permission_manager()
                .session_rules()
                .len(),
            1
        );

        // invalidate + rebuild: the rule comes back from the DB
        manager.invalidate(&dir_a);
        let entry = manager.get_or_create_for_session(&s1).await.unwrap();
        let rules = entry.lock().await.permission_manager().session_rules();
        assert!(
            rules
                .iter()
                .any(|r| r.permission == "shell" && r.pattern == "git *"),
            "rebuilt instance must restore the always rule from the DB, got: {rules:?}"
        );
    }
}
