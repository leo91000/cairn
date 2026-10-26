use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

pub const MAX_ATTEMPTS: u64 = 5;
pub const MAX_RECOVERY_MS: i64 = 10 * 60 * 1000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
pub enum FailureKind {
    Connection,
    Timeout,
    RateLimit,
    Server,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct Cause {
    pub kind: FailureKind,
    pub message: String,
    /// The adapter confirmed that no new agent turn was submitted.
    #[serde(default)]
    pub startup: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct Retry {
    #[ts(type = "number")]
    pub attempt: u64,
    #[ts(type = "number")]
    pub limit: u64,
    #[ts(type = "number | null")]
    pub next_attempt_at: Option<i64>,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub started_at: Option<i64>,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub deadline_at: Option<i64>,
    pub cause: Cause,
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

impl Retry {
    /// The planned timestamp is stored once, so restarting does not resample the
    /// delay or reset the attempt budget. Jitter spreads simultaneous failures.
    pub fn next(previous: Option<&Self>, cause: Cause, at: i64) -> Option<Self> {
        let attempt = previous.map(|retry| retry.attempt).unwrap_or(0);
        if attempt >= MAX_ATTEMPTS {
            return None;
        }
        let started_at = previous.and_then(|retry| retry.started_at).unwrap_or(at);
        let deadline_at = previous
            .and_then(|retry| retry.deadline_at)
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

    pub fn expired(retry: Option<&Self>, at: i64) -> bool {
        retry
            .and_then(|r| r.deadline_at)
            .is_some_and(|deadline| at >= deadline)
    }

    pub fn waiting(retry: Option<&Self>, at: i64) -> bool {
        retry
            .and_then(|r| r.next_attempt_at)
            .is_some_and(|next| at < next)
    }
}
