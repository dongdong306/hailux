use color_eyre::Result;

use super::ChatStorage;
use super::models::{MessageRole, StoredMessage};

impl ChatStorage {
    pub async fn append_message(&self, session_id: &str, msg: &StoredMessage) -> Result<()> {
        let now = Self::now_iso();
        sqlx::query(
            "INSERT INTO messages (session_id, role, content, tool_calls, tool_call_id, reasoning_content, prompt_tokens, completion_tokens, cached_tokens, model, runtime_meta, think_ms, compacted, attachments, mentions, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        .bind(&msg.mentions)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Runs the given SQL query and maps the result rows into a
    /// `StoredMessage` list. `sql` must select the following columns in
    /// order and take exactly one bound parameter, `session_id`:
    /// `id, role, content, tool_calls, tool_call_id, reasoning_content,
    ///  prompt_tokens, completion_tokens, cached_tokens, model, runtime_meta, think_ms, compacted, attachments, mentions`
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
                    mentions,
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
                        mentions,
                    })
                },
            )
            .collect::<Result<Vec<_>>>()
    }

    pub async fn load_messages(&self, session_id: &str) -> Result<Vec<StoredMessage>> {
        self.query_messages(
            "SELECT id, role, content, tool_calls, tool_call_id, reasoning_content, prompt_tokens, completion_tokens, cached_tokens, model, runtime_meta, think_ms, compacted, attachments, mentions FROM messages WHERE session_id = ? ORDER BY id ASC",
            session_id,
        )
        .await
    }

    /// Load active context messages (compacted=0 only), ordered by id ASC.
    pub async fn load_active_messages(&self, session_id: &str) -> Result<Vec<StoredMessage>> {
        self.query_messages(
            "SELECT id, role, content, tool_calls, tool_call_id, reasoning_content, prompt_tokens, completion_tokens, cached_tokens, model, runtime_meta, think_ms, compacted, attachments, mentions FROM messages WHERE session_id = ? AND compacted = 0 ORDER BY id ASC",
            session_id,
        )
        .await
    }

    /// Count the uncompacted messages in a session.
    pub async fn count_active_messages(&self, session_id: &str) -> Result<i64> {
        let row: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM messages WHERE session_id = ? AND compacted = 0")
                .bind(session_id)
                .fetch_one(&self.pool)
                .await?;
        Ok(row.0)
    }

    /// Mark all uncompacted messages in a session as compacted.
    pub async fn mark_messages_compacted(&self, session_id: &str) -> Result<()> {
        sqlx::query("UPDATE messages SET compacted = 1 WHERE session_id = ? AND compacted = 0")
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Set the compaction summary.
    pub async fn set_compact_summary(&self, session_id: &str, summary: &str) -> Result<()> {
        sqlx::query("UPDATE sessions SET compact_summary = ? WHERE id = ?")
            .bind(summary)
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Get the compaction summary.
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

    /// Append a transient `<system-reminder>` block to the last user row's
    /// `runtime_meta.system_reminder` array. The read-modify-write runs in a
    /// transaction and the update targets the row id captured by the SELECT,
    /// so the target row is anchored even under concurrent writers. The value
    /// is an array — multiple blocks per message are supported and rebuilt in
    /// order, so request bytes stay identical across restarts (prompt-cache
    /// stable). `content` itself is never touched — UI/export read it
    /// verbatim.
    pub async fn append_user_message_reminder(
        &self,
        session_id: &str,
        reminder: &str,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let row: Option<(i64, Option<String>)> = sqlx::query_as(
            "SELECT id, runtime_meta FROM messages WHERE id = (SELECT MAX(id) FROM messages WHERE session_id = ? AND role = 'user')",
        )
        .bind(session_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((row_id, runtime_meta)) = row else {
            return Ok(());
        };
        let mut obj: serde_json::Map<String, serde_json::Value> = match runtime_meta.as_deref() {
            Some(s) => match serde_json::from_str(s) {
                Ok(obj) => obj,
                Err(e) => {
                    // Self-written field; corruption means manual editing or a
                    // bug — proceed with a fresh object but say so loudly
                    // instead of silently dropping the existing keys.
                    eprintln!(
                        "[warn] corrupt runtime_meta on last user row of session {session_id}: {e}; existing keys are reset"
                    );
                    Default::default()
                }
            },
            None => Default::default(),
        };
        match obj.get_mut("system_reminder") {
            Some(serde_json::Value::Array(blocks)) => {
                blocks.push(serde_json::Value::String(reminder.to_string()));
            }
            _ => {
                obj.insert(
                    "system_reminder".to_string(),
                    serde_json::Value::Array(vec![serde_json::Value::String(reminder.to_string())]),
                );
            }
        }
        let updated = serde_json::Value::Object(obj).to_string();
        sqlx::query("UPDATE messages SET runtime_meta = ? WHERE id = ?")
            .bind(&updated)
            .bind(row_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Repair orphaned tool calls: scan the tool_calls of the last active
    /// (compacted = 0) assistant message and append a "Tool execution
    /// aborted" message for each tool_call_id missing its tool result.
    pub async fn repair_orphaned_tool_calls(&self, session_id: &str) -> Result<()> {
        // Lightweight query: fetch only the role and tool call fields instead
        // of pulling full content and base64 attachment blobs into memory
        // (this repair only needs to scan trailing tool call pairing).
        // Active range only: dangling calls in the compacted range never
        // re-enter the rebuilt context, and stubbing them would create
        // orphan tool messages (no preceding assistant → same 400)
        let rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT role, tool_calls, tool_call_id FROM messages WHERE session_id = ? AND compacted = 0 ORDER BY id ASC",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;

        // Scan backwards for the last assistant message carrying tool_calls
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

        // Extract all tool_call ids
        let tool_call_ids: Vec<String> = tool_calls
            .iter()
            .filter_map(|tc| tc.get("id").and_then(|v| v.as_str()).map(String::from))
            .collect();

        if tool_call_ids.is_empty() {
            return Ok(());
        }

        // Collect tool_call_ids of tool results already present after it
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

        // Append a tool result for each missing tool_call_id
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
                    mentions: None,
                };
                self.append_message(session_id, &stored).await?;
            }
        }

        Ok(())
    }
}
