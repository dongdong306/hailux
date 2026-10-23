use chrono::Local;
use color_eyre::Result;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::path::PathBuf;

mod convert;
mod messages;
mod models;
mod sessions;
mod usage;

#[cfg(test)]
mod tests;

pub use convert::{from_stored_message, to_stored_message};
pub use models::{
    DailyUsage, MessageRole, ModelUsage, ProjectUsage, SessionSummary, StoredMessage,
    SubsessionSummary, UsageRecord, UsageSummary, WorkDirInfo,
};

#[derive(Debug, Clone)]
pub struct ChatStorage {
    pub(super) pool: SqlitePool,
    /// 启动时迁移失败的原因（如校验和不符、缺列）。失败不阻塞启动，
    /// 由 UI 提示用户用 /rebuild-db 重建数据库。
    migration_error: Option<String>,
}

impl ChatStorage {
    pub async fn new() -> Result<Self> {
        let db_path = Self::db_path()?;
        if let Some(parent) = db_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let options = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Delete);

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(10))
            .connect_with(options)
            .await?;

        let mut storage = Self {
            pool,
            migration_error: None,
        };
        // 迁移失败不阻塞启动：schema 大概率已就绪，真正缺列会在后续查询中
        // 报明确错误；错误原因保存下来，由 UI 提示用户 /rebuild-db 重建。
        if let Err(e) = storage.run_migration().await {
            storage.migration_error = Some(e.to_string());
        }
        Ok(storage)
    }

    /// 启动时迁移失败的原因（无则 None）。
    pub fn migration_error(&self) -> Option<&str> {
        self.migration_error.as_deref()
    }

    /// 重建数据库：先把旧库文件备份为独立文件，再清空全部表并重新执行迁移。
    /// 所有持有同一连接池的 ChatStorage 副本共享 pool，重建后立即生效，
    /// 无需替换任何引用。旧数据保留在备份文件中（测试环境返回 None，不备份）。
    pub async fn rebuild(&mut self) -> Result<Option<PathBuf>> {
        // 1. 备份数据库文件（尽力而为，失败不阻塞重建）
        #[cfg(not(test))]
        let backup_path = self.backup_db_file().await.ok();
        #[cfg(test)]
        let backup_path = None;

        // 2. 清空全部表（会话/消息/迁移记录）
        let mut tx = self.pool.begin().await?;
        sqlx::query("DROP TABLE IF EXISTS _sqlx_migrations")
            .execute(&mut *tx)
            .await?;
        sqlx::query("DROP TABLE IF EXISTS messages")
            .execute(&mut *tx)
            .await?;
        sqlx::query("DROP TABLE IF EXISTS sessions")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        // 3. 重新执行迁移，重建 schema
        self.run_migration().await?;
        self.migration_error = None;
        Ok(backup_path)
    }

    /// 复制 DB 文件做备份（非 WAL 模式，主文件始终包含最新数据）。
    /// 仅生产路径使用：测试环境（in-memory 库）不备份，避免复制真实用户数据。
    #[cfg(not(test))]
    async fn backup_db_file(&self) -> Result<PathBuf> {
        let db_path = Self::db_path()?;
        let stamp = Local::now().format("%Y%m%d-%H%M%S");
        let backup_path = db_path.with_file_name(format!("chat.db.bak-{stamp}"));
        tokio::fs::copy(&db_path, &backup_path).await?;
        Ok(backup_path)
    }

    pub(super) async fn run_migration(&self) -> Result<()> {
        // 版本化迁移（pre-0.4.0 旧库的引导升级已移除：此类库会在迁移中报错，
        // 由 new()/rebuild() 捕获后提示用户 /rebuild-db 重建）。
        sqlx::migrate!("./migrations").run(&self.pool).await?;
        Ok(())
    }

    fn db_path() -> Result<PathBuf> {
        let home = dirs::home_dir().ok_or_else(|| color_eyre::eyre::eyre!("无法获取用户主目录"))?;
        Ok(home.join(".hailux").join("db").join("chat.db"))
    }

    pub(super) fn now_iso() -> String {
        Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
    }
}

/// work_dir 的统一存储形式：Windows 盘符绝对路径补 `\\?\` verbatim 前缀。
/// 常规链路（session 层 canonicalize、TUI current_work_dir）已产出 verbatim，
/// 此处兜底归一绕过 canonicalize 的调用点，避免同一目录在
/// GROUP BY work_dir 的聚合/项目列表中拆成两行。
/// Unix 路径、UNC（`\\server\...`）、盘符相对路径（`C:foo`）与已带前缀的原样返回。
pub(super) fn ensure_verbatim(dir: &str) -> std::borrow::Cow<'_, str> {
    let b = dir.as_bytes();
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
        std::borrow::Cow::Owned(format!(r"\\?\{dir}"))
    } else {
        std::borrow::Cow::Borrowed(dir)
    }
}

/// work_dir 的 DB 匹配变体：原样 + Windows verbatim 前缀形式（去重后至多两个）。
/// 兼容历史数据中 TUI（带 `\\?\` 前缀）与 Web（剥离前缀）两种写入写法。
/// 仅 Windows 存在两种写法（见 `ensure_verbatim`）；其他平台原样重复占位。
#[cfg(windows)]
pub(super) fn work_dir_variants(dir: &str) -> [String; 2] {
    let verbatim = if dir.starts_with(r"\\?\") {
        dir.strip_prefix(r"\\?\").unwrap_or(dir).to_string()
    } else {
        format!(r"\\?\{}", dir)
    };
    [dir.to_string(), verbatim]
}

#[cfg(not(windows))]
pub(super) fn work_dir_variants(dir: &str) -> [String; 2] {
    [dir.to_string(), dir.to_string()]
}
