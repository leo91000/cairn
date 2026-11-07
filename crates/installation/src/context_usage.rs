//! How full a conversation's model context is, as its coding agent measures it.
//!
//! The numerator is always what the model's latest request occupied, never a
//! conversation's cumulative spend: dividing cumulative tokens by the window
//! would report occupancy that does not exist. Values a coding agent does not
//! report stay absent rather than being inferred.
use serde::Serialize;
use serde_json::{Value, json};

/// The `usage` of a `context.usage` event, and the latest one saved on its run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
    /// Tokens the model's latest request occupied in its context window.
    pub context_tokens: u64,
    /// The model's context window, when the coding agent reports it.
    pub context_window: Option<u64>,
    /// Of `context_tokens`, input read from the prompt cache, when reported.
    pub cached_tokens: Option<u64>,
    /// The coding agent's local estimate rather than a model measurement: before
    /// the first request of a session, or right after a compaction.
    pub estimated: bool,
}

impl ContextUsage {
    pub fn event(&self) -> Value {
        json!({
            "type": "context.usage",
            "usage": self,
        })
    }

    /// Codex `thread/tokenUsage/updated`. `last` is the latest request, whose total is the
    /// active context size; `total` accumulates the whole thread and is never used here.
    pub fn codex(token_usage: &Value) -> Option<Self> {
        let last = &token_usage["last"];
        let context_tokens = positive(&last["totalTokens"])?;
        // Codex reports its local estimate after compaction as a request without input.
        let estimated = last["inputTokens"].as_u64() == Some(0);
        Some(Self {
            context_tokens,
            context_window: positive(&token_usage["modelContextWindow"]),
            cached_tokens: (!estimated)
                .then(|| last["cachedInputTokens"].as_u64())
                .flatten(),
            estimated,
        })
    }

    /// The usage of a Claude Code API response: the prompt it sent, cached or not, which
    /// is how Claude Code itself measures context occupancy.
    pub fn claude_response(usage: &Value, context_window: Option<u64>) -> Option<Self> {
        let prompt = [
            "input_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
        ]
        .into_iter()
        .map(|field| usage[field].as_u64().unwrap_or(0))
        .sum::<u64>();
        (prompt > 0).then(|| Self {
            context_tokens: prompt,
            context_window,
            cached_tokens: usage["cache_read_input_tokens"].as_u64(),
            estimated: false,
        })
    }

    /// Claude Code's `get_context_usage` answer. Without `apiUsage` its total is a local
    /// estimate of the transcript, system prompt and tools.
    pub fn claude_report(report: &Value) -> Option<Self> {
        let context_window = positive(&report["maxTokens"]);
        if report["apiUsage"].is_object() {
            return Self::claude_response(&report["apiUsage"], context_window);
        }
        Some(Self {
            context_tokens: positive(&report["totalTokens"])?,
            context_window,
            cached_tokens: None,
            estimated: true,
        })
    }
}

fn positive(value: &Value) -> Option<u64> {
    value.as_u64().filter(|value| *value > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_occupancy_is_the_latest_request_not_the_thread_total() {
        let usage = ContextUsage::codex(&json!({
            "last": {
                "inputTokens": 41_000,
                "cachedInputTokens": 38_000,
                "outputTokens": 900,
                "reasoningOutputTokens": 300,
                "totalTokens": 41_900,
            },
            "total": {
                "inputTokens": 900_000,
                "cachedInputTokens": 800_000,
                "outputTokens": 20_000,
                "reasoningOutputTokens": 5_000,
                "totalTokens": 920_000,
            },
            "modelContextWindow": 258_400,
        }))
        .unwrap();

        assert_eq!(
            usage,
            ContextUsage {
                context_tokens: 41_900,
                context_window: Some(258_400),
                cached_tokens: Some(38_000),
                estimated: false,
            }
        );
    }

    #[test]
    fn codex_unknown_window_and_empty_requests_stay_unknown() {
        let unknown_window = ContextUsage::codex(&json!({
            "last": { "inputTokens": 10, "cachedInputTokens": 0, "outputTokens": 2, "reasoningOutputTokens": 0, "totalTokens": 12 },
            "total": { "inputTokens": 10, "cachedInputTokens": 0, "outputTokens": 2, "reasoningOutputTokens": 0, "totalTokens": 12 },
            "modelContextWindow": null,
        }))
        .unwrap();
        assert_eq!(unknown_window.context_window, None);

        let no_request = ContextUsage::codex(&json!({
            "last": { "inputTokens": 0, "cachedInputTokens": 0, "outputTokens": 0, "reasoningOutputTokens": 0, "totalTokens": 0 },
            "total": { "inputTokens": 500, "cachedInputTokens": 0, "outputTokens": 20, "reasoningOutputTokens": 0, "totalTokens": 520 },
            "modelContextWindow": 200_000,
        }));
        assert_eq!(no_request, None);
    }

    #[test]
    fn codex_compaction_estimate_is_marked_estimated() {
        let usage = ContextUsage::codex(&json!({
            "last": { "inputTokens": 0, "cachedInputTokens": 0, "outputTokens": 0, "reasoningOutputTokens": 0, "totalTokens": 6_200 },
            "total": { "inputTokens": 90_000, "cachedInputTokens": 0, "outputTokens": 3_000, "reasoningOutputTokens": 0, "totalTokens": 93_000 },
            "modelContextWindow": 200_000,
        }))
        .unwrap();
        assert!(usage.estimated);
        assert_eq!(usage.context_tokens, 6_200);
        assert_eq!(usage.cached_tokens, None);
    }

    #[test]
    fn claude_counts_cached_and_uncached_prompt_tokens_but_not_output() {
        let usage = ContextUsage::claude_response(
            &json!({
                "input_tokens": 200,
                "cache_creation_input_tokens": 2_000,
                "cache_read_input_tokens": 40_002,
                "output_tokens": 7,
            }),
            Some(200_000),
        )
        .unwrap();
        assert_eq!(usage.context_tokens, 42_202);
        assert_eq!(usage.cached_tokens, Some(40_002));
        assert!(!usage.estimated);

        assert_eq!(
            ContextUsage::claude_response(
                &json!({ "input_tokens": 0, "output_tokens": 0 }),
                Some(200_000)
            ),
            None
        );
    }

    #[test]
    fn claude_reports_without_a_response_are_estimates() {
        // Observed from Claude Code 2.1.280 before its first request and after `/compact`.
        let usage = ContextUsage::claude_report(&json!({
            "totalTokens": 26_309,
            "maxTokens": 200_000,
            "percentage": 13,
            "apiUsage": null,
        }))
        .unwrap();
        assert_eq!(
            usage,
            ContextUsage {
                context_tokens: 26_309,
                context_window: Some(200_000),
                cached_tokens: None,
                estimated: true,
            }
        );

        let measured = ContextUsage::claude_report(&json!({
            "totalTokens": 42_202,
            "maxTokens": 200_000,
            "apiUsage": {
                "input_tokens": 200,
                "output_tokens": 7,
                "cache_creation_input_tokens": 2_000,
                "cache_read_input_tokens": 40_002,
            },
        }))
        .unwrap();
        assert!(!measured.estimated);
        assert_eq!(measured.context_tokens, 42_202);

        assert_eq!(ContextUsage::claude_report(&json!({ "models": [] })), None);
    }
}
