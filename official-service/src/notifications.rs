use super::{ApiError, Service, digest, installations};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, Method, StatusCode},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx_core::{query::query, query_as::query_as};

const MAX_DEVICES: i64 = 50;

#[derive(Debug)]
pub enum PushError {
    Gone,
    Unavailable,
}

#[async_trait::async_trait]
pub trait PushSender: Send + Sync {
    fn public_key(&self) -> &str;

    async fn send(&self, subscription: &PushSubscription, payload: &Value)
    -> Result<(), PushError>;
}

#[derive(Clone, Deserialize, Serialize)]
pub struct PushKeys {
    pub p256dh: String,
    pub auth: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct PushSubscription {
    pub endpoint: String,
    pub keys: PushKeys,
}

fn valid_key(value: &str, length: usize, maximum: usize) -> bool {
    value.len() <= maximum
        && URL_SAFE_NO_PAD
            .decode(value.trim_end_matches('='))
            .is_ok_and(|bytes| bytes.len() == length)
}

fn valid_subscription(input: &PushSubscription) -> bool {
    let Ok(url) = url::Url::parse(&input.endpoint) else {
        return false;
    };
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
    input.endpoint.len() <= 4096
        && url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.fragment().is_none()
        && allowed
        && valid_key(&input.keys.p256dh, 65, 100)
        && valid_key(&input.keys.auth, 16, 30)
}

pub(super) async fn subscribe(
    State(service): State<Service>,
    headers: HeaderMap,
    Json(input): Json<PushSubscription>,
) -> Result<Json<Value>, ApiError> {
    let account = installations::account(&service, &headers, &Method::POST).await?;

    if !valid_subscription(&input) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Invalid or unsupported browser push subscription",
        ));
    }

    let id = digest(&input.endpoint);
    let mut transaction = service.pool.begin().await?;
    // Serialize additions for this account so concurrent devices cannot exceed its limit.
    query("SELECT id FROM leo_accounts WHERE id = $1 FOR UPDATE")
        .bind(&account)
        .execute(&mut *transaction)
        .await?;

    let (count,): (i64,) =
        query_as("SELECT count(*) FROM notification_devices WHERE account_id = $1 AND id <> $2")
            .bind(&account)
            .bind(&id)
            .fetch_one(&mut *transaction)
            .await?;

    if count >= MAX_DEVICES {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Too many notification devices are registered",
        ));
    }

    // A browser endpoint has one current account, even after switching accounts.
    query("INSERT INTO notification_devices (id, account_id, endpoint, p256dh, auth) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (id) DO UPDATE SET account_id = EXCLUDED.account_id, endpoint = EXCLUDED.endpoint, p256dh = EXCLUDED.p256dh, auth = EXCLUDED.auth")
        .bind(&id).bind(account).bind(input.endpoint).bind(input.keys.p256dh).bind(input.keys.auth)
        .execute(&mut *transaction).await?;

    transaction.commit().await?;
    Ok(Json(json!({ "id": id })))
}

pub(super) async fn registered(
    State(service): State<Service>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let account = installations::account(&service, &headers, &Method::GET).await?;
    let (registered,): (bool,) = query_as(
        "SELECT EXISTS(SELECT 1 FROM notification_devices WHERE id = $1 AND account_id = $2)",
    )
    .bind(id)
    .bind(account)
    .fetch_one(&service.pool)
    .await?;
    Ok(Json(json!({ "registered": registered })))
}

pub(super) async fn unsubscribe(
    State(service): State<Service>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let account = installations::account(&service, &headers, &Method::DELETE).await?;
    query("DELETE FROM notification_devices WHERE id = $1 AND account_id = $2")
        .bind(id)
        .bind(account)
        .execute(&service.pool)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

fn payload(installation: &str, event: &leo_relay_protocol::NotificationEvent) -> Option<Value> {
    use leo_relay_protocol::NotificationKind;
    if uuid::Uuid::parse_str(&event.chat_id).is_err() {
        return None;
    }
    let mut payload = match &event.kind {
        NotificationKind::Question { question_id } => {
            if event.id != *question_id
                || question_id.len() != 64
                || !question_id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return None;
            }
            json!({
                "title": "Your agent has a question",
                "body": "Open the chat to answer.",
                "questionId": question_id,
            })
        }
        NotificationKind::Alert {
            alert_id,
            title,
            body,
        } => {
            if event.id != *alert_id
                || uuid::Uuid::parse_str(alert_id).is_err()
                || title.chars().count() > 120
                || body.chars().count() > 300
            {
                return None;
            }
            json!({
                "title": title,
                "body": body,
                "alertId": alert_id,
            })
        }
    };
    payload["installationId"] = installation.into();
    payload["chatId"] = event.chat_id.clone().into();
    Some(payload)
}

/// No event content or recipient queue is persisted by the official service.
/// Each send serializes with sharing changes and rechecks the current device owner.
pub(super) async fn deliver(
    service: &Service,
    installation: &str,
    token_digest: &str,
    event: &leo_relay_protocol::NotificationEvent,
) -> Result<bool, ApiError> {
    let Some(payload) = payload(installation, event) else {
        return Ok(true);
    };

    let Some(sender) = &service.push else {
        return Ok(false);
    };

    let devices: Vec<(String,)> = query_as("SELECT d.id FROM notification_devices d WHERE EXISTS(SELECT 1 FROM installations i LEFT JOIN installation_members m ON m.installation_id = i.id AND m.account_id = d.account_id WHERE i.id = $1 AND i.token_digest = $2 AND i.owner_id IS NOT NULL AND (i.owner_id = d.account_id OR m.account_id = d.account_id)) ORDER BY d.id")
        .bind(installation).bind(token_digest).fetch_all(&service.pool).await?;

    let mut complete = true;
    for (id,) in devices {
        let mut transaction = service.pool.begin().await?;
        let current: Option<(String,)> = query_as("SELECT id FROM installations WHERE id = $1 AND token_digest = $2 AND owner_id IS NOT NULL FOR SHARE")
            .bind(installation).bind(token_digest).fetch_optional(&mut *transaction).await?;
        if current.is_none() {
            return Ok(true);
        }

        let device: Option<(String, String, String)> = query_as("SELECT d.endpoint, d.p256dh, d.auth FROM notification_devices d WHERE d.id = $1 AND EXISTS(SELECT 1 FROM installations i LEFT JOIN installation_members m ON m.installation_id = i.id AND m.account_id = d.account_id WHERE i.id = $2 AND (i.owner_id = d.account_id OR m.account_id = d.account_id)) FOR UPDATE OF d")
            .bind(&id).bind(installation).fetch_optional(&mut *transaction).await?;
        let Some((endpoint, p256dh, auth)) = device else {
            continue;
        };

        let subscription = PushSubscription {
            endpoint,
            keys: PushKeys { p256dh, auth },
        };

        let result = if valid_subscription(&subscription) {
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                sender.send(&subscription, &payload),
            )
            .await
        } else {
            Ok(Err(PushError::Gone))
        };

        match result {
            Ok(Ok(())) => {}
            Ok(Err(PushError::Gone)) => {
                query("DELETE FROM notification_devices WHERE id = $1")
                    .bind(&id)
                    .execute(&mut *transaction)
                    .await?;
            }
            _ => complete = false,
        }

        transaction.commit().await?;
    }

    Ok(complete)
}

pub(super) async fn configuration(
    State(service): State<Service>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    installations::account(&service, &headers, &Method::GET).await?;
    let sender = service.push.as_ref().ok_or(ApiError(
        StatusCode::SERVICE_UNAVAILABLE,
        "Push notifications are not configured on the official service",
    ))?;
    Ok(Json(json!({ "publicKey": sender.public_key() })))
}

/// The private key comes only from operator configuration, never an installation or database.
pub struct WebPushSender {
    private_key: String,
    public_key: String,
    subject: String,
}

impl WebPushSender {
    pub fn new(private_key: String, subject: String) -> Result<Self, &'static str> {
        use p256::elliptic_curve::sec1::ToEncodedPoint;
        let bytes = URL_SAFE_NO_PAD
            .decode(private_key.trim_end_matches('='))
            .map_err(|_| "Invalid official VAPID private key")?;
        let private = p256::SecretKey::from_slice(&bytes)
            .map_err(|_| "Invalid official VAPID private key")?;
        let contact = url::Url::parse(&subject).map_err(|_| "Invalid official VAPID subject")?;
        if !(contact.scheme() == "https" && contact.host_str().is_some()
            || contact.scheme() == "mailto"
                && email_address::EmailAddress::is_valid(contact.path()))
            || !contact.username().is_empty()
            || contact.password().is_some()
            || contact.fragment().is_some()
        {
            return Err(
                "Use an HTTPS URL or mailto operator contact for the official VAPID subject",
            );
        }
        Ok(Self {
            private_key: URL_SAFE_NO_PAD.encode(private.to_bytes()),
            public_key: URL_SAFE_NO_PAD
                .encode(private.public_key().to_encoded_point(false).as_bytes()),
            subject,
        })
    }
}

#[async_trait::async_trait]
impl PushSender for WebPushSender {
    fn public_key(&self) -> &str {
        &self.public_key
    }

    async fn send(
        &self,
        subscription: &PushSubscription,
        payload: &Value,
    ) -> Result<(), PushError> {
        use web_push::{
            ContentEncoding, HyperWebPushClient, SubscriptionInfo, Urgency, VapidSignatureBuilder,
            WebPushClient, WebPushError, WebPushMessageBuilder,
        };
        let send = async {
            let info = SubscriptionInfo::new(
                &subscription.endpoint,
                &subscription.keys.p256dh,
                &subscription.keys.auth,
            );
            let mut signature = VapidSignatureBuilder::from_base64(&self.private_key, &info)?;
            signature.add_claim("sub", self.subject.as_str());
            let payload = payload.to_string();
            let mut message = WebPushMessageBuilder::new(&info);
            message.set_vapid_signature(signature.build()?);
            message.set_ttl(3600);
            message.set_urgency(Urgency::High);
            message.set_payload(ContentEncoding::Aes128Gcm, payload.as_bytes());
            HyperWebPushClient::new().send(message.build()?).await
        };
        match send.await {
            Ok(()) => Ok(()),
            Err(WebPushError::EndpointNotValid(_) | WebPushError::EndpointNotFound(_)) => {
                Err(PushError::Gone)
            }
            Err(_) => Err(PushError::Unavailable),
        }
    }
}
