//! Owner alerts for execution events that need attention away from the conversation.
use crate::{
    config::{id, now},
    error::Result,
    service::Service,
    validation::text,
};
use serde::Serialize;
use serde_json::Value;

/// The same kind of alert for one conversation is repeated at most once per hour.
const REPEAT_MS: i64 = 3_600_000;
const RETENTION_MS: i64 = 7 * 24 * 3_600_000;
const RECENT_LIMIT: usize = 50;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Alert<'a> {
    id: String,
    run_id: &'a str,
    chat_id: &'a Value,
    kind: &'a str,
    title: &'a str,
    body: &'a str,
    created_at: i64,
}

fn created_at(alert: &Value) -> i64 {
    alert["createdAt"].as_i64().unwrap_or(0)
}

/// Records the alert in the conversation, keeps it for Android polling and queues web push.
pub async fn raise(s: &Service, run: &str, kind: &str, title: &str, body: &str) -> Result<()> {
    let (run, kind, title, body) = (
        run.to_owned(),
        kind.to_owned(),
        title.to_owned(),
        body.to_owned(),
    );
    s.store
        .transaction(move |db| {
            let throttle = format!("node-alert-last:{run}:{kind}");
            if db
                .kv(&throttle)?
                .and_then(|at| at.as_i64())
                .is_some_and(|at| now() - at < REPEAT_MS)
            {
                return Ok(());
            }
            db.set(&throttle, &now().into(), Some(now() + REPEAT_MS))?;
            db.event(&run, "status", &body, None)?;
            let Some(chat) = db.list("chats")?.into_iter().find(|c| c["runId"] == run) else {
                return Ok(());
            };
            for old in db.list("node-alerts")? {
                if created_at(&old) < now() - RETENTION_MS {
                    db.remove("node-alerts", text(&old, "id"))?;
                }
            }
            let alert = serde_json::to_value(Alert {
                id: id(),
                run_id: &run,
                chat_id: &chat["id"],
                kind: &kind,
                title: &title,
                body: &body,
                created_at: now(),
            })?;
            db.put("node-alerts", &alert)?;
            crate::notifications::enqueue_alert(db, &alert)
        })
        .await
}

/// Newest first, for clients that poll instead of receiving web push.
pub async fn recent(s: &Service) -> Result<Value> {
    let mut alerts = s.store.list("node-alerts").await?;
    alerts.sort_by_key(|a| std::cmp::Reverse(created_at(a)));
    alerts.truncate(RECENT_LIMIT);
    Ok(alerts.into())
}
