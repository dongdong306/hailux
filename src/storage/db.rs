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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
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

/// 用量汇总（基于 messages 表中带 usage 的 assistant 行 = 一次 LLM 请求）
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct UsageSummary {
    pub requests: i64,
    pub prompt_tokens: i64,
    pub cached_tokens: i64,
    pub completion_tokens: i64,
}

/// 按日聚合的用量
#[derive(Debug, Clone, serde::Serialize)]
pub struct DailyUsage {
    /// 日期（YYYY-MM-DD，本地时区）
    pub date: String,
    pub requests: i64,
    pub prompt_tokens: i64,
    pub cached_tokens: i64,
    pub completion_tokens: i64,
}

/// 按模型聚合的用量
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelUsage {
    pub model: String,
    pub requests: i64,
    pub prompt_tokens: i64,
    pub cached_tokens: i64,
    pub completion_tokens: i64,
}

/// 按项目聚合的用量
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProjectUsage {
    pub work_dir: String,
    pub requests: i64,
    pub prompt_tokens: i64,
    pub cached_tokens: i64,
    pub completion_tokens: i64,
}

/// 已知项目（sessions 表 distinct work_dir，最近活跃降序）
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkDirInfo {
    pub work_dir: String,
    pub sessions: i64,
    /// 最近活跃时间（该目录下会话的最大 updated_at，ISO 本地时间）
    pub last_active: String,
}

/// 最近一次请求的用量明细
#[derive(Debug, Clone, serde::Serialize)]
pub struct UsageRecord {
    pub created_at: String,
    pub model: String,
    pub work_dir: String,
    pub prompt_tokens: i64,
    pub cached_tokens: i64,
    pub completion_tokens: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StoredMessage {
    pub role: MessageRole,
    pub content: String,
    pub tool_calls: Option<String>,
    pub tool_call_id: Option<String>,
    pub reasoning_content: Option<String>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub cached_tokens: Option<i64>,
    /// 该次 LLM 请求实际使用的模型（provider/model display）；用量统计按此分组
    pub model: Option<String>,
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
        // 版本化迁移（pre-0.4.0 旧库的引导升级已移除：此类库会在迁移中报错，
        // 由 new()/rebuild() 捕获后提示用户 /rebuild-db 重建）。
        sqlx::migrate!("./migrations").run(&self.pool).await?;
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

    pub async fn append_message(&self, session_id: &str, msg: &StoredMessage) -> Result<()> {
        let now = Self::now_iso();
        sqlx::query(
            "INSERT INTO messages (session_id, role, content, tool_calls, tool_call_id, reasoning_content, prompt_tokens, completion_tokens, cached_tokens, model, runtime_meta, think_ms, compacted, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(session_id)
        .bind(msg.role.as_str())
        .bind(&msg.content)
        .bind(&msg.tool_calls)
        .bind(&msg.tool_call_id)
        .bind(&msg.reasoning_content)
        .bind(msg.prompt_tokens)
        .bind(msg.completion_tokens)
        .bind(msg.cached_tokens)
        .bind(&msg.model)
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
    ///  prompt_tokens, completion_tokens, cached_tokens, model, runtime_meta, think_ms, compacted`
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
            Option<i64>,
            Option<String>,
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
                    cached_tokens,
                    model,
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
                        cached_tokens,
                        model,
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
            "SELECT id, role, content, tool_calls, tool_call_id, reasoning_content, prompt_tokens, completion_tokens, cached_tokens, model, runtime_meta, think_ms, compacted FROM messages WHERE session_id = ? ORDER BY id ASC",
            session_id,
        )
        .await
    }

    /// 加载活跃上下文消息（仅 compacted=0），按 id 升序。
    pub async fn load_active_messages(&self, session_id: &str) -> Result<Vec<StoredMessage>> {
        self.query_messages(
            "SELECT id, role, content, tool_calls, tool_call_id, reasoning_content, prompt_tokens, completion_tokens, cached_tokens, model, runtime_meta, think_ms, compacted FROM messages WHERE session_id = ? AND compacted = 0 ORDER BY id ASC",
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

    // ── 用量统计（基于 messages 表的 assistant usage 行）────────

    /// 统计的时间下限（ISO 本地时间）；days = 0 表示不限。
    fn usage_cutoff_iso(days: u32) -> String {
        if days == 0 {
            String::new()
        } else {
            (Local::now() - chrono::Duration::days(days as i64))
                .format("%Y-%m-%dT%H:%M:%S")
                .to_string()
        }
    }

    /// 用量统计的公共条件绑定参数（条件片段已内联在各查询的静态 SQL 中，
    /// 共 5 个占位符：NULL 检查 1 + IN 2 + 空串检查 1 + 日期下限 1）。
    /// work_dir = None 时首三参绑 NULL（`? IS NULL` 恒真，跳过目录过滤）；
    /// days = 0 时日期下限绑空串（`? = ''` 恒真，跳过时间过滤）。
    fn usage_filter_binds(work_dir: Option<&str>, days: u32) -> [Option<String>; 5] {
        let variants = work_dir.map(work_dir_variants);
        let cutoff = Self::usage_cutoff_iso(days);
        [
            variants.as_ref().map(|v| v[0].clone()),
            variants.as_ref().map(|v| v[0].clone()),
            variants.as_ref().map(|v| v[1].clone()),
            Some(cutoff.clone()),
            Some(cutoff),
        ]
    }

    /// 汇总：请求数 / 输入 / 缓存命中 / 输出 token 累计。
    pub async fn usage_summary(&self, work_dir: Option<&str>, days: u32) -> Result<UsageSummary> {
        let binds = Self::usage_filter_binds(work_dir, days);
        let mut q = sqlx::query_as::<_, (i64, i64, i64, i64)>(
            "SELECT COUNT(*), COALESCE(SUM(m.prompt_tokens),0), COALESCE(SUM(m.cached_tokens),0), COALESCE(SUM(m.completion_tokens),0) \
             FROM messages m JOIN sessions s ON s.id = m.session_id \
             WHERE m.role = 'assistant' AND m.prompt_tokens IS NOT NULL \
             AND (? IS NULL OR s.work_dir IN (?, ?)) AND (? = '' OR m.created_at >= ?)",
        );
        for b in binds {
            q = q.bind(b);
        }
        let (requests, prompt_tokens, cached_tokens, completion_tokens) =
            q.fetch_one(&self.pool).await?;
        Ok(UsageSummary {
            requests,
            prompt_tokens,
            cached_tokens,
            completion_tokens,
        })
    }

    /// 按日聚合（本地时区日期），date 升序。
    pub async fn usage_daily(&self, work_dir: Option<&str>, days: u32) -> Result<Vec<DailyUsage>> {
        let binds = Self::usage_filter_binds(work_dir, days);
        let mut q = sqlx::query_as::<_, (String, i64, i64, i64, i64)>(
            "SELECT substr(m.created_at, 1, 10) AS day, COUNT(*), COALESCE(SUM(m.prompt_tokens),0), COALESCE(SUM(m.cached_tokens),0), COALESCE(SUM(m.completion_tokens),0) \
             FROM messages m JOIN sessions s ON s.id = m.session_id \
             WHERE m.role = 'assistant' AND m.prompt_tokens IS NOT NULL \
             AND (? IS NULL OR s.work_dir IN (?, ?)) AND (? = '' OR m.created_at >= ?) \
             GROUP BY day ORDER BY day ASC",
        );
        for b in binds {
            q = q.bind(b);
        }
        let rows = q.fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(
                |(date, requests, prompt_tokens, cached_tokens, completion_tokens)| DailyUsage {
                    date,
                    requests,
                    prompt_tokens,
                    cached_tokens,
                    completion_tokens,
                },
            )
            .collect())
    }

    /// 按模型聚合（消息级 model，旧行回退 sessions.model），token 降序。
    pub async fn usage_by_model(
        &self,
        work_dir: Option<&str>,
        days: u32,
    ) -> Result<Vec<ModelUsage>> {
        let binds = Self::usage_filter_binds(work_dir, days);
        let mut q = sqlx::query_as::<_, (String, i64, i64, i64, i64)>(
            "SELECT COALESCE(m.model, s.model), COUNT(*), COALESCE(SUM(m.prompt_tokens),0), COALESCE(SUM(m.cached_tokens),0), COALESCE(SUM(m.completion_tokens),0) \
             FROM messages m JOIN sessions s ON s.id = m.session_id \
             WHERE m.role = 'assistant' AND m.prompt_tokens IS NOT NULL \
             AND (? IS NULL OR s.work_dir IN (?, ?)) AND (? = '' OR m.created_at >= ?) \
             GROUP BY COALESCE(m.model, s.model) ORDER BY SUM(m.prompt_tokens) + SUM(m.completion_tokens) DESC",
        );
        for b in binds {
            q = q.bind(b);
        }
        let rows = q.fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(
                |(model, requests, prompt_tokens, cached_tokens, completion_tokens)| ModelUsage {
                    model,
                    requests,
                    prompt_tokens,
                    cached_tokens,
                    completion_tokens,
                },
            )
            .collect())
    }

    /// 按项目聚合（sessions.work_dir），token 降序，最多 limit 条。
    pub async fn usage_by_project(
        &self,
        work_dir: Option<&str>,
        days: u32,
        limit: u32,
    ) -> Result<Vec<ProjectUsage>> {
        let binds = Self::usage_filter_binds(work_dir, days);
        let mut q = sqlx::query_as::<_, (String, i64, i64, i64, i64)>(
            "SELECT s.work_dir, COUNT(*), COALESCE(SUM(m.prompt_tokens),0), COALESCE(SUM(m.cached_tokens),0), COALESCE(SUM(m.completion_tokens),0) \
             FROM messages m JOIN sessions s ON s.id = m.session_id \
             WHERE m.role = 'assistant' AND m.prompt_tokens IS NOT NULL \
             AND (? IS NULL OR s.work_dir IN (?, ?)) AND (? = '' OR m.created_at >= ?) \
             GROUP BY s.work_dir ORDER BY SUM(m.prompt_tokens) + SUM(m.completion_tokens) DESC \
             LIMIT ?",
        );
        for b in binds {
            q = q.bind(b);
        }
        let rows = q.bind(limit).fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(
                |(work_dir, requests, prompt_tokens, cached_tokens, completion_tokens)| {
                    ProjectUsage {
                        work_dir,
                        requests,
                        prompt_tokens,
                        cached_tokens,
                        completion_tokens,
                    }
                },
            )
            .collect())
    }

    /// 最近请求明细（id 降序，limit 条）。
    pub async fn list_recent_usage(
        &self,
        work_dir: Option<&str>,
        days: u32,
        limit: u32,
    ) -> Result<Vec<UsageRecord>> {
        let binds = Self::usage_filter_binds(work_dir, days);
        let mut q = sqlx::query_as::<_, (String, String, String, i64, i64, i64)>(
            "SELECT m.created_at, COALESCE(m.model, s.model), s.work_dir, m.prompt_tokens, COALESCE(m.cached_tokens,0), m.completion_tokens \
             FROM messages m JOIN sessions s ON s.id = m.session_id \
             WHERE m.role = 'assistant' AND m.prompt_tokens IS NOT NULL \
             AND (? IS NULL OR s.work_dir IN (?, ?)) AND (? = '' OR m.created_at >= ?) \
             ORDER BY m.id DESC LIMIT ?",
        );
        for b in binds {
            q = q.bind(b);
        }
        let rows = q.bind(limit).fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(
                |(created_at, model, work_dir, prompt_tokens, cached_tokens, completion_tokens)| {
                    UsageRecord {
                        created_at,
                        model,
                        work_dir,
                        prompt_tokens,
                        cached_tokens,
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
                    cached_tokens: None,
                    model: None,
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

/// work_dir 的统一存储形式：Windows 盘符绝对路径补 `\\?\` verbatim 前缀。
/// 常规链路（session 层 canonicalize、TUI current_work_dir）已产出 verbatim，
/// 此处兜底归一绕过 canonicalize 的调用点，避免同一目录在
/// GROUP BY work_dir 的聚合/项目列表中拆成两行。
/// Unix 路径、UNC（`\\server\...`）、盘符相对路径（`C:foo`）与已带前缀的原样返回。
fn ensure_verbatim(dir: &str) -> std::borrow::Cow<'_, str> {
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
fn work_dir_variants(dir: &str) -> [String; 2] {
    let verbatim = if dir.starts_with(r"\\?\") {
        dir.strip_prefix(r"\\?\").unwrap_or(dir).to_string()
    } else {
        format!(r"\\?\{}", dir)
    };
    [dir.to_string(), verbatim]
}

#[cfg(not(windows))]
fn work_dir_variants(dir: &str) -> [String; 2] {
    [dir.to_string(), dir.to_string()]
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
        cached_tokens: None,
        model: None,
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
    pub(crate) async fn new_in_memory() -> Result<Self> {
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
            cached_tokens: None,
            model: None,
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
    async fn migration4_backfills_message_model() {
        // 模拟迁移 3 时代的库：每轮最后一条 assistant 行的 runtime_meta 带 {"model": ...}，
        // 中间行为展示文本（非 JSON）；最后一行模拟崩溃轮次（本轮无标记行）。
        let storage = ChatStorage::new_in_memory().await.unwrap();
        let sid = storage
            .create_session("deepseek/chat", "/tmp")
            .await
            .unwrap();
        let rows: &[(&str, &str, &str)] = &[
            ("user", "u1", ""),
            ("assistant", "a-mid1", "tool display text"),
            (
                "assistant",
                "a-end1",
                r#"{"total_ms":100,"model":"deepseek/chat"}"#,
            ),
            ("assistant", "a-mid2", "tool display text"),
            (
                "assistant",
                "a-end2",
                r#"{"total_ms":200,"model":"glm-4.7"}"#,
            ),
            ("assistant", "a-crash", "tool display text"),
        ];
        for (role, content, meta) in rows {
            sqlx::query(
                "INSERT INTO messages (session_id, role, content, runtime_meta, created_at) VALUES (?, ?, ?, ?, ?)",
            )
            .bind(&sid)
            .bind(role)
            .bind(content)
            .bind(meta)
            .bind(ChatStorage::now_iso())
            .execute(&storage.pool)
            .await
            .unwrap();
        }

        // 回退到迁移 3 状态（移除版本 4 记录、model 列及其索引），重跑迁移触发回填
        sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 4")
            .execute(&storage.pool)
            .await
            .unwrap();
        sqlx::query("DROP INDEX IF EXISTS idx_messages_usage")
            .execute(&storage.pool)
            .await
            .unwrap();
        sqlx::query("ALTER TABLE messages DROP COLUMN model")
            .execute(&storage.pool)
            .await
            .unwrap();
        storage.run_migration().await.unwrap();

        let msgs = storage.load_messages(&sid).await.unwrap();
        let model_of = |content: &str| {
            msgs.iter()
                .find(|m| m.content == content)
                .and_then(|m| m.model.clone())
        };
        // 中间行取同轮后继标记行（含自身）的模型；user 行不在回填范围
        assert_eq!(model_of("a-mid1").as_deref(), Some("deepseek/chat"));
        assert_eq!(model_of("a-end1").as_deref(), Some("deepseek/chat"));
        assert_eq!(model_of("a-mid2").as_deref(), Some("glm-4.7"));
        assert_eq!(model_of("a-end2").as_deref(), Some("glm-4.7"));
        // 崩溃轮次行后面无标记行，保持 NULL（统计时回退 sessions.model）
        assert_eq!(model_of("a-crash"), None);
        assert_eq!(msgs.iter().find(|m| m.content == "u1").unwrap().model, None);
    }

    #[tokio::test]
    async fn legacy_pre_040_db_fails_migration() {
        // 模拟旧版（手写迁移时代，< 0.4.0）的库：没有 _sqlx_migrations 表，且缺列。
        // 引导升级已移除：此类库在版本化迁移中报错（迁移 4 的回填引用 runtime_meta），
        // 由上层捕获后提示用户 /rebuild-db 重建，不做静默升级。
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

        let mut storage = ChatStorage {
            pool,
            migration_error: None,
        };
        let result = storage.run_migration().await;
        assert!(result.is_err(), "pre-0.4.0 旧库应迁移失败而非静默升级");

        // rebuild 后恢复正常
        storage.rebuild().await.unwrap();
        let sid = storage.create_session("m", "/tmp").await.unwrap();
        storage
            .append_message(&sid, &make_message(MessageRole::User, "hi"))
            .await
            .unwrap();
        let msgs = storage.load_messages(&sid).await.unwrap();
        assert_eq!(msgs.len(), 1);
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
    async fn usage_stats_roundtrip() {
        let storage = ChatStorage::new_in_memory().await.unwrap();
        let a = storage
            .create_session("deepseek/chat", "/tmp/proj")
            .await
            .unwrap();
        let b = storage
            .create_session("glm-4.7", "/tmp/other")
            .await
            .unwrap();

        let mut assistant_with_usage = make_message(MessageRole::Assistant, "a1");
        assistant_with_usage.prompt_tokens = Some(100);
        assistant_with_usage.completion_tokens = Some(50);
        assistant_with_usage.cached_tokens = Some(80);
        storage
            .append_message(&a, &assistant_with_usage)
            .await
            .unwrap();

        let mut assistant2 = make_message(MessageRole::Assistant, "a2");
        assistant2.prompt_tokens = Some(200);
        assistant2.completion_tokens = Some(10);
        assistant2.cached_tokens = Some(0);
        storage.append_message(&b, &assistant2).await.unwrap();

        // 同一 session（a，创建模型 deepseek/chat）中途切换到 glm-4.7：
        // 消息级 model 优先生效；NULL 则回退 sessions.model（旧数据）
        let mut assistant3 = make_message(MessageRole::Assistant, "a3");
        assistant3.prompt_tokens = Some(400);
        assistant3.completion_tokens = Some(20);
        assistant3.cached_tokens = Some(0);
        assistant3.model = Some("glm-4.7".to_string());
        storage.append_message(&a, &assistant3).await.unwrap();

        // 不带 usage 的消息不计入
        storage
            .append_message(&a, &make_message(MessageRole::User, "u"))
            .await
            .unwrap();

        // load 往返保留消息级 model
        let loaded = storage.load_messages(&a).await.unwrap();
        assert_eq!(loaded[0].model, None);
        assert_eq!(loaded[1].model.as_deref(), Some("glm-4.7"));

        // 全局汇总
        let summary = storage.usage_summary(None, 0).await.unwrap();
        assert_eq!(summary.requests, 3);
        assert_eq!(summary.prompt_tokens, 700);
        assert_eq!(summary.cached_tokens, 80);
        assert_eq!(summary.completion_tokens, 80);

        // 按项目过滤
        let proj = storage.usage_summary(Some("/tmp/proj"), 0).await.unwrap();
        assert_eq!(proj.requests, 2);
        assert_eq!(proj.prompt_tokens, 500);

        // 按日聚合
        let daily = storage.usage_daily(None, 0).await.unwrap();
        assert_eq!(daily.len(), 1);
        assert_eq!(daily[0].requests, 3);

        // 按模型聚合：消息级 model 优先，NULL 回退 sessions.model
        // glm-4.7 = a3(400) + b 的 a2(200)；deepseek/chat = a1(100)
        let by_model = storage.usage_by_model(None, 0).await.unwrap();
        assert_eq!(by_model.len(), 2);
        assert_eq!(by_model[0].model, "glm-4.7");
        assert_eq!(by_model[0].requests, 2);
        assert_eq!(by_model[0].prompt_tokens, 600);
        assert_eq!(by_model[1].model, "deepseek/chat");
        assert_eq!(by_model[1].requests, 1);
        assert_eq!(by_model[1].prompt_tokens, 100);

        // 按项目聚合
        let by_project = storage.usage_by_project(None, 0, 10).await.unwrap();
        assert_eq!(by_project.len(), 2);
        // 570 > 210，/tmp/proj 在前
        assert_eq!(by_project[0].work_dir, "/tmp/proj");
        assert_eq!(by_project[0].prompt_tokens, 500);
        assert_eq!(by_project[1].work_dir, "/tmp/other");

        // 最近明细
        let recent = storage.list_recent_usage(None, 0, 10).await.unwrap();
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].model, "glm-4.7"); // id 降序，a3 消息级模型
        assert_eq!(recent[0].work_dir, "/tmp/proj");
        assert_eq!(recent[1].model, "glm-4.7"); // a2 回退 sessions.model
        assert_eq!(recent[2].model, "deepseek/chat"); // a1 回退 sessions.model

        // 已知项目列表
        let dirs = storage.list_work_dirs().await.unwrap();
        assert_eq!(dirs.len(), 2);
        let names: Vec<&str> = dirs.iter().map(|d| d.work_dir.as_str()).collect();
        assert!(names.contains(&"/tmp/proj"));
        assert!(names.contains(&"/tmp/other"));
        assert!(dirs.iter().all(|d| d.sessions == 1));

        // verbatim 前缀变体匹配
        let proj_verbatim = storage
            .usage_summary(Some(r"\\?\C:\tmp\proj"), 0)
            .await
            .unwrap();
        assert_eq!(proj_verbatim.requests, 0); // 变体不同，不匹配 /tmp/proj
    }

    #[tokio::test]
    async fn usage_stats_respects_days_filter() {
        let storage = ChatStorage::new_in_memory().await.unwrap();
        let sid = storage.create_session("m", "/tmp").await.unwrap();
        let mut assistant = make_message(MessageRole::Assistant, "a");
        assistant.prompt_tokens = Some(10);
        assistant.completion_tokens = Some(5);
        storage.append_message(&sid, &assistant).await.unwrap();

        // days=0 全量可见
        let all = storage.usage_summary(None, 0).await.unwrap();
        assert_eq!(all.requests, 1);

        // 旧库中 created_at 已是现在，30 天窗口内可见
        let recent = storage.usage_summary(None, 30).await.unwrap();
        assert_eq!(recent.requests, 1);
    }

    #[test]
    fn ensure_verbatim_forms() {
        assert_eq!(ensure_verbatim(r"D:\proj").as_ref(), r"\\?\D:\proj");
        assert_eq!(ensure_verbatim(r"\\?\D:\proj").as_ref(), r"\\?\D:\proj");
        assert_eq!(ensure_verbatim("/tmp/proj").as_ref(), "/tmp/proj");
        assert_eq!(
            ensure_verbatim(r"\\server\share").as_ref(),
            r"\\server\share"
        );
        assert_eq!(ensure_verbatim("C:relative").as_ref(), "C:relative");
        assert_eq!(ensure_verbatim("").as_ref(), "");
    }

    #[tokio::test]
    async fn create_session_normalizes_work_dir_to_verbatim() {
        let storage = ChatStorage::new_in_memory().await.unwrap();
        let sid = storage.create_session("m", r"D:\proj").await.unwrap();
        let dir = storage.get_session_work_dir(&sid).await.unwrap().unwrap();
        assert_eq!(dir, r"\\?\D:\proj");
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
