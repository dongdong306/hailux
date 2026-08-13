use crate::agent::models::{
    CompatibleChatCompletionRequestAssistantMessage, CompatibleChatCompletionRequestMessage,
};
use async_openai::types::chat::{
    ChatCompletionMessageToolCalls, ChatCompletionRequestAssistantMessage,
    ChatCompletionRequestAssistantMessageContent, ChatCompletionRequestAssistantMessageContentPart,
    ChatCompletionRequestDeveloperMessageContent, ChatCompletionRequestDeveloperMessageContentPart,
    ChatCompletionRequestSystemMessage, ChatCompletionRequestSystemMessageContent,
    ChatCompletionRequestSystemMessageContentPart, ChatCompletionRequestToolMessage,
    ChatCompletionRequestToolMessageContent, ChatCompletionRequestToolMessageContentPart,
    ChatCompletionRequestUserMessage, ChatCompletionRequestUserMessageContent,
    ChatCompletionRequestUserMessageContentPart,
};
use chrono::Local;
use color_eyre::Result;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

impl MessageRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

impl fmt::Display for MessageRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MessageRole {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "system" => Ok(Self::System),
            "user" => Ok(Self::User),
            "assistant" => Ok(Self::Assistant),
            "tool" => Ok(Self::Tool),
            other => Err(format!("unknown message role: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub model: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct SubsessionSummary {
    pub id: String,
    pub model: String,
    pub title: String,
    #[allow(dead_code)]
    pub created_at: String,
    #[allow(dead_code)]
    pub updated_at: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
}

#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub role: MessageRole,
    pub content: String,
    pub tool_calls: Option<String>,
    pub tool_call_id: Option<String>,
    pub reasoning_content: Option<String>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub runtime_meta: Option<String>,
    pub think_ms: Option<i64>,
    pub compacted: bool,
}

#[derive(Debug, Clone)]
pub struct ChatStorage {
    pool: SqlitePool,
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

    async fn run_migration(&self) -> Result<()> {
        // 旧版（< 0.4.0）用手写 ALTER TABLE fallback 演进 schema，没有 _sqlx_migrations 表。
        // 检测到这类旧库时，先用幂等的 pragma 检查补齐缺失列（一次性引导），
        // 之后所有 schema 演进交给版本化迁移（migrations/*.sql）管理。
        let has_migrations_table: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
        )
        .fetch_one(&self.pool)
        .await?;

        let has_sessions: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'sessions'",
        )
        .fetch_one(&self.pool)
        .await?;

        if has_migrations_table.0 == 0 && has_sessions.0 == 1 {
            self.upgrade_legacy_schema().await?;
        }

        // 版本化迁移。迁移失败由 new()/rebuild() 捕获处理。
        sqlx::migrate!("./migrations").run(&self.pool).await?;
        Ok(())
    }

    /// 旧版（< 0.4.0）手写迁移的一次性引导：仅用于还没有 `_sqlx_migrations`
    /// 表的旧库，幂等地补齐缺失列。新库和已迁移库不会走到这里。
    async fn upgrade_legacy_schema(&self) -> Result<()> {
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS sessions (
                id                TEXT PRIMARY KEY,
                title             TEXT NOT NULL DEFAULT '',
                model             TEXT NOT NULL DEFAULT '',
                work_dir          TEXT NOT NULL DEFAULT '',
                created_at        TEXT NOT NULL,
                updated_at        TEXT NOT NULL,
                prompt_tokens     INTEGER NOT NULL DEFAULT 0,
                completion_tokens INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS messages (
                id                INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id        TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                role              TEXT NOT NULL,
                content           TEXT NOT NULL DEFAULT '',
                tool_calls        TEXT,
                tool_call_id      TEXT,
                reasoning_content TEXT,
                prompt_tokens     INTEGER,
                completion_tokens INTEGER,
                created_at        TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_messages_session_id ON messages(session_id);
            "#,
        )
        .execute(&mut *tx)
        .await?;

        // 兼容旧表：若缺少 reasoning_content 列则补加
        let has_column: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'reasoning_content'",
        )
        .fetch_one(&mut *tx)
        .await?;

        if has_column.0 == 0 {
            sqlx::query("ALTER TABLE messages ADD COLUMN reasoning_content TEXT")
                .execute(&mut *tx)
                .await?;
        }

        // sessions.prompt_tokens
        let has_col: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name = 'prompt_tokens'",
        )
        .fetch_one(&mut *tx)
        .await?;
        if has_col.0 == 0 {
            sqlx::query("ALTER TABLE sessions ADD COLUMN prompt_tokens INTEGER NOT NULL DEFAULT 0")
                .execute(&mut *tx)
                .await?;
        }

        // sessions.completion_tokens
        let has_col: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name = 'completion_tokens'",
        )
        .fetch_one(&mut *tx)
        .await?;
        if has_col.0 == 0 {
            sqlx::query(
                "ALTER TABLE sessions ADD COLUMN completion_tokens INTEGER NOT NULL DEFAULT 0",
            )
            .execute(&mut *tx)
            .await?;
        }

        // messages.prompt_tokens
        let has_col: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'prompt_tokens'",
        )
        .fetch_one(&mut *tx)
        .await?;
        if has_col.0 == 0 {
            sqlx::query("ALTER TABLE messages ADD COLUMN prompt_tokens INTEGER")
                .execute(&mut *tx)
                .await?;
        }

        // messages.completion_tokens
        let has_col: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'completion_tokens'",
        )
        .fetch_one(&mut *tx)
        .await?;
        if has_col.0 == 0 {
            sqlx::query("ALTER TABLE messages ADD COLUMN completion_tokens INTEGER")
                .execute(&mut *tx)
                .await?;
        }

        // sessions.parent_id
        let has_col: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name = 'parent_id'",
        )
        .fetch_one(&mut *tx)
        .await?;
        if has_col.0 == 0 {
            sqlx::query("ALTER TABLE sessions ADD COLUMN parent_id TEXT")
                .execute(&mut *tx)
                .await?;
        }

        // messages.runtime_meta
        let has_col: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'runtime_meta'",
        )
        .fetch_one(&mut *tx)
        .await?;
        if has_col.0 == 0 {
            sqlx::query("ALTER TABLE messages ADD COLUMN runtime_meta TEXT")
                .execute(&mut *tx)
                .await?;
        }

        // messages.think_ms
        let has_col: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'think_ms'",
        )
        .fetch_one(&mut *tx)
        .await?;
        if has_col.0 == 0 {
            sqlx::query("ALTER TABLE messages ADD COLUMN think_ms INTEGER")
                .execute(&mut *tx)
                .await?;
        }

        // messages.compacted
        let has_col: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'compacted'",
        )
        .fetch_one(&mut *tx)
        .await?;
        if has_col.0 == 0 {
            sqlx::query("ALTER TABLE messages ADD COLUMN compacted INTEGER NOT NULL DEFAULT 0")
                .execute(&mut *tx)
                .await?;
        }

        // sessions.compact_summary
        let has_col: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name = 'compact_summary'",
        )
        .fetch_one(&mut *tx)
        .await?;
        if has_col.0 == 0 {
            sqlx::query("ALTER TABLE sessions ADD COLUMN compact_summary TEXT")
                .execute(&mut *tx)
                .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    fn db_path() -> Result<PathBuf> {
        let home = dirs::home_dir().ok_or_else(|| color_eyre::eyre::eyre!("无法获取用户主目录"))?;
        Ok(home.join(".hailux").join("db").join("chat.db"))
    }

    fn now_iso() -> String {
        Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
    }

    pub async fn create_session(&self, model: &str, work_dir: &str) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = Self::now_iso();
        sqlx::query(
            "INSERT INTO sessions (id, title, model, work_dir, created_at, updated_at) VALUES (?, '', ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(model)
        .bind(work_dir)
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
        .bind(work_dir)
        .bind(&now)
        .bind(&now)
        .bind(parent_id)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    pub async fn append_message(&self, session_id: &str, msg: &StoredMessage) -> Result<()> {
        let now = Self::now_iso();
        sqlx::query(
            "INSERT INTO messages (session_id, role, content, tool_calls, tool_call_id, reasoning_content, prompt_tokens, completion_tokens, runtime_meta, think_ms, compacted, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(session_id)
        .bind(msg.role.as_str())
        .bind(&msg.content)
        .bind(&msg.tool_calls)
        .bind(&msg.tool_call_id)
        .bind(&msg.reasoning_content)
        .bind(msg.prompt_tokens)
        .bind(msg.completion_tokens)
        .bind(&msg.runtime_meta)
        .bind(msg.think_ms)
        .bind(if msg.compacted { 1 } else { 0 })
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 执行给定 SQL 查询并将结果映射为 `StoredMessage` 列表。
    /// `sql` 必须按顺序选择以下列且只接收一个 `session_id` 绑定参数：
    /// `id, role, content, tool_calls, tool_call_id, reasoning_content,
    ///  prompt_tokens, completion_tokens, runtime_meta, think_ms, compacted`
    async fn query_messages(
        &self,
        sql: &'static str,
        session_id: &str,
    ) -> Result<Vec<StoredMessage>> {
        type MessageRow = (
            i64,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<i64>,
            Option<String>,
            Option<i64>,
            i64,
        );
        let rows: Vec<MessageRow> = sqlx::query_as(sql)
            .bind(session_id)
            .fetch_all(&self.pool)
            .await?;

        rows.into_iter()
            .map(
                |(
                    _,
                    role,
                    content,
                    tool_calls,
                    tool_call_id,
                    reasoning_content,
                    prompt_tokens,
                    completion_tokens,
                    runtime_meta,
                    think_ms,
                    compacted,
                )| {
                    let role: MessageRole = role
                        .parse()
                        .map_err(|e| color_eyre::eyre::eyre!("解析消息角色失败: {e}"))?;
                    Ok::<_, color_eyre::eyre::Report>(StoredMessage {
                        role,
                        content,
                        tool_calls,
                        tool_call_id,
                        reasoning_content,
                        prompt_tokens,
                        completion_tokens,
                        runtime_meta,
                        think_ms,
                        compacted: compacted != 0,
                    })
                },
            )
            .collect::<Result<Vec<_>>>()
    }

    pub async fn load_messages(&self, session_id: &str) -> Result<Vec<StoredMessage>> {
        self.query_messages(
            "SELECT id, role, content, tool_calls, tool_call_id, reasoning_content, prompt_tokens, completion_tokens, runtime_meta, think_ms, compacted FROM messages WHERE session_id = ? ORDER BY id ASC",
            session_id,
        )
        .await
    }

    /// 加载活跃上下文消息（仅 compacted=0），按 id 升序。
    pub async fn load_active_messages(&self, session_id: &str) -> Result<Vec<StoredMessage>> {
        self.query_messages(
            "SELECT id, role, content, tool_calls, tool_call_id, reasoning_content, prompt_tokens, completion_tokens, runtime_meta, think_ms, compacted FROM messages WHERE session_id = ? AND compacted = 0 ORDER BY id ASC",
            session_id,
        )
        .await
    }

    /// 返回 session 中未压缩消息的数量。
    pub async fn count_active_messages(&self, session_id: &str) -> Result<i64> {
        let row: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM messages WHERE session_id = ? AND compacted = 0")
                .bind(session_id)
                .fetch_one(&self.pool)
                .await?;
        Ok(row.0)
    }

    /// 标记 session 的所有未压缩消息为已压缩。
    pub async fn mark_messages_compacted(&self, session_id: &str) -> Result<()> {
        sqlx::query("UPDATE messages SET compacted = 1 WHERE session_id = ? AND compacted = 0")
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 设置压缩摘要。
    pub async fn set_compact_summary(&self, session_id: &str, summary: &str) -> Result<()> {
        sqlx::query("UPDATE sessions SET compact_summary = ? WHERE id = ?")
            .bind(summary)
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 获取压缩摘要。
    pub async fn get_compact_summary(&self, session_id: &str) -> Result<Option<String>> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT compact_summary FROM sessions WHERE id = ?")
                .bind(session_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.and_then(|(s,)| s))
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
    pub async fn list_top_level_sessions(&self, work_dir: &str) -> Result<Vec<SessionSummary>> {
        let rows: Vec<(String, String, String, String)> = sqlx::query_as(
            "SELECT id, title, model, updated_at FROM sessions WHERE work_dir = ? AND parent_id IS NULL ORDER BY updated_at DESC",
        )
        .bind(work_dir)
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

    pub async fn update_last_message_runtime_meta(
        &self,
        session_id: &str,
        runtime_meta: &str,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE messages SET runtime_meta = ? WHERE id = (SELECT MAX(id) FROM messages WHERE session_id = ?)",
        )
        .bind(runtime_meta)
        .bind(session_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_last_assistant_runtime_meta(
        &self,
        session_id: &str,
        runtime_meta: &str,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE messages SET runtime_meta = ? WHERE id = (SELECT MAX(id) FROM messages WHERE session_id = ? AND role = 'assistant')",
        )
        .bind(runtime_meta)
        .bind(session_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 修复 orphaned tool calls：扫描最后一条 assistant 消息的 tool_calls，
    /// 为缺少 tool result 的 tool_call_id 补一条 "Tool execution aborted" 消息。
    pub async fn repair_orphaned_tool_calls(&self, session_id: &str) -> Result<()> {
        let messages = self.load_messages(session_id).await?;

        // 反向找到最后一条带 tool_calls 的 assistant 消息
        let Some(assistant_idx) = messages
            .iter()
            .enumerate()
            .rev()
            .find(|(_, m)| m.role == MessageRole::Assistant && m.tool_calls.is_some())
            .map(|(i, _)| i)
        else {
            return Ok(());
        };

        let assistant_msg = &messages[assistant_idx];
        let tool_calls_json = assistant_msg.tool_calls.as_ref().unwrap();
        let tool_calls: Vec<serde_json::Value> =
            serde_json::from_str(tool_calls_json).unwrap_or_default();

        // 提取所有 tool_call id
        let tool_call_ids: Vec<String> = tool_calls
            .iter()
            .filter_map(|tc| tc.get("id").and_then(|v| v.as_str()).map(String::from))
            .collect();

        if tool_call_ids.is_empty() {
            return Ok(());
        }

        // 收集该 assistant 消息之后已有的 tool result 的 tool_call_id
        let existing_ids: std::collections::HashSet<&str> = messages[assistant_idx..]
            .iter()
            .filter_map(|m| {
                if m.role == MessageRole::Tool {
                    m.tool_call_id.as_deref()
                } else {
                    None
                }
            })
            .collect();

        // 为缺失的 tool_call_id 补 tool result
        for id in &tool_call_ids {
            if !existing_ids.contains(id.as_str()) {
                let stored = StoredMessage {
                    role: MessageRole::Tool,
                    content: "Tool execution aborted".to_string(),
                    tool_calls: None,
                    tool_call_id: Some(id.clone()),
                    reasoning_content: None,
                    prompt_tokens: None,
                    completion_tokens: None,
                    runtime_meta: None,
                    think_ms: None,
                    compacted: false,
                };
                self.append_message(session_id, &stored).await?;
            }
        }

        Ok(())
    }
}

fn extract_user_content(content: &ChatCompletionRequestUserMessageContent) -> String {
    match content {
        ChatCompletionRequestUserMessageContent::Text(t) => t.clone(),
        ChatCompletionRequestUserMessageContent::Array(parts) => parts
            .iter()
            .filter_map(|p| match p {
                ChatCompletionRequestUserMessageContentPart::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn extract_assistant_content(
    content: &Option<ChatCompletionRequestAssistantMessageContent>,
) -> String {
    match content {
        Some(ChatCompletionRequestAssistantMessageContent::Text(t)) => t.clone(),
        Some(ChatCompletionRequestAssistantMessageContent::Array(parts)) => parts
            .iter()
            .filter_map(|p| match p {
                ChatCompletionRequestAssistantMessageContentPart::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        None => String::new(),
    }
}

fn extract_tool_content(content: &ChatCompletionRequestToolMessageContent) -> String {
    match content {
        ChatCompletionRequestToolMessageContent::Text(t) => t.clone(),
        ChatCompletionRequestToolMessageContent::Array(parts) => parts
            .iter()
            .map(|p| match p {
                ChatCompletionRequestToolMessageContentPart::Text(t) => t.text.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn extract_system_content(content: &ChatCompletionRequestSystemMessageContent) -> String {
    match content {
        ChatCompletionRequestSystemMessageContent::Text(t) => t.clone(),
        ChatCompletionRequestSystemMessageContent::Array(parts) => parts
            .iter()
            .map(|p| match p {
                ChatCompletionRequestSystemMessageContentPart::Text(t) => t.text.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn extract_developer_content(content: &ChatCompletionRequestDeveloperMessageContent) -> String {
    match content {
        ChatCompletionRequestDeveloperMessageContent::Text(t) => t.clone(),
        ChatCompletionRequestDeveloperMessageContent::Array(parts) => parts
            .iter()
            .map(|p| match p {
                ChatCompletionRequestDeveloperMessageContentPart::Text(t) => t.text.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn compatible_reasoning_content(msg: &CompatibleChatCompletionRequestMessage) -> Option<String> {
    match msg {
        CompatibleChatCompletionRequestMessage::Assistant(m) => m.reasoning_content.clone(),
        _ => None,
    }
}

pub fn compatible_message_role(msg: &CompatibleChatCompletionRequestMessage) -> MessageRole {
    match msg {
        CompatibleChatCompletionRequestMessage::System(_) => MessageRole::System,
        CompatibleChatCompletionRequestMessage::Developer(_) => MessageRole::System,
        CompatibleChatCompletionRequestMessage::User(_) => MessageRole::User,
        CompatibleChatCompletionRequestMessage::Assistant(_) => MessageRole::Assistant,
        CompatibleChatCompletionRequestMessage::Tool(_) => MessageRole::Tool,
        CompatibleChatCompletionRequestMessage::Function(_) => MessageRole::Tool,
    }
}

pub fn compatible_message_content_text(msg: &CompatibleChatCompletionRequestMessage) -> String {
    match msg {
        CompatibleChatCompletionRequestMessage::User(m) => extract_user_content(&m.content),
        CompatibleChatCompletionRequestMessage::Assistant(m) => {
            extract_assistant_content(&m.base.content)
        }
        CompatibleChatCompletionRequestMessage::Tool(m) => extract_tool_content(&m.content),
        CompatibleChatCompletionRequestMessage::System(m) => extract_system_content(&m.content),
        CompatibleChatCompletionRequestMessage::Developer(m) => {
            extract_developer_content(&m.content)
        }
        CompatibleChatCompletionRequestMessage::Function(m) => {
            m.content.clone().unwrap_or_default()
        }
    }
}

pub fn compatible_message_tool_calls_json(
    msg: &CompatibleChatCompletionRequestMessage,
) -> Option<String> {
    match msg {
        CompatibleChatCompletionRequestMessage::Assistant(m) => m
            .base
            .tool_calls
            .as_ref()
            .map(|tc| serde_json::to_string(tc).unwrap_or_default()),
        _ => None,
    }
}

pub fn compatible_message_tool_call_id(
    msg: &CompatibleChatCompletionRequestMessage,
) -> Option<String> {
    match msg {
        CompatibleChatCompletionRequestMessage::Tool(m) => Some(m.tool_call_id.clone()),
        _ => None,
    }
}

pub fn to_stored_message(msg: &CompatibleChatCompletionRequestMessage) -> StoredMessage {
    StoredMessage {
        role: compatible_message_role(msg),
        content: compatible_message_content_text(msg),
        tool_calls: compatible_message_tool_calls_json(msg),
        tool_call_id: compatible_message_tool_call_id(msg),
        reasoning_content: compatible_reasoning_content(msg),
        prompt_tokens: None,
        completion_tokens: None,
        runtime_meta: None,
        think_ms: None,
        compacted: false,
    }
}

pub fn from_stored_message(msg: &StoredMessage) -> Option<CompatibleChatCompletionRequestMessage> {
    match msg.role {
        MessageRole::System => Some(
            ChatCompletionRequestSystemMessage {
                content: ChatCompletionRequestSystemMessageContent::Text(msg.content.clone()),
                name: None,
            }
            .into(),
        ),
        MessageRole::User => Some(
            ChatCompletionRequestUserMessage {
                content: ChatCompletionRequestUserMessageContent::Text(msg.content.clone()),
                name: None,
            }
            .into(),
        ),
        MessageRole::Assistant => {
            let tool_calls: Option<Vec<ChatCompletionMessageToolCalls>> = msg
                .tool_calls
                .as_ref()
                .and_then(|tc| serde_json::from_str(tc).ok());
            Some(
                CompatibleChatCompletionRequestAssistantMessage {
                    base: ChatCompletionRequestAssistantMessage {
                        content: if msg.content.is_empty() {
                            None
                        } else {
                            Some(ChatCompletionRequestAssistantMessageContent::Text(
                                msg.content.clone(),
                            ))
                        },
                        tool_calls,
                        ..Default::default()
                    },
                    reasoning_content: msg.reasoning_content.clone(),
                }
                .into(),
            )
        }
        MessageRole::Tool => Some(
            ChatCompletionRequestToolMessage {
                content: ChatCompletionRequestToolMessageContent::Text(msg.content.clone()),
                tool_call_id: msg.tool_call_id.clone().unwrap_or_default(),
            }
            .into(),
        ),
    }
}

#[cfg(test)]
impl ChatStorage {
    async fn new_in_memory() -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(":memory:")
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        let storage = Self {
            pool,
            migration_error: None,
        };
        storage.run_migration().await?;
        Ok(storage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_message(role: MessageRole, content: &str) -> StoredMessage {
        StoredMessage {
            role,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            prompt_tokens: None,
            completion_tokens: None,
            runtime_meta: None,
            think_ms: None,
            compacted: false,
        }
    }

    #[tokio::test]
    async fn migration_adds_compacted_column() {
        let storage = ChatStorage::new_in_memory().await.unwrap();
        let session_id = storage.create_session("test-model", "/tmp").await.unwrap();
        storage
            .append_message(&session_id, &make_message(MessageRole::User, "hi"))
            .await
            .unwrap();
        let msgs = storage.load_messages(&session_id).await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert!(!msgs[0].compacted);
    }

    #[tokio::test]
    async fn migration_adds_compact_summary_column() {
        let storage = ChatStorage::new_in_memory().await.unwrap();
        let session_id = storage.create_session("test-model", "/tmp").await.unwrap();
        let summary = storage.get_compact_summary(&session_id).await.unwrap();
        assert!(summary.is_none());
    }

    #[tokio::test]
    async fn legacy_db_upgrades_to_versioned_schema() {
        // 模拟旧版（手写迁移时代）的库：没有 _sqlx_migrations 表，且缺列
        let options = SqliteConnectOptions::new()
            .filename(":memory:")
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT NOT NULL DEFAULT '', model TEXT NOT NULL DEFAULT '', work_dir TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, role TEXT NOT NULL, content TEXT NOT NULL DEFAULT '', tool_calls TEXT, tool_call_id TEXT, created_at TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let storage = ChatStorage {
            pool,
            migration_error: None,
        };
        storage.run_migration().await.unwrap();

        // 版本化迁移表已建立
        let has_migrations: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
        )
        .fetch_one(&storage.pool)
        .await
        .unwrap();
        assert_eq!(has_migrations.0, 1);

        // 缺失列已补齐
        for col in [
            "reasoning_content",
            "prompt_tokens",
            "completion_tokens",
            "runtime_meta",
            "think_ms",
            "compacted",
        ] {
            let n: (i64,) =
                sqlx::query_as("SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = ?")
                    .bind(col)
                    .fetch_one(&storage.pool)
                    .await
                    .unwrap();
            assert_eq!(n.0, 1, "messages 缺少列 {col}");
        }
        for col in [
            "prompt_tokens",
            "completion_tokens",
            "parent_id",
            "compact_summary",
        ] {
            let n: (i64,) =
                sqlx::query_as("SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name = ?")
                    .bind(col)
                    .fetch_one(&storage.pool)
                    .await
                    .unwrap();
            assert_eq!(n.0, 1, "sessions 缺少列 {col}");
        }

        // 升级后读写正常
        let sid = storage.create_session("m", "/tmp").await.unwrap();
        storage
            .append_message(&sid, &make_message(MessageRole::User, "hi"))
            .await
            .unwrap();
        let msgs = storage.load_messages(&sid).await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert!(!msgs[0].compacted);

        // 再次启动（幂等）不报错
        storage.run_migration().await.unwrap();
    }

    #[tokio::test]
    async fn mark_compacted_sets_all_uncompacted() {
        let storage = ChatStorage::new_in_memory().await.unwrap();
        let sid = storage.create_session("m", "/tmp").await.unwrap();
        storage
            .append_message(&sid, &make_message(MessageRole::System, "sys"))
            .await
            .unwrap();
        storage
            .append_message(&sid, &make_message(MessageRole::User, "u1"))
            .await
            .unwrap();
        storage
            .append_message(&sid, &make_message(MessageRole::Assistant, "a1"))
            .await
            .unwrap();

        storage.mark_messages_compacted(&sid).await.unwrap();

        let msgs = storage.load_messages(&sid).await.unwrap();
        assert!(msgs.iter().all(|m| m.compacted));
    }

    #[tokio::test]
    async fn mark_compacted_only_affects_uncompacted() {
        let storage = ChatStorage::new_in_memory().await.unwrap();
        let sid = storage.create_session("m", "/tmp").await.unwrap();
        storage
            .append_message(&sid, &make_message(MessageRole::User, "old"))
            .await
            .unwrap();
        storage.mark_messages_compacted(&sid).await.unwrap();
        storage
            .append_message(&sid, &make_message(MessageRole::User, "new"))
            .await
            .unwrap();

        storage.mark_messages_compacted(&sid).await.unwrap();

        let msgs = storage.load_messages(&sid).await.unwrap();
        assert!(msgs.iter().all(|m| m.compacted));
        assert_eq!(msgs.len(), 2);
    }

    #[tokio::test]
    async fn set_and_get_compact_summary() {
        let storage = ChatStorage::new_in_memory().await.unwrap();
        let sid = storage.create_session("m", "/tmp").await.unwrap();
        storage.set_compact_summary(&sid, "摘要文本").await.unwrap();
        let summary = storage.get_compact_summary(&sid).await.unwrap();
        assert_eq!(summary.as_deref(), Some("摘要文本"));
    }

    #[tokio::test]
    async fn load_active_excludes_compacted() {
        let storage = ChatStorage::new_in_memory().await.unwrap();
        let sid = storage.create_session("m", "/tmp").await.unwrap();
        storage
            .append_message(&sid, &make_message(MessageRole::System, "sys"))
            .await
            .unwrap();
        storage
            .append_message(&sid, &make_message(MessageRole::User, "u1"))
            .await
            .unwrap();
        storage
            .append_message(&sid, &make_message(MessageRole::Assistant, "a1"))
            .await
            .unwrap();

        storage.mark_messages_compacted(&sid).await.unwrap();
        storage
            .append_message(&sid, &make_message(MessageRole::User, "u2"))
            .await
            .unwrap();
        storage
            .append_message(&sid, &make_message(MessageRole::Assistant, "a2"))
            .await
            .unwrap();

        let active = storage.load_active_messages(&sid).await.unwrap();
        assert_eq!(active.len(), 2);
        assert_eq!(active[0].content, "u2");
        assert_eq!(active[1].content, "a2");
        assert!(active.iter().all(|m| !m.compacted));
    }

    #[tokio::test]
    async fn load_active_returns_all_when_no_compaction() {
        let storage = ChatStorage::new_in_memory().await.unwrap();
        let sid = storage.create_session("m", "/tmp").await.unwrap();
        storage
            .append_message(&sid, &make_message(MessageRole::User, "u1"))
            .await
            .unwrap();
        storage
            .append_message(&sid, &make_message(MessageRole::Assistant, "a1"))
            .await
            .unwrap();

        let active = storage.load_active_messages(&sid).await.unwrap();
        assert_eq!(active.len(), 2);
    }

    #[tokio::test]
    async fn multiple_compactions_active_messages() {
        let storage = ChatStorage::new_in_memory().await.unwrap();
        let sid = storage.create_session("m", "/tmp").await.unwrap();

        storage
            .append_message(&sid, &make_message(MessageRole::User, "u1"))
            .await
            .unwrap();
        storage.mark_messages_compacted(&sid).await.unwrap();

        storage
            .append_message(&sid, &make_message(MessageRole::Assistant, "a1"))
            .await
            .unwrap();
        storage.mark_messages_compacted(&sid).await.unwrap();

        storage
            .append_message(&sid, &make_message(MessageRole::User, "u2"))
            .await
            .unwrap();

        let active = storage.load_active_messages(&sid).await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].content, "u2");

        let all = storage.load_messages(&sid).await.unwrap();
        assert_eq!(all.len(), 3);
        assert!(all[0].compacted);
        assert!(all[1].compacted);
        assert!(!all[2].compacted);
    }

    #[tokio::test]
    async fn append_message_defaults_uncompacted() {
        let storage = ChatStorage::new_in_memory().await.unwrap();
        let sid = storage.create_session("m", "/tmp").await.unwrap();
        let stored = make_message(MessageRole::User, "hello");
        storage.append_message(&sid, &stored).await.unwrap();

        let loaded = storage.load_messages(&sid).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(!loaded[0].compacted);
    }

    #[tokio::test]
    async fn rebuild_drops_all_tables_and_recreates_schema() {
        let mut storage = ChatStorage::new_in_memory().await.unwrap();
        let sid = storage.create_session("m", "/tmp").await.unwrap();
        storage
            .append_message(&sid, &make_message(MessageRole::User, "hi"))
            .await
            .unwrap();

        // 测试环境（in-memory）不备份文件
        let backup_path = storage.rebuild().await.unwrap();
        assert!(backup_path.is_none());

        // 旧数据全部清空
        let sessions = storage.list_top_level_sessions("/tmp").await.unwrap();
        assert!(sessions.is_empty());
        // 迁移错误被清除，schema 就绪
        assert!(storage.migration_error().is_none());
        let has_migrations: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
        )
        .fetch_one(&storage.pool)
        .await
        .unwrap();
        assert_eq!(has_migrations.0, 1);
        // 重建后可正常写入
        let new_sid = storage.create_session("m", "/tmp").await.unwrap();
        storage
            .append_message(&new_sid, &make_message(MessageRole::User, "after"))
            .await
            .unwrap();
        let msgs = storage.load_messages(&new_sid).await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "after");
    }
}
