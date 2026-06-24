use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use reqwest::{Method, Request, StatusCode, Url};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use rs_retryablehttp::{Client, DefaultRetryPolicy, Error, RetryPolicy, RetryableOutcome};

fn test_url(server: &MockServer) -> Url {
    format!("{}/", server.uri()).parse().unwrap()
}

fn persistent_500(
    counter: Arc<AtomicUsize>,
) -> impl Fn(&wiremock::Request) -> ResponseTemplate + Send + Sync + 'static {
    move |_: &wiremock::Request| {
        counter.fetch_add(1, Ordering::SeqCst);
        ResponseTemplate::new(500)
    }
}

#[tokio::test]
async fn successful_request_no_retry() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::builder()
        .with_client(reqwest::Client::new())
        .with_retry_policy(DefaultRetryPolicy::builder().max_retries(3).build())
        .build();
    let req = Request::new(Method::GET, test_url(&server));
    let resp = client.execute(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn retries_on_server_error_then_succeeds() {
    let server = MockServer::start().await;
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();

    Mock::given(method("GET"))
        .respond_with(move |_: &wiremock::Request| {
            let n = counter_clone.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                ResponseTemplate::new(500)
            } else {
                ResponseTemplate::new(200)
            }
        })
        .expect(2)
        .mount(&server)
        .await;

    let client = Client::builder()
        .with_client(reqwest::Client::new())
        .with_retry_policy(
            DefaultRetryPolicy::builder()
                .max_retries(3)
                .backoff(Duration::from_millis(1), Duration::from_millis(100))
                .build(),
        )
        .build();
    let req = Request::new(Method::GET, test_url(&server));
    let resp = client.execute(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn max_retries_exceeded_returns_error() {
    let server = MockServer::start().await;
    let counter = Arc::new(AtomicUsize::new(0));

    Mock::given(method("GET"))
        .respond_with(persistent_500(counter.clone()))
        .expect(4)
        .mount(&server)
        .await;

    let client = Client::builder()
        .with_client(reqwest::Client::new())
        .with_retry_policy(
            DefaultRetryPolicy::builder()
                .max_retries(3)
                .backoff(Duration::from_millis(1), Duration::from_millis(100))
                .build(),
        )
        .build();
    let req = Request::new(Method::GET, test_url(&server));
    let err = client.execute(req).await.unwrap_err();

    assert_eq!(counter.load(Ordering::SeqCst), 4);
    assert!(matches!(err, Error::MaxRetriesExceeded { attempts: 4, .. }));
}

#[tokio::test]
async fn no_retry_on_client_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(400))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::builder()
        .with_client(reqwest::Client::new())
        .with_retry_policy(DefaultRetryPolicy::builder().max_retries(3).build())
        .build();
    let req = Request::new(Method::GET, test_url(&server));
    let resp = client.execute(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn retries_on_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(10)))
        .expect(3)
        .mount(&server)
        .await;

    let inner = reqwest::Client::builder()
        .timeout(Duration::from_millis(50))
        .build()
        .unwrap();
    let client = Client::builder()
        .with_client(inner)
        .with_retry_policy(
            DefaultRetryPolicy::builder()
                .max_retries(2)
                .backoff(Duration::from_millis(1), Duration::from_millis(100))
                .build(),
        )
        .build();
    let req = Request::new(Method::GET, test_url(&server));
    let err = client.execute(req).await.unwrap_err();

    assert!(matches!(err, Error::MaxRetriesExceeded { attempts: 3, .. }));
}

#[tokio::test]
async fn custom_policy_respected() {
    struct NoRetryPolicy;

    impl RetryPolicy for NoRetryPolicy {
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
            &[]
        }
    }

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::builder()
        .with_client(reqwest::Client::new())
        .with_retry_policy(NoRetryPolicy)
        .build();
    let req = Request::new(Method::GET, test_url(&server));
    let resp = client.execute(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn body_is_rewinded_on_retry() {
    let server = MockServer::start().await;
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();

    Mock::given(method("POST"))
        .and(path("/echo"))
        .respond_with(move |req: &wiremock::Request| {
            let n = counter_clone.fetch_add(1, Ordering::SeqCst);
            assert_eq!(req.body, b"hello");
            if n == 0 {
                ResponseTemplate::new(500)
            } else {
                ResponseTemplate::new(200)
            }
        })
        .expect(2)
        .mount(&server)
        .await;

    let client = Client::builder()
        .with_client(reqwest::Client::new())
        .with_retry_policy(
            DefaultRetryPolicy::builder()
                .max_retries(3)
                .retryable_methods(&[Method::POST]) // POST is not retryable by default; opt it in
                .backoff(Duration::from_millis(1), Duration::from_millis(100))
                .build(),
        )
        .build();
    let mut req = Request::new(
        Method::POST,
        format!("{}/echo", server.uri()).parse::<Url>().unwrap(),
    );
    *req.body_mut() = Some("hello".into());
    let resp = client.execute(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn post_not_retried_by_default() {
    let server = MockServer::start().await;
    let counter = Arc::new(AtomicUsize::new(0));

    Mock::given(method("POST"))
        .respond_with(persistent_500(counter.clone()))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::builder().build();
    let req = Request::new(
        Method::POST,
        format!("{}/", server.uri()).parse::<Url>().unwrap(),
    );
    let resp = client.execute(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

// Retry-count and method-gating edge cases.

#[tokio::test]
async fn default_builder_retries() {
    let server = MockServer::start().await;
    let counter = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .respond_with(persistent_500(counter.clone()))
        .expect(4)
        .mount(&server)
        .await;

    let client = Client::builder().build();
    let req = Request::new(Method::GET, test_url(&server));
    let err = client.execute(req).await.unwrap_err();

    assert!(matches!(err, Error::MaxRetriesExceeded { attempts: 4, .. }));
    assert_eq!(
        counter.load(Ordering::SeqCst),
        4,
        "default builder must retry up to default max"
    );
}

#[tokio::test]
async fn client_new_retries() {
    let server = MockServer::start().await;
    let counter = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .respond_with(persistent_500(counter.clone()))
        .expect(4)
        .mount(&server)
        .await;

    let client = Client::new(reqwest::Client::new());
    let req = Request::new(Method::GET, test_url(&server));
    let err = client.execute(req).await.unwrap_err();

    assert!(matches!(err, Error::MaxRetriesExceeded { attempts: 4, .. }));
    assert_eq!(counter.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn max_retries_above_3_is_honored() {
    let server = MockServer::start().await;
    let counter = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .respond_with(persistent_500(counter.clone()))
        .expect(6)
        .mount(&server)
        .await;

    let policy = DefaultRetryPolicy::builder()
        .max_retries(5)
        .backoff(Duration::from_millis(1), Duration::from_millis(10))
        .build();
    let client = Client::builder()
        .with_client(reqwest::Client::new())
        .with_retry_policy(policy)
        .build();
    let req = Request::new(Method::GET, test_url(&server));
    let err = client.execute(req).await.unwrap_err();

    assert!(matches!(err, Error::MaxRetriesExceeded { attempts: 6, .. }));
    assert_eq!(
        counter.load(Ordering::SeqCst),
        6,
        "max_retries=5 must yield 6 total attempts"
    );
}

#[tokio::test]
async fn non_retryable_method_not_retried() {
    let server = MockServer::start().await;
    let counter = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .respond_with(persistent_500(counter.clone()))
        .expect(1)
        .mount(&server)
        .await;

    let policy = DefaultRetryPolicy::builder()
        .max_retries(3)
        .retryable_methods(&[Method::GET, Method::HEAD]) // POST excluded
        .backoff(Duration::from_millis(1), Duration::from_millis(10))
        .build();
    let client = Client::builder()
        .with_client(reqwest::Client::new())
        .with_retry_policy(policy)
        .build();
    let req = Request::new(
        Method::POST,
        format!("{}/", server.uri()).parse::<Url>().unwrap(),
    );
    let resp = client.execute(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "POST must not be retried when excluded from retryable_methods"
    );
}

#[tokio::test]
async fn no_retry_exported_never_retries() {
    use rs_retryablehttp::NoRetry;
    let server = MockServer::start().await;
    let counter = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .respond_with(persistent_500(counter.clone()))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::builder()
        .with_client(reqwest::Client::new())
        .with_retry_policy(NoRetry)
        .build();
    let req = Request::new(Method::GET, test_url(&server));
    let resp = client.execute(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn streaming_body_is_not_rewindable() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let client = Client::builder()
        .with_client(reqwest::Client::new())
        .with_retry_policy(
            DefaultRetryPolicy::builder()
                .max_retries(3)
                .backoff(Duration::from_millis(1), Duration::from_millis(10))
                .build(),
        )
        .build();

    let stream = futures_util::stream::once(async { Ok::<_, std::io::Error>(b"hello".to_vec()) });
    let mut req = Request::new(Method::POST, test_url(&server));
    *req.body_mut() = Some(reqwest::Body::wrap_stream(stream));

    match client.execute(req).await {
        Err(Error::BodyNotRewindable) => {}
        other => panic!(
            "expected BodyNotRewindable, got {:?}",
            other.map(|r| r.status())
        ),
    }
}

// 429 / Retry-After handling and connection errors.

#[tokio::test]
async fn retries_on_429_too_many_requests() {
    let server = MockServer::start().await;
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    Mock::given(method("GET"))
        .respond_with(move |_: &wiremock::Request| {
            let n = counter_clone.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                ResponseTemplate::new(429)
            } else {
                ResponseTemplate::new(200)
            }
        })
        .expect(2)
        .mount(&server)
        .await;

    let client = Client::builder().build();
    let req = Request::new(Method::GET, test_url(&server));
    let resp = client.execute(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn retries_on_408_request_timeout() {
    let server = MockServer::start().await;
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    Mock::given(method("GET"))
        .respond_with(move |_: &wiremock::Request| {
            let n = counter_clone.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                ResponseTemplate::new(408)
            } else {
                ResponseTemplate::new(200)
            }
        })
        .expect(2)
        .mount(&server)
        .await;

    let client = Client::builder()
        .with_client(reqwest::Client::new())
        .with_retry_policy(
            DefaultRetryPolicy::builder()
                .max_retries(3)
                .backoff(Duration::from_millis(1), Duration::from_millis(10))
                .build(),
        )
        .build();
    let req = Request::new(Method::GET, test_url(&server));
    let resp = client.execute(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn honors_retry_after_delay_seconds() {
    let server = MockServer::start().await;
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    Mock::given(method("GET"))
        .respond_with(move |_: &wiremock::Request| {
            let n = counter_clone.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                ResponseTemplate::new(429).insert_header("retry-after", "0")
            } else {
                ResponseTemplate::new(200)
            }
        })
        .expect(2)
        .mount(&server)
        .await;

    let policy = DefaultRetryPolicy::builder()
        .max_retries(3)
        .backoff(Duration::from_secs(60), Duration::from_secs(120)) // would wait 60s otherwise
        .build();
    let client = Client::builder()
        .with_client(reqwest::Client::new())
        .with_retry_policy(policy)
        .build();

    let req = Request::new(Method::GET, test_url(&server));
    let resp = client.execute(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn honors_retry_after_on_503_not_just_429() {
    // A 503 carrying Retry-After should follow the server guidance rather than
    // the configured backoff; the default policy already retries it as a 5xx.
    let server = MockServer::start().await;
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    Mock::given(method("GET"))
        .respond_with(move |_: &wiremock::Request| {
            let n = counter_clone.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                ResponseTemplate::new(503).insert_header("retry-after", "0")
            } else {
                ResponseTemplate::new(200)
            }
        })
        .expect(2)
        .mount(&server)
        .await;

    // Backoff would wait 60s if Retry-After were ignored; the cap (120s) is well
    // above the header value, so honoring "0" is what keeps this test fast.
    let policy = DefaultRetryPolicy::builder()
        .max_retries(3)
        .backoff(Duration::from_secs(60), Duration::from_secs(120))
        .build();
    let client = Client::builder()
        .with_client(reqwest::Client::new())
        .with_retry_policy(policy)
        .build();

    let start = std::time::Instant::now();
    let req = Request::new(Method::GET, test_url(&server));
    let resp = client.execute(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(counter.load(Ordering::SeqCst), 2);
    assert!(
        start.elapsed() < Duration::from_secs(30),
        "503 Retry-After: 0 was ignored; fell back to 60s backoff"
    );
}

#[tokio::test]
async fn honors_retry_after_http_date() {
    use std::time::SystemTime;

    let server = MockServer::start().await;
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();

    // A Retry-After date 1 second in the future.
    let retry_after = httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(1));
    Mock::given(method("GET"))
        .respond_with(move |_: &wiremock::Request| {
            let n = counter_clone.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                ResponseTemplate::new(429).insert_header("retry-after", retry_after.as_str())
            } else {
                ResponseTemplate::new(200)
            }
        })
        .expect(2)
        .mount(&server)
        .await;

    let policy = DefaultRetryPolicy::builder()
        .max_retries(3)
        .backoff(Duration::from_secs(60), Duration::from_secs(120))
        .build();
    let client = Client::builder()
        .with_client(reqwest::Client::new())
        .with_retry_policy(policy)
        .build();

    let req = Request::new(Method::GET, test_url(&server));
    let resp = client.execute(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn retries_on_connect_error() {
    // Points at a closed port (ECONNREFUSED). The default policy retries on
    // is_connect(); with max_retries=2 that yields 3 attempts and then
    // MaxRetriesExceeded. If connect errors were not retried, we would get a
    // bare Error::Reqwest after a single attempt.
    let inner = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    let client = Client::builder()
        .with_client(inner)
        .with_retry_policy(
            DefaultRetryPolicy::builder()
                .max_retries(2)
                .backoff(Duration::from_millis(1), Duration::from_millis(10))
                .build(),
        )
        .build();

    let req = Request::new(Method::GET, "http://127.0.0.1:1/".parse::<Url>().unwrap());
    let err = client.execute(req).await.unwrap_err();
    assert!(
        matches!(err, Error::MaxRetriesExceeded { attempts: 3, .. }),
        "expected MaxRetriesExceeded after 3 connect-error attempts, got {err:?}"
    );
}

#[tokio::test]
async fn custom_check_retry_closure_respected() {
    let server = MockServer::start().await;
    let counter = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .respond_with(persistent_500(counter.clone()))
        .expect(1)
        .mount(&server)
        .await;

    let policy = DefaultRetryPolicy::builder()
        .max_retries(3)
        .check_retry(|_, _| false)
        .backoff(Duration::from_millis(1), Duration::from_millis(10))
        .build();
    let client = Client::builder()
        .with_client(reqwest::Client::new())
        .with_retry_policy(policy)
        .build();
    let req = Request::new(Method::GET, test_url(&server));
    let resp = client.execute(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn max_retries_zero_makes_one_attempt() {
    let server = MockServer::start().await;
    let counter = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .respond_with(persistent_500(counter.clone()))
        .expect(1)
        .mount(&server)
        .await;

    let policy = DefaultRetryPolicy::builder().max_retries(0).build();
    let client = Client::builder()
        .with_client(reqwest::Client::new())
        .with_retry_policy(policy)
        .build();
    let req = Request::new(Method::GET, test_url(&server));
    let resp = client.execute(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn max_retries_one_makes_two_attempts() {
    let server = MockServer::start().await;
    let counter = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .respond_with(persistent_500(counter.clone()))
        .expect(2)
        .mount(&server)
        .await;

    let policy = DefaultRetryPolicy::builder()
        .max_retries(1)
        .backoff(Duration::from_millis(1), Duration::from_millis(10))
        .build();
    let client = Client::builder()
        .with_client(reqwest::Client::new())
        .with_retry_policy(policy)
        .build();
    let req = Request::new(Method::GET, test_url(&server));
    let err = client.execute(req).await.unwrap_err();
    assert!(matches!(err, Error::MaxRetriesExceeded { attempts: 2, .. }));
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn client_is_send_sync_and_concurrently_usable() {
    // Static assertion that Client is Send + Sync (the compiler enforces this at the
    // trait-object boundary inside Client, but make it explicit).
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Client>();

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .expect(20)
        .mount(&server)
        .await;

    let client = std::sync::Arc::new(Client::builder().build());
    let url: Url = format!("{}/", server.uri()).parse().unwrap();

    let mut handles = Vec::new();
    for _ in 0..20 {
        let client = client.clone();
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            let req = Request::new(Method::GET, url);
            let resp = client.execute(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
}

#[tokio::test]
async fn inner_accessor_returns_underlying_client() {
    // The inner reqwest::Client shares its pool with the retrying client.
    let client = Client::builder().build();
    let _: &reqwest::Client = client.inner();
    // Smoke-test that the accessor type is correct and the client is usable directly.
    assert!(client.inner().get("http://127.0.0.1:1/").build().is_ok());
}

// Boundary behaviour around the final attempt.

#[tokio::test]
async fn success_on_final_allowed_attempt_is_returned() {
    // A success on the final allowed attempt must be returned, not turned into
    // MaxRetriesExceeded.
    let server = MockServer::start().await;
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    Mock::given(method("GET"))
        .respond_with(move |_: &wiremock::Request| {
            let i = counter_clone.fetch_add(1, Ordering::SeqCst);
            // 500, 500, 500, then 200 on the 4th call (attempt == max_retries).
            if i < 3 {
                ResponseTemplate::new(500)
            } else {
                ResponseTemplate::new(200)
            }
        })
        .expect(4)
        .mount(&server)
        .await;

    let client = Client::builder()
        .with_client(reqwest::Client::new())
        .with_retry_policy(
            DefaultRetryPolicy::builder()
                .max_retries(3)
                .backoff(Duration::from_millis(1), Duration::from_millis(5))
                .build(),
        )
        .build();
    let req = Request::new(Method::GET, test_url(&server));
    let resp = client
        .execute(req)
        .await
        .expect("final 200 must be returned");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(counter.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn oversized_retry_after_is_capped() {
    // A huge Retry-After value must be capped so it cannot pin the client.
    use wiremock::matchers::header;
    let server = MockServer::start().await;
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    Mock::given(method("GET"))
        .and(header("x-test", "1"))
        .respond_with(move |_: &wiremock::Request| {
            let i = counter_clone.fetch_add(1, Ordering::SeqCst);
            if i == 0 {
                ResponseTemplate::new(429).insert_header("retry-after", "999999")
            } else {
                ResponseTemplate::new(200)
            }
        })
        .expect(2)
        .mount(&server)
        .await;

    let client = Client::builder()
        .with_client(reqwest::Client::new())
        .with_retry_policy(
            DefaultRetryPolicy::builder()
                .max_retries(3)
                .backoff(Duration::from_millis(1), Duration::from_millis(5))
                .build(),
        )
        .build();

    let start = std::time::Instant::now();
    let mut req = Request::new(Method::GET, test_url(&server));
    req.headers_mut().insert("x-test", "1".parse().unwrap());
    let resp = client.execute(req).await.unwrap();
    let elapsed = start.elapsed();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(counter.load(Ordering::SeqCst), 2);
    // Cap is the configured max_wait (5ms). Allow generous slack for scheduling.
    assert!(
        elapsed < Duration::from_secs(5),
        "Retry-After=999999 was not capped; elapsed={elapsed:?}"
    );
}

// MaxRetriesExceeded should expose the underlying cause.

#[tokio::test]
async fn max_retries_exceeded_preserves_transport_error_cause() {
    use std::error::Error as StdError;

    // Connect failures exhaust on the *error* path, so the final reqwest::Error
    // must be preserved as the source.
    let inner = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    let client = Client::builder()
        .with_client(inner)
        .with_retry_policy(
            DefaultRetryPolicy::builder()
                .max_retries(2)
                .backoff(Duration::from_millis(1), Duration::from_millis(10))
                .build(),
        )
        .build();

    let req = Request::new(Method::GET, "http://127.0.0.1:1/".parse::<Url>().unwrap());
    let err = client.execute(req).await.unwrap_err();

    match err {
        Error::MaxRetriesExceeded {
            attempts: 3,
            source: Some(ref e),
        } => {
            assert!(
                e.is_connect(),
                "expected a connect error as the cause, got {e:?}"
            );
        }
        other => panic!("expected MaxRetriesExceeded with a source, got {other:?}"),
    }
    // The cause is also reachable through the std Error trait.
    assert!(
        err.source().is_some(),
        "source() must expose the underlying cause"
    );
}

#[tokio::test]
async fn max_retries_exceeded_on_response_has_no_cause() {
    // Persistent 500s exhaust on the *response* path; there is no transport
    // error, so source must be None.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(persistent_500(Arc::new(AtomicUsize::new(0))))
        .mount(&server)
        .await;

    let client = Client::builder()
        .with_client(reqwest::Client::new())
        .with_retry_policy(
            DefaultRetryPolicy::builder()
                .max_retries(2)
                .backoff(Duration::from_millis(1), Duration::from_millis(10))
                .build(),
        )
        .build();
    let req = Request::new(Method::GET, test_url(&server));
    let err = client.execute(req).await.unwrap_err();

    assert!(
        matches!(
            err,
            Error::MaxRetriesExceeded {
                attempts: 3,
                source: None
            }
        ),
        "response-path exhaustion must carry no source, got {err:?}"
    );
}
