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

    /// 获取 session 级权限规则（JSON 字符串）。
    pub async fn get_session_permission(&self, session_id: &str) -> Result<Option<String>> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT permission FROM sessions WHERE id = ?")
                .bind(session_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.and_then(|(s,)| s))
    }

    /// 设置 session 级权限规则（JSON 字符串）。
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

    /// 列出指定工作目录下的顶层 session（排除 subagent 子会话），按更新时间倒序。
    ///
    /// work_dir 做变体匹配：兼容 TUI 写入的 `\\?\` verbatim 前缀
    /// 与 Web 写入的剥离形式（历史数据与两进程写入并存）。
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

    /// 列出所有工作目录下的顶层 session（排除 subagent 子会话），带 work_dir，按更新时间倒序。
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

    /// 查询某 session 的标题；不存在返回 None。
    pub async fn get_session_title(&self, session_id: &str) -> Result<Option<String>> {
        let row: Option<(String,)> = sqlx::query_as("SELECT title FROM sessions WHERE id = ?")
            .bind(session_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|(title,)| title))
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

    /// 列出历史会话中出现过的全部工作目录（DISTINCT，按最近使用倒序），
    /// 附带会话数与最近活跃时间（供项目选择器展示）。
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

    /// 查询会话归属的工作目录
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

    /// 查询某 session 的所有子 session（subagent 执行记录），按创建时间升序。
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
        // 级联删除 subagent 子会话及其消息，避免残留孤儿行
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
