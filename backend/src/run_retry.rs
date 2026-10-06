//! Classify confirmed agent failures and plan bounded, durable recovery.
//! Tool output and unconfirmed stream failures must not be passed to this policy.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_ATTEMPTS: u64 = 3;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureKind {
    Connection,
    Timeout,
    RateLimit,
    Server,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cause {
    pub kind: FailureKind,
    pub message: String,
}

impl Cause {
    /// Structured codes take precedence. Unknown errors remain terminal; the
    /// exact routing timeout is a fallback for adapters without an error code.
    pub fn classify(error: &Value) -> Option<Self> {
        let code = error["code"].as_str().unwrap_or("");
        let info = &error["codexErrorInfo"];
        let empty = Value::Null;
        let (info_code, details) = if let Some(code) = info.as_str() {
            (code, &empty)
        } else if let Some((code, details)) = info.as_object().and_then(|map| map.iter().next()) {
            (code.as_str(), details)
        } else {
            ("", &empty)
        };
        let status = error["httpStatusCode"]
            .as_u64()
            .or_else(|| details["httpStatusCode"].as_u64());
        if status.is_some_and(|status| (400..500).contains(&status) && !matches!(status, 408 | 429))
        {
            return None;
        }
        if [code, info_code].iter().any(|code| {
            matches!(
                *code,
                "authentication_error"
                    | "permission_denied"
                    | "invalid_request"
                    | "context_length_exceeded"
                    | "session_budget_exceeded"
                    | "usage_limit_reached"
                    | "usage_limit_exceeded"
                    | "usageLimitExceeded"
                    | "credit_balance_exhausted"
                    | "project_spend_limit_exceeded"
                    | "organization_spend_limit_exceeded"
                    | "organization_usage_limit_exceeded"
                    | "sandbox_error"
                    | "sandboxError"
                    | "authenticationError"
                    | "permissionDenied"
                    | "invalidRequest"
                    | "executor_version_incompatible"
                    | "cancelled"
                    | "interrupted"
                    | "turnCancelled"
                    | "contextWindowExceeded"
            )
        }) {
            return None;
        }

        let kind = [code, info_code]
            .iter()
            .find_map(|code| match *code {
                "connection_failed"
                | "httpConnectionFailed"
                | "responseStreamConnectionFailed"
                | "responseStreamDisconnected" => Some(FailureKind::Connection),
                "request_timeout" | "requestTimeout" => Some(FailureKind::Timeout),
                "rate_limit_exceeded" | "rateLimitExceeded" => Some(FailureKind::RateLimit),
                "server_error"
                | "internal_error"
                | "server_overloaded"
                | "server_is_overloaded"
                | "service_unavailable_error"
                | "internalServerError" => Some(FailureKind::Server),
                _ => None,
            })
            .or(match status {
                Some(408 | 504) => Some(FailureKind::Timeout),
                Some(429) => Some(FailureKind::RateLimit),
                Some(500 | 502 | 503) => Some(FailureKind::Server),
                _ => None,
            })
            .or_else(|| {
                let message = error["message"].as_str().unwrap_or("").trim();
                // Do not override an unrecognized structured error with message heuristics.
                (code.is_empty()
                    && info_code.is_empty()
                    && message.eq_ignore_ascii_case("workspace routing discovery timed out"))
                .then_some(FailureKind::Timeout)
            })?;
        let message = error["message"]
            .as_str()
            .unwrap_or("Temporary agent failure.")
            .to_owned();
        Some(Self { kind, message })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Retry {
    pub attempt: u64,
    pub limit: u64,
    pub next_attempt_at: Option<i64>,
    pub cause: Cause,
}

impl Retry {
    /// The planned timestamp is stored once, so restarting does not resample the
    /// delay or reset the attempt budget. Jitter spreads simultaneous failures.
    pub fn next(previous: Option<&Value>, cause: Cause, at: i64) -> Option<Self> {
        let attempt = previous
            .and_then(|retry| retry["attempt"].as_u64())
            .unwrap_or(0);
        if attempt >= MAX_ATTEMPTS {
            return None;
        }
        let base_ms = 30_000_i64 * (1 << attempt);
        let jitter_ms = rand::random_range(0..=base_ms / 5);
        Some(Self {
            attempt: attempt + 1,
            limit: MAX_ATTEMPTS,
            next_attempt_at: Some(at.saturating_add(base_ms + jitter_ms)),
            cause,
        })
    }

    pub fn waiting(run: &Value, at: i64) -> bool {
        run["retry"]["nextAttemptAt"]
            .as_i64()
            .is_some_and(|next| at < next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn known_transient_codes_and_the_reported_routing_error_are_retriable() {
        for error in [
            json!({ "message": "workspace routing discovery timed out" }),
            json!({ "code": "request_timeout", "message": "upstream stalled" }),
            json!({ "code": "server_overloaded", "message": "busy" }),
            json!({ "code": "connection_failed", "message": "network" }),
            json!({ "code": "rate_limit_exceeded", "message": "too many requests" }),
            json!({ "codexErrorInfo": { "httpConnectionFailed": { "httpStatusCode": 503 } } }),
        ] {
            assert!(Cause::classify(&error).is_some(), "{error}");
        }
    }

    #[test]
    fn terminal_errors_and_tool_messages_never_override_the_safety_guards() {
        for error in [
            json!({ "code": "invalid_request", "message": "workspace routing discovery timed out" }),
            json!({ "code": "authentication_error", "httpStatusCode": 503 }),
            json!({ "codexErrorInfo": "usageLimitExceeded" }),
            json!({ "codexErrorInfo": "contextWindowExceeded" }),
            json!({ "code": "sandbox_error" }),
            json!({ "code": "server_error", "httpStatusCode": 403 }),
            json!({ "message": "Command failed: workspace routing discovery timed out" }),
            json!({ "message": "Conversation interrupted." }),
            json!({ "message": "Unknown failure" }),
        ] {
            assert!(Cause::classify(&error).is_none(), "{error}");
        }
    }

    #[test]
    fn attempts_are_bounded_and_delays_survive_a_round_trip() {
        let cause = Cause::classify(&json!({ "message": "workspace routing discovery timed out" }))
            .unwrap();
        let mut previous = None;
        for attempt in 1..=3 {
            let retry = Retry::next(previous.as_ref(), cause.clone(), 1000).unwrap();
            assert_eq!(retry.attempt, attempt);
            let base = 30_000 * (1 << (attempt - 1));
            let due = retry.next_attempt_at.unwrap();
            assert!((1000 + base..=1000 + base + base / 5).contains(&due));
            previous = Some(serde_json::to_value(retry).unwrap());
            let run = json!({ "retry": previous });
            assert!(Retry::waiting(&run, due - 1));
            assert!(!Retry::waiting(&run, due));
        }
        assert!(Retry::next(previous.as_ref(), cause, 1000).is_none());
    }
}
