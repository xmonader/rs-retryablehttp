// Verifies the README "Custom RetryPolicy implementation" example compiles.
use rs_retryablehttp::{Client, RetryPolicy, RetryableOutcome};
use std::time::Duration;

struct Policy;

impl RetryPolicy for Policy {
    fn max_retries(&self) -> u32 {
        0
    }
    fn should_retry(&self, _attempt: u32, _outcome: RetryableOutcome<'_>) -> bool {
        false
    }
    fn backoff(&self, _attempt: u32) -> Duration {
        Duration::ZERO
    }
    fn retryable_methods(&self) -> &[reqwest::Method] {
        &[]
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::builder()
        .with_client(reqwest::Client::new())
        .with_retry_policy(Policy)
        .build();
    let _ = client;
    Ok(())
}
