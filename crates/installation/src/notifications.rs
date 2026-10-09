//! Notification events remain on the installation; only the Beacon owns devices and sends push.
use crate::{
    chats::{Question, QuestionStatus},
    config::now,
    error::Result,
    service::Service,
    store::Db,
    validation::text,
};
use cairn_protocol::{MAX_NOTIFICATION_IN_FLIGHT, NotificationEvent, NotificationKind};
use serde_json::Value;
use std::collections::HashSet;

const DELIVERY_TTL_MS: i64 = 3_600_000;

/// Local registrations and their encrypted keys cannot authorize beacon account push.
pub fn remove_local_registrations(db: &mut Db<'_>) -> Result<()> {
    for prefix in ["push-device:", "mcp-secret:push-device:"] {
        for (key, _) in db.keys(prefix)? {
            db.delete(&key)?;
        }
    }

    db.delete("mcp-secret:push-vapid")
}

fn enqueue_event(db: &Db<'_>, event: &NotificationEvent) -> Result<()> {
    let key = format!("push-outbox:{}", event.id);
    if db.kv(&key)?.is_none() {
        db.set_as(&key, event, Some(now() + DELIVERY_TTL_MS))?;
    }
    Ok(())
}

pub fn enqueue(db: &Db<'_>, question: &Question) -> Result<()> {
    enqueue_event(
        db,
        &NotificationEvent {
            id: question.id.clone(),
            chat_id: question.chat_id.clone(),
            kind: NotificationKind::Question {
                question_id: question.id.clone(),
            },
        },
    )
}

pub fn enqueue_alert(db: &Db<'_>, alert: &Value) -> Result<()> {
    enqueue_event(
        db,
        &NotificationEvent {
            id: text(alert, "id").to_owned(),
            chat_id: text(alert, "chatId").to_owned(),
            kind: NotificationKind::Alert {
                alert_id: text(alert, "id").to_owned(),
                title: text(alert, "title").chars().take(120).collect(),
                body: text(alert, "body").chars().take(300).collect(),
            },
        },
    )
}

/// Questions answered while offline are discarded before they enter the relay.
pub async fn pending(
    service: &Service,
    recently_sent: HashSet<String>,
    after: String,
    limit: usize,
) -> Result<Vec<NotificationEvent>> {
    let limit = limit.min(MAX_NOTIFICATION_IN_FLIGHT);
    if limit == 0 {
        return Ok(Vec::new());
    }

    service
        .store
        .transaction(move |db| {
            let mut events = Vec::new();
            let mut pending = db.keys("push-outbox:")?;
            pending.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
            let after_key = format!("push-outbox:{after}");
            let start = pending.partition_point(|(key, _)| key <= &after_key);
            pending.rotate_left(start);

            for (key, value) in pending {
                // Old local deliveries are not registrations on the Beacon.
                let Ok(event) = serde_json::from_value::<NotificationEvent>(value) else {
                    db.delete(&key)?;
                    continue;
                };
                // Rotate before applying cooldowns and the limit so every pending
                // event gets a turn even when earlier deliveries keep failing.
                if recently_sent.contains(&event.id) {
                    continue;
                }

                if let NotificationKind::Question { question_id } = &event.kind {
                    let question = db.kv(&format!(
                        "{}{}",
                        crate::chats::question_prefix(&event.chat_id),
                        question_id
                    ))?;
                    if !question
                        .is_some_and(|question| question["status"] == QuestionStatus::Pending)
                    {
                        db.delete(&key)?;
                        continue;
                    }
                }
                events.push(event);
                if events.len() == limit {
                    break;
                }
            }
            Ok(events)
        })
        .await
}

pub async fn acknowledge(service: &Service, id: &str) -> Result<()> {
    service.store.delete(&format!("push-outbox:{id}")).await
}
