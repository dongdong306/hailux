//! Web 服务端共享状态。

use std::path::PathBuf;
use std::sync::Arc;

use crate::session::SessionManager;

use super::task_registry::TaskRegistry;

pub struct WebServerState {
    pub manager: SessionManager,
    pub registry: Arc<TaskRegistry>,
    /// 服务器默认工作目录（请求未指定 work_dir 时使用）
    pub default_work_dir: PathBuf,
    /// 配置写锁：串行化模型的切换/添加/删除请求（cfg 读-改-写全程，防并发丢失更新）
    pub config_write_lock: tokio::sync::Mutex<()>,
}
