use std::time::Duration;

use reqwest::{Method, Response, StatusCode};

type CheckRetryFn = Box<dyn Fn(u32, RetryableOutcome<'_>) -> bool + Send + Sync>;
type BackoffFn = Box<dyn Fn(u32) -> Duration + Send + Sync>;

/// Outcome of a single request attempt, passed to retry policies.
pub enum RetryableOutcome<'a> {
    /// The request returned a response.
    Response(&'a Response),
    /// The request failed before producing a response.
    Error(&'a reqwest::Error),
}

/// Defines whether and how requests should be retried.
///
/// Implementors must be `Send + Sync` because `Client` is shared across tasks.
pub trait RetryPolicy: Send + Sync {
    /// Maximum number of retry attempts after the initial request.
    fn max_retries(&self) -> u32;

    /// Returns `true` if the attempt should be retried.
    ///
    /// The `attempt` argument is zero-indexed (0 = first retry attempt).
    fn should_retry(&self, attempt: u32, outcome: RetryableOutcome<'_>) -> bool;

    /// Backoff duration to wait before the next retry attempt.
    fn backoff(&self, attempt: u32) -> Duration;

    /// HTTP methods that are eligible for retry.
    fn retryable_methods(&self) -> &[Method];

    /// Upper bound on a server-supplied `Retry-After` wait. Defaults to 5 minutes.
    ///
    /// A malicious or buggy server can send `Retry-After: 9999999`; this cap
    /// prevents one header value from pinning the client.
    fn max_retry_after(&self) -> Duration {
        Duration::from_secs(300)
    }
}

/// Default retry policy: retries idempotent methods on 5xx, 429, connect, and timeout errors.
pub struct DefaultRetryPolicy {
    max_retries: u32,
    check_retry: CheckRetryFn,
    backoff_strategy: BackoffFn,
    retryable_methods: Vec<Method>,
    max_retry_after: Duration,
}

impl DefaultRetryPolicy {
    /// Returns a builder for configuring the default policy.
    pub fn builder() -> DefaultRetryPolicyBuilder {
        DefaultRetryPolicyBuilder::new()
    }
}

impl RetryPolicy for DefaultRetryPolicy {
    fn max_retries(&self) -> u32 {
        self.max_retries
    }

    fn should_retry(&self, attempt: u32, outcome: RetryableOutcome<'_>) -> bool {
        (self.check_retry)(attempt, outcome)
    }

    fn backoff(&self, attempt: u32) -> Duration {
        (self.backoff_strategy)(attempt)
    }

    fn retryable_methods(&self) -> &[Method] {
        &self.retryable_methods
    }

    fn max_retry_after(&self) -> Duration {
        self.max_retry_after
    }
}

pub(crate) const DEFAULT_MIN_WAIT: Duration = Duration::from_millis(100);
pub(crate) const DEFAULT_MAX_WAIT: Duration = Duration::from_secs(30);

/// Exponential backoff with full jitter. The result is always in `[0, max_wait]`.
///
/// Uses saturating integer arithmetic so it cannot panic on overflow regardless of
/// the `attempt` value.
fn exponential_jitter_backoff(min_wait: Duration, max_wait: Duration, attempt: u32) -> Duration {
    let max_ms = max_wait.as_millis() as u64;
    let base = (min_wait.as_millis() as u64).saturating_mul(2u64.saturating_pow(attempt));
    let capped = std::cmp::min(base, max_ms);
    // Full jitter: uniform in [0, capped]. capped.max(1) avoids `% 0`.
    Duration::from_millis(rand::random::<u64>() % (capped.max(1) + 1))
}

/// Builder for `DefaultRetryPolicy`.
pub struct DefaultRetryPolicyBuilder {
    max_retries: u32,
    check_retry: Option<CheckRetryFn>,
    backoff_strategy: Option<BackoffFn>,
    retryable_methods: Vec<Method>,
    max_retry_after: Option<Duration>,
}

impl DefaultRetryPolicyBuilder {
    fn new() -> Self {
        Self {
            max_retries: 3,
            check_retry: None,
            backoff_strategy: None,
            // Only idempotent/safe methods are retried by default. Non-idempotent methods such as
            // POST/PATCH can create duplicate side-effects if retried blindly; opt them in explicitly.
            retryable_methods: vec![
                Method::GET,
                Method::HEAD,
                Method::PUT,
                Method::DELETE,
                Method::OPTIONS,
                Method::TRACE,
            ],
            max_retry_after: None,
        }
    }

    /// Sets the maximum number of retry attempts after the initial request.
    pub fn max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Configures exponential backoff with full jitter between `min_wait` and `max_wait`.
    ///
    /// Also implicitly caps server-supplied `Retry-After` at `max_wait` (unless overridden
    /// by [`max_retry_after`](Self::max_retry_after)).
    pub fn backoff(mut self, min_wait: Duration, max_wait: Duration) -> Self {
        self.max_retry_after.get_or_insert(max_wait);
        self.backoff_strategy = Some(Box::new(move |attempt: u32| {
            exponential_jitter_backoff(min_wait, max_wait, attempt)
        }));
        self
    }

    /// Replaces the default retry predicate.
    pub fn check_retry(
        mut self,
        check: impl Fn(u32, RetryableOutcome<'_>) -> bool + 'static + Send + Sync,
    ) -> Self {
        self.check_retry = Some(Box::new(check));
        self
    }

    /// Replaces the set of HTTP methods that may be retried.
    pub fn retryable_methods(mut self, methods: &[Method]) -> Self {
        self.retryable_methods = methods.to_vec();
        self
    }

    /// Sets the upper bound applied to a server-supplied `Retry-After` header value.
    pub fn max_retry_after(mut self, cap: Duration) -> Self {
        self.max_retry_after = Some(cap);
        self
    }

    /// Builds the policy.
    pub fn build(self) -> DefaultRetryPolicy {
        DefaultRetryPolicy {
            max_retries: self.max_retries,
            check_retry: self.check_retry.unwrap_or_else(|| {
                Box::new(
                    |_attempt: u32, outcome: RetryableOutcome<'_>| match outcome {
                        RetryableOutcome::Error(err) => err.is_timeout() || err.is_connect(),
                        RetryableOutcome::Response(resp) => {
                            let status = resp.status();
                            status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS
                        }
                    },
                )
            }),
            backoff_strategy: self.backoff_strategy.unwrap_or_else(|| {
                Box::new(|attempt: u32| {
                    exponential_jitter_backoff(DEFAULT_MIN_WAIT, DEFAULT_MAX_WAIT, attempt)
                })
            }),
            retryable_methods: self.retryable_methods,
            max_retry_after: self.max_retry_after.unwrap_or(DEFAULT_MAX_WAIT),
        }
    }
}

/// A policy that never retries.
pub struct NoRetry;

impl RetryPolicy for NoRetry {
    fn max_retries(&self) -> u32 {
        0
    }

    fn should_retry(&self, _attempt: u32, _outcome: RetryableOutcome<'_>) -> bool {
        false
    }

    fn backoff(&self, _attempt: u32) -> Duration {
        Duration::ZERO
    }

    fn retryable_methods(&self) -> &[Method] {
        const EMPTY: &[Method] = &[];
        EMPTY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_always_within_bounds() {
        for attempt in 0..32u32 {
            for _ in 0..100 {
                let wait = exponential_jitter_backoff(
                    Duration::from_millis(100),
                    Duration::from_secs(30),
                    attempt,
                );
                assert!(
                    wait <= Duration::from_secs(30),
                    "attempt {attempt}: {wait:?} > max"
                );
            }
        }
    }

    #[test]
    fn backoff_huge_attempt_does_not_panic() {
        // A very large attempt count must not panic or overflow.
        let _ = exponential_jitter_backoff(
            Duration::from_millis(100),
            Duration::from_secs(30),
            u32::MAX,
        );
    }

    #[test]
    fn backoff_respects_custom_max() {
        for _ in 0..1000 {
            let wait =
                exponential_jitter_backoff(Duration::from_secs(1), Duration::from_millis(5), 10);
            assert!(wait <= Duration::from_millis(5));
        }
    }
}
