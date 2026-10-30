//! Process-wide upload outcomes, including rejection before the JSON handler.
use axum::{
    Router,
    extract::{Request, State},
    http::{Method, StatusCode},
    middleware::{self, Next},
    response::Response,
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const PATH: &str = "/api/account/telemetry/performance";

/// Wrap the complete server once, after its final host/security layers.
pub fn observe_performance_uploads(router: Router) -> Router {
    observe_with_metrics(router, Metrics::new())
}

fn observe_with_metrics(router: Router, metrics: Arc<Metrics>) -> Router {
    router.layer(middleware::from_fn_with_state(metrics, observe))
}

#[derive(Default)]
struct Window {
    started: u64,
    completed: u64,
    abandoned: u64,
    max_active: u64,
    duration_us: u64,
    max_duration_us: u64,
    accepted: u64,
    invalid: u64,
    unauthenticated: u64,
    forbidden: u64,
    too_large: u64,
    unsupported: u64,
    limited: u64,
    server_error: u64,
    other: u64,
}

struct Counters {
    since: Instant,
    active: u64,
    window: Window,
}

struct Metrics(Mutex<Counters>);

impl Metrics {
    pub fn new() -> Arc<Self> {
        let metrics = Arc::new(Self(Mutex::new(Counters {
            since: Instant::now(),
            active: 0,
            window: Window::default(),
        })));
        let weak = Arc::downgrade(&metrics);
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(Duration::from_secs(30));
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            ticks.tick().await;
            loop {
                ticks.tick().await;
                let Some(metrics) = weak.upgrade() else {
                    return;
                };
                metrics.report();
            }
        });
        metrics
    }

    fn begin(self: &Arc<Self>) -> InFlight {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active = state.active.saturating_add(1);
        state.window.started = state.window.started.saturating_add(1);
        state.window.max_active = state.window.max_active.max(state.active);
        InFlight {
            metrics: self.clone(),
            since: Instant::now(),
            completed: false,
        }
    }

    fn finish(&self, status: Option<StatusCode>, elapsed: Duration) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active = state.active.saturating_sub(1);
        let window = &mut state.window;
        let micros = elapsed.as_micros().min(u128::from(u64::MAX)) as u64;
        window.duration_us = window.duration_us.saturating_add(micros);
        window.max_duration_us = window.max_duration_us.max(micros);
        let Some(status) = status else {
            window.abandoned = window.abandoned.saturating_add(1);
            return;
        };
        window.completed = window.completed.saturating_add(1);
        let count = match status {
            StatusCode::NO_CONTENT => &mut window.accepted,
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => &mut window.invalid,
            StatusCode::UNAUTHORIZED => &mut window.unauthenticated,
            StatusCode::FORBIDDEN => &mut window.forbidden,
            StatusCode::PAYLOAD_TOO_LARGE => &mut window.too_large,
            StatusCode::UNSUPPORTED_MEDIA_TYPE => &mut window.unsupported,
            StatusCode::TOO_MANY_REQUESTS => &mut window.limited,
            status if status.is_server_error() => &mut window.server_error,
            _ => &mut window.other,
        };
        *count = count.saturating_add(1);
    }

    fn take(&self) -> (u64, u64, Window) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let interval_ms = state.since.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        state.since = Instant::now();
        let active = state.active;
        let window = std::mem::replace(
            &mut state.window,
            Window {
                max_active: active,
                ..Window::default()
            },
        );
        (interval_ms, active, window)
    }

    fn report(&self) {
        let (interval_ms, active, window) = self.take();
        tracing::info!(
            target: "cairn_performance",
            operation = "client_performance_ingress",
            interval_ms,
            started = window.started,
            completed = window.completed,
            abandoned = window.abandoned,
            active,
            max_active = window.max_active,
            duration_us = window.duration_us,
            max_duration_us = window.max_duration_us,
            accepted = window.accepted,
            invalid = window.invalid,
            unauthenticated = window.unauthenticated,
            forbidden = window.forbidden,
            too_large = window.too_large,
            unsupported = window.unsupported,
            limited = window.limited,
            server_error = window.server_error,
            other = window.other,
        );
    }
}

struct InFlight {
    metrics: Arc<Metrics>,
    since: Instant,
    completed: bool,
}

impl InFlight {
    fn complete(mut self, status: StatusCode) {
        self.completed = true;
        self.metrics.finish(Some(status), self.since.elapsed());
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        if !self.completed {
            self.metrics.finish(None, self.since.elapsed());
        }
    }
}

async fn observe(State(metrics): State<Arc<Metrics>>, request: Request, next: Next) -> Response {
    if request.method() != Method::POST || request.uri().path() != PATH {
        return next.run(request).await;
    }
    let guard = metrics.begin();
    let response = next.run(request).await;
    guard.complete(response.status());
    response
}

#[cfg(test)]
mod tests;
