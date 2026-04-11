//! Retry helpers for Anthropic API calls.
//!
//! Provides two primitives:
//!
//! - [`is_recoverable`] — classify an [`anyhow::Error`] as worth retrying.
//!   Walks the error chain for `misanthropic::client::Error` /
//!   `AnthropicError` and returns true for transient failures
//!   (network blips, 5xx, 429, non-JSON error bodies) and false for
//!   programmer/auth errors (4xx, parse errors).
//!
//! - [`retry_recoverable`] — a small exponential-backoff retry loop
//!   that consults [`is_recoverable`]. Use it to wrap individual
//!   submit/poll calls to the Anthropic Batch API (or any misanthropic
//!   call) where transient edge failures should not kill an otherwise-
//!   successful cycle.
//!
//! Behavior is deliberately conservative: a single non-recoverable
//! error short-circuits immediately without waiting. The retry budget
//! is caller-controlled via `max_retries`.
//!
//! # Feature flag
//!
//! This module is behind the `retry` feature to keep the misanthropic
//! and tokio dependencies optional.

use anyhow::Result;

/// Is this backend error worth retrying?
///
/// Retry rules:
/// - Network errors (reqwest): **true** — tcp flaps, brief DNS hiccups.
/// - Anthropic 5xx, 429, overloaded, timeout: **true**.
/// - Anthropic 4xx (other than 429): **false** — caller's fault, won't
///   fix itself.
/// - Parse / unexpected-response errors: **false**.
/// - Non-JSON error bodies (Cloudflare HTML pages, gateway timeouts):
///   **true** — almost always transient edge failures. If the underlying
///   issue is permanent, successive retries will keep returning the
///   same `NonJsonResponse` and the caller's retry budget bounds total
///   wait.
/// - Anything we can't classify (Ollama, plain anyhow strings, other
///   backends): **true** — one extra call is cheap compared to losing
///   the cycle.
///
/// Walks the [`anyhow::Error::chain`] looking for
/// `misanthropic::client::Error` and `misanthropic::client::AnthropicError`.
pub fn is_recoverable(err: &anyhow::Error) -> bool {
    use misanthropic::client::{AnthropicError, Error as ClientError};

    for cause in err.chain() {
        if let Some(client_err) = cause.downcast_ref::<ClientError>() {
            return match client_err {
                ClientError::HTTP(_) => true, // network blip
                ClientError::Parse(_) => false,
                ClientError::UnexpectedResponse { .. } => false,
                ClientError::Anthropic(a) => anthropic_err_recoverable(a),
                // Non-JSON error bodies are almost always transient
                // edge failures (Cloudflare 502/504 HTML pages,
                // rate-limit challenge pages, gateway timeouts). Retry.
                ClientError::NonJsonResponse { .. } => true,
            };
        }
        if let Some(a) = cause.downcast_ref::<AnthropicError>() {
            return anthropic_err_recoverable(a);
        }
    }
    // Unknown error type — default to retrying once. Cheap.
    true
}

fn anthropic_err_recoverable(err: &misanthropic::client::AnthropicError) -> bool {
    use misanthropic::client::AnthropicError::*;
    match err {
        // Recoverable: transient server or rate limit.
        RateLimit { .. } | API { .. } | Overloaded { .. } | Timeout { .. } => true,
        // Unknown code: retry on 5xx, skip on 4xx.
        Unknown { code, .. } => code.get() >= 500,
        // Everything else (400/401/403/404/413/billing) is caller-side.
        InvalidRequest { .. }
        | Authentication { .. }
        | Billing { .. }
        | Permission { .. }
        | NotFound { .. }
        | RequestTooLarge { .. } => false,
    }
}

/// Retry `f` with exponential backoff when it returns a recoverable
/// error (per [`is_recoverable`]).
///
/// Backoff schedule for `max_retries = 5`: 1s → 2s → 4s → 8s → 16s.
/// Total worst-case wait before giving up is ~31s on top of the
/// actual call latency. Backoff is capped at 30s per step.
///
/// If `f` returns a non-recoverable error, returns immediately
/// without waiting.
///
/// `label` is prefixed on every warning / error log so operators can
/// tell which call site is retrying.
pub async fn retry_recoverable<F, Fut, T>(
    label: &str,
    max_retries: usize,
    mut f: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut delay = std::time::Duration::from_secs(1);
    let mut attempt = 0usize;
    loop {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if !is_recoverable(&e) {
                    tracing::error!(
                        "{label}: non-recoverable error, not retrying: {e}"
                    );
                    return Err(e);
                }
                if attempt >= max_retries {
                    tracing::error!(
                        "{label}: giving up after {max_retries} retries: {e}"
                    );
                    return Err(e);
                }
                tracing::warn!(
                    "{label}: recoverable error (attempt {}/{max_retries}), \
                     retrying in {}s: {e}",
                    attempt + 1,
                    delay.as_secs(),
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
                delay = std::cmp::min(delay * 2, std::time::Duration::from_secs(30));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use misanthropic::client::{AnthropicError, Error as ClientError};
    use std::num::NonZeroU16;

    fn anyhowed<E>(err: E) -> anyhow::Error
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        anyhow::Error::new(err)
    }

    // ---- is_recoverable ----------------------------------------------------

    #[test]
    fn is_recoverable_anthropic_5xx_retries() {
        assert!(is_recoverable(&anyhowed(ClientError::Anthropic(
            AnthropicError::API {
                message: "internal error".to_string(),
            }
        ))));
    }

    #[test]
    fn is_recoverable_rate_limit_retries() {
        assert!(is_recoverable(&anyhowed(ClientError::Anthropic(
            AnthropicError::RateLimit {
                message: "too many requests".to_string(),
            }
        ))));
    }

    #[test]
    fn is_recoverable_overloaded_retries() {
        assert!(is_recoverable(&anyhowed(ClientError::Anthropic(
            AnthropicError::Overloaded {
                message: "overloaded".to_string(),
            }
        ))));
    }

    #[test]
    fn is_recoverable_unknown_5xx_retries() {
        assert!(is_recoverable(&anyhowed(ClientError::Anthropic(
            AnthropicError::Unknown {
                code: NonZeroU16::new(502).unwrap(),
                message: "bad gateway".to_string(),
            }
        ))));
    }

    #[test]
    fn is_recoverable_anthropic_4xx_does_not_retry() {
        assert!(!is_recoverable(&anyhowed(ClientError::Anthropic(
            AnthropicError::InvalidRequest {
                message: "bad request".to_string(),
            }
        ))));
    }

    #[test]
    fn is_recoverable_auth_does_not_retry() {
        assert!(!is_recoverable(&anyhowed(ClientError::Anthropic(
            AnthropicError::Authentication {
                message: "bad key".to_string(),
            }
        ))));
    }

    #[test]
    fn is_recoverable_parse_does_not_retry() {
        // Synthesize a parse error by trying to parse invalid JSON.
        let parse_err: serde_json::Error =
            serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        assert!(!is_recoverable(&anyhowed(ClientError::Parse(parse_err))));
    }

    #[test]
    fn is_recoverable_non_json_response_retries() {
        assert!(is_recoverable(&anyhowed(ClientError::NonJsonResponse {
            status: 502,
            body: "<html>Bad Gateway</html>".to_string(),
        })));
    }

    #[test]
    fn is_recoverable_unknown_error_defaults_to_retry() {
        // Plain anyhow string — classification unknown, default true.
        assert!(is_recoverable(&anyhow::anyhow!(
            "some random backend error"
        )));
    }

    // ---- retry_recoverable -------------------------------------------------

    #[tokio::test]
    async fn retry_recoverable_succeeds_immediately() {
        let mut attempts = 0;
        let result: Result<i32> = retry_recoverable("test", 3, || {
            attempts += 1;
            async { Ok(42) }
        })
        .await;
        assert!(matches!(result, Ok(42)));
        assert_eq!(attempts, 1);
    }

    #[tokio::test]
    async fn retry_recoverable_stops_on_non_recoverable() {
        let mut attempts = 0;
        let result: Result<i32> = retry_recoverable("test", 5, || {
            attempts += 1;
            async {
                Err(anyhow::Error::new(ClientError::Anthropic(
                    AnthropicError::Authentication {
                        message: "no".to_string(),
                    },
                )))
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(
            attempts, 1,
            "auth errors should short-circuit without retrying"
        );
    }

    #[tokio::test]
    async fn retry_recoverable_gives_up_after_max_retries() {
        // Use max_retries=0 so the test finishes instantly instead of
        // sleeping through the backoff schedule.
        let mut attempts = 0;
        let result: Result<i32> = retry_recoverable("test", 0, || {
            attempts += 1;
            async {
                Err(anyhow::Error::new(ClientError::Anthropic(
                    AnthropicError::Overloaded {
                        message: "busy".to_string(),
                    },
                )))
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(
            attempts, 1,
            "max_retries=0 means one attempt, no retries"
        );
    }
}
