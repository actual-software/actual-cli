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
        })
    }

    /// Maximum number of attempts (including the initial attempt).
    pub fn max_attempts(&self) -> u32 {
        self.max_attempts
    }

    /// Delay before the first retry.
    pub fn initial_delay(&self) -> Duration {
        self.initial_delay
    }

    /// Multiplier applied to the delay after each failed attempt.
    pub fn backoff_factor(&self) -> u32 {
        self.backoff_factor
    }
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_delay: Duration::from_secs(1),
            backoff_factor: 2,
        }
    }
}

/// Execute an async operation with retry and exponential backoff.
///
/// Only retries on [`ActualError::ApiError`] (network/5xx errors).
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
                None => Duration::MAX,
            };
            tokio::time::sleep(delay).await;
        }

        match f().await {
            Ok(value) => return Ok(value),
            Err(ActualError::ApiError(msg)) => {
                last_error = ActualError::ApiError(msg);
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
        };

        // All 34 attempts fail with ApiError.
        // attempt=33 computes 2^32 which overflows u32, exercising the
        // checked_pow → None → Duration::MAX fallback path.
        // This must NOT panic (debug) or silently wrap (release).
        let behaviors: Vec<u32> = vec![0; 34];
        let (result, calls) = run_retry(&config, &behaviors).await;

        let err = result.unwrap_err();
        assert!(matches!(err, ActualError::ApiError(_)));
        assert_eq!(calls, 34);
    }
}
