use crate::{PushError, PushSender, PushSubscription, WebPushSender};
use jwt_simple::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

#[derive(Deserialize)]
struct ServiceAccount {
    project_id: String,
    client_email: String,
    private_key: String,
}

/// Data-only FCM transport. Provider credentials never reach installations or clients.
pub struct FcmPushSender {
    email: String,
    key: RS256KeyPair,
    client: reqwest::Client,
    token_url: String,
    send_url: String,
    access: Mutex<Option<(String, Instant)>>,
}

impl FcmPushSender {
    pub fn new(service_account: &str) -> Result<Self, &'static str> {
        let config: ServiceAccount =
            serde_json::from_str(service_account).map_err(|_| "Invalid FCM service account")?;
        if config.project_id.is_empty()
            || !config
                .project_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        {
            return Err("Invalid FCM project ID");
        }
        Self::with_endpoints(
            service_account,
            "https://oauth2.googleapis.com/token",
            &format!(
                "https://fcm.googleapis.com/v1/projects/{}/messages:send",
                config.project_id
            ),
        )
    }

    /// Explicit transport seam for controlled HTTP tests; production uses `new`.
    pub fn with_endpoints(
        service_account: &str,
        token_url: &str,
        send_url: &str,
    ) -> Result<Self, &'static str> {
        let config: ServiceAccount =
            serde_json::from_str(service_account).map_err(|_| "Invalid FCM service account")?;
        Ok(Self {
            email: config.client_email,
            key: RS256KeyPair::from_pem(&config.private_key)
                .map_err(|_| "Invalid FCM private key")?,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(4))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| "Could not configure FCM delivery")?,
            token_url: token_url.into(),
            send_url: send_url.into(),
            access: Mutex::new(None),
        })
    }

    async fn access_token(&self) -> Result<String, PushError> {
        let mut cached = self.access.lock().await;
        if let Some((token, _)) = cached
            .as_ref()
            .filter(|(_, expires)| *expires > Instant::now())
        {
            return Ok(token.clone());
        }
        let claims = Claims::with_custom_claims(
            json!({
                "scope": "https://www.googleapis.com/auth/firebase.messaging",
            }),
            jwt_simple::prelude::Duration::from_secs(3600),
        )
        .with_issuer(&self.email)
        .with_audience(&self.token_url);
        let assertion = self.key.sign(claims).map_err(|_| PushError::Unavailable)?;
        let response: Value = self
            .client
            .post(&self.token_url)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .await
            .map_err(|_| PushError::Unavailable)?
            .error_for_status()
            .map_err(|_| PushError::Unavailable)?
            .json()
            .await
            .map_err(|_| PushError::Unavailable)?;
        let token = response["access_token"]
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or(PushError::Unavailable)?
            .to_owned();
        let lifetime = response["expires_in"].as_u64().unwrap_or(0).min(3600);
        *cached = Some((
            token.clone(),
            Instant::now() + Duration::from_secs(lifetime.saturating_sub(60)),
        ));
        Ok(token)
    }
}

#[async_trait::async_trait]
impl PushSender for FcmPushSender {
    fn public_key(&self) -> &str {
        ""
    }

    fn android_available(&self) -> bool {
        true
    }

    async fn send(
        &self,
        subscription: &PushSubscription,
        payload: &Value,
    ) -> Result<(), PushError> {
        let token = subscription
            .endpoint
            .strip_prefix("fcm:")
            .ok_or(PushError::Gone)?;
        let data: serde_json::Map<String, Value> = [
            "accountId",
            "installationId",
            "chatId",
            "questionId",
            "alertId",
        ]
        .into_iter()
        .filter_map(|name| {
            payload[name]
                .as_str()
                .map(|value| (name.into(), value.into()))
        })
        .collect();
        let access = self.access_token().await?;
        let response = self
            .client
            .post(&self.send_url)
            .bearer_auth(access)
            .json(&json!({
                "message": {
                    "token": token,
                    "data": data,
                    "android": { "priority": "HIGH", "ttl": "3600s" },
                },
            }))
            .send()
            .await
            .map_err(|_| PushError::Unavailable)?;
        if response.status().is_success() {
            return Ok(());
        }
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            *self.access.lock().await = None;
        }
        let device_error = matches!(
            response.status(),
            reqwest::StatusCode::BAD_REQUEST | reqwest::StatusCode::NOT_FOUND
        );
        let body: Value = response.json().await.map_err(|_| PushError::Unavailable)?;
        let expired = body["error"]["details"].as_array().is_some_and(|details| {
            details.iter().any(|detail| {
                detail["@type"] == "type.googleapis.com/google.firebase.fcm.v1.FcmError"
                    && detail["errorCode"] == "UNREGISTERED"
            })
        });
        Err(if device_error && expired {
            PushError::Gone
        } else {
            PushError::Unavailable
        })
    }
}

/// Routes both account device types without changing the web subscription contract.
pub struct AccountPushSender {
    pub web: Option<WebPushSender>,
    pub android: Option<FcmPushSender>,
}

#[async_trait::async_trait]
impl PushSender for AccountPushSender {
    fn public_key(&self) -> &str {
        self.web.as_ref().map_or("", PushSender::public_key)
    }

    fn android_available(&self) -> bool {
        self.android.is_some()
    }

    async fn send(
        &self,
        subscription: &PushSubscription,
        payload: &Value,
    ) -> Result<(), PushError> {
        if subscription.endpoint.starts_with("fcm:") {
            self.android
                .as_ref()
                .ok_or(PushError::Unavailable)?
                .send(subscription, payload)
                .await
        } else {
            self.web
                .as_ref()
                .ok_or(PushError::Unavailable)?
                .send(subscription, payload)
                .await
        }
    }
}
