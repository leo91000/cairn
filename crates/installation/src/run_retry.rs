//! Adapter startup failures; retry wire state and pure policy have one contract.
pub use cairn_contracts::{Cause, FailureKind, MAX_ATTEMPTS, MAX_RECOVERY_MS, Retry};
use serde_json::Value;

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
        let previous: Retry =
            serde_json::from_value(serde_json::to_value(&first).unwrap()).unwrap();
        assert_eq!(first.started_at, Some(1000));
        assert_eq!(first.deadline_at, Some(1000 + MAX_RECOVERY_MS));
        let next = Retry::next(Some(&previous), cause.clone(), 3000).unwrap();
        assert_eq!(next.started_at, first.started_at);
        assert_eq!(next.deadline_at, first.deadline_at);
        assert!(Retry::next(Some(&previous), cause, 1000 + MAX_RECOVERY_MS).is_none());
        assert!(Retry::expired(Some(&first), 1000 + MAX_RECOVERY_MS));
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
            previous = Some(serde_json::from_value(serde_json::to_value(retry).unwrap()).unwrap());
            assert!(Retry::waiting(previous.as_ref(), due - 1));
            assert!(!Retry::waiting(previous.as_ref(), due));
        }
        assert!(Retry::next(previous.as_ref(), cause, 1000).is_none());
    }
}
