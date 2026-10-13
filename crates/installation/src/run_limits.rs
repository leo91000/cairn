use serde_json::Value;

/// No budget is represented by null in persisted checkpoints and execution plans.
/// Zero remaining milliseconds still means an exhausted, finite budget.
pub(crate) fn budget_ms(agent: &Value) -> Option<i64> {
    agent["timeoutMinutes"]
        .as_i64()
        .filter(|minutes| *minutes > 0)
        .map(|minutes| minutes * 60000)
}
