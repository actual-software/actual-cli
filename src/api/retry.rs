use std::time::Duration;

use crate::error::ActualError;

/// Configuration for retry behavior with exponential backoff.
pub struct RetryConfig {
    /// Maximum number of attempts (including the initial attempt).
    pub max_attempts: u32,
    /// Delay before the first retry.
    pub initial_delay: Duration,
    /// Multiplier applied to the delay after each failed attempt.
    pub backoff_factor: u32,
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
    let mut last_error: Option<ActualError> = None;

    for attempt in 0..config.max_attempts {
        // Apply backoff delay before retries (not before the first attempt).
        if attempt > 0 {
            let delay = config
                .initial_delay
                .saturating_mul(config.backoff_factor.pow(attempt - 1));
            tokio::time::sleep(delay).await;
        }

        match f().await {
            Ok(value) => return Ok(value),
            Err(ActualError::ApiError(msg)) => {
                last_error = Some(ActualError::ApiError(msg));
                // Continue to next attempt.
            }
            Err(e) => return Err(e),
        }
    }

    Err(last_error.expect("max_attempts must be >= 1"))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    use super::*;

    #[tokio::test]
    async fn test_retry_succeeds_after_transient_failures() {
        tokio::time::pause();

        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = Arc::clone(&counter);

        let config = RetryConfig::default();
        let result = with_retry(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                let n = counter.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err(ActualError::ApiError("503".to_string()))
                } else {
                    Ok(42)
                }
            }
        })
        .await;

        assert_eq!(result.unwrap(), 42);
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_no_retry_on_client_error() {
        tokio::time::pause();

        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = Arc::clone(&counter);

        let config = RetryConfig::default();
        let result: Result<(), _> = with_retry(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err(ActualError::ApiResponseError {
                    code: "400".to_string(),
                    message: "bad".to_string(),
                })
            }
        })
        .await;

        assert!(matches!(result, Err(ActualError::ApiResponseError { .. })));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_retry_exhausted() {
        tokio::time::pause();

        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = Arc::clone(&counter);

        let config = RetryConfig::default();
        let result: Result<(), _> = with_retry(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err(ActualError::ApiError("500".to_string()))
            }
        })
        .await;

        assert!(matches!(result, Err(ActualError::ApiError(_))));
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_backoff_delays() {
        tokio::time::pause();

        let timestamps = Arc::new(std::sync::Mutex::new(Vec::new()));
        let timestamps_clone = Arc::clone(&timestamps);

        let config = RetryConfig::default();
        let _result: Result<(), _> = with_retry(&config, || {
            let timestamps = Arc::clone(&timestamps_clone);
            async move {
                timestamps.lock().unwrap().push(tokio::time::Instant::now());
                Err(ActualError::ApiError("500".to_string()))
            }
        })
        .await;

        let ts = timestamps.lock().unwrap();
        assert_eq!(ts.len(), 3);

        // First call happens immediately.
        let first_to_second = ts[1] - ts[0];
        let second_to_third = ts[2] - ts[1];

        // With paused time, delays are nearly exact but tokio's auto-advance
        // adds up to 1ms per `.await` point on the paused clock.
        // After attempt 0 fails: sleep(1s * 2^0) = ~1s
        // After attempt 1 fails: sleep(1s * 2^1) = ~2s
        let tolerance = Duration::from_millis(10);
        assert!(first_to_second >= Duration::from_secs(1));
        assert!(first_to_second <= Duration::from_secs(1) + tolerance);
        assert!(second_to_third >= Duration::from_secs(2));
        assert!(second_to_third <= Duration::from_secs(2) + tolerance);
    }
}
