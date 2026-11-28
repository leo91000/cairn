//! Normalize Antigravity's terminal diagnostics without exposing its raw error text.
use super::turn::emit;
use crate::{
    config::now,
    error::{Error, Result},
    run_retry::{Cause, FailureKind},
    validation::text,
};
use regex::Regex;
use serde_json::{Value, json};
use std::sync::LazyLock;
use tokio::sync::mpsc;

pub(crate) const CAPACITY: &str = "antigravity_model_capacity";
pub(crate) const RATE_LIMIT: &str = "antigravity_rate_limit";
pub(crate) const REFUSED: &str = "antigravity_request_refused";

const QUOTA_MESSAGE: &str =
    "Usage limit reached for Antigravity. Check this account's quota or connect another account.";

pub(crate) const CAPACITY_MESSAGE: &str = "The Antigravity model is temporarily unavailable.";
pub(crate) const RATE_LIMIT_MESSAGE: &str = "Antigravity is temporarily rate limiting requests.";

pub(crate) const REFUSED_MESSAGE: &str =
    "Antigravity refused the request (429). Try again or choose another coding agent.";

pub(crate) const FRESH_INPUT_MESSAGE: &str = "Send a new message to continue.";

pub(crate) const RETRY_UNCONFIRMED_MESSAGE: &str =
    "Antigravity was interrupted during an automatic retry. Send a new message to continue.";

#[derive(Clone, Copy)]
pub(super) enum Recovery {
    Automatic,
    Manual,
    FollowUp,
}

pub(crate) fn recoverable_refusal(message: &str) -> bool {
    matches!(
        message,
        CAPACITY_MESSAGE | RATE_LIMIT_MESSAGE | REFUSED_MESSAGE
    )
}

pub(crate) fn retry_cause(error: &Value) -> Option<Cause> {
    if error["automaticRecovery"] != true || error["retryable"] == false {
        return None;
    }
    let kind = match text(error, "code") {
        CAPACITY => FailureKind::Server,
        RATE_LIMIT => FailureKind::RateLimit,
        _ => return None,
    };
    Some(Cause {
        kind,
        message: text(error, "message").to_owned(),
        startup: false,
    })
}

const REASONS: &[&str] = &[
    "QUOTA_EXHAUSTED",
    "MODEL_CAPACITY_EXHAUSTED",
    "PREFILL_QUEUE_OVERLOADED",
    "RATE_LIMIT_EXCEEDED",
];

static TEXT_REASON: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?-u:\b)reason(?:"\s*:\s*"|:\s*)(QUOTA_EXHAUSTED|MODEL_CAPACITY_EXHAUSTED|PREFILL_QUEUE_OVERLOADED|RATE_LIMIT_EXCEEDED)(?-u:\b)"#).unwrap()
});

static TEXT_DELAY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?-u:\b)(quotaResetDelay|retryDelay)(?:"\s*:\s*"|:\s*)((?:[0-9]+(?:\.[0-9]+)?(?:ms|h|m|s))+)(?:["\s\],}]|$)"#).unwrap()
});

static RESET_TIME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?-u:\b)quotaResetTimeStamp(?:"\s*:\s*"|:\s*)([^"\s\],}]+)"#).unwrap()
});

static RESETS_IN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bResets in ((?:[0-9]+(?:\.[0-9]+)?(?:ms|h|m|s))+)(?:\.|\s|$)").unwrap()
});

static DURATION_PART: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([0-9]+(?:\.[0-9]+)?)(ms|h|m|s)").unwrap());

fn delay_ms(value: &str) -> Option<i64> {
    let mut parsed = 0;
    let mut total = 0.0;
    for part in DURATION_PART.captures_iter(value) {
        let span = part.get(0)?;
        if span.start() != parsed {
            return None;
        }
        parsed = span.end();
        let amount = part[1].parse::<f64>().ok()?;
        let unit = match &part[2] {
            "h" => 3_600_000.0,
            "m" => 60_000.0,
            "s" => 1_000.0,
            _ => 1.0,
        };
        total += amount * unit;
    }
    (parsed > 0 && parsed == value.len() && (0.0..=9_007_199_254_740_991.0).contains(&total))
        .then(|| total.ceil() as i64)
}

fn timestamp(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.timestamp_millis())
        .filter(|time| *time > 0)
}

fn provider_error_id(value: &str) -> Option<&str> {
    let id = value.get(..36)?;
    crate::validation::uuid(id).ok()?;
    let suffix = value.get(36..)?;
    (suffix.is_empty()
        || suffix.strip_prefix('-').is_some_and(|digits| {
            !digits.is_empty() && digits.len() <= 6 && digits.bytes().all(|c| c.is_ascii_digit())
        }))
    .then_some(value)
}

fn split_diagnostic(source: &str) -> (&str, Option<Value>) {
    let mut end = source.len().min(16_384);
    while !source.is_char_boundary(end) {
        end -= 1;
    }
    let source = &source[..end];
    let Some(start) = source
        .find('{')
        .filter(|_| !source.contains(", Details: ["))
    else {
        return (source, None);
    };
    let (prefix, json) = source.split_at(start);
    // agy can prefix the Google JSON object with its transport status.
    let body = serde_json::Deserializer::from_str(json)
        .into_iter::<Value>()
        .next()
        .and_then(std::result::Result::ok);
    (prefix, body)
}

fn transport_details(text: &str) -> Option<(u64, &str)> {
    let (before_message, _) = text.split_once(", Message: ")?;
    let code = before_message
        .rsplit_once("Error ")?
        .1
        .parse::<u64>()
        .ok()?;
    let (_, status) = text.rsplit_once(", Status: ")?;
    let status = status
        .split_once(", Details: [")
        .map_or(status, |(status, _)| status);
    (code <= 599).then_some((code, status))
}

enum TextDetail<'a> {
    Inline(&'a str),
    ErrorInfo { reason: &'a str, metadata: &'a str },
    RetryInfo(&'a str),
    QuotaFailure(&'a str),
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum GeminiQuota {
    RateLimit,
    Exhausted,
}

fn gemini_quota(id: &str, metric: &str, zero_limit: bool) -> Option<GeminiQuota> {
    if !metric.starts_with("generativelanguage.googleapis.com/") {
        return None;
    }
    let id = id
        .strip_suffix("-FreeTier")
        .or_else(|| id.strip_suffix("-PaidTier"))
        .unwrap_or(id);
    match id {
        "GenerateRequestsPerDayPerProjectPerModel" | "GenerateContentInputTokensPerModelPerDay" => {
            Some(GeminiQuota::Exhausted)
        }
        "GenerateRequestsPerMinutePerProjectPerModel"
        | "GenerateContentInputTokensPerModelPerMinute" => Some(if zero_limit {
            GeminiQuota::Exhausted
        } else {
            GeminiQuota::RateLimit
        }),
        _ => None,
    }
}

fn take_go_map(input: &str) -> Option<(&str, &str)> {
    let entries = input.strip_prefix("map[")?;
    let mut depth = 1;
    let end = entries.char_indices().find_map(|(index, ch)| {
        match ch {
            '[' => depth += 1,
            ']' => depth -= 1,
            _ => {}
        }
        (depth == 0).then_some(index)
    })?;
    Some((&entries[..end], entries[end + 1..].trim_start()))
}

impl<'a> TextDetail<'a> {
    fn reason(&self) -> Option<&'a str> {
        match self {
            Self::Inline(fields) => TEXT_REASON
                .captures(fields)?
                .get(1)
                .map(|reason| reason.as_str()),
            Self::ErrorInfo { reason, .. } => Some(reason),
            Self::RetryInfo(_) | Self::QuotaFailure(_) => None,
        }
    }

    fn delay(&self, key: &str) -> Option<i64> {
        let fields = match (self, key) {
            (Self::RetryInfo(delay), "retryDelay") => return delay_ms(delay),
            (Self::Inline(fields), _)
            | (
                Self::ErrorInfo {
                    metadata: fields, ..
                },
                "quotaResetDelay",
            ) => fields,
            _ => return None,
        };
        TEXT_DELAY
            .captures_iter(fields)
            .filter(|hit| &hit[1] == key)
            .filter_map(|hit| delay_ms(&hit[2]))
            .max()
    }

    fn quota_reset(&self) -> Option<i64> {
        let fields = match self {
            Self::Inline(fields)
            | Self::ErrorInfo {
                metadata: fields, ..
            } => fields,
            Self::RetryInfo(_) | Self::QuotaFailure(_) => return None,
        };
        RESET_TIME
            .captures(fields)
            .and_then(|hit| timestamp(&hit[1]))
    }

    fn gemini_quota(&self) -> Option<GeminiQuota> {
        let Self::QuotaFailure(mut rest) = *self else {
            return None;
        };
        let mut quota = None;
        while let Some((entry, tail)) = take_go_map(rest) {
            rest = tail;
            let fields = entry
                .strip_prefix("quotaId:")
                .or_else(|| entry.rsplit_once(" quotaId:").map(|(_, fields)| fields));
            if let Some(fields) = fields {
                // Go's map format orders quotaId immediately before quotaMetric.
                let mut values = fields.split_ascii_whitespace();
                if let (Some(id), Some(metric)) = (values.next(), values.next())
                    && let Some(metric) = metric.strip_prefix("quotaMetric:")
                {
                    let zero_limit = values.any(|field| field == "quotaValue:0");
                    quota = quota.max(gemini_quota(id, metric, zero_limit));
                }
            }
        }
        if rest.is_empty() { quota } else { None }
    }
}

fn legacy_fields(text: &str) -> Vec<TextDetail<'_>> {
    if text.starts_with("reason:") || text.starts_with("Resource has been exhausted. reason:") {
        return vec![TextDetail::Inline(text)];
    }
    let Some((prefix, details)) = text.trim_end().rsplit_once(", Details: [") else {
        return Vec::new();
    };
    if !prefix.ends_with(", Status: RESOURCE_EXHAUSTED") {
        return Vec::new();
    }
    let Some(mut rest) = details.strip_suffix(']') else {
        return Vec::new();
    };
    // agy 1.5 renders Google RPC details using Go map notation. Parse full
    // top-level entries so the message and unrelated maps cannot supply fields.
    let mut recognized = Vec::new();
    while let Some((entry, tail)) = take_go_map(rest) {
        rest = tail;
        if let Some((kind, fields)) = entry
            .strip_prefix("@type:type.googleapis.com/google.rpc.")
            .and_then(|entry| entry.split_once(' '))
        {
            match kind {
                "ErrorInfo" => {
                    // fmt prints the map's keys in order. The top-level reason
                    // is the last ErrorInfo field, outside its metadata map.
                    let (fields, reason) = fields
                        .strip_prefix("reason:")
                        .map(|reason| ("", reason))
                        .or_else(|| fields.rsplit_once(" reason:"))
                        .filter(|(_, reason)| REASONS.contains(reason))
                        .unwrap_or((fields, ""));
                    let metadata = fields
                        .split_once("metadata:map[")
                        .and_then(|(_, metadata)| metadata.strip_suffix(']'))
                        .unwrap_or_default();
                    recognized.push(TextDetail::ErrorInfo { reason, metadata });
                }
                "RetryInfo" => {
                    if let Some(delay) = fields.strip_prefix("retryDelay:") {
                        recognized.push(TextDetail::RetryInfo(delay));
                    }
                }
                "QuotaFailure" => {
                    if let Some(violations) = fields
                        .strip_prefix("violations:[")
                        .and_then(|list| list.strip_suffix(']'))
                    {
                        recognized.push(TextDetail::QuotaFailure(violations));
                    }
                }
                _ => {}
            }
        }
    }
    if !rest.is_empty() {
        return Vec::new();
    }
    recognized
}

fn diagnostic(value: &Value, message: &str, at: i64) -> Value {
    let [(short, short_body), (result, result_body)] =
        [text(value, "short_error"), message].map(split_diagnostic);
    let texts = [short, result];
    let legacy: Vec<_> = texts.into_iter().flat_map(legacy_fields).collect();
    let payloads = [Some(value), short_body.as_ref(), result_body.as_ref()];
    let fields = || {
        payloads
            .into_iter()
            .flatten()
            .flat_map(|body| [body, &body["error"]])
    };
    let details = || {
        fields()
            .filter_map(|body| body["details"].as_array())
            .flatten()
    };
    let error_info =
        || details().filter(|detail| detail["@type"] == "type.googleapis.com/google.rpc.ErrorInfo");
    let retry_info =
        || details().filter(|detail| detail["@type"] == "type.googleapis.com/google.rpc.RetryInfo");
    let gemini_quota = details()
        .filter(|detail| detail["@type"] == "type.googleapis.com/google.rpc.QuotaFailure")
        .filter_map(|detail| detail["violations"].as_array())
        .flatten()
        .filter_map(|violation| {
            let limit = &violation["quotaValue"];
            gemini_quota(
                text(violation, "quotaId"),
                text(violation, "quotaMetric"),
                limit.as_u64() == Some(0) || limit == "0",
            )
        })
        .chain(legacy.iter().filter_map(TextDetail::gemini_quota))
        .max();
    let code = value["error_code"]
        .as_u64()
        .or_else(|| fields().find_map(|body| body["code"].as_u64()))
        .or_else(|| {
            texts
                .iter()
                .find_map(|text| transport_details(text).map(|(code, _)| code))
        })
        .filter(|code| *code <= 599);
    let status = fields()
        .map(|body| text(body, "status"))
        .chain(
            texts
                .iter()
                .filter_map(|text| transport_details(text).map(|(_, status)| status)),
        )
        .find(|status| !status.is_empty())
        .unwrap_or_default();
    let candidates = [text(value, "reason"), status]
        .into_iter()
        .chain(error_info().map(|detail| text(detail, "reason")))
        .chain(legacy.iter().filter_map(TextDetail::reason))
        .filter(|reason| REASONS.contains(reason));
    let mut reason = None;
    for candidate in candidates {
        if candidate == "QUOTA_EXHAUSTED" {
            reason = Some(candidate);
            break;
        }
        reason.get_or_insert(candidate);
    }
    let quota_messages = || {
        let starts_with_quota = |message: &str| {
            message
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("individual quota reached")
        };
        texts
            .iter()
            .copied()
            .filter(move |message| {
                starts_with_quota(message)
                    || message
                        .to_ascii_lowercase()
                        .contains(": individual quota reached")
            })
            .chain(
                fields()
                    .map(|body| text(body, "message"))
                    .filter(move |message| starts_with_quota(message)),
            )
    };
    let explicit_quota =
        gemini_quota == Some(GeminiQuota::Exhausted) || quota_messages().next().is_some();
    let limited = code == Some(429)
        || status == "RESOURCE_EXHAUSTED"
        || reason.is_some()
        || gemini_quota.is_some()
        || explicit_quota
        || texts.iter().any(|text| {
            let lower = text.to_ascii_lowercase();
            lower.contains("resource_exhausted")
                || lower.starts_with("error 429:")
                || lower.contains("error 429, message:")
        });
    let retryable = value["retryable"].as_bool();
    let (kind, message) = if limited {
        match reason {
            Some("QUOTA_EXHAUSTED") => ("usage_limit_reached", QUOTA_MESSAGE),
            _ if explicit_quota => ("usage_limit_reached", QUOTA_MESSAGE),
            Some("MODEL_CAPACITY_EXHAUSTED" | "PREFILL_QUEUE_OVERLOADED") => {
                (CAPACITY, CAPACITY_MESSAGE)
            }
            Some("RATE_LIMIT_EXCEEDED") => (RATE_LIMIT, RATE_LIMIT_MESSAGE),
            _ if gemini_quota == Some(GeminiQuota::RateLimit) => (RATE_LIMIT, RATE_LIMIT_MESSAGE),
            _ => (REFUSED, REFUSED_MESSAGE),
        }
    } else if matches!(code, Some(401 | 403))
        || matches!(status, "UNAUTHENTICATED" | "PERMISSION_DENIED")
        || texts
            .iter()
            .any(|text| text.to_ascii_lowercase().contains("authentication"))
    {
        (
            "antigravity_authentication",
            "Antigravity authentication failed. Replace this account's credentials in Connections.",
        )
    } else if texts.iter().any(|text| {
        text.to_ascii_lowercase()
            .contains("invalid model selection")
    }) {
        (
            "antigravity_model_selection",
            "This Antigravity account does not offer the selected model or reasoning level. Choose another model.",
        )
    } else {
        (
            "antigravity_error",
            "Antigravity could not complete this response. Check the model, account credentials, and available capacity.",
        )
    };

    let quota_values = |key| {
        std::iter::once(text(value, key))
            .chain(error_info().map(move |detail| text(&detail["metadata"], key)))
    };
    let deadline_delay = |key, values: Vec<&str>| {
        values
            .into_iter()
            .filter_map(delay_ms)
            .chain(legacy.iter().filter_map(|detail| detail.delay(key)))
            .max()
    };
    let retry_after_ms = deadline_delay(
        "retryDelay",
        std::iter::once(text(value, "retryDelay"))
            .chain(retry_info().map(|detail| text(detail, "retryDelay")))
            .collect(),
    );
    let resets_at = (kind == "usage_limit_reached")
        .then(|| {
            quota_values("quotaResetTimeStamp")
                .find_map(timestamp)
                .or_else(|| legacy.iter().find_map(TextDetail::quota_reset))
                .or_else(|| {
                    let reset_delay = deadline_delay(
                        "quotaResetDelay",
                        quota_values("quotaResetDelay").collect(),
                    )
                    .or_else(|| {
                        quota_messages().find_map(|text| {
                            RESETS_IN.captures(text).and_then(|hit| delay_ms(&hit[1]))
                        })
                    });
                    reset_delay.and_then(|ms| at.checked_add(ms))
                })
        })
        .flatten();
    let provider_status = [
        "RESOURCE_EXHAUSTED",
        "UNAUTHENTICATED",
        "PERMISSION_DENIED",
        "UNAVAILABLE",
        "INTERNAL",
        "INVALID_ARGUMENT",
    ]
    .into_iter()
    .find(|known| *known == status);
    json!({
        "code": kind,
        "message": message,
        "resetsAt": resets_at,
        "retryAfterMs": retry_after_ms,
        "providerStatus": provider_status,
        "providerCode": code,
        "providerReason": reason,
        "retryable": retryable,
        "providerErrorId": provider_error_id(text(value, "error_id"))
    })
}

pub(super) fn normalize(
    native: Option<&Value>,
    message: &str,
    correlated: bool,
    recovery: Recovery,
) -> Value {
    let at = now();
    let native = native.unwrap_or(&Value::Null);
    let mut error = diagnostic(if correlated { native } else { &Value::Null }, message, at);
    if !correlated && !native.is_null() {
        let observed = diagnostic(native, "", at);
        if observed["code"] == "usage_limit_reached" {
            // All inputs in this launch used the same leased account. Keep its
            // quota evidence separate when we cannot identify the refused input.
            error["accountQuotaExhausted"] = true.into();
            error["accountQuotaResetAt"] = observed["resetsAt"].clone();
        }
        if message.is_empty()
            && (observed["code"] == "usage_limit_reached"
                || recoverable_refusal(text(&observed, "message")))
        {
            error["code"] = REFUSED.into();
            error["message"] = REFUSED_MESSAGE.into();
        }
    }
    error["automaticRecovery"] = (matches!(recovery, Recovery::Automatic)
        && matches!(text(&error, "code"), CAPACITY | RATE_LIMIT)
        && error["retryable"] != false)
        .into();
    if matches!(recovery, Recovery::FollowUp) {
        require_fresh_input(&mut error);
    }
    error
}

pub(super) fn require_fresh_input(error: &mut Value) {
    if error["requiresFreshInput"] == true {
        return;
    }
    let reason = if error["code"] == REFUSED {
        "Antigravity refused the request (429)."
    } else {
        text(error, "message")
    };
    error["message"] = format!("{reason} {FRESH_INPUT_MESSAGE}").into();
    error["requiresFreshInput"] = true.into();
    error["automaticRecovery"] = false.into();
}

pub(super) async fn report<T>(events: &mpsc::Sender<Value>, error: Value) -> Result<T> {
    let failure = Error::coded(502, text(&error, "code"), text(&error, "message"));
    emit(events, json!({ "type": "turn.failed", "error": error })).await?;
    Err(failure)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmed_quota_takes_precedence_over_a_transient_diagnostic() {
        let rate_limit = json!({
            "reason": "RATE_LIMIT_EXCEEDED",
            "status": "RESOURCE_EXHAUSTED",
            "error_code": 429,
            "retryable": true,
        });
        let typed = json!({
            "error": {
                "details": [{
                    "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                    "reason": "QUOTA_EXHAUSTED",
                    "metadata": { "quotaResetTimeStamp": "2030-11-28T08:35:49Z" },
                }],
            },
        });
        for details in [
            typed.to_string(),
            "Individual quota reached. Resets in 40m.".to_owned(),
        ] {
            let error = normalize(Some(&rate_limit), &details, true, Recovery::Automatic);
            assert_eq!(error["code"], "usage_limit_reached");
            assert!(error["resetsAt"].is_i64());
            assert_eq!(error["automaticRecovery"], false);
        }
    }

    #[test]
    fn typed_gemini_daily_quota_is_distinct_from_a_short_window_limit() {
        let metric = "generativelanguage.googleapis.com/generate_content_free_tier_requests";
        for (window, limit, code) in [
            ("Day", 500, "usage_limit_reached"),
            ("Minute", 500, RATE_LIMIT),
            ("Minute", 0, "usage_limit_reached"),
            ("Week", 500, REFUSED),
        ] {
            let id = format!("GenerateRequestsPer{window}PerProjectPerModel-FreeTier");
            let json = json!({
                "code": 429,
                "status": "RESOURCE_EXHAUSTED",
                "details": [{
                    "@type": "type.googleapis.com/google.rpc.QuotaFailure",
                    "violations": [{
                        "quotaId": id,
                        "quotaMetric": metric,
                        "quotaValue": limit.to_string(),
                    }]
                }, {
                    "@type": "type.googleapis.com/google.rpc.RetryInfo",
                    "retryDelay": "42s",
                }]
            });
            let native = format!(
                "Error 429, Message: Resource exhausted., Status: RESOURCE_EXHAUSTED, Details: [map[@type:type.googleapis.com/google.rpc.QuotaFailure violations:[map[description:Quota exceeded quotaDimensions:map[location:global model:gemini] quotaId:{id} quotaMetric:{metric} quotaValue:{limit}]]] map[@type:type.googleapis.com/google.rpc.RetryInfo retryDelay:42s]]"
            );
            for source in [json.to_string(), native] {
                let error = normalize(None, &source, true, Recovery::Automatic);
                assert_eq!(error["code"], code);
                assert_eq!(error["retryAfterMs"], 42_000);
                assert!(error["resetsAt"].is_null());
                assert_eq!(error["automaticRecovery"], code == RATE_LIMIT);
            }
        }
    }

    #[test]
    fn native_json_quota_keeps_its_reset_and_discards_the_provider_body() {
        let provider_body = json!({
            "error": {
                "message": "private provider detail",
                "details": [{
                    "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                    "reason": "QUOTA_EXHAUSTED",
                    "metadata": { "quotaResetTimeStamp": "2030-11-28T08:35:49Z" }
                }],
            },
        });
        let native = json!({
            "error_code": 429,
            "status": "RESOURCE_EXHAUSTED",
            "short_error": format!("RESOURCE_EXHAUSTED (code 429): {provider_body}"),
        });
        let error = diagnostic(&native, "", 1000);
        assert_eq!(error["code"], "usage_limit_reached");
        assert_eq!(
            error["resetsAt"].as_i64(),
            timestamp("2030-11-28T08:35:49Z")
        );
        assert!(!error.to_string().contains("private provider detail"));
    }

    #[test]
    fn a_google_json_body_can_explicitly_report_quota_in_its_message() {
        let native = json!({
            "short_error": format!("RESOURCE_EXHAUSTED (code 429): {}", json!({
                "error": {
                    "code": 429,
                    "status": "RESOURCE_EXHAUSTED",
                    "message": "Individual quota reached. Resets in 40m. retryDelay:1s",
                },
            })),
        });
        let error = diagnostic(&native, "", 1000);
        assert_eq!(error["code"], "usage_limit_reached");
        assert_eq!(error["resetsAt"], 2_401_000);
        assert!(error["retryAfterMs"].is_null());
    }

    #[test]
    fn prose_inside_a_google_error_cannot_supply_a_reason_or_retry_delay() {
        let native = json!({
            "short_error": format!("RESOURCE_EXHAUSTED (code 429): {}", json!({
                "error": {
                    "code": 429,
                    "message": "Diagnostic prose: reason:QUOTA_EXHAUSTED retryDelay:1s",
                    "details": [],
                },
            })),
        });
        let error = diagnostic(&native, "A log mentioned reason:QUOTA_EXHAUSTED", 1000);
        assert_eq!(error["code"], REFUSED);
        assert!(error["providerReason"].is_null());
        assert!(error["retryAfterMs"].is_null());
    }

    #[test]
    fn typed_go_maps_preserve_quota_and_the_announced_absolute_reset() {
        let error = normalize(
            None,
            "agent executor error: generating and executing: Error 429, Message: Synthetic quota refusal, Status: RESOURCE_EXHAUSTED, Details: [map[@type:type.googleapis.com/google.rpc.ErrorInfo domain:generativelanguage.googleapis.com metadata:map[quotaResetDelay:3s quotaResetTimeStamp:2030-11-28T08:35:49Z] reason:QUOTA_EXHAUSTED]]",
            true,
            Recovery::Automatic,
        );
        assert_eq!(error["code"], "usage_limit_reached");
        assert_eq!(error["providerReason"], "QUOTA_EXHAUSTED");
        assert_eq!(
            error["resetsAt"].as_i64(),
            timestamp("2030-11-28T08:35:49Z")
        );
        assert_eq!(error["providerCode"], 429);
        assert_eq!(error["providerStatus"], "RESOURCE_EXHAUSTED");
        assert_eq!(error["automaticRecovery"], false);
    }

    #[test]
    fn only_the_typed_error_info_reason_classifies_a_refusal() {
        for (reason, expected) in [
            ("UNRECOGNIZED", REFUSED),
            ("MODEL_CAPACITY_EXHAUSTED", CAPACITY),
        ] {
            let message = format!(
                "Error 429, Message: Unavailable, Status: RESOURCE_EXHAUSTED, Details: [map[@type:type.googleapis.com/google.rpc.ErrorInfo domain:example.test metadata:map[reason:QUOTA_EXHAUSTED] reason:{reason}]]"
            );
            let error = normalize(None, &message, true, Recovery::Automatic);
            assert_eq!(error["code"], expected);
        }
        let native = json!({
            "error_code": 429,
            "details": [{
                "@type": "type.googleapis.com/google.rpc.Help",
                "reason": "QUOTA_EXHAUSTED",
            }],
        });
        assert_eq!(diagnostic(&native, "", 1000)["code"], REFUSED);
    }

    #[test]
    fn typed_go_maps_preserve_the_rate_limit_delay_and_retryable_flag() {
        let message = "Error 429, Message: retryDelay:1s, Status: RESOURCE_EXHAUSTED, Details: [map[@type:type.googleapis.com/google.rpc.ErrorInfo domain:example.test reason:RATE_LIMIT_EXCEEDED] map[@type:type.googleapis.com/google.rpc.RetryInfo retryDelay:42s]]";
        let error = normalize(None, message, true, Recovery::Automatic);
        assert_eq!(error["code"], RATE_LIMIT);
        assert_eq!(error["retryAfterMs"], 42_000);
        assert_eq!(error["automaticRecovery"], true);
        let terminal = normalize(
            Some(&json!({"retryable": false})),
            message,
            true,
            Recovery::Automatic,
        );
        assert_eq!(terminal["automaticRecovery"], false);
    }

    #[test]
    fn multiple_verified_provider_delays_keep_the_longest_wait() {
        let native = json!({
            "error_code": 429,
            "reason": "RATE_LIMIT_EXCEEDED",
            "retryDelay": "5s",
            "short_error": json!({
                "error": {
                    "message": "A previous request advertised retryDelay:1s",
                    "details": [
                        { "@type": "type.googleapis.com/google.rpc.RetryInfo", "retryDelay": "42s" }
                    ],
                },
            }).to_string(),
        });
        let error = diagnostic(&native, "reason:RATE_LIMIT_EXCEEDED retryDelay:10s", 1000);
        assert_eq!(error["code"], RATE_LIMIT);
        assert_eq!(error["retryAfterMs"], 42_000);
    }

    #[test]
    fn a_quota_retry_delay_is_not_an_announced_reset() {
        let native = json!({
            "error_code": 429,
            "short_error": "reason:QUOTA_EXHAUSTED retryDelay:5s",
        });
        let error = diagnostic(&native, "", 1000);
        assert_eq!(error["code"], "usage_limit_reached");
        assert_eq!(error["retryAfterMs"], 5000);
        assert!(error["resetsAt"].is_null());
    }

    #[test]
    fn only_a_transient_native_failure_with_recoverable_input_can_retry() {
        let error = |code, automatic, retryable| {
            json!({
                "code": code,
                "message": "Provider temporarily unavailable",
                "retryable": retryable,
                "automaticRecovery": automatic,
            })
        };
        assert!(retry_cause(&error(CAPACITY, true, true)).is_some());
        assert!(retry_cause(&error(RATE_LIMIT, true, true)).is_some());
        for unavailable in [
            error(CAPACITY, false, true),
            error(CAPACITY, true, false),
            error(REFUSED, true, true),
            error("usage_limit_reached", true, true),
        ] {
            assert!(retry_cause(&unavailable).is_none());
        }
    }
}
