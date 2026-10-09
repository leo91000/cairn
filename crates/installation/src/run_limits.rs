use crate::config::now;
use serde_json::Value;
use std::time::Duration;

/// No budget is represented by null in persisted checkpoints and execution plans.
/// Zero remaining milliseconds still means an exhausted, finite budget.
pub(crate) fn budget_ms(agent: &Value) -> Option<i64> {
    agent["timeoutMinutes"]
        .as_i64()
        .filter(|minutes| *minutes > 0)
        .map(|minutes| minutes * 60000)
}

pub(crate) async fn wait_until(deadline: Option<i64>) {
    match deadline {
        Some(deadline) => {
            tokio::time::sleep(Duration::from_millis(
                (deadline - now()).max(0).unsigned_abs(),
            ))
            .await;
        }
        None => std::future::pending::<()>().await,
    }
}
