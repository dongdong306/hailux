use color_eyre::Result;

use super::ChatStorage;
use super::models::{SessionSummary, SubsessionSummary, WorkDirInfo};
use super::{ensure_verbatim, work_dir_variants};

impl ChatStorage {
    pub async fn create_session(&self, model: &str, work_dir: &str) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = Self::now_iso();
        sqlx::query(
            "INSERT INTO sessions (id, title, model, work_dir, created_at, updated_at) VALUES (?, '', ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(model)
        .bind(ensure_verbatim(work_dir).as_ref())
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    pub async fn create_subsession(
        &self,
        parent_id: &str,
        model: &str,
        work_dir: &str,
    ) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = Self::now_iso();
        sqlx::query(
            "INSERT INTO sessions (id, title, model, work_dir, created_at, updated_at, parent_id) VALUES (?, '', ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(model)
        .bind(ensure_verbatim(work_dir).as_ref())
        .bind(&now)
        .bind(&now)
        .bind(parent_id)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Get the session-level permission rules (JSON string).
    pub async fn get_session_permission(&self, session_id: &str) -> Result<Option<String>> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT permission FROM sessions WHERE id = ?")
                .bind(session_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.and_then(|(s,)| s))
    }

    /// Set the session-level permission rules (JSON string).
    pub async fn set_session_permission(
        &self,
        session_id: &str,
        permission_json: &str,
    ) -> Result<()> {
        sqlx::query("UPDATE sessions SET permission = ? WHERE id = ?")
            .bind(permission_json)
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// List top-level sessions under the given work dir (excluding subagent
    /// subsessions), ordered by updated_at DESC.
    ///
    /// work_dir is matched by variants: tolerates the `\\?\` verbatim prefix
    /// written by TUI and the stripped form written by Web (historical data
    /// and writes from both processes coexist).
    pub async fn list_top_level_sessions(&self, work_dir: &str) -> Result<Vec<SessionSummary>> {
        let variants = work_dir_variants(work_dir);
        let rows: Vec<(String, String, String, String)> = sqlx::query_as(
            "SELECT id, title, model, updated_at FROM sessions \
             WHERE work_dir IN (?, ?) AND parent_id IS NULL ORDER BY updated_at DESC",
        )
        .bind(&variants[0])
        .bind(&variants[1])
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(id, title, model, updated_at)| SessionSummary {
                id,
                title,
                model,
                updated_at,
            })
            .collect())
    }

    /// List top-level sessions across all work dirs (excluding subagent
    /// subsessions), with work_dir, ordered by updated_at DESC.
    pub async fn list_all_top_level_sessions(&self) -> Result<Vec<(SessionSummary, String)>> {
        let rows: Vec<(String, String, String, String, String)> = sqlx::query_as(
            "SELECT id, title, model, updated_at, work_dir FROM sessions WHERE parent_id IS NULL ORDER BY updated_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(id, title, model, updated_at, work_dir)| {
                (
                    SessionSummary {
                        id,
                        title,
                        model,
                        updated_at,
                    },
                    work_dir,
                )
            })
            .collect())
    }

    /// Get a session's title; returns None if not found.
    pub async fn get_session_title(&self, session_id: &str) -> Result<Option<String>> {
        let row: Option<(String,)> = sqlx::query_as("SELECT title FROM sessions WHERE id = ?")
            .bind(session_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|(title,)| title))
    }

    /// Get a single session's metadata by ID (title/model/updated_at);
    /// returns None if not found.
    pub async fn get_session_info(&self, session_id: &str) -> Result<Option<SessionSummary>> {
        let row: Option<(String, String, String, String)> =
            sqlx::query_as("SELECT id, title, model, updated_at FROM sessions WHERE id = ?")
                .bind(session_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|(id, title, model, updated_at)| SessionSummary {
            id,
            title,
            model,
            updated_at,
        }))
    }

    pub async fn update_session_title(&self, session_id: &str, title: &str) -> Result<()> {
        let now = Self::now_iso();
        sqlx::query("UPDATE sessions SET title = ?, updated_at = ? WHERE id = ?")
            .bind(title)
            .bind(&now)
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Compare-and-set a session's title: replaces it only while it still
    /// equals `expected`. Returns whether a row was updated — `false` means
    /// the title changed underneath the caller (e.g. a user rename), and
    /// the caller's replacement must not win.
    pub async fn update_session_title_if(
        &self,
        session_id: &str,
        expected: &str,
        title: &str,
    ) -> Result<bool> {
        let now = Self::now_iso();
        let res =
            sqlx::query("UPDATE sessions SET title = ?, updated_at = ? WHERE id = ? AND title = ?")
                .bind(title)
                .bind(&now)
                .bind(session_id)
                .bind(expected)
                .execute(&self.pool)
                .await?;
        Ok(res.rows_affected() == 1)
    }

    /// List every work dir seen in session history (DISTINCT, most recently
    /// used first), with session counts and last-active times (for the
    /// project picker).
    pub async fn list_work_dirs(&self) -> Result<Vec<WorkDirInfo>> {
        let rows = sqlx::query_as::<_, (String, i64, String)>(
            "SELECT work_dir, COUNT(*), COALESCE(MAX(updated_at), '') \
             FROM sessions WHERE work_dir != '' \
             GROUP BY work_dir ORDER BY MAX(updated_at) DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(work_dir, sessions, last_active)| WorkDirInfo {
                work_dir,
                sessions,
                last_active,
            })
            .collect())
    }

    /// Get the work dir a session belongs to
    pub async fn get_session_work_dir(&self, session_id: &str) -> Result<Option<String>> {
        let row: Option<(String,)> = sqlx::query_as("SELECT work_dir FROM sessions WHERE id = ?")
            .bind(session_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|(d,)| d))
    }

    pub async fn touch_session(&self, session_id: &str) -> Result<()> {
        let now = Self::now_iso();
        sqlx::query("UPDATE sessions SET updated_at = ? WHERE id = ?")
            .bind(&now)
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn update_session_usage(
        &self,
        session_id: &str,
        prompt_tokens: i64,
        completion_tokens: i64,
    ) -> Result<()> {
        let now = Self::now_iso();
        sqlx::query("UPDATE sessions SET prompt_tokens = ?, completion_tokens = ?, updated_at = ? WHERE id = ?")
            .bind(prompt_tokens)
            .bind(completion_tokens)
            .bind(&now)
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_session_usage(&self, session_id: &str) -> Result<(i64, i64)> {
        let row: (i64, i64) =
            sqlx::query_as("SELECT prompt_tokens, completion_tokens FROM sessions WHERE id = ?")
                .bind(session_id)
                .fetch_one(&self.pool)
                .await?;
        Ok(row)
    }

    /// List all subsessions of a session (subagent runs), ordered by created_at ASC.
    pub async fn list_subsessions(&self, parent_id: &str) -> Result<Vec<SubsessionSummary>> {
        let rows: Vec<(String, String, String, String, String, i64, i64)> = sqlx::query_as(
            "SELECT id, model, title, created_at, updated_at, prompt_tokens, completion_tokens \
             FROM sessions WHERE parent_id = ? ORDER BY created_at ASC",
        )
        .bind(parent_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(id, model, title, created_at, updated_at, prompt_tokens, completion_tokens)| {
                    SubsessionSummary {
                        id,
                        model,
                        title,
                        created_at,
                        updated_at,
                        prompt_tokens,
                        completion_tokens,
                    }
                },
            )
            .collect())
    }

    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        // Cascade-delete subagent subsessions and their messages to avoid orphan rows
        sqlx::query(
            "DELETE FROM messages WHERE session_id IN (SELECT id FROM sessions WHERE parent_id = ?)",
        )
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM sessions WHERE parent_id = ?")
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM messages WHERE session_id = ?")
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}
