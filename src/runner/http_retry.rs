//! Shared HTTP retry utilities for the OpenAI and Anthropic API runners.
//!
//! This module provides:
//! - [`MAX_RATE_LIMIT_RETRIES`]: shared retry limit constant
//! - [`parse_retry_after`]: parse the `Retry-After` header
//! - [`extract_error_body`]: read and truncate an HTTP error body
//! - [`backoff_secs`]: compute exponential backoff wait time
//! - [`RetryState`]: track per-loop retry attempt state

use std::time::Duration;

/// Maximum number of retries on HTTP 429 / 529 rate-limit responses.
pub const MAX_RATE_LIMIT_RETRIES: u32 = 3;

/// Parse the `Retry-After` header value into a number of seconds.
///
/// Returns `Some(seconds)` if the header is present and contains a valid
/// non-negative integer.  Returns `None` if the header is absent, non-UTF-8,
/// or not an integer (e.g., an HTTP-date string).
pub fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let value = headers.get("retry-after")?.to_str().ok()?;
    value.trim().parse::<u64>().ok()
}

/// Extract a truncated body string from an HTTP response byte result,
/// returning a fallback message if the body could not be read.
pub fn extract_error_body(
    body_result: Result<impl AsRef<[u8]>, impl std::fmt::Display>,
    max: usize,
) -> String {
    match body_result {
        Ok(bytes) => {
            let bytes = bytes.as_ref();
            let truncated = &bytes[..bytes.len().min(max)];
            String::from_utf8_lossy(truncated).into_owned()
        }
        Err(e) => format!("<body read error: {e}>"),
    }
}

/// Compute the number of seconds to wait before the next retry.
///
/// If a `Retry-After` header value is present it is used directly.
/// Otherwise exponential back-off is applied: 1 s, 2 s, 4 s, … capped at 60 s.
///
/// `attempt` is the 1-based current retry attempt (i.e. the value of the
/// counter *after* it has been incremented for this retry).
pub fn backoff_secs(attempt: u32, retry_after: Option<u64>) -> u64 {
    retry_after
        .unwrap_or_else(|| 1u64.checked_shl(attempt - 1).unwrap_or(u64::MAX))
        .min(60)
}

/// Tracks the retry attempt counter for an HTTP rate-limit retry loop.
///
/// Callers create one `RetryState` at the top of their loop, call
/// [`RetryState::increment`] on each retryable response, and use the
/// accessor methods for logging and back-off computation.
#[derive(Debug)]
pub struct RetryState {
    attempt: u32,
    max_retries: u32,
    /// Base duration multiplied by [`backoff_secs`] to obtain the actual
    /// sleep duration.  Exposed so tests can set it to [`Duration::ZERO`].
    pub retry_base: Duration,
}

impl RetryState {
    /// Create a new `RetryState` with the given limits.
    pub fn new(max_retries: u32, retry_base: Duration) -> Self {
        Self {
            attempt: 0,
            max_retries,
            retry_base,
        }
    }

    /// Increment the attempt counter.
    ///
    /// Returns `true` if the caller should retry (i.e. `attempt <=
    /// max_retries` after incrementing), or `false` when all retries have
    /// been exhausted.
    pub fn increment(&mut self) -> bool {
        self.attempt += 1;
        self.attempt <= self.max_retries
    }

    /// The current attempt number (1-based; meaningful after the first call
    /// to [`increment`](Self::increment)).
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// The configured maximum number of retries.
    pub fn max_retries(&self) -> u32 {
        self.max_retries
    }

    /// Compute the sleep [`Duration`] for this attempt.
    pub fn sleep_duration(&self, retry_after: Option<u64>) -> Duration {
        self.retry_base * backoff_secs(self.attempt, retry_after) as u32
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    // ── parse_retry_after ─────────────────────────────────────────────────

    #[test]
    fn test_parse_retry_after_valid_integer() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("30"));
        assert_eq!(parse_retry_after(&headers), Some(30));
    }

    #[test]
    fn test_parse_retry_after_with_whitespace() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("  5  "));
        assert_eq!(parse_retry_after(&headers), Some(5));
    }

    #[test]
    fn test_parse_retry_after_zero() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("0"));
        assert_eq!(parse_retry_after(&headers), Some(0));
    }

    #[test]
    fn test_parse_retry_after_missing_header() {
        let headers = HeaderMap::new();
        assert_eq!(parse_retry_after(&headers), None);
    }

    #[test]
    fn test_parse_retry_after_http_date_string() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "retry-after",
            HeaderValue::from_static("Mon, 01 Jan 2024 00:00:00 GMT"),
        );
        assert_eq!(parse_retry_after(&headers), None);
    }

    #[test]
    fn test_parse_retry_after_non_numeric() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("abc"));
        assert_eq!(parse_retry_after(&headers), None);
    }

    #[test]
    fn test_parse_retry_after_negative() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("-1"));
        assert_eq!(parse_retry_after(&headers), None);
    }

    // ── extract_error_body ────────────────────────────────────────────────

    #[test]
    fn test_extract_error_body_ok_short() {
        let result: Result<Vec<u8>, String> = Ok(b"hello".to_vec());
        assert_eq!(extract_error_body(result, 100), "hello");
    }

    #[test]
    fn test_extract_error_body_ok_truncated() {
        let long = "a".repeat(200);
        let result: Result<Vec<u8>, String> = Ok(long.as_bytes().to_vec());
        let out = extract_error_body(result, 10);
        assert_eq!(out.len(), 10);
        assert_eq!(out, "aaaaaaaaaa");
    }

    #[test]
    fn test_extract_error_body_ok_exact_limit() {
        let result: Result<Vec<u8>, String> = Ok(b"1234567890".to_vec());
        assert_eq!(extract_error_body(result, 10), "1234567890");
    }

    #[test]
    fn test_extract_error_body_err() {
        let result: Result<Vec<u8>, String> = Err("network error".to_string());
        let out = extract_error_body(result, 100);
        assert!(
            out.contains("body read error"),
            "expected 'body read error' in: {out}"
        );
        assert!(
            out.contains("network error"),
            "expected original error in: {out}"
        );
    }

    // ── backoff_secs ──────────────────────────────────────────────────────

    #[test]
    fn test_backoff_secs_exponential_no_header() {
        assert_eq!(backoff_secs(1, None), 1);
        assert_eq!(backoff_secs(2, None), 2);
        assert_eq!(backoff_secs(3, None), 4);
        assert_eq!(backoff_secs(4, None), 8);
    }

    #[test]
    fn test_backoff_secs_capped_at_60() {
        // 2^7 = 128, should be capped to 60
        assert_eq!(backoff_secs(8, None), 60);
        assert_eq!(backoff_secs(100, None), 60);
    }

    #[test]
    fn test_backoff_secs_retry_after_overrides() {
        assert_eq!(backoff_secs(1, Some(45)), 45);
        assert_eq!(backoff_secs(3, Some(0)), 0);
    }

    #[test]
    fn test_backoff_secs_retry_after_capped_at_60() {
        assert_eq!(backoff_secs(1, Some(120)), 60);
    }

    // ── RetryState ────────────────────────────────────────────────────────

    #[test]
    fn test_retry_state_initial_state() {
        let state = RetryState::new(3, Duration::from_secs(1));
        assert_eq!(state.attempt(), 0);
        assert_eq!(state.max_retries(), 3);
    }

    #[test]
    fn test_retry_state_increment_within_limit() {
        let mut state = RetryState::new(3, Duration::from_secs(1));
        assert!(state.increment()); // attempt = 1
        assert_eq!(state.attempt(), 1);
        assert!(state.increment()); // attempt = 2
        assert!(state.increment()); // attempt = 3
        assert_eq!(state.attempt(), 3);
    }

    #[test]
    fn test_retry_state_increment_exhausted() {
        let mut state = RetryState::new(3, Duration::from_secs(1));
        state.increment(); // 1
        state.increment(); // 2
        state.increment(); // 3
        assert!(!state.increment()); // 4 > max_retries=3 → exhausted
    }

    #[test]
    fn test_retry_state_sleep_duration_no_header() {
        let state_after_first_attempt = {
            let mut s = RetryState::new(3, Duration::from_secs(1));
            s.increment();
            s
        };
        // attempt=1, no retry-after → 1s * 1 = 1s
        assert_eq!(
            state_after_first_attempt.sleep_duration(None),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn test_retry_state_sleep_duration_with_retry_after() {
        let state_after_first_attempt = {
            let mut s = RetryState::new(3, Duration::from_secs(1));
            s.increment();
            s
        };
        // retry_after=5 → 5s * 1 = 5s
        assert_eq!(
            state_after_first_attempt.sleep_duration(Some(5)),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn test_retry_state_sleep_duration_zero_base() {
        let state_after_first_attempt = {
            let mut s = RetryState::new(3, Duration::ZERO);
            s.increment();
            s
        };
        // retry_base=0 → always zero sleep regardless of wait_secs
        assert_eq!(
            state_after_first_attempt.sleep_duration(None),
            Duration::ZERO
        );
        assert_eq!(
            state_after_first_attempt.sleep_duration(Some(100)),
            Duration::ZERO
        );
    }
}
