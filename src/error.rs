use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("request failed: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("request body is not rewindable")]
    BodyNotRewindable,
    /// All retry attempts were exhausted.
    ///
    /// `source` carries the underlying transport error from the final attempt
    /// when exhaustion was caused by an error (timeout, connect, etc.). It is
    /// `None` when exhaustion was caused by a retryable *response* (e.g. a
    /// persistent 5xx or 429), since that path has no error to attach.
    #[error("max retries exceeded after {attempts} attempts")]
    MaxRetriesExceeded {
        attempts: u32,
        #[source]
        source: Option<reqwest::Error>,
    },
}
