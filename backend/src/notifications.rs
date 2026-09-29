use crate::{
    auth::hex_digest,
    chats::{Question, QuestionStatus},
    config::now,
    error::{Error, Result},
    service::Service,
    store::Db,
    validation::text,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;
use web_push::{
    ContentEncoding, HyperWebPushClient, SubscriptionInfo, Urgency, VapidSignatureBuilder,
    WebPushClient, WebPushError, WebPushMessageBuilder,
};

const DELIVERY_TTL_MS: i64 = 3_600_000;
const MAX_DEVICES: usize = 50;

#[derive(Clone, Default)]
pub struct Notifications {
    delivery: Arc<Mutex<()>>,
}

/// A pending push, `push-outbox:{questionId | alert-{alertId}}:{subscriptionId}`.
/// A question push is dropped once its question is no longer pending.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Delivery {
    #[serde(default)]
    subscription_id: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    question_id: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    alert_id: Value,
    #[serde(default)]
    chat_id: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    title: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    body: Value,
    #[serde(default)]
    attempts: u64,
    #[serde(default)]
    next_at: Option<i64>,
    #[serde(default)]
    expires_at: Option<i64>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

impl Delivery {
    fn new(subscription: &str, chat_id: Value) -> Self {
        Self {
            subscription_id: subscription.to_owned(),
            question_id: Value::Null,
            alert_id: Value::Null,
            chat_id,
            title: Value::Null,
            body: Value::Null,
            attempts: 0,
            next_at: Some(now()),
            expires_at: Some(now() + DELIVERY_TTL_MS),
            extra: Map::new(),
        }
    }

    fn is_alert(&self) -> bool {
        self.alert_id.is_string()
    }

    fn payload(&self) -> Value {
        if self.is_alert() {
            return json!({
                "title": self.title,
                "body": self.body,
                "chatId": self.chat_id,
                "alertId": self.alert_id,
            });
        }
        json!({
            "title": "Your agent has a question",
            "body": "Open the chat to answer.",
            "chatId": self.chat_id,
            "questionId": self.question_id,
        })
    }

    /// Exponential backoff from 10 seconds, capped at 5 minutes.
    fn retry_later(&mut self) {
        let attempts = self.attempts;
        self.attempts = attempts.saturating_add(1);
        self.next_at = Some(now() + (10_000_i64 * (1_i64 << attempts.min(5))).min(300_000));
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct PushKeys {
    p256dh: String,
    auth: String,
}

/// A browser push subscription, stored encrypted as `push-device:{id}`.
#[derive(Clone, Serialize, Deserialize)]
struct PushSubscription {
    endpoint: String,
    keys: PushKeys,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VapidKeys {
    private_key: String,
    public_key: String,
}

fn subscriptions(db: &Db<'_>) -> Result<Vec<String>> {
    Ok(db
        .keys("push-device:")?
        .into_iter()
        .map(|(key, _)| key.trim_start_matches("push-device:").to_owned())
        .collect())
}

pub fn enqueue(db: &Db<'_>, question: &Question) -> Result<()> {
    for subscription in subscriptions(db)? {
        let key = format!("push-outbox:{}:{subscription}", question.id);
        if db.kv(&key)?.is_none() {
            let mut delivery = Delivery::new(&subscription, question.chat_id.clone().into());
            delivery.question_id = question.id.clone().into();
            db.set_as(&key, &delivery, None)?;
        }
    }
    Ok(())
}

/// Execution alerts need no answer, so they are delivered as long as the device is subscribed.
pub fn enqueue_alert(db: &Db<'_>, alert: &Value) -> Result<()> {
    for subscription in subscriptions(db)? {
        let mut delivery = Delivery::new(&subscription, alert["chatId"].clone());
        delivery.alert_id = alert["id"].clone();
        delivery.title = alert["title"].clone();
        delivery.body = alert["body"].clone();
        db.set_as(
            &format!("push-outbox:alert-{}:{subscription}", text(alert, "id")),
            &delivery,
            None,
        )?;
    }
    Ok(())
}

fn allowed_push_host(host: &str) -> bool {
    [
        "fcm.googleapis.com",
        "updates.push.services.mozilla.com",
        "web.push.apple.com",
    ]
    .contains(&host)
        || [".push.services.mozilla.com", ".notify.windows.com"]
            .iter()
            .any(|suffix| host.ends_with(suffix))
}

fn valid_key(value: &str, length: usize, maximum: usize) -> bool {
    value.len() <= maximum
        && URL_SAFE_NO_PAD
            .decode(value.trim_end_matches('='))
            .is_ok_and(|bytes| bytes.len() == length)
}

fn subscription(input: &Value) -> Result<PushSubscription> {
    let endpoint = text(input, "endpoint");
    let url =
        url::Url::parse(endpoint).map_err(|_| Error::bad("Invalid notification subscription."))?;
    if endpoint.len() > 4096
        || url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
        || !allowed_push_host(url.host_str().unwrap_or(""))
    {
        return Err(Error::bad("This browser push service is not supported."));
    }
    let keys = PushKeys {
        p256dh: text(&input["keys"], "p256dh").to_owned(),
        auth: text(&input["keys"], "auth").to_owned(),
    };
    if !valid_key(&keys.p256dh, 65, 100) || !valid_key(&keys.auth, 16, 30) {
        return Err(Error::bad("Invalid notification subscription keys."));
    }
    Ok(PushSubscription {
        endpoint: endpoint.to_owned(),
        keys,
    })
}

fn generate_vapid_keys() -> VapidKeys {
    let private = p256::SecretKey::random(&mut p256::elliptic_curve::rand_core::OsRng);
    let public = private.public_key().to_encoded_point(false);
    VapidKeys {
        private_key: URL_SAFE_NO_PAD.encode(private.to_bytes()),
        public_key: URL_SAFE_NO_PAD.encode(public.as_bytes()),
    }
}

impl Notifications {
    async fn keys(&self, service: &Service) -> Result<VapidKeys> {
        let vault = service.vault.clone();
        service
            .store
            .transaction(move |db| {
                if let Some(value) = db.kv("mcp-secret:push-vapid")? {
                    return Ok(serde_json::from_value(
                        vault.decrypt("push-vapid", &value)?,
                    )?);
                }
                let keys = generate_vapid_keys();
                vault.set_in(db, "push-vapid", &serde_json::to_value(&keys)?)?;
                Ok(keys)
            })
            .await
    }

    pub async fn configuration(&self, service: &Service) -> Result<Value> {
        let keys = self.keys(service).await?;
        Ok(json!({ "publicKey": keys.public_key }))
    }

    pub async fn subscribe(&self, service: &Service, input: Value) -> Result<Value> {
        let value = serde_json::to_value(subscription(&input)?)?;
        let id = hex_digest(text(&value, "endpoint"));
        let vault = service.vault.clone();
        service
            .store
            .transaction(move |db| {
                let key = format!("push-device:{id}");
                if db.kv(&key)?.is_none() && db.keys("push-device:")?.len() >= MAX_DEVICES {
                    return Err(Error::conflict(
                        "Too many notification devices are registered.",
                    ));
                }
                vault.set_in(db, &key, &value)?;
                db.set(&key, &json!({ "createdAt": now() }), None)?;
                Ok(json!({ "id": id }))
            })
            .await
    }

    pub async fn unsubscribe(&self, service: &Service, id: &str) -> Result<Value> {
        let id = id.to_owned();
        service
            .store
            .transaction(move |db| {
                db.delete(&format!("mcp-secret:push-device:{id}"))?;
                db.delete(&format!("push-device:{id}"))?;
                for (key, value) in db.keys("push-outbox:")? {
                    if value["subscriptionId"] == id {
                        db.delete(&key)?;
                    }
                }
                Ok(json!({ "ok": true }))
            })
            .await
    }

    pub async fn flush(&self, service: &Service) -> Result<()> {
        let Ok(_delivery) = self.delivery.try_lock() else {
            return Ok(());
        };
        for (key, delivery) in service.store.keys_as::<Delivery>("push-outbox:").await? {
            if delivery.next_at.unwrap_or(0) > now() {
                continue;
            }
            self.deliver(service, key, delivery).await?;
        }
        Ok(())
    }

    async fn awaits_answer(service: &Service, delivery: &Delivery) -> Result<bool> {
        let key = format!(
            "{}{}",
            crate::chats::question_prefix(delivery.chat_id.as_str().unwrap_or("")),
            delivery.question_id.as_str().unwrap_or("")
        );
        let question = service.store.kv(&key).await?;
        Ok(question.is_some_and(|question| question["status"] == QuestionStatus::Pending))
    }

    async fn deliver(&self, service: &Service, key: String, mut delivery: Delivery) -> Result<()> {
        let device = format!("push-device:{}", delivery.subscription_id);
        let subscription = service.vault.get(&device).await?;
        let wanted = delivery.is_alert() || Self::awaits_answer(service, &delivery).await?;
        let expired = delivery.expires_at.unwrap_or(0) < now();
        let Some(subscription) = subscription.filter(|_| wanted && !expired) else {
            return service.store.delete(&key).await;
        };
        // Recheck the allowlist before any network request, including migrated records.
        let Ok(subscription) = self::subscription(&subscription) else {
            self.unsubscribe(service, &delivery.subscription_id).await?;
            return Ok(());
        };
        let keys = self.keys(service).await?;
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            send(service, &subscription, &keys, &delivery),
        )
        .await;
        match result {
            Ok(Ok(())) => service.store.delete(&key).await,
            Ok(Err(WebPushError::EndpointNotValid(_) | WebPushError::EndpointNotFound(_))) => {
                self.unsubscribe(service, &delivery.subscription_id).await?;
                Ok(())
            }
            _ => {
                delivery.retry_later();
                service.store.set_as(&key, delivery, None).await
            }
        }
    }
}

async fn send(
    service: &Service,
    subscription: &PushSubscription,
    keys: &VapidKeys,
    delivery: &Delivery,
) -> std::result::Result<(), WebPushError> {
    let info = SubscriptionInfo::new(
        &subscription.endpoint,
        &subscription.keys.p256dh,
        &subscription.keys.auth,
    );
    let mut signature = VapidSignatureBuilder::from_base64(&keys.private_key, &info)?;
    let contact = if service.config.public_url.starts_with("https:") {
        &service.config.public_url
    } else {
        "mailto:notifications@example.com"
    };
    signature.add_claim("sub", contact);
    let payload = delivery.payload().to_string();
    let mut message = WebPushMessageBuilder::new(&info);
    message.set_vapid_signature(signature.build()?);
    message.set_ttl(3600);
    message.set_urgency(Urgency::High);
    message.set_payload(ContentEncoding::Aes128Gcm, payload.as_bytes());
    HyperWebPushClient::new().send(message.build()?).await
}
