use crate::{
    auth::hex_digest,
    config::now,
    error::{Error, Result},
    service::Service,
    store::Db,
    validation::text,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;
use web_push::{
    ContentEncoding, HyperWebPushClient, SubscriptionInfo, Urgency, VapidSignatureBuilder,
    WebPushClient, WebPushError, WebPushMessageBuilder,
};
#[derive(Clone, Default)]
pub struct Notifications {
    delivery: Arc<Mutex<()>>,
}
pub fn enqueue(db: &Db<'_>, question: &Value) -> Result<()> {
    for (key, _) in db.keys("push-device:")? {
        let subscription = key.trim_start_matches("push-device:");
        let key = format!("push-outbox:{}:{subscription}", text(question, "id"));
        if db.kv(&key)?.is_none() {
            db.set(
                &key,
                &json!({
                "subscriptionId":subscription,"questionId":question["id"],"chatId":question["chatId"],"attempts":0,"nextAt":now(),"expiresAt":now()+3600000}
                ),
                None,
            )?;
        }
    }
    Ok(())
}
/// Execution alerts need no answer, so they are delivered as long as the device is subscribed.
pub fn enqueue_alert(db: &Db<'_>, alert: &Value) -> Result<()> {
    for (key, _) in db.keys("push-device:")? {
        let subscription = key.trim_start_matches("push-device:");
        db.set(
            &format!("push-outbox:alert-{}:{subscription}", text(alert, "id")),
            &json!({
            "subscriptionId":subscription,"alertId":alert["id"],"chatId":alert["chatId"],"title":alert["title"],"body":alert["body"],"attempts":0,"nextAt":now(),"expiresAt":now()+3600000}
            ),
            None,
        )?;
    }
    Ok(())
}
fn subscription(input: Value) -> Result<Value> {
    let endpoint = text(&input, "endpoint");
    let url =
        url::Url::parse(endpoint).map_err(|_| Error::bad("Invalid notification subscription."))?;
    let host = url.host_str().unwrap_or("");
    let allowed = [
        "fcm.googleapis.com",
        "updates.push.services.mozilla.com",
        "web.push.apple.com",
    ]
    .contains(&host)
        || [".push.services.mozilla.com", ".notify.windows.com"]
            .iter()
            .any(|suffix| host.ends_with(suffix));
    if endpoint.len() > 4096
        || url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
        || !allowed
    {
        return Err(Error::bad("This browser push service is not supported."));
    }
    for (key, length, maximum) in [("p256dh", 65, 100), ("auth", 16, 30)] {
        let value = text(&input["keys"], key);
        if value.len() > maximum
            || URL_SAFE_NO_PAD
                .decode(value.trim_end_matches('='))
                .map_or(true, |bytes| bytes.len() != length)
        {
            return Err(Error::bad("Invalid notification subscription keys."));
        }
    }
    Ok(json!({
    "endpoint":endpoint,"keys":{
    "p256dh":input["keys"]["p256dh"],"auth":input["keys"]["auth"]}
    }
    ))
}
impl Notifications {
    async fn keys(&self, service: &Service) -> Result<Value> {
        let vault = service.vault.clone();
        service
            .store
            .transaction(move |db| {
                if let Some(value) = db.kv("mcp-secret:push-vapid")? {
                    return vault.decrypt("push-vapid", &value);
                }
                let private = p256::SecretKey::random(&mut p256::elliptic_curve::rand_core::OsRng);
                let value = json!({
                "privateKey":URL_SAFE_NO_PAD.encode(private.to_bytes()),"publicKey":URL_SAFE_NO_PAD.encode(private.public_key().to_encoded_point(false).as_bytes())}
                );
                vault.set_in(db, "push-vapid", &value)?;
                Ok(value)
            })
            .await
    }
    pub async fn configuration(&self, service: &Service) -> Result<Value> {
        Ok(json!({
        "publicKey":self.keys(service).await?["publicKey"]}
        ))
    }
    pub async fn subscribe(&self, service: &Service, input: Value) -> Result<Value> {
        let value = subscription(input)?;
        let id = hex_digest(text(&value, "endpoint"));
        let vault = service.vault.clone();
        service
            .store
            .transaction(move |db| {
                let key = format!("push-device:{id}");
                if db.kv(&key)?.is_none() && db.keys("push-device:")?.len() >= 50 {
                    return Err(Error::new(
                        409,
                        "Too many notification devices are registered.",
                    ));
                }
                vault.set_in(db, &key, &value)?;
                db.set(
                    &key,
                    &json!({
                    "createdAt":now()}
                    ),
                    None,
                )?;
                Ok(json!({
                "id":id}
                ))
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
                Ok(json!({
                "ok":true}
                ))
            })
            .await
    }
    pub async fn flush(&self, service: &Service) -> Result<()> {
        let Ok(_delivery) = self.delivery.try_lock() else {
            return Ok(());
        };
        for (key, mut delivery) in service.store.keys("push-outbox:").await? {
            if delivery["nextAt"].as_i64().unwrap_or(0) > now() {
                continue;
            }
            let alert = delivery["alertId"].is_string();
            let question = if alert {
                None
            } else {
                service
                    .store
                    .kv(&format!(
                        "chat-question:{}:{}",
                        text(&delivery, "chatId"),
                        text(&delivery, "questionId")
                    ))
                    .await?
            };
            let subscription = service
                .vault
                .get(&format!(
                    "push-device:{}",
                    text(&delivery, "subscriptionId")
                ))
                .await?;
            if subscription.is_none()
                || (!alert && question.is_none_or(|question| question["status"] != "pending"))
                || delivery["expiresAt"].as_i64().unwrap_or(0) < now()
            {
                service.store.delete(&key).await?;
                continue;
            }
            let subscription = subscription.unwrap();
            // Recheck the allowlist before any network request, including migrated records.
            if self::subscription(subscription.clone()).is_err() {
                self.unsubscribe(service, text(&delivery, "subscriptionId"))
                    .await?;
                continue;
            }
            let keys = self.keys(service).await?;
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                send(service, &subscription, &keys, &delivery),
            )
            .await;
            match result {
                Ok(Ok(())) => service.store.delete(&key).await?,
                Ok(Err(WebPushError::EndpointNotValid(_) | WebPushError::EndpointNotFound(_))) => {
                    self.unsubscribe(service, text(&delivery, "subscriptionId"))
                        .await?;
                }
                _ => {
                    let attempts = delivery["attempts"].as_u64().unwrap_or(0);
                    delivery["attempts"] = attempts.saturating_add(1).into();
                    delivery["nextAt"] =
                        (now() + (10000_i64 * (1_i64 << attempts.min(5))).min(300000)).into();
                    service.store.set(&key, delivery, None).await?;
                }
            }
        }
        Ok(())
    }
}
async fn send(
    service: &Service,
    subscription: &Value,
    keys: &Value,
    delivery: &Value,
) -> std::result::Result<(), WebPushError> {
    let info = SubscriptionInfo::new(
        text(subscription, "endpoint"),
        text(&subscription["keys"], "p256dh"),
        text(&subscription["keys"], "auth"),
    );
    let mut signature = VapidSignatureBuilder::from_base64(text(keys, "privateKey"), &info)?;
    signature.add_claim(
        "sub",
        if service.config.public_url.starts_with("https:") {
            &service.config.public_url
        } else {
            "mailto:notifications@example.com"
        },
    );
    let payload = if delivery["alertId"].is_string() {
        json!({"title":delivery["title"],"body":delivery["body"],"chatId":delivery["chatId"],"alertId":delivery["alertId"]})
    } else {
        json!({"title":"Your agent has a question","body":"Open the chat to answer.","chatId":delivery["chatId"],"questionId":delivery["questionId"]})
    }
    .to_string();
    let mut message = WebPushMessageBuilder::new(&info);
    message.set_vapid_signature(signature.build()?);
    message.set_ttl(3600);
    message.set_urgency(Urgency::High);
    message.set_payload(ContentEncoding::Aes128Gcm, payload.as_bytes());
    HyperWebPushClient::new().send(message.build()?).await
}
