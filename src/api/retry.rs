use std::time::Duration;

use crate::error::ActualError;

/// Configuration for retry behavior with exponential backoff.
#[derive(Debug)]
pub struct RetryConfig {
    /// Maximum number of attempts (including the initial attempt).
    max_attempts: u32,
    /// Delay before the first retry.
    initial_delay: Duration,
    /// Multiplier applied to the delay after each failed attempt.
    backoff_factor: u32,
    /// Maximum delay between retries. The computed backoff is capped at this
    /// value to prevent overflow (e.g. `Duration::MAX` ≈ 584 years) from
    /// causing an effective infinite hang. Defaults to 30 seconds.
    max_delay: Duration,
}

impl RetryConfig {
    /// Create a new `RetryConfig`, rejecting `max_attempts == 0`.
    ///
    /// # Errors
    ///
    /// Returns [`ActualError::ConfigError`] if `max_attempts` is zero.
    pub fn new(
        max_attempts: u32,
        initial_delay: Duration,
        backoff_factor: u32,
    ) -> Result<Self, ActualError> {
        if max_attempts == 0 {
            return Err(ActualError::ConfigError(
                "RetryConfig max_attempts must be >= 1".to_string(),
            ));
        }
        Ok(Self {
            max_attempts,
            initial_delay,
            backoff_factor,
            max_delay: Duration::from_secs(30),
        })
    }

    /// Create a new `RetryConfig` with a custom maximum delay cap.
    ///
    /// # Errors
    ///
    /// Returns [`ActualError::ConfigError`] if `max_attempts` is zero.
    #[cfg(test)]
    pub(crate) fn new_with_max_delay(
        max_attempts: u32,
        initial_delay: Duration,
        backoff_factor: u32,
        max_delay: Duration,
    ) -> Result<Self, ActualError> {
        if max_attempts == 0 {
            return Err(ActualError::ConfigError(
                "RetryConfig max_attempts must be >= 1".to_string(),
            ));
        }
        Ok(Self {
            max_attempts,
            initial_delay,
            backoff_factor,
            max_delay,
        })
    }

    /// Maximum number of attempts (including the initial attempt).
    #[cfg(test)]
    pub(crate) fn max_attempts(&self) -> u32 {
        self.max_attempts
    }

    /// Delay before the first retry.
    #[cfg(test)]
    pub(crate) fn initial_delay(&self) -> Duration {
        self.initial_delay
    }

    /// Multiplier applied to the delay after each failed attempt.
    #[cfg(test)]
    pub(crate) fn backoff_factor(&self) -> u32 {
        self.backoff_factor
    }

    /// Maximum delay between retries. The computed backoff is always capped at
    /// this value. Defaults to 30 seconds.
    #[cfg(test)]
    pub(crate) fn max_delay(&self) -> Duration {
        self.max_delay
    }
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_delay: Duration::from_secs(1),
            backoff_factor: 2,
            max_delay: Duration::from_secs(30),
        }
    }
}

/// Execute an async operation with retry and exponential backoff.
///
/// Retries on [`ActualError::ApiError`] (network/5xx errors) and
/// [`ActualError::ServiceUnavailable`] (HTTP 503 responses).
/// All other error variants (including [`ActualError::ApiResponseError`]) are
/// returned immediately without retry.
pub async fn with_retry<F, Fut, T>(config: &RetryConfig, mut f: F) -> Result<T, ActualError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, ActualError>>,
{
    debug_assert!(
        config.max_attempts >= 1,
        "RetryConfig::max_attempts must be >= 1, got {}",
        config.max_attempts
    );

    let mut last_error = ActualError::ApiError("no attempts made".to_string());

    for attempt in 0..config.max_attempts {
        if attempt > 0 {
            let delay = match config.backoff_factor.checked_pow(attempt - 1) {
                Some(multiplier) => config.initial_delay.saturating_mul(multiplier),
                // Overflow: fall back to the maximum allowed delay rather than
                // Duration::MAX to avoid an effective infinite hang.
                None => config.max_delay,
            };
            // Cap the delay so that a large backoff_factor cannot produce a
            // delay approaching Duration::MAX (~584 years).
            let delay = delay.min(config.max_delay);
            tokio::time::sleep(delay).await;
        }

        match f().await {
            Ok(value) => return Ok(value),
            Err(ActualError::ApiError(msg)) => {
                last_error = ActualError::ApiError(msg);
            }
            Err(ActualError::ServiceUnavailable) => {
                last_error = ActualError::ServiceUnavailable;
            }
            Err(e) => return Err(e),
        }
    }

    Err(last_error)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    use super::*;

    /// Shared test helper — a single monomorphization covers all match arms.
    /// `behavior` controls what happens on each call:
    ///   - 0 => ApiError (retryable)
    ///   - 1 => ApiResponseError (not retryable)
    ///   - _ => Ok(value)
    async fn run_retry(config: &RetryConfig, behaviors: &[u32]) -> (Result<i32, ActualError>, u32) {
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = Arc::clone(&counter);
        let behaviors: Arc<Vec<u32>> = Arc::new(behaviors.to_vec());

        let result = with_retry(config, || {
            let counter = Arc::clone(&counter_clone);
            let behaviors = Arc::clone(&behaviors);
            async move {
                let n = counter.fetch_add(1, Ordering::SeqCst) as usize;
                let behavior = behaviors.get(n).copied().unwrap_or(2);
                match behavior {
                    0 => Err(ActualError::ApiError("server error".to_string())),
                    1 => Err(ActualError::ApiResponseError {
                        code: "400".to_string(),
                        message: "bad request".to_string(),
                    }),
                    _ => Ok(42),
                }
            }
        })
        .await;

        let calls = counter.load(Ordering::SeqCst);
        (result, calls)
    }

    #[test]
    fn test_default_config() {
        let config = RetryConfig::default();
        assert_eq!(config.max_attempts(), 3);
        assert_eq!(config.initial_delay(), Duration::from_secs(1));
        assert_eq!(config.backoff_factor(), 2);
        assert_eq!(config.max_delay(), Duration::from_secs(30));
    }

    #[test]
    fn test_new_rejects_zero_attempts() {
        let err = RetryConfig::new(0, Duration::from_secs(1), 2).unwrap_err();
        assert!(
            err.to_string().contains("max_attempts must be >= 1"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn test_new_accepts_one_attempt() {
        let config = RetryConfig::new(1, Duration::from_secs(1), 2).unwrap();
        assert_eq!(config.max_attempts(), 1);
    }

    #[test]
    fn test_new_valid_config() {
        let config = RetryConfig::new(5, Duration::from_millis(500), 3).unwrap();
        assert_eq!(config.max_attempts(), 5);
        assert_eq!(config.initial_delay(), Duration::from_millis(500));
        assert_eq!(config.backoff_factor(), 3);
        // new() must default max_delay to 30 seconds.
        assert_eq!(config.max_delay(), Duration::from_secs(30));
    }

    #[test]
    fn test_new_with_max_delay_valid() {
        let config = RetryConfig::new_with_max_delay(
            4,
            Duration::from_millis(200),
            3,
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(config.max_attempts(), 4);
        assert_eq!(config.initial_delay(), Duration::from_millis(200));
        assert_eq!(config.backoff_factor(), 3);
        assert_eq!(config.max_delay(), Duration::from_secs(10));
    }

    #[test]
    fn test_new_with_max_delay_rejects_zero_attempts() {
        let err =
            RetryConfig::new_with_max_delay(0, Duration::from_secs(1), 2, Duration::from_secs(30))
                .unwrap_err();
        assert!(
            err.to_string().contains("max_attempts must be >= 1"),
            "unexpected error message: {err}"
        );
    }

    #[tokio::test]
    async fn test_single_attempt_no_retry() {
        tokio::time::pause();

        let config = RetryConfig::new(1, Duration::from_secs(1), 2).unwrap();
        // Single attempt fails — should NOT retry.
        let (result, calls) = run_retry(&config, &[0]).await;

        let err = result.unwrap_err();
        assert!(matches!(err, ActualError::ApiError(_)));
        assert_eq!(calls, 1);
    }

    #[tokio::test]
    async fn test_single_attempt_succeeds() {
        tokio::time::pause();

        let config = RetryConfig::new(1, Duration::from_secs(1), 2).unwrap();
        // Single attempt succeeds.
        let (result, calls) = run_retry(&config, &[2]).await;

        assert_eq!(result.unwrap(), 42);
        assert_eq!(calls, 1);
    }

    #[tokio::test]
    async fn test_retry_succeeds_after_transient_failures() {
        tokio::time::pause();

        let config = RetryConfig::default();
        // Fail twice with ApiError, then succeed.
        let (result, calls) = run_retry(&config, &[0, 0, 2]).await;

        assert_eq!(result.unwrap(), 42);
        assert_eq!(calls, 3);
    }

    #[tokio::test]
    async fn test_no_retry_on_client_error() {
        tokio::time::pause();

        let config = RetryConfig::default();
        // First call returns ApiResponseError — should NOT retry.
        let (result, calls) = run_retry(&config, &[1]).await;

        let err = result.unwrap_err();
        assert_eq!(err.to_string(), "API returned error: 400: bad request");
        assert_eq!(calls, 1);
    }

    #[tokio::test]
    async fn test_retry_exhausted() {
        tokio::time::pause();

        let config = RetryConfig::default();
        // All 3 calls return ApiError — should exhaust retries.
        let (result, calls) = run_retry(&config, &[0, 0, 0]).await;

        let err = result.unwrap_err();
        assert_eq!(err.to_string(), "API request failed: server error");
        assert_eq!(calls, 3);
    }

    #[tokio::test]
    async fn test_backoff_delays() {
        tokio::time::pause();

        let timestamps = Arc::new(std::sync::Mutex::new(Vec::new()));
        let timestamps_clone = Arc::clone(&timestamps);

        let config = RetryConfig::default();
        // All calls fail — we just want to measure the backoff timing.
        let _result: Result<i32, _> = with_retry(&config, || {
            let timestamps = Arc::clone(&timestamps_clone);
            async move {
                timestamps.lock().unwrap().push(tokio::time::Instant::now());
                Err(ActualError::ApiError("500".to_string()))
            }
        })
        .await;

        let ts = timestamps.lock().unwrap();
        assert_eq!(ts.len(), 3);

        let first_to_second = ts[1] - ts[0];
        let second_to_third = ts[2] - ts[1];

        // With paused time, delays are nearly exact but tokio's auto-advance
        // adds up to 1ms per `.await` point on the paused clock.
        let tolerance = Duration::from_millis(10);
        assert!(first_to_second >= Duration::from_secs(1));
        assert!(first_to_second <= Duration::from_secs(1) + tolerance);
        assert!(second_to_third >= Duration::from_secs(2));
        assert!(second_to_third <= Duration::from_secs(2) + tolerance);
    }

    #[tokio::test]
    async fn test_backoff_no_overflow_on_large_attempts() {
        tokio::time::pause();

        let config = RetryConfig {
            max_attempts: 34,
            initial_delay: Duration::from_secs(1),
            backoff_factor: 2,
            max_delay: Duration::from_secs(30),
        };

        // All 34 attempts fail with ApiError.
        // attempt=33 computes 2^32 which overflows u32, exercising the
        // checked_pow → None → max_delay fallback path.
        // This must NOT panic (debug) or silently wrap (release), and the
        // delay must be capped at max_delay rather than Duration::MAX.
        let behaviors: Vec<u32> = vec![0; 34];
        let (result, calls) = run_retry(&config, &behaviors).await;

        let err = result.unwrap_err();
        assert!(matches!(err, ActualError::ApiError(_)));
        assert_eq!(calls, 34);
    }

    /// Verify that a large `backoff_factor` never produces a delay exceeding
    /// `max_delay` (30 seconds by default).
    #[tokio::test]
    async fn test_backoff_capped_with_large_factor() {
        tokio::time::pause();

        let timestamps = Arc::new(std::sync::Mutex::new(Vec::new()));
        let timestamps_clone = Arc::clone(&timestamps);

        // backoff_factor=1000 would produce 1000^1 = 1000s on the second
        // attempt and 1000^2 = 1_000_000s on the third — both must be capped
        // at 30 seconds.
        let config = RetryConfig::new(3, Duration::from_secs(1), 1000).unwrap();

        let _result: Result<i32, _> = with_retry(&config, || {
            let timestamps = Arc::clone(&timestamps_clone);
            async move {
                timestamps.lock().unwrap().push(tokio::time::Instant::now());
                Err(ActualError::ApiError("500".to_string()))
            }
        })
        .await;

        let ts = timestamps.lock().unwrap();
        assert_eq!(ts.len(), 3);

        let first_to_second = ts[1] - ts[0];
        let second_to_third = ts[2] - ts[1];

        let max_delay = Duration::from_secs(30);
        let tolerance = Duration::from_millis(10);

        // Both gaps must be at most 30 seconds (plus a small tolerance for
        // tokio's paused-clock auto-advance).
        assert!(
            first_to_second <= max_delay + tolerance,
            "first→second delay {first_to_second:?} exceeded 30s cap"
        );
        assert!(
            second_to_third <= max_delay + tolerance,
            "second→third delay {second_to_third:?} exceeded 30s cap"
        );
        // With factor=1000 and initial=1s, without a cap the second gap would
        // be 1000s. Assert we're nowhere near that.
        assert!(
            first_to_second <= max_delay + tolerance,
            "delay should be capped at 30s, not 1000s"
        );
    }

    #[tokio::test]
    async fn test_retry_on_service_unavailable_then_success() {
        tokio::time::pause();

        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = Arc::clone(&counter);
        let config = RetryConfig::default();

        let result: Result<i32, _> = with_retry(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                let n = counter.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    Err(ActualError::ServiceUnavailable)
                } else {
                    Ok(42)
                }
            }
        })
        .await;

        assert_eq!(result.unwrap(), 42);
        assert_eq!(
            counter.load(Ordering::SeqCst),
            2,
            "should retry once after ServiceUnavailable"
        );
    }

    #[tokio::test]
    async fn test_retry_exhausted_on_service_unavailable() {
        tokio::time::pause();

        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = Arc::clone(&counter);
        let config = RetryConfig::default(); // 3 attempts

        let result: Result<i32, _> = with_retry(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err(ActualError::ServiceUnavailable)
            }
        })
        .await;

        let err = result.unwrap_err();
        assert!(
            matches!(err, ActualError::ServiceUnavailable),
            "expected ServiceUnavailable after exhausting retries"
        );
        assert_eq!(
            counter.load(Ordering::SeqCst),
            3,
            "should exhaust all 3 attempts"
        );
    }

    #[tokio::test]
    async fn test_no_retry_on_client_error_after_service_unavailable() {
        tokio::time::pause();

        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = Arc::clone(&counter);
        let config = RetryConfig::default();

        // ServiceUnavailable then ApiResponseError (non-retryable) — should stop after 2nd call.
        let result: Result<i32, _> = with_retry(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                let n = counter.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    Err(ActualError::ServiceUnavailable)
                } else {
                    Err(ActualError::ApiResponseError {
                        code: "400".to_string(),
                        message: "bad request".to_string(),
                    })
                }
            }
        })
        .await;

        let err = result.unwrap_err();
        assert!(
            matches!(err, ActualError::ApiResponseError { .. }),
            "expected ApiResponseError to propagate immediately"
        );
        assert_eq!(
            counter.load(Ordering::SeqCst),
            2,
            "should stop retrying on non-retryable error"
        );
    }

    /// Verify that `new_with_max_delay` respects a custom cap.
    #[tokio::test]
    async fn test_backoff_custom_max_delay() {
        tokio::time::pause();

        let timestamps = Arc::new(std::sync::Mutex::new(Vec::new()));
        let timestamps_clone = Arc::clone(&timestamps);

        // Custom cap of 5 seconds; factor=1000 would overflow without cap.
        let config = RetryConfig::new_with_max_delay(
            3,
            Duration::from_secs(1),
            1000,
            Duration::from_secs(5),
        )
        .unwrap();

        let _result: Result<i32, _> = with_retry(&config, || {
            let timestamps = Arc::clone(&timestamps_clone);
            async move {
                timestamps.lock().unwrap().push(tokio::time::Instant::now());
                Err(ActualError::ApiError("500".to_string()))
            }
        })
        .await;

        let ts = timestamps.lock().unwrap();
        assert_eq!(ts.len(), 3);

        let custom_cap = Duration::from_secs(5);
        let tolerance = Duration::from_millis(10);

        let first_to_second = ts[1] - ts[0];
        let second_to_third = ts[2] - ts[1];

        assert!(
            first_to_second <= custom_cap + tolerance,
            "first→second delay {first_to_second:?} exceeded 5s cap"
        );
        assert!(
            second_to_third <= custom_cap + tolerance,
            "second→third delay {second_to_third:?} exceeded 5s cap"
        );
    }
}
