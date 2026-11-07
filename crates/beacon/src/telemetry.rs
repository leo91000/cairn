use super::*;
use serde::Serialize;
use std::collections::HashSet;

const SCREENS: &[&str] = &[
    "STARTUP", "FIL", "CHAT", "MISSIONS", "RUN", "ATELIER", "SETTINGS", "OTHER",
];

const OPERATIONS: &[&str] = &[
    "API_READ",
    "API_WRITE",
    "JSON_DECODE",
    "CACHE_READ",
    "CACHE_SAVE",
    "HISTORY_MERGE",
    "TIMELINE",
    "MARKDOWN",
    "LIVE_REDUCE",
    "LIVE_CATCHUP",
];

const MAX_DURATION_MS: f64 = 86_400_000.0;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct AndroidReport {
    schema: u32,
    app_version: String,
    build_type: String,
    manufacturer: String,
    model: String,
    android_api: u32,
    connection: String,
    heap_used_bytes: u64,
    heap_limit_bytes: u64,
    measurements: Measurements,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Measurements {
    window_id: String,
    sequence: i64,
    elapsed_ms: u64,
    operations: Vec<Timing>,
    frames: Vec<Frame>,
    recent_slow_samples: Vec<SlowSample>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Timing {
    operation: String,
    screen: String,
    count: u64,
    started_on_main_thread: u64,
    mean_ms: f64,
    max_ms: f64,
    p95_upper_bound_ms: f64,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Frame {
    screen: String,
    swiping: bool,
    count: u64,
    missed_deadline: u64,
    frozen: u64,
    dropped_reports: u64,
    max_ms: f64,
    max_ui_delay_ms: f64,
    max_layout_ms: f64,
    max_draw_ms: f64,
    max_gpu_ms: f64,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SlowSample {
    elapsed_ms: u64,
    screen: String,
    kind: String,
    duration_ms: f64,
    started_on_main_thread: bool,
    swiping: bool,
}

fn duration(value: f64) -> bool {
    value.is_finite() && (0.0..=MAX_DURATION_MS).contains(&value)
}

fn label(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}

impl AndroidReport {
    fn valid(&self) -> bool {
        let data = &self.measurements;
        if self.schema != 1
            || !label(&self.app_version, 64)
            || !label(&self.manufacturer, 128)
            || !label(&self.model, 128)
            || !matches!(self.build_type.as_str(), "debug" | "release")
            || !(26..=100).contains(&self.android_api)
            || !matches!(
                self.connection.as_str(),
                "NONE" | "OFFLINE" | "DIRECT" | "RELAY"
            )
            || self.heap_used_bytes > self.heap_limit_bytes
            || self.heap_limit_bytes > 1 << 40
            || uuid::Uuid::parse_str(&data.window_id).is_err()
            || data.sequence <= 0
            || data.operations.len() > 80
            || data.frames.len() > 16
            || data.recent_slow_samples.len() > 40
            || data.elapsed_ms > 365 * 86_400_000
        {
            return false;
        }
        let mut keys = HashSet::new();
        for stat in &data.operations {
            if !OPERATIONS.contains(&stat.operation.as_str())
                || !SCREENS.contains(&stat.screen.as_str())
                || !keys.insert((&stat.operation, &stat.screen))
                || stat.count == 0
                || stat.count > 1_000_000_000_000
                || stat.started_on_main_thread > stat.count
                || !duration(stat.mean_ms)
                || !duration(stat.max_ms)
                || !duration(stat.p95_upper_bound_ms)
                || stat.mean_ms > stat.max_ms + 0.000001
                || stat.p95_upper_bound_ms > stat.max_ms
            {
                return false;
            }
        }
        let mut keys = HashSet::new();
        for frame in &data.frames {
            if !SCREENS.contains(&frame.screen.as_str())
                || !keys.insert((&frame.screen, frame.swiping))
                || frame.count == 0
                || frame.count > 1_000_000_000_000
                || frame.missed_deadline > frame.count
                || frame.frozen > frame.count
                || ![
                    frame.max_ms,
                    frame.max_ui_delay_ms,
                    frame.max_layout_ms,
                    frame.max_draw_ms,
                    frame.max_gpu_ms,
                ]
                .into_iter()
                .all(duration)
            {
                return false;
            }
        }
        data.recent_slow_samples.iter().all(|sample| {
            SCREENS.contains(&sample.screen.as_str())
                && (sample.kind == "FRAME" || OPERATIONS.contains(&sample.kind.as_str()))
                && sample.elapsed_ms <= data.elapsed_ms
                && duration(sample.duration_ms)
        })
    }
}

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
pub(super) enum Report {
    Android(AndroidReport),
    Web(WebReport),
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct WebReport {
    schema: u32,
    platform: String,
    build_id: String,
    build_type: String,
    browser: String,
    connection: String,
    measurements: WebMeasurements,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WebMeasurements {
    window_id: String,
    sequence: i64,
    elapsed_ms: u64,
    foreground_ms: u64,
    operations: Vec<Timing>,
    api_failures: Vec<ApiFailures>,
    in_flight: Vec<InFlight>,
    omitted_in_flight: u64,
    event_loop: Vec<EventLoop>,
    long_task_supported: bool,
    long_tasks: Vec<LongTasks>,
    recent_slow_samples: Vec<WebSlowSample>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ApiFailures {
    operation: String,
    screen: String,
    errors: u64,
    cancelled: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InFlight {
    operation: String,
    screen: String,
    count: u64,
    max_elapsed_ms: f64,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EventLoop {
    screen: String,
    reason: String,
    count: u64,
    late: u64,
    mean_delay_ms: f64,
    max_delay_ms: f64,
    p95_upper_bound_delay_ms: f64,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LongTasks {
    screen: String,
    count: u64,
    total_ms: f64,
    max_ms: f64,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WebSlowSample {
    elapsed_ms: u64,
    screen: String,
    kind: String,
    duration_ms: f64,
    outcome: String,
}

fn count(value: u64) -> bool {
    (1..=1_000_000_000_000).contains(&value)
}

fn timing(mean: f64, maximum: f64, p95: f64) -> bool {
    [mean, maximum, p95].into_iter().all(duration) && mean <= maximum + 0.000001 && p95 <= maximum
}

impl WebReport {
    fn valid(&self) -> bool {
        let data = &self.measurements;
        let valid_build = (self.build_id.len() == 64
            && self
                .build_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
            || (self.build_type == "debug" && self.build_id == "development");
        if self.schema != 2
            || self.platform != "web"
            || !valid_build
            || !matches!(self.build_type.as_str(), "debug" | "release")
            || !matches!(
                self.browser.as_str(),
                "CHROME" | "EDGE" | "FIREFOX" | "SAFARI" | "OTHER"
            )
            || !matches!(
                self.connection.as_str(),
                "NONE" | "OFFLINE" | "DIRECT" | "RELAY"
            )
            || uuid::Uuid::parse_str(&data.window_id).is_err()
            || data.sequence <= 0
            || data.elapsed_ms > 365 * 86_400_000
            || data.foreground_ms > data.elapsed_ms
            || data.operations.len() > 80
            || data.api_failures.len() > 16
            || data.in_flight.len() > 64
            || data.omitted_in_flight > 1_000_000_000_000
            || data.event_loop.len() > 24
            || data.long_tasks.len() > 8
            || (!data.long_task_supported && !data.long_tasks.is_empty())
            || data.recent_slow_samples.len() > 40
        {
            return false;
        }

        let mut keys = HashSet::new();
        for stat in &data.operations {
            if !OPERATIONS.contains(&stat.operation.as_str())
                || !SCREENS.contains(&stat.screen.as_str())
                || !keys.insert((&stat.operation, &stat.screen))
                || !count(stat.count)
                || stat.started_on_main_thread != stat.count
                || !timing(stat.mean_ms, stat.max_ms, stat.p95_upper_bound_ms)
            {
                return false;
            }
        }

        let mut keys = HashSet::new();
        for stat in &data.api_failures {
            let operation = data
                .operations
                .iter()
                .find(|row| row.operation == stat.operation && row.screen == stat.screen);
            let valid_failures = stat
                .errors
                .checked_add(stat.cancelled)
                .is_some_and(|failures| {
                    failures > 0 && operation.is_some_and(|row| failures <= row.count)
                });
            if !matches!(stat.operation.as_str(), "API_READ" | "API_WRITE")
                || !keys.insert((&stat.operation, &stat.screen))
                || !valid_failures
            {
                return false;
            }
        }

        let mut keys = HashSet::new();
        let mut pending = 0_u64;
        for stat in &data.in_flight {
            if !OPERATIONS.contains(&stat.operation.as_str())
                || !SCREENS.contains(&stat.screen.as_str())
                || !keys.insert((&stat.operation, &stat.screen))
                || !(1..=64).contains(&stat.count)
                || !duration(stat.max_elapsed_ms)
            {
                return false;
            }
            pending += stat.count;
        }
        if pending > 64 {
            return false;
        }

        let mut keys = HashSet::new();
        for stat in &data.event_loop {
            if !SCREENS.contains(&stat.screen.as_str())
                || !matches!(stat.reason.as_str(), "INTERACTION" | "API" | "NAVIGATION")
                || !keys.insert((&stat.screen, &stat.reason))
                || !count(stat.count)
                || stat.late > stat.count
                || !timing(
                    stat.mean_delay_ms,
                    stat.max_delay_ms,
                    stat.p95_upper_bound_delay_ms,
                )
            {
                return false;
            }
        }

        let mut keys = HashSet::new();
        for stat in &data.long_tasks {
            if !SCREENS.contains(&stat.screen.as_str())
                || !keys.insert(&stat.screen)
                || !count(stat.count)
                || !duration(stat.max_ms)
                || stat.max_ms < 50.0
                || !stat.total_ms.is_finite()
                || stat.total_ms < stat.max_ms
                || stat.total_ms > MAX_DURATION_MS * stat.count as f64
            {
                return false;
            }
        }
        data.recent_slow_samples.iter().all(|sample| {
            SCREENS.contains(&sample.screen.as_str())
                && (OPERATIONS.contains(&sample.kind.as_str())
                    || matches!(sample.kind.as_str(), "EVENT_LOOP_DELAY" | "LONG_TASK"))
                && sample.elapsed_ms <= data.elapsed_ms
                && duration(sample.duration_ms)
                && sample.duration_ms >= 50.0
                && matches!(sample.outcome.as_str(), "ok" | "error" | "cancelled")
        })
    }
}

impl Report {
    fn valid(&self) -> bool {
        match self {
            Self::Android(report) => report.valid(),
            Self::Web(report) => report.valid(),
        }
    }

    fn window(&self) -> (&str, i64) {
        match self {
            Self::Android(report) => (&report.measurements.window_id, report.measurements.sequence),
            Self::Web(report) => (&report.measurements.window_id, report.measurements.sequence),
        }
    }
}

pub(super) async fn receive(
    State(service): State<Service>,
    headers: HeaderMap,
    Json(input): Json<Report>,
) -> Result<StatusCode, ApiError> {
    let (account, _) = methods::authenticated(&service, &headers, true).await?;
    if !input.valid() {
        return Err(ApiError::Http(
            StatusCode::BAD_REQUEST,
            "Invalid performance report",
        ));
    }
    let encoded = serde_json::to_string(&input)
        .map_err(|_| ApiError::Http(StatusCode::BAD_REQUEST, "Invalid performance report"))?;
    let mut transaction = service.pool.begin().await?;
    // This per-account row also serializes window retention across concurrent uploads.
    consume_limit_on(&mut transaction, &format!("performance:{account}"), 6, 60).await?;
    let (window_id, sequence) = input.window();
    query("INSERT INTO client_performance (account_id, window_id, sequence, report) VALUES ($1, $2, $3, $4::jsonb) ON CONFLICT (account_id, window_id) DO UPDATE SET sequence = EXCLUDED.sequence, report = EXCLUDED.report, updated_at = now() WHERE client_performance.sequence < EXCLUDED.sequence")
        .bind(&account).bind(window_id).bind(sequence).bind(encoded).execute(&mut *transaction).await?;
    query("DELETE FROM client_performance WHERE account_id = $1 AND (updated_at <= now() - interval '14 days' OR window_id IN (SELECT window_id FROM client_performance WHERE account_id = $1 ORDER BY updated_at DESC, window_id LIMIT ALL OFFSET 256))")
        .bind(&account).execute(&mut *transaction).await?;
    transaction.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn list(
    State(service): State<Service>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (account, _) = methods::authenticated(&service, &headers, false).await?;
    let rows: Vec<(String, i64)> = query_as("SELECT report::text, floor(extract(epoch FROM updated_at) * 1000)::bigint FROM client_performance WHERE account_id = $1 AND updated_at > now() - interval '14 days' ORDER BY updated_at DESC LIMIT 20")
        .bind(account).fetch_all(&service.pool).await?;
    let reports: Vec<Value> = rows
        .into_iter()
        .filter_map(|(report, received_at)| {
            let mut report: Value = serde_json::from_str(&report).ok()?;
            report
                .as_object_mut()?
                .insert("receivedAt".into(), received_at.into());
            Some(report)
        })
        .collect();
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({ "reports": reports })),
    )
        .into_response())
}
