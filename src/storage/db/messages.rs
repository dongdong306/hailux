use color_eyre::Result;

use super::ChatStorage;
use super::models::{MessageRole, StoredMessage};

impl ChatStorage {
    pub async fn append_message(&self, session_id: &str, msg: &StoredMessage) -> Result<()> {
        let now = Self::now_iso();
        sqlx::query(
            "INSERT INTO messages (session_id, role, content, tool_calls, tool_call_id, reasoning_content, prompt_tokens, completion_tokens, cached_tokens, model, runtime_meta, think_ms, compacted, attachments, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        .bind(&msg.attachments)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 执行给定 SQL 查询并将结果映射为 `StoredMessage` 列表。
    /// `sql` 必须按顺序选择以下列且只接收一个 `session_id` 绑定参数：
    /// `id, role, content, tool_calls, tool_call_id, reasoning_content,
    ///  prompt_tokens, completion_tokens, cached_tokens, model, runtime_meta, think_ms, compacted, attachments`
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
            Option<String>,
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
                    attachments,
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
                        attachments,
                    })
                },
            )
            .collect::<Result<Vec<_>>>()
    }

    pub async fn load_messages(&self, session_id: &str) -> Result<Vec<StoredMessage>> {
        self.query_messages(
            "SELECT id, role, content, tool_calls, tool_call_id, reasoning_content, prompt_tokens, completion_tokens, cached_tokens, model, runtime_meta, think_ms, compacted, attachments FROM messages WHERE session_id = ? ORDER BY id ASC",
            session_id,
        )
        .await
    }

    /// 加载活跃上下文消息（仅 compacted=0），按 id 升序。
    pub async fn load_active_messages(&self, session_id: &str) -> Result<Vec<StoredMessage>> {
        self.query_messages(
            "SELECT id, role, content, tool_calls, tool_call_id, reasoning_content, prompt_tokens, completion_tokens, cached_tokens, model, runtime_meta, think_ms, compacted, attachments FROM messages WHERE session_id = ? AND compacted = 0 ORDER BY id ASC",
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
        // 轻量查询：只取角色与 tool call 字段，避免把正文与附件 base64
        // 大字段全量拉入内存（本修复只需扫描尾部 tool call 配对）
        let rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT role, tool_calls, tool_call_id FROM messages WHERE session_id = ? ORDER BY id ASC",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;

        // 反向找到最后一条带 tool_calls 的 assistant 消息
        let Some(assistant_idx) = rows
            .iter()
            .enumerate()
            .rev()
            .find(|(_, (role, tool_calls, _))| role == "assistant" && tool_calls.is_some())
            .map(|(i, _)| i)
        else {
            return Ok(());
        };

        let tool_calls_json = rows[assistant_idx].1.as_ref().unwrap();
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
        let existing_ids: std::collections::HashSet<&str> = rows[assistant_idx..]
            .iter()
            .filter_map(|(role, _, tool_call_id)| {
                if role == "tool" {
                    tool_call_id.as_deref()
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
                    attachments: None,
                };
                self.append_message(session_id, &stored).await?;
            }
        }

        Ok(())
    }
}
