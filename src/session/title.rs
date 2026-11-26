//! LLM-generated session titles, shared by the TUI and the Web backend.
//!
//! Flow: the first user message writes a truncation placeholder
//! synchronously — the "empty title = never messaged" invariant must break
//! immediately (the web blank-session reuse depends on it) — then a
//! background task asks the model for a proper title and swaps it in for the
//! placeholder (compare-and-set: a user rename issued in the meantime wins).
//! Best-effort end to end: any failure keeps the placeholder.

use async_openai::{
    Client,
    types::chat::{
        ChatCompletionRequestSystemMessage, ChatCompletionRequestSystemMessageContent,
        ChatCompletionRequestUserMessage, ChatCompletionRequestUserMessageContent,
        CreateChatCompletionResponse,
    },
};
use color_eyre::Result;
use std::sync::Arc;

use crate::agent::models::{
    CompatibleCreateChatCompletionRequestArgs, SharedMessage, ThinkingConfig,
};
use crate::config::ResolvedModel;
use crate::prompts::TITLE;
use crate::storage::ChatStorage;

/// Hard cap of a stored title (chars; aligns with the truncation placeholder)
pub const TITLE_MAX_CHARS: usize = 50;
/// First-message input cap fed to the title prompt (chars)
const TITLE_INPUT_MAX_CHARS: usize = 2000;
/// Output token cap for the title request
const TITLE_MAX_TOKENS: u32 = 64;

/// Truncation placeholder written synchronously when a session gets its
/// first message
pub fn placeholder_title(first_message: &str) -> String {
    truncate_chars(first_message, TITLE_MAX_CHARS)
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        format!("{}...", s.chars().take(max).collect::<String>())
    } else {
        s.to_string()
    }
}

/// Hard cap without the placeholder ellipsis (generated text, not a
/// truncation marker)
fn cap_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// Clean raw model output: keep the first non-empty line (models sometimes
/// append an explanation despite the prompt), trim, strip wrapping quote /
/// markdown emphasis pairs and trailing sentence punctuation (alternating
/// until stable — quotes may wrap a punctuation-suffixed title), hard-cap
/// length. `None` when nothing usable remains.
pub fn sanitize_title(raw: &str) -> Option<String> {
    let mut owned = raw
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string();
    loop {
        let mut changed = false;
        let chars: Vec<char> = owned.chars().collect();
        if chars.len() >= 2 {
            let (first, last) = (chars[0], chars[chars.len() - 1]);
            let paired = matches!(
                (first, last),
                ('"', '"')
                    | ('“', '”')
                    | ('‘', '’')
                    | ('\'', '\'')
                    | ('《', '》')
                    | ('「', '」')
                    | ('『', '』')
                    | ('*', '*')
                    | ('`', '`')
            );
            if paired {
                let inner: String = chars[1..chars.len() - 1].iter().collect();
                owned = inner.trim().to_string();
                changed = true;
            }
        }
        while let Some(last) = owned.chars().last() {
            if matches!(last, '.' | '。' | '！' | '!' | '？' | '?') {
                owned.pop();
                owned = owned.trim_end().to_string();
                changed = true;
            } else {
                break;
            }
        }
        if !changed {
            break;
        }
    }
    let out = cap_chars(&owned, TITLE_MAX_CHARS);
    if out.is_empty() { None } else { Some(out) }
}

/// One-shot non-streaming title request against the resolved model.
/// No retry: the placeholder stays on failure and the next surface that
/// needs the title is unaffected.
async fn generate_title(resolved: &ResolvedModel, first_message: &str) -> Result<String> {
    let input = truncate_chars(first_message, TITLE_INPUT_MAX_CHARS);
    let system = ChatCompletionRequestSystemMessage {
        content: ChatCompletionRequestSystemMessageContent::Text(TITLE.to_string()),
        name: None,
    };
    let user = ChatCompletionRequestUserMessage {
        content: ChatCompletionRequestUserMessageContent::Text(input),
        name: None,
    };
    let messages: Vec<SharedMessage> = vec![Arc::new(system.into()), Arc::new(user.into())];

    let mut builder = CompatibleCreateChatCompletionRequestArgs::default();
    builder
        .max_completion_tokens(TITLE_MAX_TOKENS)
        .model(&resolved.model_id)
        .stream(false)
        .messages(messages);
    // Native-reasoning providers get the private thinking switch off:
    // titles don't need reasoning and it would dominate latency and tokens
    if resolved.native_reasoning {
        builder.thinking(ThinkingConfig::disabled());
    }
    let request = builder.build()?;

    let client = Client::with_config(resolved.config.clone());
    let resp = client
        .chat()
        .create_byot::<_, CreateChatCompletionResponse>(request)
        .await?;
    let text = resp
        .choices
        .first()
        .and_then(|c| c.message.content.clone())
        .unwrap_or_default();
    Ok(text)
}

/// Shared entry point for both surfaces, called after the first user message
/// is persisted: writes the placeholder when the session has no title yet,
/// then spawns the background generation task. Returns whether the placeholder
/// was written (callers use this to advance the web durable watermark).
/// The generated title only replaces the placeholder (compare-and-set), so a
/// user rename issued in the meantime wins.
///
/// `on_done` fires with the generated title after it is persisted (skipped on
/// failure — the placeholder stays).
pub async fn ensure_session_title(
    storage: &ChatStorage,
    session_id: &str,
    first_message: &str,
    resolved: &ResolvedModel,
    on_done: impl FnOnce(String) + Send + 'static,
) -> Result<bool> {
    if storage
        .get_session_title(session_id)
        .await?
        .is_some_and(|t| !t.is_empty())
    {
        return Ok(false);
    }
    let placeholder = placeholder_title(first_message);
    if placeholder.is_empty() {
        return Ok(false);
    }
    storage
        .update_session_title(session_id, &placeholder)
        .await?;

    let storage = storage.clone();
    let session_id = session_id.to_string();
    let first_message = first_message.to_string();
    let resolved = resolved.clone();
    tokio::spawn(async move {
        let outcome = generate_title(&resolved, &first_message)
            .await
            .map_err(|e| e.to_string())
            .and_then(|raw| sanitize_title(&raw).ok_or_else(|| "empty title".to_string()));
        match outcome {
            Ok(title) => match storage
                .update_session_title_if(&session_id, &placeholder, &title)
                .await
            {
                Ok(true) => on_done(title),
                // The title moved on (user rename): keep theirs, no event
                Ok(false) => {}
                Err(e) => eprintln!("[warn] Failed to persist generated session title: {e}"),
            },
            Err(e) => eprintln!("[warn] Failed to generate session title: {e}"),
        }
    });
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use async_openai::config::OpenAIConfig;

    use super::*;

    #[test]
    fn placeholder_short_message_unchanged() {
        assert_eq!(placeholder_title("hello"), "hello");
    }

    #[test]
    fn placeholder_long_message_truncated_with_ellipsis() {
        let msg = "a".repeat(80);
        let title = placeholder_title(&msg);
        assert_eq!(title.chars().count(), 53); // 50 + "..."
        assert!(title.ends_with("..."));
    }

    #[test]
    fn placeholder_counts_chars_not_bytes() {
        let msg = "中".repeat(60);
        let title = placeholder_title(&msg);
        assert_eq!(title.chars().count(), 53);
    }

    #[test]
    fn sanitize_strips_wrapping_quotes_and_trailing_period() {
        assert_eq!(
            sanitize_title("\"修复登录 bug\"。"),
            Some("修复登录 bug".to_string())
        );
        assert_eq!(
            sanitize_title("  Refactor the session layer.  "),
            Some("Refactor the session layer".to_string())
        );
    }

    #[test]
    fn sanitize_caps_length() {
        let raw = "很".repeat(80);
        let title = sanitize_title(&raw).unwrap();
        assert_eq!(title.chars().count(), TITLE_MAX_CHARS);
    }

    #[test]
    fn sanitize_empty_returns_none() {
        assert_eq!(sanitize_title("   "), None);
        assert_eq!(sanitize_title("\"\""), None);
        assert_eq!(sanitize_title("。"), None);
        assert_eq!(sanitize_title("\n \n"), None);
    }

    #[test]
    fn sanitize_keeps_first_non_empty_line_only() {
        assert_eq!(
            sanitize_title("修复登录 bug\n\n（说明：略）"),
            Some("修复登录 bug".to_string())
        );
        assert_eq!(
            sanitize_title("\n\n  标题  \r\n第二行"),
            Some("标题".to_string())
        );
    }

    #[test]
    fn sanitize_strips_markdown_emphasis() {
        assert_eq!(sanitize_title("**Refactor**"), Some("Refactor".to_string()));
        assert_eq!(sanitize_title("`git`"), Some("git".to_string()));
    }

    /// Points at a dead endpoint so the background request fails fast and
    /// never touches the network (same pattern as the agent tests)
    fn dead_model() -> ResolvedModel {
        ResolvedModel {
            config: OpenAIConfig::new().with_api_base("http://localhost:0"),
            model_id: "test".to_string(),
            max_tokens: 1,
            context_window: 1,
            display: "test".to_string(),
            supports_vision: false,
            native_reasoning: false,
        }
    }

    #[tokio::test]
    async fn ensure_skips_session_with_existing_title() {
        let storage = ChatStorage::new_in_memory().await.unwrap();
        let sid = storage.create_session("m", "/tmp").await.unwrap();
        storage.update_session_title(&sid, "custom").await.unwrap();

        let wrote = ensure_session_title(&storage, &sid, "hello", &dead_model(), |_| {
            panic!("on_done must not fire")
        })
        .await
        .unwrap();

        assert!(!wrote);
        assert_eq!(
            storage.get_session_title(&sid).await.unwrap().as_deref(),
            Some("custom")
        );
    }

    #[tokio::test]
    async fn ensure_writes_placeholder_when_title_empty() {
        let storage = ChatStorage::new_in_memory().await.unwrap();
        let sid = storage.create_session("m", "/tmp").await.unwrap();
        let fired = Arc::new(AtomicBool::new(false));
        let flag = fired.clone();

        let wrote = ensure_session_title(&storage, &sid, "hello world", &dead_model(), move |_| {
            flag.store(true, Ordering::SeqCst)
        })
        .await
        .unwrap();

        assert!(wrote);
        assert_eq!(
            storage.get_session_title(&sid).await.unwrap().as_deref(),
            Some("hello world")
        );
        // Give the background task a chance to fail against the dead endpoint;
        // the placeholder must survive and on_done must never fire
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!fired.load(Ordering::SeqCst));
        assert_eq!(
            storage.get_session_title(&sid).await.unwrap().as_deref(),
            Some("hello world")
        );
    }
}
