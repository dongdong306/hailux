//! Transient-error retry policy for LLM requests.
//!
//! Errors such as network jitter, rate limiting (429), and transient server failures (5xx)
//! recover by simply resending the request; retrying auth/parameter 4xx errors is pointless.
//! Retries apply only to a single LLM request (connection + stream consumption);
//! partial output from a failed attempt never enters the message context nor gets persisted.

use async_openai::error::OpenAIError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Cancel-flag polling interval (aligns with the CANCEL_POLL_INTERVAL semantics in agent.rs:
/// this interval is the upper bound on cancellation response latency)
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Backoff jitter magnitude ±20% (avoids multiple instances retrying in sync and
/// saturating the provider)
const JITTER_RATIO: f64 = 0.2;

/// Retry policy configuration (the config.toml `[retry]` section can override defaults)
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Cap on extra retries (total attempts = 1 + max_retries)
    pub max_retries: u32,
    /// Backoff wait before the first retry
    pub initial_delay: Duration,
    /// Upper bound on the backoff wait
    pub max_delay: Duration,
    /// Exponential backoff multiplier
    pub multiplier: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay: Duration::from_millis(1000),
            max_delay: Duration::from_millis(20_000),
            multiplier: 2.0,
        }
    }
}

/// Determines whether an error is transient (worth retrying).
///
/// - `Reqwest`: network-layer errors (connection failure / timeout / TLS, etc.),
///   excluding builder (a locally invalid request construction would fail again
///   on retry) and redirect (redirect loops); the 429 `Retry-After` response
///   header cannot be obtained because the upstream `ApiErrorResponse` does not
///   keep headers, so the backoff policy cannot take it into account
/// - `StreamError`: the SSE stream breaks mid-flight (including connection
///   resets; chunk JSON deserialization failures surface as the
///   `JSONDeserialize` variant and fail fast instead)
/// - `ApiError`: only rate limiting (408/429) and server failures (5xx);
///   other 4xx errors (auth, parameters, nonexistent model, etc.) would fail
///   again on retry
/// - The rest (deserialization failures, invalid parameters, and other
///   client-side problems) are not retried
pub fn is_retryable(err: &OpenAIError) -> bool {
    match err {
        OpenAIError::Reqwest(e) => !e.is_builder() && !e.is_redirect(),
        OpenAIError::StreamError(_) => true,
        OpenAIError::ApiError(resp) => matches!(resp.status_code.as_u16(), 408 | 429 | 500..=599),
        _ => false,
    }
}

/// Backoff wait before the `retry_no`-th retry (1-based): initial × multiplier^(n-1),
/// capped at max_delay, then ±[`JITTER_RATIO`] jitter is applied (no rand dependency;
/// SystemTime sub-second nanoseconds serve as the random source).
pub fn backoff_delay(cfg: &RetryConfig, retry_no: u32) -> Duration {
    let exp = cfg.multiplier.powi((retry_no.saturating_sub(1)) as i32);
    let base_ms =
        (cfg.initial_delay.as_millis() as f64 * exp).min(cfg.max_delay.as_millis() as f64);
    Duration::from_millis(jitter_ms(base_ms))
}

/// Symmetric jitter based on SystemTime nanoseconds, returning
/// `[base×(1-ratio), base×(1+ratio)]`, at least 1ms.
fn jitter_ms(base_ms: f64) -> u64 {
    let unit = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| f64::from(d.subsec_nanos() % 1000) / 1000.0)
        .unwrap_or(0.5);
    let factor = 1.0 + (unit * 2.0 - 1.0) * JITTER_RATIO;
    (base_ms * factor).round().max(1.0) as u64
}

/// Hard upper bound on a single backoff sleep: prevents `Instant + Duration` overflow
/// panics from an extreme `max_delay_ms` config (24h far exceeds any sane backoff scenario)
const MAX_SLEEP: Duration = Duration::from_secs(24 * 60 * 60);

/// Cancellable backoff sleep: polls `cancel` at [`CANCEL_POLL_INTERVAL`].
/// Returns `false` = the user cancelled while sleeping (the caller finalizes the abort
/// and does not retry).
pub async fn sleep_cancellable(delay: Duration, cancel: &AtomicBool) -> bool {
    let deadline = tokio::time::Instant::now() + delay.min(MAX_SLEEP);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return true;
        }
        tokio::time::sleep(deadline.min(now + CANCEL_POLL_INTERVAL) - now).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::error::{ApiError, ApiErrorResponse};
    use reqwest::StatusCode;

    fn api_err(code: u16) -> OpenAIError {
        OpenAIError::ApiError(ApiErrorResponse {
            status_code: StatusCode::from_u16(code).unwrap(),
            api_error: ApiError {
                message: "err".to_string(),
                r#type: None,
                param: None,
                code: None,
            },
        })
    }

    #[test]
    fn api_status_classification() {
        for code in [408, 429, 500, 502, 503, 504] {
            assert!(is_retryable(&api_err(code)), "{code} 应可重试");
        }
        for code in [400, 401, 403, 404, 422] {
            assert!(!is_retryable(&api_err(code)), "{code} 不应重试");
        }
    }

    #[test]
    fn non_api_errors_classification() {
        // Deserialization failure: client-side problem, do not retry
        let err = OpenAIError::JSONDeserialize(
            serde_json::from_str::<serde_json::Value>("{").unwrap_err(),
            "body".to_string(),
        );
        assert!(!is_retryable(&err));
        assert!(!is_retryable(&OpenAIError::InvalidArgument("bad".into())));
    }

    #[test]
    fn reqwest_builder_error_not_retryable() {
        // URL parse failure → reqwest builder error: locally invalid request construction,
        // retrying would fail the same way
        let err = reqwest::Client::new().get("://").build().unwrap_err();
        assert!(err.is_builder(), "应构造出 builder 错误");
        assert!(!is_retryable(&OpenAIError::Reqwest(err)));
    }

    #[test]
    fn backoff_grows_exponentially_and_caps() {
        let cfg = RetryConfig {
            max_retries: 10,
            initial_delay: Duration::from_millis(1000),
            max_delay: Duration::from_millis(5000),
            multiplier: 2.0,
        };
        // Verify within the ±20% jitter range
        let d1 = backoff_delay(&cfg, 1).as_millis() as f64;
        assert!((800.0..=1200.0).contains(&d1), "d1={d1}");
        let d3 = backoff_delay(&cfg, 3).as_millis() as f64;
        assert!((3200.0..=4800.0).contains(&d3), "d3={d3}");
        // 1000×2^7 far exceeds max_delay=5000 → after capping: 4000..=6000
        let d8 = backoff_delay(&cfg, 8).as_millis() as f64;
        assert!((4000.0..=6000.0).contains(&d8), "d8={d8}");
    }

    #[tokio::test]
    async fn sleep_returns_immediately_when_cancelled() {
        let cancel = AtomicBool::new(true);
        assert!(!sleep_cancellable(Duration::from_secs(60), &cancel).await);
    }

    #[tokio::test]
    async fn sleep_elapses_normally() {
        let cancel = AtomicBool::new(false);
        assert!(sleep_cancellable(Duration::from_millis(60), &cancel).await);
    }
}
