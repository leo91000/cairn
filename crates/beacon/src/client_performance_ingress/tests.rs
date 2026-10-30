use super::*;
use axum::{Json, Router, http::HeaderMap, routing::post};
use serde::Deserialize;
use std::io::Write;

#[derive(Clone)]
struct LogWriter(Arc<Mutex<Vec<u8>>>);

impl Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Deserialize)]
struct Input {
    value: u64,
}

async fn upload(headers: HeaderMap, Json(input): Json<Input>) -> StatusCode {
    assert_eq!(input.value, 7);
    headers
        .get("x-test-status")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u16>().ok())
        .and_then(|status| StatusCode::from_u16(status).ok())
        .unwrap_or(StatusCode::NO_CONTENT)
}

#[tokio::test]
async fn observes_http_rejections_without_reading_payloads_or_changing_responses() {
    let output = Arc::new(Mutex::new(Vec::new()));
    let writer = output.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || LogWriter(writer.clone()))
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);
    let metrics = Metrics::new();
    let app = Router::new()
        .route(PATH, post(upload).get(|| async { StatusCode::NO_CONTENT }))
        .route("/unrelated", post(|| async { StatusCode::NO_CONTENT }))
        .layer(axum::extract::DefaultBodyLimit::max(64));
    let app = observe_with_metrics(app, metrics.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    let sentinel = "private-report-marker";
    for status in [204, 401, 403, 429, 503, 202] {
        let response = client
            .post(format!("{origin}{PATH}?ignored={sentinel}"))
            .header("authorization", sentinel)
            .header("x-test-status", status)
            .json(&serde_json::json!({ "value": 7, "ignored": sentinel }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status);
    }
    for (content_type, body, expected) in [
        ("application/json", "{".to_owned(), 400),
        ("application/json", "{\"value\":\"wrong\"}".to_owned(), 422),
        ("application/json", "x".repeat(1024), 413),
        ("text/plain", "{\"value\":7}".to_owned(), 415),
    ] {
        let response = client
            .post(format!("{origin}{PATH}"))
            .header("content-type", content_type)
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), expected);
    }
    assert_eq!(
        client
            .get(format!("{origin}{PATH}"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        client
            .post(format!("{origin}/unrelated"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    {
        let state = metrics.0.lock().unwrap();
        let window = &state.window;
        assert_eq!(state.active, 0);
        assert_eq!(window.started, 10);
        assert_eq!(window.completed, 10);
        assert_eq!(window.abandoned, 0);
        assert_eq!(window.accepted, 1);
        assert_eq!(window.invalid, 2);
        assert_eq!(window.unauthenticated, 1);
        assert_eq!(window.forbidden, 1);
        assert_eq!(window.too_large, 1);
        assert_eq!(window.unsupported, 1);
        assert_eq!(window.limited, 1);
        assert_eq!(window.server_error, 1);
        assert_eq!(window.other, 1);
    }
    metrics.report();
    let text = String::from_utf8(output.lock().unwrap().clone()).unwrap();
    assert!(text.contains("operation=\"client_performance_ingress\""));
    assert!(text.contains("started=10"));
    assert!(text.contains("completed=10"));
    assert!(!text.contains(sentinel));
    assert!(!text.contains(PATH));
    let (_, active, next) = metrics.take();
    assert_eq!(active, 0);
    assert_eq!(next.started, 0);
    assert_eq!(next.completed, 0);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn production_host_guards_are_observed_once_including_database_failures() {
    let pool = sqlx_postgres::PgPoolOptions::new()
        .connect_lazy("postgres://fixture:fixture@127.0.0.1:1/unused")
        .unwrap();
    pool.close().await;
    let hosts = crate::InstallationHosts::new("http://cairn.localhost", true).unwrap();
    let api = hosts.layer(Router::new().route(PATH, post(upload)), pool.clone());
    // Production adds a second host guard around the API and static routes.
    let server_routes = hosts.layer(api, pool);
    let metrics = Metrics::new();
    let app = observe_with_metrics(server_routes, metrics.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    for (host, expected) in [
        ("cairn.localhost", StatusCode::NO_CONTENT),
        ("wrong.example.test", StatusCode::MISDIRECTED_REQUEST),
        ("tenant.cairn.localhost", StatusCode::INTERNAL_SERVER_ERROR),
    ] {
        let response = client
            .post(format!("{origin}{PATH}"))
            .header("host", host)
            .json(&serde_json::json!({ "value": 7 }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    let (_, active, window) = metrics.take();
    assert_eq!(active, 0);
    assert_eq!(window.started, 3);
    assert_eq!(window.completed, 3);
    assert_eq!(window.accepted, 1);
    assert_eq!(window.server_error, 1);
    assert_eq!(window.other, 1);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn completion_and_abandonment_survive_interval_boundaries() {
    let metrics = Metrics::new();
    let completed = metrics.begin();
    let abandoned = metrics.begin();
    let (_, active, first) = metrics.take();
    assert_eq!(active, 2);
    assert_eq!(first.started, 2);
    assert_eq!(first.max_active, 2);
    assert_eq!(first.completed, 0);
    completed.complete(StatusCode::NO_CONTENT);
    drop(abandoned);
    let (_, active, next) = metrics.take();
    assert_eq!(active, 0);
    assert_eq!(next.started, 0);
    assert_eq!(next.completed, 1);
    assert_eq!(next.accepted, 1);
    assert_eq!(next.abandoned, 1);
    assert_eq!(next.max_active, 2);
}

#[tokio::test]
async fn concurrent_uploads_do_not_lose_counts_or_keep_the_reporter_alive() {
    let metrics = Metrics::new();
    let weak = Arc::downgrade(&metrics);
    let mut requests = Vec::new();
    for _ in 0..100 {
        let metrics = metrics.clone();
        requests.push(tokio::spawn(async move {
            let guard = metrics.begin();
            tokio::task::yield_now().await;
            guard.complete(StatusCode::NO_CONTENT);
        }));
    }
    for request in requests {
        request.await.unwrap();
    }
    let (_, active, window) = metrics.take();
    assert_eq!(active, 0);
    assert_eq!(window.started, 100);
    assert_eq!(window.completed, 100);
    assert_eq!(window.accepted, 100);
    assert_eq!(window.abandoned, 0);
    drop(metrics);
    assert!(weak.upgrade().is_none());
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "explicit performance-report counter overhead measurement"]
async fn measure_ingress_counter_overhead() {
    fn cpu_ns() -> u128 {
        let mut time = std::mem::MaybeUninit::<libc::timespec>::uninit();
        // SAFETY: the successful call initializes the supplied timespec.
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, time.as_mut_ptr()) },
            0
        );
        // SAFETY: clock_gettime returned success above.
        let time = unsafe { time.assume_init() };
        time.tv_sec as u128 * 1_000_000_000 + time.tv_nsec as u128
    }
    let metrics = Metrics::new();
    let requests = 100_000;
    let cpu = cpu_ns();
    let started = Instant::now();
    for _ in 0..requests {
        metrics
            .begin()
            .complete(std::hint::black_box(StatusCode::NO_CONTENT));
    }
    let elapsed_ns = started.elapsed().as_nanos();
    let cpu_ns = cpu_ns() - cpu;
    let (_, active, window) = metrics.take();
    assert_eq!(active, 0);
    assert_eq!(window.started, requests);
    assert_eq!(window.accepted, requests);
    println!(
        "{}",
        serde_json::json!({
            "benchmark": "client_performance_ingress_counters", "requests": requests,
            "elapsedNs": elapsed_ns, "cpuNs": cpu_ns,
            "meanCounterNs": elapsed_ns / u128::from(requests),
            "metricsBytes": std::mem::size_of::<Metrics>(),
            "includesHttp": false, "includesLogEncoding": false,
        })
    );
}
