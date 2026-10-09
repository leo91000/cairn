//! Guest-local native transport timings. Raw OTLP attributes never enter logs.
//! Codex batches export; use event time/duration rather than HTTP arrival time.
use crate::error::Result;
use axum::{Json, Router, body::Bytes, extract::DefaultBodyLimit, http::StatusCode, routing::post};
use serde_json::{Value, json};
use tokio::{net::TcpListener, task::JoinHandle};
use tracing::Instrument;

pub(crate) struct Collector {
    endpoint: String,
    task: JoinHandle<()>,
}

impl Collector {
    pub(crate) async fn start() -> Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let endpoint = format!("http://{}/v1/logs", listener.local_addr()?);
        let application = Router::new()
            .route("/v1/logs", post(receive))
            .route("/v1/traces", post(receive))
            .layer(DefaultBodyLimit::max(2 * 1024 * 1024));
        let task = tokio::spawn(
            async move {
                let _ = axum::serve(listener, application).await;
            }
            .instrument(tracing::Span::current()),
        );
        Ok(Self { endpoint, task })
    }

    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

/// Guest collectors outlive attempt output leases. Host sessions own a collector
/// themselves; only an explicit local guest destination can replace that owner.
pub(crate) fn guest_endpoint() -> Option<String> {
    let endpoint = std::env::var("CAIRN_CODEX_TIMING_ENDPOINT").ok()?;
    local_endpoint(&endpoint).then_some(endpoint)
}

fn local_endpoint(endpoint: &str) -> bool {
    let Ok(url) = url::Url::parse(endpoint) else {
        return false;
    };
    url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.port().is_some_and(|port| port > 0)
        && url.path() == "/v1/logs"
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}

pub(crate) fn arguments(endpoint: &str) -> [String; 6] {
    [
        "-c".into(),
        "otel.log_user_prompt=false".into(),
        "-c".into(),
        format!(
            "otel.exporter={{otlp-http={{endpoint=\"{}\",protocol=\"json\"}}}}",
            endpoint
        ),
        "-c".into(),
        format!(
            "otel.trace_exporter={{otlp-http={{endpoint=\"{}\",protocol=\"json\"}}}}",
            endpoint.replace("/v1/logs", "/v1/traces")
        ),
    ]
}

impl Drop for Collector {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn receive(bytes: Bytes) -> std::result::Result<Json<Value>, StatusCode> {
    let Ok(document) = serde_json::from_slice::<Value>(&bytes) else {
        return Err(StatusCode::BAD_REQUEST);
    };
    let mut records = 0_u64;
    let mut timings = 0_u64;
    for resource in array(&document, "resourceLogs") {
        for scope in array(resource, "scopeLogs") {
            for record in array(scope, "logRecords") {
                records += 1;
                if let Some(timing) = Timing::parse(record) {
                    timings += 1;
                    timing.record();
                }
            }
        }
    }
    for resource in array(&document, "resourceSpans") {
        for scope in array(resource, "scopeSpans") {
            for span in array(scope, "spans") {
                records += 1;
                if let Some(timing) = Timing::parse_span(span) {
                    timings += 1;
                    timing.record();
                }
            }
        }
    }
    tracing::debug!(target: "cairn_performance", operation = "codex_telemetry", event = "batch",
        bytes = bytes.len(), records, timings);
    Ok(Json(json!({})))
}

fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value[key].as_array().map_or(&[], Vec::as_slice)
}

fn attribute<'a>(record: &'a Value, key: &str) -> Option<&'a Value> {
    array(record, "attributes")
        .iter()
        .find(|attribute| attribute["key"] == key)
        .map(|attribute| &attribute["value"])
}

fn text<'a>(record: &'a Value, key: &str) -> Option<&'a str> {
    attribute(record, key)?["stringValue"].as_str()
}

fn numeric(record: &Value, key: &str) -> Option<f64> {
    let value = attribute(record, key)?;
    let value = value["doubleValue"]
        .as_f64()
        .or_else(|| value["intValue"].as_f64())
        .or_else(|| value["intValue"].as_str()?.parse().ok())
        .or_else(|| value["stringValue"].as_str()?.parse().ok())?;
    (value.is_finite() && (0.0..=3_600_000.0).contains(&value)).then_some(value)
}

fn milliseconds(value: &Value) -> Option<u64> {
    let nanoseconds = value.as_str()?.parse::<u64>().ok()?;
    (nanoseconds > 0).then_some(nanoseconds / 1_000_000)
}

struct Timing<'a> {
    event: &'static str,
    endpoint: &'static str,
    thread: &'a str,
    completed_at_ms: u64,
    duration_ms: f64,
    status: Option<u16>,
}

impl<'a> Timing<'a> {
    fn parse(record: &'a Value) -> Option<Self> {
        let event = match text(record, "event.name")? {
            "codex.api_request" => "http_request",
            "codex.websocket_request" | "codex.websocket.request" => "websocket_request",
            "codex.websocket_connect" => "websocket_connect",
            "codex.startup_phase" => match text(record, "startup.phase")? {
                "thread_start_create_thread" => "thread_create",
                "thread_start_total" => "thread_start",
                "startup_prewarm_create_turn_context" => "startup_prewarm_create_turn_context",
                "startup_prewarm_build_tools" => "startup_prewarm_build_tools",
                "startup_prewarm_build_prompt" => "startup_prewarm_build_prompt",
                "startup_prewarm_websocket_warmup" => "startup_prewarm_websocket_warmup",
                "startup_prewarm_resolve" => "startup_prewarm_resolve",
                "startup_prewarm_total" => "startup_prewarm_total",
                _ => return None,
            },
            _ => return None,
        };
        let endpoint = match text(record, "endpoint") {
            Some(endpoint) => endpoint_kind(endpoint),
            None => "none",
        };
        let duration_ms = numeric(record, "duration_ms")?;
        let completed_at_ms = milliseconds(&record["timeUnixNano"])
            .or_else(|| milliseconds(&record["observedTimeUnixNano"]))?;
        let thread = crate::performance::identity(text(record, "conversation.id").unwrap_or(""));
        let status = numeric(record, "http.response.status_code")
            .filter(|status| (100.0..=599.0).contains(status) && status.fract() == 0.0)
            .map(|status| status as u16);
        Some(Self {
            event,
            endpoint,
            thread,
            completed_at_ms,
            duration_ms,
            status,
        })
    }

    fn parse_span(span: &'a Value) -> Option<Self> {
        let event = match span["name"].as_str()? {
            "model_client.stream_responses_websocket" => {
                match attribute(span, "websocket.warmup")?["boolValue"].as_bool()? {
                    true => "websocket_warmup_setup",
                    false => "websocket_inference_setup",
                }
            }
            "model_client.websocket_connection" => "websocket_connection",
            // The native worker retains this span until the response stream
            // completes. Unlike setup, it includes waiting for the peer.
            "responses_websocket.stream_request" => "websocket_response_stream",
            _ => return None,
        };
        let started_at_ms = milliseconds(&span["startTimeUnixNano"])?;
        let completed_at_ms = milliseconds(&span["endTimeUnixNano"])?;
        let duration_ms = completed_at_ms.checked_sub(started_at_ms)?;
        if duration_ms > 3_600_000 {
            return None;
        }
        Some(Self {
            event,
            endpoint: "responses",
            thread: crate::performance::identity(text(span, "conversation.id").unwrap_or("")),
            completed_at_ms,
            duration_ms: duration_ms as f64,
            status: None,
        })
    }

    fn record(&self) {
        tracing::info!(
            target: "cairn_performance",
            operation = "codex_transport",
            event = self.event,
            endpoint = self.endpoint,
            thread_id = self.thread,
            completed_at_ms = self.completed_at_ms,
            request_started_at_ms = self.completed_at_ms.saturating_sub(self.duration_ms as u64),
            duration_ms = self.duration_ms,
            status = self.status
        );
    }
}

fn endpoint_kind(endpoint: &str) -> &'static str {
    // Never emit an URL, query string or a native error containing credentials.
    if matches!(endpoint, "responses" | "models" | "requirements") {
        return match endpoint {
            "responses" => "responses",
            "models" => "models",
            _ => "requirements",
        };
    }
    let Ok(endpoint) = url::Url::parse(endpoint)
        .or_else(|_| url::Url::parse("http://native.invalid")?.join(endpoint))
    else {
        return "other";
    };
    match endpoint.path().trim_end_matches('/').rsplit('/').next() {
        Some("responses") => "responses",
        Some("models") => "models",
        Some("requirements") => "requirements",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn guest_export_is_restricted_to_the_local_collector() {
        assert!(local_endpoint("http://127.0.0.1:4319/v1/logs"));
        for endpoint in [
            "https://127.0.0.1:4319/v1/logs",
            "http://example.invalid:4319/v1/logs",
            "http://127.0.0.1:0/v1/logs",
            "http://127.0.0.1:4319/other",
            "http://token@127.0.0.1:4319/v1/logs",
            "http://127.0.0.1:4319/v1/logs?token=private",
            "http://127.0.0.1:4319/v1/logs#private",
        ] {
            assert!(!local_endpoint(endpoint));
        }
    }

    #[test]
    fn native_endpoint_paths_keep_only_the_fixed_category() {
        assert_eq!(endpoint_kind("/responses"), "responses");
        assert_eq!(
            endpoint_kind("/backend-api/codex/models?client_version=0.159.3"),
            "models"
        );
        assert_eq!(
            endpoint_kind("https://example.invalid/responses?token=private"),
            "responses"
        );
        assert_eq!(endpoint_kind("/unknown/private"), "other");
    }

    #[test]
    fn native_batch_time_is_not_request_time_and_secrets_are_discarded() {
        let record = json!({
            "timeUnixNano": "0",
            "observedTimeUnixNano": "1790865467117810705",
            "attributes": [
                {"key":"event.name","value":{"stringValue":"codex.api_request"}},
                {"key":"duration_ms","value":{"stringValue":"8"}},
                {"key":"http.response.status_code","value":{"intValue":"200"}},
                {"key":"endpoint","value":{"stringValue":"https://example.invalid/responses?secret=discard"}},
                {"key":"conversation.id","value":{"stringValue":"not-an-identity"}},
                {"key":"prompt","value":{"stringValue":"private prompt"}},
                {"key":"error.message","value":{"stringValue":"private token"}}
            ]
        });
        let timing = Timing::parse(&record).unwrap();
        assert_eq!(timing.completed_at_ms, 1_790_865_467_117);
        assert_eq!(timing.duration_ms, 8.0);
        assert_eq!(timing.endpoint, "responses");
        assert_eq!(timing.thread, "unknown");
        assert_eq!(timing.status, Some(200));
    }

    #[test]
    fn token_stream_and_invalid_durations_do_not_generate_timings() {
        for (event, duration) in [
            ("codex.sse_event", "1"),
            ("codex.tool_result", "1"),
            ("codex.api_request", "NaN"),
            ("codex.api_request", "inf"),
            ("codex.api_request", "-1"),
        ] {
            let record = json!({
                "observedTimeUnixNano":"1790865467117810705",
                "attributes":[
                    {"key":"event.name","value":{"stringValue":event}},
                    {"key":"duration_ms","value":{"stringValue":duration}}
                ]
            });
            assert!(Timing::parse(&record).is_none());
        }
    }

    #[test]
    fn native_startup_phases_are_bounded_and_explicitly_allowed() {
        let mut record = json!({
            "timeUnixNano": "1790865467300000000",
            "attributes": [
                {"key":"event.name","value":{"stringValue":"codex.startup_phase"}},
                {"key":"startup.phase","value":{"stringValue":"startup_prewarm_websocket_warmup"}},
                {"key":"duration_ms","value":{"stringValue":"300"}}
            ]
        });
        let timing = Timing::parse(&record).unwrap();
        assert_eq!(timing.event, "startup_prewarm_websocket_warmup");
        assert_eq!(timing.duration_ms, 300.0);
        record["attributes"][1]["value"]["stringValue"] = "private_phase".into();
        assert!(Timing::parse(&record).is_none());
    }

    #[test]
    fn websocket_spans_distinguish_warmup_without_exporting_attributes() {
        let mut span = json!({
            "name": "model_client.stream_responses_websocket",
            "startTimeUnixNano": "1790865467000000000",
            "endTimeUnixNano": "1790865467300000000",
            "attributes": [
                {"key":"websocket.warmup","value":{"boolValue":true}},
                {"key":"prompt","value":{"stringValue":"private prompt"}},
                {"key":"api.path","value":{"stringValue":"https://private.invalid/?token=private"}}
            ]
        });
        let timing = Timing::parse_span(&span).unwrap();
        assert_eq!(timing.event, "websocket_warmup_setup");
        assert_eq!(timing.duration_ms, 300.0);
        assert_eq!(timing.endpoint, "responses");
        assert_eq!(timing.thread, "unknown");
        span["attributes"][0]["value"]["boolValue"] = false.into();
        assert_eq!(
            Timing::parse_span(&span).unwrap().event,
            "websocket_inference_setup"
        );
        span["name"] = "responses_websocket.stream_request".into();
        span["attributes"] = json!([]);
        let stream = Timing::parse_span(&span).unwrap();
        assert_eq!(stream.event, "websocket_response_stream");
        assert_eq!(stream.duration_ms, 300.0);
        assert_eq!(stream.endpoint, "responses");
        span["endTimeUnixNano"] = "1790865466000000000".into();
        assert!(Timing::parse_span(&span).is_none());
        span["name"] = "private_tool_call".into();
        assert!(Timing::parse_span(&span).is_none());
    }
}
