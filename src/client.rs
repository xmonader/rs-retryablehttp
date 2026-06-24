use std::time::Duration;

use reqwest::{Request, Response};

use crate::error::Error;
use crate::policy::{DefaultRetryPolicy, RetryPolicy, RetryableOutcome};

/// A retrying HTTP client.
///
/// `Client` wraps a [`reqwest::Client`] and a [`RetryPolicy`]. The retrying entry
/// point is [`Client::execute`]. There are no per-method convenience builders
/// because a raw `reqwest::RequestBuilder::send()` cannot be intercepted for
/// retries.
///
/// `Client` holds a cheaply-clonable inner `reqwest::Client` (a handle to a
/// shared connection pool). To share a client across tasks, clone the
/// underlying `reqwest::Client` via [`Client::inner`] rather than the
/// `rs-retryablehttp` `Client` itself (which is not `Clone`).
pub struct Client {
    inner: reqwest::Client,
    retry_policy: Box<dyn RetryPolicy>,
}

impl Client {
    /// Creates a client with the default retry policy.
    pub fn new(inner: reqwest::Client) -> Self {
        Self {
            inner,
            retry_policy: Box::new(DefaultRetryPolicy::builder().build()),
        }
    }

    /// Creates a client with the given retry policy.
    pub fn with_policy(inner: reqwest::Client, policy: impl RetryPolicy + 'static) -> Self {
        Self {
            inner,
            retry_policy: Box::new(policy),
        }
    }

    /// Returns a builder for configuring a client.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::new()
    }

    /// Returns a reference to the inner `reqwest::Client`.
    ///
    /// Useful when a single request should bypass retry logic but reuse the
    /// same connection pool.
    pub fn inner(&self) -> &reqwest::Client {
        &self.inner
    }

    /// Executes a request, retrying according to the configured policy.
    ///
    /// Returns [`Error::MaxRetriesExceeded`] when the policy's `max_retries`
    /// is reached and the last attempt did not succeed. If `max_retries` is 0
    /// and the single attempt fails, the original outcome is returned.
    pub async fn execute(&self, req: Request) -> Result<Response, Error> {
        let method_retryable = self.retry_policy.retryable_methods().contains(req.method());
        let max_retries = self.retry_policy.max_retries();
        let mut attempt = 0;
        loop {
            tracing::debug!(attempt, method = %req.method(), "sending request");
            let req_to_send = req.try_clone().ok_or(Error::BodyNotRewindable)?;
            match self.inner.execute(req_to_send).await {
                Ok(resp) => {
                    let status = resp.status();
                    tracing::debug!(attempt, status = %status, "response received");
                    // Decide retryability FIRST. A successful response on the final
                    // allowed attempt must be returned, not discarded.
                    let do_retry = method_retryable
                        && self
                            .retry_policy
                            .should_retry(attempt, RetryableOutcome::Response(&resp));
                    if !do_retry {
                        return Ok(resp);
                    }
                    // The response is retryable. Are we still allowed to retry?
                    if attempt >= max_retries {
                        if attempt > 0 {
                            // Exhausted on a retryable response; there is no
                            // transport error to attach as the cause.
                            return Err(Error::MaxRetriesExceeded {
                                attempts: attempt + 1,
                                source: None,
                            });
                        }
                        // max_retries == 0: never retried, hand back the original outcome.
                        return Ok(resp);
                    }
                    let wait = retry_after_or_backoff(
                        &resp,
                        self.retry_policy.backoff(attempt),
                        self.retry_policy.max_retry_after(),
                    );
                    // Release the connection back to the pool before sleeping so a long
                    // Retry-After cannot exhaust the pool.
                    drop(resp);
                    tracing::info!(attempt, status = %status, ?wait, "retrying after response");
                    tokio::time::sleep(wait).await;
                }
                Err(err) => {
                    tracing::debug!(attempt, error = %err, "request errored");
                    let do_retry = method_retryable
                        && self
                            .retry_policy
                            .should_retry(attempt, RetryableOutcome::Error(&err));
                    if !do_retry {
                        return Err(err.into());
                    }
                    if attempt >= max_retries {
                        if attempt > 0 {
                            // Preserve the final transport error as the cause so
                            // callers can see *why* retries were exhausted.
                            return Err(Error::MaxRetriesExceeded {
                                attempts: attempt + 1,
                                source: Some(err),
                            });
                        }
                        // max_retries == 0: never retried, hand back the original error.
                        return Err(err.into());
                    }
                    let wait = self.retry_policy.backoff(attempt);
                    tracing::info!(attempt, ?wait, error = %err, "retrying after error");
                    tokio::time::sleep(wait).await;
                }
            }
            attempt += 1;
        }
    }
}

/// Honors a `Retry-After` header on a response that is about to be retried.
///
/// This function is only called once the policy has decided to retry, so any
/// response reaching it should respect the server's explicit guidance. Both the
/// canonical carriers — `429 Too Many Requests` and `503 Service Unavailable` —
/// as well as any other retried status are covered (RFC 9110 §10.2.3).
///
/// Supports the delay-seconds form and the HTTP-date form. The result is capped
/// at `cap` so a malicious header cannot pin the client. A missing or
/// unparseable value falls back to `default` (the configured backoff).
fn retry_after_or_backoff(resp: &Response, default: Duration, cap: Duration) -> Duration {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_retry_after)
        .map(|d| std::cmp::min(d, cap))
        .unwrap_or(default)
}

fn parse_retry_after(value: &str) -> Option<Duration> {
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    httpdate::parse_http_date(value).ok().map(|dt| {
        dt.duration_since(std::time::SystemTime::now())
            .unwrap_or_default()
    })
}

/// Builder for [`Client`].
#[derive(Default)]
pub struct ClientBuilder {
    inner: Option<reqwest::Client>,
    retry_policy: Option<Box<dyn RetryPolicy>>,
}

impl ClientBuilder {
    /// Creates a new builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Uses the provided `reqwest::Client` for the underlying requests.
    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.inner = Some(client);
        self
    }

    /// Uses the provided retry policy.
    pub fn with_retry_policy(mut self, policy: impl RetryPolicy + 'static) -> Self {
        self.retry_policy = Some(Box::new(policy));
        self
    }

    /// Builds the client.
    ///
    /// If no retry policy was set, uses [`DefaultRetryPolicy::builder().build()`].
    pub fn build(self) -> Client {
        let policy = self
            .retry_policy
            .unwrap_or_else(|| Box::new(DefaultRetryPolicy::builder().build()));
        Client {
            inner: self.inner.unwrap_or_default(),
            retry_policy: policy,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_retry_after_delay_seconds() {
        assert_eq!(parse_retry_after("0"), Some(Duration::ZERO));
        assert_eq!(parse_retry_after("120"), Some(Duration::from_secs(120)));
    }

    #[test]
    fn parse_retry_after_invalid_returns_none() {
        assert_eq!(parse_retry_after("not-a-date"), None);
        assert_eq!(parse_retry_after(""), None);
        assert_eq!(parse_retry_after("-1"), None);
    }

    #[test]
    fn parse_retry_after_http_date_future() {
        let future = std::time::SystemTime::now() + Duration::from_secs(10);
        let s = httpdate::fmt_http_date(future);
        let parsed = parse_retry_after(&s).expect("future HTTP-date must parse");
        assert!(parsed <= Duration::from_secs(10));
        // Allow minor clock skew.
        assert!(parsed > Duration::from_secs(7));
    }

    #[test]
    fn parse_retry_after_http_date_past_is_zero() {
        let past = std::time::SystemTime::now() - Duration::from_secs(60);
        let s = httpdate::fmt_http_date(past);
        assert_eq!(parse_retry_after(&s), Some(Duration::ZERO));
    }
}
