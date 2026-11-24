//! Web server shared state.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::session::SessionManager;

use super::bus::BusRegistry;
use super::task_registry::TaskRegistry;

pub struct WebServerState {
    pub manager: SessionManager,
    pub registry: Arc<TaskRegistry>,
    /// Session event bus registry (run -> SSE subscriber fanout + reconnect
    /// replay, see bus.rs)
    pub buses: BusRegistry,
    /// Server start timestamp (secs): the buses' epoch. Changes across
    /// restarts, invalidating client cursors (full rebuild from history).
    pub epoch: u64,
    /// Global concurrency cap: max concurrent top-level runs (0 = unlimited)
    pub run_permits: Arc<tokio::sync::Semaphore>,
    /// Raw cap value for the 429 message (0 = unlimited)
    pub run_limit: u32,
    /// Server default work dir (used when a request carries no work_dir)
    pub default_work_dir: PathBuf,
    /// Config write lock: serializes model switch/add/delete requests
    /// (full read-modify-write of cfg against lost updates)
    pub config_write_lock: tokio::sync::Mutex<()>,
}

impl WebServerState {
    pub fn new_epoch() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Build the run-permit semaphore from the config (0 = unlimited).
    pub fn run_permits_from_limit(limit: u32) -> Arc<tokio::sync::Semaphore> {
        if limit == 0 {
            Arc::new(tokio::sync::Semaphore::new(usize::MAX / 2))
        } else {
            Arc::new(tokio::sync::Semaphore::new(limit as usize))
        }
    }
}
