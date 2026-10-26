//! Classify confirmed agent failures and plan bounded, durable recovery.
//! Tool output and unconfirmed stream failures must not be passed to this policy.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_ATTEMPTS: u64 = 5;
pub const MAX_RECOVERY_MS: i64 = 10 * 60 * 1000;

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
    /// The adapter confirmed that no new agent turn was submitted.
    #[serde(default)]
    pub startup: bool,
}

impl Cause {
    /// Structured codes take precedence. Unknown errors remain terminal; the
    /// exact routing timeout also covers Codex's generic `other` classification.
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

        let startup = error["recoveryPhase"] == "startup"
            && matches!(code, "cairn_startup_timeout" | "cairn_startup_connection");
        let kind = [code, info_code]
            .iter()
            .find_map(|code| match *code {
                "connection_failed"
                | "httpConnectionFailed"
                | "responseStreamConnectionFailed"
                | "responseStreamDisconnected" => Some(FailureKind::Connection),
                "request_timeout" | "requestTimeout" => Some(FailureKind::Timeout),
                "cairn_startup_timeout" if startup => Some(FailureKind::Timeout),
                "cairn_startup_connection" if startup => Some(FailureKind::Connection),
                "rate_limit_exceeded" | "rateLimitExceeded" => Some(FailureKind::RateLimit),
                "server_error"
                | "internal_error"
                | "server_overloaded"
                | "server_is_overloaded"
                | "service_unavailable_error"
                | "internalServerError"
                | "serverOverloaded" => Some(FailureKind::Server),
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
                // Codex exposes routing discovery I/O failures as `other`, and its
                // older event stream reports a model at capacity without a code.
                // Only these exact known messages may recover without a specific code.
                if !code.is_empty() || !matches!(info_code, "" | "other") {
                    None
                } else if message.eq_ignore_ascii_case("workspace routing discovery timed out") {
                    Some(FailureKind::Timeout)
                } else if message.eq_ignore_ascii_case(
                    "Selected model is at capacity. Please try a different model.",
                ) {
                    Some(FailureKind::Server)
                } else {
                    None
                }
            })?;
        let message = error["message"]
            .as_str()
            .unwrap_or("Temporary agent failure.")
            .to_owned();
        Some(Self {
            kind,
            message,
            startup,
        })
    }
}

/// Only an adapter still before turn submission may use this classification.
/// An uncertain turn/start or tool operation must not enter this path.
pub fn startup_failure(error: &crate::error::Error) -> Option<Value> {
    let timeout = error.status == 504
        && [
            "initialize",
            "account/login/start",
            "thread/resume",
            "thread/read",
            "thread/turns/list",
            "thread/items/list",
        ]
        .iter()
        .any(|method| error.message == format!("Codex {method} request timed out."));
    let disconnected = error.status == 503
        && error.message
            == "Codex disconnected before finishing the operation. Try again or resume the conversation.";
    if !timeout && !disconnected {
        return None;
    }
    Some(serde_json::json!({
        "type": "turn.failed",
        "error": {
            "code": if timeout { "cairn_startup_timeout" } else { "cairn_startup_connection" },
            "recoveryPhase": "startup",
            "message": error.message,
        },
    }))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Retry {
    pub attempt: u64,
    pub limit: u64,
    pub next_attempt_at: Option<i64>,
    #[serde(default)]
    pub started_at: Option<i64>,
    #[serde(default)]
    pub deadline_at: Option<i64>,
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
        let started_at = previous
            .and_then(|retry| retry["startedAt"].as_i64())
            .unwrap_or(at);
        let deadline_at = previous
            .and_then(|retry| retry["deadlineAt"].as_i64())
            .unwrap_or_else(|| started_at.saturating_add(MAX_RECOVERY_MS));
        let base_ms = 5_000_i64 * (1 << attempt);
        let jitter_ms = rand::random_range(0..=base_ms / 5);
        let next_attempt_at = at.saturating_add(base_ms + jitter_ms);
        if next_attempt_at >= deadline_at {
            return None;
        }
        Some(Self {
            attempt: attempt + 1,
            limit: MAX_ATTEMPTS,
            next_attempt_at: Some(next_attempt_at),
            started_at: Some(started_at),
            deadline_at: Some(deadline_at),
            cause,
        })
    }

    pub fn expired(run: &Value, at: i64) -> bool {
        run["retry"]["deadlineAt"]
            .as_i64()
            .is_some_and(|deadline| at >= deadline)
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
    fn startup_timeouts_are_explicit_and_never_include_turn_or_tool_submission() {
        for method in [
            "initialize",
            "account/login/start",
            "thread/resume",
            "thread/read",
            "thread/turns/list",
            "thread/items/list",
        ] {
            let error =
                crate::error::Error::gateway_timeout(format!("Codex {method} request timed out."));
            let event = startup_failure(&error).unwrap();
            let cause = Cause::classify(&event["error"]).unwrap();
            assert!(cause.startup);
            assert_eq!(cause.kind, FailureKind::Timeout);
        }
        for method in ["turn/start", "turn/steer", "command/exec", "unknown"] {
            let error =
                crate::error::Error::gateway_timeout(format!("Codex {method} request timed out."));
            assert!(startup_failure(&error).is_none());
        }
        for status in [400, 401, 403, 409, 500, 502] {
            assert!(
                startup_failure(&crate::error::Error::new(
                    status,
                    "Codex account/login/start request timed out."
                ))
                .is_none()
            );
        }
        assert!(
            Cause::classify(
                &serde_json::json!({ "code": "cairn_startup_timeout", "message": "unconfirmed" })
            )
            .is_none()
        );
        assert!(
            startup_failure(&crate::error::Error::unavailable(
                "Account authentication is unavailable."
            ))
            .is_none()
        );
    }

    #[test]
    fn the_recovery_window_survives_restart_and_does_not_schedule_after_its_deadline() {
        let cause = Cause::classify(&serde_json::json!({ "code": "request_timeout" })).unwrap();
        let first = Retry::next(None, cause.clone(), 1000).unwrap();
        let previous = serde_json::to_value(&first).unwrap();
        assert_eq!(first.started_at, Some(1000));
        assert_eq!(first.deadline_at, Some(1000 + MAX_RECOVERY_MS));
        let next = Retry::next(Some(&previous), cause.clone(), 3000).unwrap();
        assert_eq!(next.started_at, first.started_at);
        assert_eq!(next.deadline_at, first.deadline_at);
        assert!(Retry::next(Some(&previous), cause, 1000 + MAX_RECOVERY_MS).is_none());
        assert!(Retry::expired(
            &serde_json::json!({ "retry": first }),
            1000 + MAX_RECOVERY_MS
        ));
    }

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
    fn native_codex_other_routing_timeout_is_retriable() {
        let error = json!({
            "additionalDetails": null,
            "codexErrorInfo": "other",
            "message": "workspace routing discovery timed out",
            "misalignment": null,
        });
        let cause = Cause::classify(&error).expect("The production Codex timeout must recover");
        assert_eq!(cause.kind, FailureKind::Timeout);
        assert_eq!(cause.message, error["message"]);
    }

    #[test]
    fn native_codex_model_capacity_is_retriable() {
        let error = json!({
            "additionalDetails": null,
            "codexErrorInfo": "serverOverloaded",
            "message": "Selected model is at capacity. Please try a different model.",
            "misalignment": null,
        });
        let cause = Cause::classify(&error).expect("A model at capacity must recover");
        assert_eq!(cause.kind, FailureKind::Server);
        assert_eq!(cause.message, error["message"]);
        // The older event stream gives the message alone.
        let legacy = json!({ "message": error["message"] });
        assert_eq!(Cause::classify(&legacy).unwrap().kind, FailureKind::Server);
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
            json!({ "codexErrorInfo": "other", "message": "Unknown failure" }),
            json!({
                "codexErrorInfo": "other",
                "message": "Command failed: workspace routing discovery timed out",
            }),
            json!({
                "codexErrorInfo": "unauthorized",
                "message": "workspace routing discovery timed out",
            }),
            json!({
                "codexErrorInfo": "newErrorCode",
                "message": "workspace routing discovery timed out",
            }),
            json!({
                "code": "invalid_request",
                "codexErrorInfo": "other",
                "message": "workspace routing discovery timed out",
            }),
            json!({
                "codexErrorInfo": "other",
                "httpStatusCode": 403,
                "message": "workspace routing discovery timed out",
            }),
        ] {
            assert!(Cause::classify(&error).is_none(), "{error}");
        }
    }

    #[test]
    fn attempts_are_bounded_and_delays_survive_a_round_trip() {
        let cause = Cause::classify(&json!({ "message": "workspace routing discovery timed out" }))
            .unwrap();
        let mut previous = None;
        for attempt in 1..=MAX_ATTEMPTS {
            let retry = Retry::next(previous.as_ref(), cause.clone(), 1000).unwrap();
            assert_eq!(retry.attempt, attempt);
            let base = 5_000 * (1 << (attempt - 1));
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
