# rs-retryablehttp

[![CI](https://github.com/xmonader/rs-retryablehttp/actions/workflows/ci.yml/badge.svg)](https://github.com/xmonader/rs-retryablehttp/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/rs-retryablehttp.svg)](https://crates.io/crates/rs-retryablehttp)
[![docs.rs](https://docs.rs/rs-retryablehttp/badge.svg)](https://docs.rs/rs-retryablehttp)

A retrying HTTP client library for Rust, built on `reqwest`.

> Retries that respect idempotency and server backpressure by default.

## Overview

`rs-retryablehttp` wraps `reqwest` and adds configurable retry logic with
exponential backoff, jitter, and `Retry-After` support. Its defaults are chosen
to be safe out of the box: only idempotent methods are retried unless you opt
in, and a server's `Retry-After` is honored (and capped) so you cooperate with
rate limits instead of fighting them.

## Features

- **Configurable retry count** via `max_retries`.
- **Exponential backoff with full jitter** by default; override with `.backoff(min, max)`.
- **Retry conditions**: 5xx responses, 429 Too Many Requests, 408 Request Timeout, connect/timeout errors.
- **Retry-After support**: when a retried response (e.g. `429 Too Many Requests`
  or `503 Service Unavailable`) carries a `Retry-After` header (delay-seconds or
  HTTP-date), it is honored instead of the configured backoff. The value is capped
  at the policy's `max_wait` (or an explicit `max_retry_after`) so a malicious
  header cannot pin the client.
- **Method gating**: only idempotent methods are retried by default.
- **Custom policies**: implement the `RetryPolicy` trait.
- **Clear errors**: [`Error::MaxRetriesExceeded`] tells you retries were exhausted.
- **Tracing**: retry attempts and wait durations are emitted via the `tracing` crate.
- **Sensible defaults**: `Client::builder().build()` retries out of the box.

> The retrying entry point is `Client::execute(req)`. There are no per-method
> convenience builders, because a raw `reqwest::RequestBuilder::send()` cannot
> be intercepted for retries. Build a `reqwest::Request` and pass it to
> `execute`.

## Usage

### Basic (zero config — retries by default)

```rust
use rs_retryablehttp::Client;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::builder().build();

    let req = reqwest::Request::new(
        reqwest::Method::GET,
        "https://example.com/api/data".parse()?,
    );
    let resp = client.execute(req).await?;
    println!("status: {}", resp.status());
    Ok(())
}
```

### Custom policy with exponential backoff

```rust
use std::time::Duration;
use rs_retryablehttp::{Client, DefaultRetryPolicy};

let policy = DefaultRetryPolicy::builder()
    .max_retries(5)
    .backoff(Duration::from_millis(100), Duration::from_secs(30))
    .build();

let client = Client::builder()
    .with_client(reqwest::Client::new())
    .with_retry_policy(policy)
    .build();
```

### Opting POST into retries

`POST` is **not** retried by default because it is not idempotent. If your
endpoints are idempotent, opt them in explicitly:

```rust
use reqwest::Method;
use rs_retryablehttp::{Client, DefaultRetryPolicy};

let policy = DefaultRetryPolicy::builder()
    .max_retries(3)
    .retryable_methods(&[Method::GET, Method::POST])
    .build();

let client = Client::builder()
    .with_client(reqwest::Client::new())
    .with_retry_policy(policy)
    .build();
```

### Disabling retries

```rust
use rs_retryablehttp::{Client, NoRetry};

let client = Client::builder()
    .with_client(reqwest::Client::new())
    .with_retry_policy(NoRetry)
    .build();
```

### Handling errors

`execute` returns `Result<reqwest::Response, Error>`. A returned `Ok` is the
final HTTP response (which may still be a 4xx/5xx the policy chose not to retry —
check `resp.status()`). The `Err` variants tell you *why* the client gave up:

```rust
use rs_retryablehttp::{Client, Error};

# async fn run(client: Client, req: reqwest::Request) {
match client.execute(req).await {
    Ok(resp) => println!("got {}", resp.status()),
    Err(Error::MaxRetriesExceeded { attempts, source }) => {
        // `source` is the underlying transport error (timeout/connect) when the
        // failure was an error, or `None` when retries were exhausted on a
        // retryable response (e.g. persistent 5xx).
        match source {
            Some(cause) => eprintln!("gave up after {attempts} attempts: {cause}"),
            None => eprintln!("gave up after {attempts} attempts (server kept failing)"),
        }
    }
    Err(Error::BodyNotRewindable) => eprintln!("streaming body can't be retried"),
    Err(Error::Reqwest(e)) => eprintln!("request failed without retry: {e}"),
}
# }
```

### Custom RetryPolicy implementation

```rust
use std::time::Duration;
use rs_retryablehttp::{RetryPolicy, RetryableOutcome};

struct Policy;

impl RetryPolicy for Policy {
    fn max_retries(&self) -> u32 { 0 }
    fn should_retry(&self, _attempt: u32, _outcome: RetryableOutcome<'_>) -> bool { false }
    fn backoff(&self, _attempt: u32) -> Duration { Duration::ZERO }
    fn retryable_methods(&self) -> &[reqwest::Method] { &[] }
}
```

## API

### `Client`

Wraps a `reqwest::Client` and a `RetryPolicy`.

- `Client::new(reqwest::Client)` — default retry policy.
- `Client::with_policy(reqwest::Client, impl RetryPolicy)` — custom policy.
- `Client::builder()` → `ClientBuilder`.
- `client.execute(req: reqwest::Request) -> Result<reqwest::Response, Error>` — the
  only retrying entry point.
- `client.inner() -> &reqwest::Client` — access the underlying client for
  non-retried requests that share the same connection pool.

### `ClientBuilder`

- `with_client(reqwest::Client)` — inject a pre-configured `reqwest::Client`.
- `with_retry_policy(impl RetryPolicy)` — inject a custom policy.
- `build()` — returns `Client`; defaults to `DefaultRetryPolicy::builder().build()` if no policy is set.

### `RetryPolicy` trait

- `max_retries(&self) -> u32`
- `should_retry(&self, attempt: u32, outcome: RetryableOutcome<'_>) -> bool`
- `backoff(&self, attempt: u32) -> Duration`
- `retryable_methods(&self) -> &[Method]`

### `DefaultRetryPolicy`

Built via `DefaultRetryPolicy::builder()`:

| method | purpose |
|---|---|
| `max_retries(u32)` | cap on retry attempts (default 3) |
| `backoff(min, max)` | exponential backoff with full jitter |
| `check_retry(Fn)` | custom retry predicate |
| `retryable_methods(&[Method])` | methods eligible for retry |
| `max_retry_after(Duration)` | cap on server-supplied `Retry-After` (defaults to `max_wait`) |

Defaults:

- Max retries: **3**
- Backoff: **exponential, 100ms base to 30s cap, full jitter**
- Retry on: 5xx, 429, 408, connect/timeout errors
- Retryable methods: GET, HEAD, PUT, DELETE, OPTIONS, TRACE

### `Error`

- `Reqwest(reqwest::Error)` — error from the underlying request.
- `BodyNotRewindable` — request body cannot be re-sent on retry (e.g. a stream).
- `MaxRetriesExceeded { attempts: u32, source: Option<reqwest::Error> }` — all
  retry attempts were exhausted. `source` carries the final transport error when
  exhaustion was caused by an error (timeout/connect), and is `None` when caused
  by a retryable response (e.g. a persistent 5xx/429). Also reachable via
  `std::error::Error::source`.

## Observability

`rs-retryablehttp` emits `tracing` events for each attempt and retry:

- `DEBUG` when a request is sent and when a response/error is received.
- `INFO` when a retry is scheduled (includes attempt, status/error, and wait duration).

Enable a `tracing` subscriber in your application to see them.

## Testing

```bash
make test
```

The suite runs against a local mock server ([`wiremock`]) and covers the retry
conditions (5xx, 429, connect/timeout), `Retry-After` parsing and capping,
method gating, body rewinding, backoff bounds, and concurrent use.

[`wiremock`]: https://crates.io/crates/wiremock

## Dependencies

- `reqwest` 0.12, `thiserror`, `tokio`, `rand`, `tracing`, `httpdate`

## License

MIT
