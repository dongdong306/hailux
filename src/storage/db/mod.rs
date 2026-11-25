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

pub use convert::{
    from_stored_message, plan_mode_runtime_meta, stored_plan_mode, to_stored_message,
};
pub use models::{
    DailyUsage, MessageRole, ModelUsage, ProjectUsage, SessionSummary, StoredMessage,
    SubsessionSummary, UsageRecord, UsageSummary, WorkDirInfo,
};

#[derive(Debug, Clone)]
pub struct ChatStorage {
    pub(super) pool: SqlitePool,
    /// Reason the startup migration failed (e.g. checksum mismatch, missing
    /// column). Failure does not block startup; the UI prompts the user to
    /// rebuild the database with /rebuild-db.
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
            // WAL: multiple parallel web sessions read concurrently while writes
            // serialize through SQLite's single-writer lock (guarded by busy_timeout
            // below). The pool size only gates reads + queued writers.
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(5));

        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .acquire_timeout(std::time::Duration::from_secs(10))
            .connect_with(options)
            .await?;

        let mut storage = Self {
            pool,
            migration_error: None,
        };
        // A failed migration does not block startup: the schema is most
        // likely already in place, and a truly missing column surfaces a
        // clear error in later queries. The cause is stored so the UI can
        // prompt the user to rebuild with /rebuild-db.
        if let Err(e) = storage.run_migration().await {
            storage.migration_error = Some(e.to_string());
        }
        Ok(storage)
    }

    /// Reason the startup migration failed (None if it succeeded).
    pub fn migration_error(&self) -> Option<&str> {
        self.migration_error.as_deref()
    }

    /// Rebuild the database: back up the old DB file to a standalone file,
    /// then drop all tables and re-run migrations. Every ChatStorage copy
    /// sharing the same pool sees the rebuild immediately — no references
    /// need replacing. Old data stays in the backup file (tests return
    /// None: no backup).
    pub async fn rebuild(&mut self) -> Result<Option<PathBuf>> {
        // 1. Back up the DB file (best effort; failure does not block the rebuild)
        #[cfg(not(test))]
        let backup_path = self.backup_db_file().await.ok();
        #[cfg(test)]
        let backup_path = None;

        // 2. Drop all tables (sessions/messages/migration records)
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

        // 3. Re-run migrations to rebuild the schema
        self.run_migration().await?;
        self.migration_error = None;
        Ok(backup_path)
    }

    /// Back up the DB file. In WAL mode the latest commits may still live in
    /// the `-wal` file, so copying the main file alone can miss the tail
    /// (and the checkpoint can fail under concurrent connections). The primary
    /// path is `VACUUM INTO` (SQLite 3.27+): a consistent snapshot that
    /// includes WAL content by construction; the checkpoint+copy fallback is
    /// best effort. Production path only: tests use in-memory DBs (never back
    /// up real user data).
    #[cfg(not(test))]
    async fn backup_db_file(&self) -> Result<PathBuf> {
        let db_path = Self::db_path()?;
        let stamp = Local::now().format("%Y%m%d-%H%M%S");
        let backup_path = db_path.with_file_name(format!("chat.db.bak-{stamp}"));
        let vacuum = sqlx::query("VACUUM INTO ?1")
            .bind(backup_path.as_os_str().to_string_lossy().as_ref())
            .execute(&self.pool)
            .await;
        if vacuum.is_err() {
            // Fallback: merge WAL into the main file, then copy (best effort —
            // checkpoint may fail with concurrent connections; the main file
            // still contains the vast majority of the data)
            let _ = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE);")
                .execute(&self.pool)
                .await;
            tokio::fs::copy(&db_path, &backup_path).await?;
        }
        Ok(backup_path)
    }

    pub(super) async fn run_migration(&self) -> Result<()> {
        // Versioned migrations (bootstrap upgrade of pre-0.4.0 legacy DBs
        // was removed: such DBs error during migration; new()/rebuild()
        // catch it and prompt the user to rebuild with /rebuild-db).
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

/// Canonical stored form of work_dir: Windows drive-letter absolute paths
/// get a `\\?\` verbatim prefix. The regular paths (session-level
/// canonicalize, TUI current_work_dir) already produce verbatim; this
/// fallback normalizes call sites that bypass canonicalize so the same
/// directory does not split into two rows in GROUP BY work_dir
/// aggregations/project lists. Unix paths, UNC (`\\server\...`),
/// drive-relative paths (`C:foo`), and already-prefixed paths are returned
/// as-is.
pub(super) fn ensure_verbatim(dir: &str) -> std::borrow::Cow<'_, str> {
    let b = dir.as_bytes();
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
        std::borrow::Cow::Owned(format!(r"\\?\{dir}"))
    } else {
        std::borrow::Cow::Borrowed(dir)
    }
}

/// DB matching variants of work_dir: as-is + the Windows verbatim-prefixed
/// form (at most two after dedup). Tolerates both historical write styles:
/// TUI (with the `\\?\` prefix) and Web (prefix stripped). Only Windows has
/// the two forms (see `ensure_verbatim`); other platforms duplicate the
/// path as a placeholder.
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
