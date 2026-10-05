//! Notification events remain on the installation; only the official service owns devices and sends push.
use crate::{
    chats::{Question, QuestionStatus},
    config::now,
    error::Result,
    service::Service,
    store::Db,
    validation::text,
};
use leo_relay_protocol::{MAX_IN_FLIGHT, NotificationEvent, NotificationKind};
use serde_json::Value;

const DELIVERY_TTL_MS: i64 = 3_600_000;

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
pub async fn pending(service: &Service) -> Result<Vec<NotificationEvent>> {
    service
        .store
        .transaction(|db| {
            let mut events = Vec::new();
            for (key, value) in db.keys("push-outbox:")? {
                // Old local deliveries are not registrations on the official service.
                let Ok(event) = serde_json::from_value::<NotificationEvent>(value) else {
                    db.delete(&key)?;
                    continue;
                };
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
                if events.len() == MAX_IN_FLIGHT {
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
