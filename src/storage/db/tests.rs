use async_openai::types::chat::{
    ChatCompletionRequestToolMessage, ChatCompletionRequestToolMessageContent,
    ChatCompletionRequestUserMessageContent, ChatCompletionRequestUserMessageContentPart,
};
use color_eyre::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::models::{MessageRole, StoredMessage};
use super::{
    ChatStorage, ensure_verbatim, from_stored_message, plan_mode_runtime_meta, stored_plan_mode,
    to_stored_message,
};
use crate::agent::models::CompatibleChatCompletionRequestMessage;

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
        attachments: None,
        mentions: None,
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
async fn attachment_roundtrip_via_storage() {
    use crate::agent::media::Attachment;

    let png_bytes: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let att = Attachment::from_bytes("image/png", &png_bytes);

    // Build a user message with an image attachment → persist → load → rebuild Array content
    let with_image = crate::agent::build_user_message("看图", std::slice::from_ref(&att));
    let stored = to_stored_message(&with_image);
    assert!(stored.attachments.is_some());
    assert_eq!(stored.content, "看图");

    let storage = ChatStorage::new_in_memory().await.unwrap();
    let sid = storage.create_session("m", "/tmp").await.unwrap();
    storage.append_message(&sid, &stored).await.unwrap();
    let loaded = storage.load_active_messages(&sid).await.unwrap();
    assert_eq!(loaded.len(), 1);
    assert!(loaded[0].attachments.is_some());

    let rebuilt = from_stored_message(&loaded[0]).unwrap();
    match &rebuilt {
        crate::agent::models::CompatibleChatCompletionRequestMessage::User(u) => match &u.content {
            ChatCompletionRequestUserMessageContent::Array(parts) => {
                assert_eq!(parts.len(), 2);
                match &parts[1] {
                    ChatCompletionRequestUserMessageContentPart::ImageUrl(img) => {
                        assert_eq!(img.image_url.url, att.data_url);
                    }
                    _ => panic!("expected image part"),
                }
            }
            _ => panic!("expected array content"),
        },
        _ => panic!("expected User message"),
    }

    // User message without attachments keeps plain-text content
    let plain = to_stored_message(&crate::agent::build_user_message("纯文本", &[]));
    assert!(plain.attachments.is_none());
}

#[test]
fn tool_attachments_not_persisted_in_tool_messages() {
    // Tool messages themselves carry no attachments column (media is
    // stripped into a synthetic user message)
    let tool_msg: crate::agent::models::CompatibleChatCompletionRequestMessage =
        ChatCompletionRequestToolMessage {
            content: ChatCompletionRequestToolMessageContent::Text(
                "Image read successfully".into(),
            ),
            tool_call_id: "call-1".into(),
        }
        .into();
    let stored = to_stored_message(&tool_msg);
    assert!(stored.attachments.is_none());
    assert_eq!(stored.role, MessageRole::Tool);
}

#[tokio::test]
async fn migration4_backfills_message_model() {
    // Simulate a migration-3-era DB: the last assistant row of each turn has
    // runtime_meta carrying {"model": ...}, middle rows hold display text
    // (not JSON); the last row simulates a crashed turn (no marker row).
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

    // Downgrade to the migration-3 state (remove the version 4 record, the
    // model column and its index), then re-run migration to backfill
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
    // Middle rows take the model of the following marker row in the same
    // turn (inclusive); user rows are not backfilled
    assert_eq!(model_of("a-mid1").as_deref(), Some("deepseek/chat"));
    assert_eq!(model_of("a-end1").as_deref(), Some("deepseek/chat"));
    assert_eq!(model_of("a-mid2").as_deref(), Some("glm-4.7"));
    assert_eq!(model_of("a-end2").as_deref(), Some("glm-4.7"));
    // The crashed-turn row has no marker row after it and stays NULL
    // (usage stats fall back to sessions.model)
    assert_eq!(model_of("a-crash"), None);
    assert_eq!(msgs.iter().find(|m| m.content == "u1").unwrap().model, None);
}

#[tokio::test]
async fn migration5_adds_attachments_column() {
    // Simulate a migration-4-era DB: data rows exist but no attachments
    // column; after re-running migration adds the column, old rows should
    // load fine (attachments = NULL → plain-text message, no attachments)
    let storage = ChatStorage::new_in_memory().await.unwrap();
    let sid = storage
        .create_session("deepseek/chat", "/tmp")
        .await
        .unwrap();
    sqlx::query("INSERT INTO messages (session_id, role, content, created_at) VALUES (?, ?, ?, ?)")
        .bind(&sid)
        .bind("user")
        .bind("旧消息")
        .bind(ChatStorage::now_iso())
        .execute(&storage.pool)
        .await
        .unwrap();

    // Downgrade to the migration-4 state (remove the version 5 record and
    // the attachments column), then re-run migration
    sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 5")
        .execute(&storage.pool)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE messages DROP COLUMN attachments")
        .execute(&storage.pool)
        .await
        .unwrap();
    storage.run_migration().await.unwrap();

    let msgs = storage.load_messages(&sid).await.unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].content, "旧消息");
    assert_eq!(msgs[0].attachments, None);

    // The rebuilt message is a plain-text user message (no image part)
    let chat = from_stored_message(&msgs[0]).unwrap();
    match chat {
        CompatibleChatCompletionRequestMessage::User(u) => match u.content {
            ChatCompletionRequestUserMessageContent::Text(t) => assert_eq!(t, "旧消息"),
            ChatCompletionRequestUserMessageContent::Array(_) => {
                panic!("旧消息不应重建出图片 part")
            }
        },
        _ => panic!("expected User message"),
    }
}

#[tokio::test]
async fn corrupted_attachments_json_falls_back_to_text() {
    // The attachments column holds malformed JSON: loading keeps the raw
    // value, rebuilding silently falls back to plain text
    let storage = ChatStorage::new_in_memory().await.unwrap();
    let sid = storage.create_session("m", "/tmp").await.unwrap();
    sqlx::query(
        "INSERT INTO messages (session_id, role, content, attachments, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&sid)
    .bind("user")
    .bind("看图")
    .bind("{not json")
    .bind(ChatStorage::now_iso())
    .execute(&storage.pool)
    .await
    .unwrap();

    let msgs = storage.load_messages(&sid).await.unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].attachments.as_deref(), Some("{not json"));

    let chat = from_stored_message(&msgs[0]).unwrap();
    match chat {
        CompatibleChatCompletionRequestMessage::User(u) => match u.content {
            ChatCompletionRequestUserMessageContent::Text(t) => assert_eq!(t, "看图"),
            ChatCompletionRequestUserMessageContent::Array(_) => {
                panic!("畸形附件 JSON 不应重建出图片 part")
            }
        },
        _ => panic!("expected User message"),
    }
}

#[tokio::test]
async fn legacy_pre_040_db_fails_migration() {
    // Simulate a legacy DB (hand-written migration era, < 0.4.0): no
    // _sqlx_migrations table and missing columns. Bootstrap upgrade was
    // removed: such DBs error during versioned migration (migration 4's
    // backfill references runtime_meta); the caller catches the error and
    // prompts the user to rebuild with /rebuild-db — no silent upgrade.
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

    // Back to normal after rebuild
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

fn make_assistant_with_tool_calls(ids: &[&str]) -> StoredMessage {
    let calls: Vec<serde_json::Value> = ids
        .iter()
        .map(|id| {
            serde_json::json!({
                "id": id,
                "type": "function",
                "function": {"name": "read", "arguments": "{}"},
            })
        })
        .collect();
    StoredMessage {
        tool_calls: Some(serde_json::to_string(&calls).unwrap()),
        ..make_message(MessageRole::Assistant, "")
    }
}

/// The repair only covers the active range (compacted = 0): dangling
/// assistants in the compacted range never re-enter the rebuilt context,
/// and stubbing them would only create orphan tool messages (same 400)
#[tokio::test]
async fn repair_skips_compacted_dangling_tool_calls() {
    let storage = ChatStorage::new_in_memory().await.unwrap();
    let sid = storage.create_session("m", "/tmp").await.unwrap();

    // Compacted range: dangling call_old (compacted=1)
    storage
        .append_message(&sid, &make_assistant_with_tool_calls(&["call_old"]))
        .await
        .unwrap();
    storage.mark_messages_compacted(&sid).await.unwrap();

    // Active range: dangling call_live
    storage
        .append_message(&sid, &make_assistant_with_tool_calls(&["call_live"]))
        .await
        .unwrap();

    storage.repair_orphaned_tool_calls(&sid).await.unwrap();

    let all = storage.load_messages(&sid).await.unwrap();
    let stubs: Vec<_> = all.iter().filter(|m| m.role == MessageRole::Tool).collect();
    assert_eq!(stubs.len(), 1);
    assert_eq!(stubs[0].tool_call_id.as_deref(), Some("call_live"));
    assert_eq!(stubs[0].content, "Tool execution aborted");

    // Active context intact: assistant(call_live) + stub, no orphan tool messages
    let active = storage.load_active_messages(&sid).await.unwrap();
    assert_eq!(active.len(), 2);
    assert!(active.iter().all(|m| !m.compacted));
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

    // The same session (a, created with model deepseek/chat) switches to
    // glm-4.7 mid-way: message-level model wins; NULL falls back to
    // sessions.model (old data)
    let mut assistant3 = make_message(MessageRole::Assistant, "a3");
    assistant3.prompt_tokens = Some(400);
    assistant3.completion_tokens = Some(20);
    assistant3.cached_tokens = Some(0);
    assistant3.model = Some("glm-4.7".to_string());
    storage.append_message(&a, &assistant3).await.unwrap();

    // Messages without usage are not counted
    storage
        .append_message(&a, &make_message(MessageRole::User, "u"))
        .await
        .unwrap();

    // Load round-trip preserves the message-level model
    let loaded = storage.load_messages(&a).await.unwrap();
    assert_eq!(loaded[0].model, None);
    assert_eq!(loaded[1].model.as_deref(), Some("glm-4.7"));

    // Global summary
    let summary = storage.usage_summary(None, 0).await.unwrap();
    assert_eq!(summary.requests, 3);
    assert_eq!(summary.prompt_tokens, 700);
    assert_eq!(summary.cached_tokens, 80);
    assert_eq!(summary.completion_tokens, 80);

    // Filtered by project
    let proj = storage.usage_summary(Some("/tmp/proj"), 0).await.unwrap();
    assert_eq!(proj.requests, 2);
    assert_eq!(proj.prompt_tokens, 500);

    // Aggregated by day
    let daily = storage.usage_daily(None, 0).await.unwrap();
    assert_eq!(daily.len(), 1);
    assert_eq!(daily[0].requests, 3);

    // By model: message-level model wins, NULL falls back to sessions.model
    // glm-4.7 = a3(400) + a2 of b (200); deepseek/chat = a1(100)
    let by_model = storage.usage_by_model(None, 0).await.unwrap();
    assert_eq!(by_model.len(), 2);
    assert_eq!(by_model[0].model, "glm-4.7");
    assert_eq!(by_model[0].requests, 2);
    assert_eq!(by_model[0].prompt_tokens, 600);
    assert_eq!(by_model[1].model, "deepseek/chat");
    assert_eq!(by_model[1].requests, 1);
    assert_eq!(by_model[1].prompt_tokens, 100);

    // By project
    let by_project = storage.usage_by_project(None, 0, 10).await.unwrap();
    assert_eq!(by_project.len(), 2);
    // 570 > 210, so /tmp/proj comes first
    assert_eq!(by_project[0].work_dir, "/tmp/proj");
    assert_eq!(by_project[0].prompt_tokens, 500);
    assert_eq!(by_project[1].work_dir, "/tmp/other");

    // Recent details
    let recent = storage.list_recent_usage(None, 0, 10).await.unwrap();
    assert_eq!(recent.len(), 3);
    assert_eq!(recent[0].model, "glm-4.7"); // id DESC; a3's message-level model
    assert_eq!(recent[0].work_dir, "/tmp/proj");
    assert_eq!(recent[1].model, "glm-4.7"); // a2 falls back to sessions.model
    assert_eq!(recent[2].model, "deepseek/chat"); // a1 falls back to sessions.model

    // Known project list
    let dirs = storage.list_work_dirs().await.unwrap();
    assert_eq!(dirs.len(), 2);
    let names: Vec<&str> = dirs.iter().map(|d| d.work_dir.as_str()).collect();
    assert!(names.contains(&"/tmp/proj"));
    assert!(names.contains(&"/tmp/other"));
    assert!(dirs.iter().all(|d| d.sessions == 1));

    // verbatim prefix variant matching
    let proj_verbatim = storage
        .usage_summary(Some(r"\\?\C:\tmp\proj"), 0)
        .await
        .unwrap();
    assert_eq!(proj_verbatim.requests, 0); // different variants, no match for /tmp/proj
}

#[tokio::test]
async fn usage_stats_respects_days_filter() {
    let storage = ChatStorage::new_in_memory().await.unwrap();
    let sid = storage.create_session("m", "/tmp").await.unwrap();
    let mut assistant = make_message(MessageRole::Assistant, "a");
    assistant.prompt_tokens = Some(10);
    assistant.completion_tokens = Some(5);
    storage.append_message(&sid, &assistant).await.unwrap();

    // days=0: everything visible
    let all = storage.usage_summary(None, 0).await.unwrap();
    assert_eq!(all.requests, 1);

    // created_at is already "now" here, inside the 30-day window
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

    // Test environment (in-memory): no file backup
    let backup_path = storage.rebuild().await.unwrap();
    assert!(backup_path.is_none());

    // All old data wiped
    let sessions = storage.list_top_level_sessions("/tmp").await.unwrap();
    assert!(sessions.is_empty());
    // Migration error cleared, schema ready
    assert!(storage.migration_error().is_none());
    let has_migrations: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_one(&storage.pool)
    .await
    .unwrap();
    assert_eq!(has_migrations.0, 1);
    // Writes work normally after the rebuild
    let new_sid = storage.create_session("m", "/tmp").await.unwrap();
    storage
        .append_message(&new_sid, &make_message(MessageRole::User, "after"))
        .await
        .unwrap();
    let msgs = storage.load_messages(&new_sid).await.unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].content, "after");
}

#[tokio::test]
async fn user_reminder_persists_multi_block_and_merges_runtime_meta() {
    let storage = ChatStorage::new_in_memory().await.unwrap();
    let sid = storage.create_session("m", "/tmp").await.unwrap();

    // User row already carrying a runtime_meta key must keep it after merge
    let mut row = make_message(MessageRole::User, "hello");
    row.runtime_meta = Some(r#"{"plan_mode":true}"#.to_string());
    storage.append_message(&sid, &row).await.unwrap();

    let r1 = "<system-reminder>failure A</system-reminder>";
    let r2 = "<system-reminder>failure B</system-reminder>";
    storage
        .append_user_message_reminder(&sid, r1)
        .await
        .unwrap();
    storage
        .append_user_message_reminder(&sid, r2)
        .await
        .unwrap();

    let loaded = storage.load_active_messages(&sid).await.unwrap();
    let meta: serde_json::Value =
        serde_json::from_str(loaded[0].runtime_meta.as_deref().unwrap()).unwrap();
    assert_eq!(meta["plan_mode"], true, "existing keys survive the merge");
    let blocks = meta["system_reminder"].as_array().unwrap();
    assert_eq!(blocks.len(), 2, "multiple reminder blocks accumulate");
    assert_eq!(blocks[0], *r1);
    assert_eq!(blocks[1], *r2);

    // Rebuild splices blocks in order — byte-identical to the in-memory
    // append_text_to_last_user_message sequence
    let rebuilt = from_stored_message(&loaded[0]).unwrap();
    let crate::agent::models::CompatibleChatCompletionRequestMessage::User(u) = &rebuilt else {
        panic!("expected User message");
    };
    let ChatCompletionRequestUserMessageContent::Text(t) = &u.content else {
        panic!("expected Text content");
    };
    assert_eq!(t, &format!("hello\n\n{r1}\n\n{r2}"));
    assert_eq!(loaded[0].content, "hello", "stored content stays pristine");
}

#[tokio::test]
async fn user_reminder_rebuild_appends_array_part_for_attachments() {
    use crate::agent::media::Attachment;

    let storage = ChatStorage::new_in_memory().await.unwrap();
    let sid = storage.create_session("m", "/tmp").await.unwrap();

    let png_bytes: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let att = Attachment::from_bytes("image/png", &png_bytes);
    let with_image = crate::agent::build_user_message("看图", std::slice::from_ref(&att));
    let stored = to_stored_message(&with_image);
    storage.append_message(&sid, &stored).await.unwrap();

    let r1 = "<system-reminder>failure A</system-reminder>";
    storage
        .append_user_message_reminder(&sid, r1)
        .await
        .unwrap();

    let loaded = storage.load_active_messages(&sid).await.unwrap();
    let rebuilt = from_stored_message(&loaded[0]).unwrap();
    let crate::agent::models::CompatibleChatCompletionRequestMessage::User(u) = &rebuilt else {
        panic!("expected User message");
    };
    let ChatCompletionRequestUserMessageContent::Array(parts) = &u.content else {
        panic!("expected Array content");
    };
    // text part + image part + reminder part — mirrors the in-memory injection
    assert_eq!(parts.len(), 3);
    match &parts[2] {
        ChatCompletionRequestUserMessageContentPart::Text(t) => {
            assert_eq!(t.text, format!("\n\n{r1}"));
        }
        _ => panic!("expected trailing reminder text part"),
    }
}

#[tokio::test]
async fn user_reminder_noop_without_user_row() {
    let storage = ChatStorage::new_in_memory().await.unwrap();
    let sid = storage.create_session("m", "/tmp").await.unwrap();
    // Only an assistant row exists — the append must not touch it
    storage
        .append_message(&sid, &make_message(MessageRole::Assistant, "hi"))
        .await
        .unwrap();
    storage
        .append_user_message_reminder(&sid, "<system-reminder>x</system-reminder>")
        .await
        .unwrap();
    let loaded = storage.load_messages(&sid).await.unwrap();
    assert!(loaded[0].runtime_meta.is_none());
}

#[test]
fn stored_plan_mode_reads_meta_and_plan_mode_runtime_meta_roundtrips() {
    let mut m = make_message(MessageRole::User, "hi");

    // No meta → false
    assert!(!stored_plan_mode(&m));

    // plan_mode_runtime_meta round-trips through stored_plan_mode
    m.runtime_meta = plan_mode_runtime_meta(true);
    assert_eq!(m.runtime_meta.as_deref(), Some(r#"{"plan_mode":true}"#));
    assert!(stored_plan_mode(&m));
    assert_eq!(plan_mode_runtime_meta(false), None);

    // Meta carrying only other keys (e.g. failure reminders) → false
    m.runtime_meta = Some(r#"{"system_reminder":["<system-reminder>x</system-reminder>"]}"#.into());
    assert!(!stored_plan_mode(&m));

    // Corrupt meta parses as false instead of panicking
    m.runtime_meta = Some("not json".to_string());
    assert!(!stored_plan_mode(&m));
}
