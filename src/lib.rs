//! A retrying HTTP client for Rust, built on top of [`reqwest`].
//!
//! `rs-retryablehttp` wraps a `reqwest::Client` and adds configurable retry
//! logic with exponential backoff, full jitter, and `Retry-After` support.
//!
//! # Quick start
//!
//! ```no_run
//! use rs_retryablehttp::Client;
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let client = Client::builder().build();
//!
//! let req = reqwest::Request::new(
//!     reqwest::Method::GET,
//!     "https://example.com".parse()?,
//! );
//! let resp = client.execute(req).await?;
//! # Ok(())
//! # }
//! ```
//!
//! The retrying entry point is [`Client::execute`]. Build a `reqwest::Request`
//! and pass it in; the client will retry according to the configured policy.
//!
//! # Defaults
//!
//! - Up to 3 retries after the initial attempt.
//! - Exponential backoff with full jitter, 100ms base to 30s cap.
//! - Retries on 5xx, 429, 408, connect, and timeout errors. A `Retry-After` header
//!   on any retried response (e.g. 429 or 503) is honored in place of the
//!   computed backoff, capped so a hostile value cannot pin the client.
//! - Only idempotent methods (GET, HEAD, PUT, DELETE, OPTIONS, TRACE) are
//!   retried by default. POST/PATCH must be opted in explicitly.
//!
//! Override any of these with [`DefaultRetryPolicy::builder`] or by
//! implementing [`RetryPolicy`] yourself.

pub mod client;
pub mod error;
pub mod policy;

pub use client::{Client, ClientBuilder};
pub use error::Error;
pub use policy::{DefaultRetryPolicy, NoRetry, RetryPolicy, RetryableOutcome};
