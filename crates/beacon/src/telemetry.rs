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
pub(super) struct Report {
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

impl Report {
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
    query("INSERT INTO client_performance (account_id, window_id, sequence, report) VALUES ($1, $2, $3, $4::jsonb) ON CONFLICT (account_id, window_id) DO UPDATE SET sequence = EXCLUDED.sequence, report = EXCLUDED.report, updated_at = now() WHERE client_performance.sequence < EXCLUDED.sequence")
        .bind(&account).bind(&input.measurements.window_id).bind(input.measurements.sequence).bind(encoded).execute(&mut *transaction).await?;
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
    let rows: Vec<(String,)> = query_as("SELECT report::text FROM client_performance WHERE account_id = $1 AND updated_at > now() - interval '14 days' ORDER BY updated_at DESC LIMIT 20")
        .bind(account).fetch_all(&service.pool).await?;
    let reports: Vec<Value> = rows
        .into_iter()
        .filter_map(|(report,)| serde_json::from_str(&report).ok())
        .collect();
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({ "reports": reports })),
    )
        .into_response())
}
