//! LLM 请求的瞬态错误重试策略。
//!
//! 网络抖动、限流（429）、服务端瞬时故障（5xx）等错误重发请求即可恢复；
//! 认证/参数类 4xx 重试无意义。重试只作用于单次 LLM 请求（连接 + 流消费），
//! 失败尝试的部分输出不进入消息上下文，也不落库。

use async_openai::error::OpenAIError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// 取消标志轮询间隔（对齐 agent.rs 的 CANCEL_POLL_INTERVAL 语义：
/// 该间隔即为取消响应的最大延迟上限）
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// 退避抖动幅度 ±20%（避免多实例同步重试打满 provider）
const JITTER_RATIO: f64 = 0.2;

/// 重试策略配置（config.toml `[retry]` 段可覆盖默认值）
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// 额外重试次数上限（总尝试次数 = 1 + max_retries）
    pub max_retries: u32,
    /// 首次重试前的退避等待
    pub initial_delay: Duration,
    /// 退避等待上限
    pub max_delay: Duration,
    /// 指数退避倍率
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

/// 判断错误是否为瞬态错误（值得重试）。
///
/// - `Reqwest`：网络层错误（连接失败 / 超时 / DNS / TLS）
/// - `StreamError`：SSE 流中途断开
/// - `ApiError`：仅限流（408/429）与服务端故障（5xx）；
///   其余 4xx（认证、参数、模型不存在等）重试必然同样失败
/// - 其余（反序列化失败、参数非法等客户端问题）不重试
pub fn is_retryable(err: &OpenAIError) -> bool {
    match err {
        OpenAIError::Reqwest(_) => true,
        OpenAIError::StreamError(_) => true,
        OpenAIError::ApiError(resp) => matches!(resp.status_code.as_u16(), 408 | 429 | 500..=599),
        _ => false,
    }
}

/// 第 `retry_no` 次重试前的退避等待（1 起始）：initial × multiplier^(n-1)，
/// 以 max_delay 封顶，再叠加 ±[`JITTER_RATIO`] 抖动（不引入 rand 依赖，
/// 以 SystemTime 亚秒纳秒为随机源）。
pub fn backoff_delay(cfg: &RetryConfig, retry_no: u32) -> Duration {
    let exp = cfg.multiplier.powi((retry_no.saturating_sub(1)) as i32);
    let base_ms =
        (cfg.initial_delay.as_millis() as f64 * exp).min(cfg.max_delay.as_millis() as f64);
    Duration::from_millis(jitter_ms(base_ms))
}

/// 基于 SystemTime 纳秒的对称抖动，返回 `[base×(1-ratio), base×(1+ratio)]`，至少 1ms。
fn jitter_ms(base_ms: f64) -> u64 {
    let unit = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| f64::from(d.subsec_nanos() % 1000) / 1000.0)
        .unwrap_or(0.5);
    let factor = 1.0 + (unit * 2.0 - 1.0) * JITTER_RATIO;
    (base_ms * factor).round().max(1.0) as u64
}

/// 可取消的退避等待：以 [`CANCEL_POLL_INTERVAL`] 轮询 `cancel`。
/// 返回 `false` = 等待期间用户取消（调用方走中断收尾，不再重试）。
pub async fn sleep_cancellable(delay: Duration, cancel: &AtomicBool) -> bool {
    let deadline = tokio::time::Instant::now() + delay;
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
        // 反序列化失败：客户端侧问题，不重试
        let err = OpenAIError::JSONDeserialize(
            serde_json::from_str::<serde_json::Value>("{").unwrap_err(),
            "body".to_string(),
        );
        assert!(!is_retryable(&err));
        assert!(!is_retryable(&OpenAIError::InvalidArgument("bad".into())));
    }

    #[test]
    fn backoff_grows_exponentially_and_caps() {
        let cfg = RetryConfig {
            max_retries: 10,
            initial_delay: Duration::from_millis(1000),
            max_delay: Duration::from_millis(5000),
            multiplier: 2.0,
        };
        // ±20% 抖动范围内校验
        let d1 = backoff_delay(&cfg, 1).as_millis() as f64;
        assert!((800.0..=1200.0).contains(&d1), "d1={d1}");
        let d3 = backoff_delay(&cfg, 3).as_millis() as f64;
        assert!((3200.0..=4800.0).contains(&d3), "d3={d3}");
        // 1000×2^7 远超 max_delay=5000 → 封顶后 4000..=6000
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
