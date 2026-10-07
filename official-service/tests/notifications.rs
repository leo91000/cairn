mod common;

use common::{Fixture, login};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;

fn request(
    app: &Fixture,
    cookie: &str,
    session: &Value,
    method: Method,
    path: &str,
) -> reqwest::RequestBuilder {
    app.client
        .request(method, format!("{}{path}", app.url))
        .header("origin", &app.url)
        .header("cookie", cookie)
        .header("x-csrf-token", session["csrf"].as_str().unwrap())
}

fn subscription(endpoint: &str) -> Value {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    json!({
        "endpoint": endpoint,
        "keys": {
            "p256dh": URL_SAFE_NO_PAD.encode([4; 65]),
            "auth": URL_SAFE_NO_PAD.encode([7; 16]),
        },
    })
}

#[tokio::test]
async fn web_push_devices_belong_to_the_signed_in_leo_account() {
    let app = Fixture::new().await;
    let (cookie, session) = login(&app, "owner@example.test").await;
    let (other_cookie, other_session) = login(&app, "other@example.test").await;
    let path = "/api/account/notifications/subscriptions";
    let response = request(&app, &cookie, &session, Method::POST, path)
        .json(&subscription("https://fcm.googleapis.com/device-one"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let registered: Value = response.json().await.unwrap();
    let device = format!("{path}/{}", registered["id"].as_str().unwrap());
    let response = request(&app, &other_cookie, &other_session, Method::GET, &device)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({ "registered": false })
    );
    assert_eq!(
        request(&app, &other_cookie, &other_session, Method::DELETE, &device)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let response = request(&app, &cookie, &session, Method::GET, &device)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({ "registered": true })
    );
    assert_eq!(
        request(&app, &cookie, &session, Method::DELETE, &device)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let response = request(&app, &cookie, &session, Method::GET, &device)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({ "registered": false })
    );
    app.close().await;
}

#[derive(Default)]
struct PushMailbox {
    messages: std::sync::Mutex<Vec<(String, Value)>>,
    failures_remaining: std::sync::atomic::AtomicUsize,
    panics_remaining: std::sync::atomic::AtomicUsize,
    unavailable_attempts: std::sync::atomic::AtomicUsize,
    send_delay: std::time::Duration,
}

#[async_trait::async_trait]
impl leo_official_service::PushSender for PushMailbox {
    fn public_key(&self) -> &str {
        "fixture-public-key"
    }

    async fn send(
        &self,
        subscription: &leo_official_service::PushSubscription,
        payload: &Value,
    ) -> Result<(), leo_official_service::PushError> {
        if self
            .panics_remaining
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |remaining| remaining.checked_sub(1),
            )
            .is_ok()
        {
            panic!("fixture push provider task failure");
        }

        if !self.send_delay.is_zero() {
            tokio::time::sleep(self.send_delay).await;
        }

        if let Some(status) = subscription
            .endpoint
            .rsplit('/')
            .next()
            .and_then(|path| path.strip_prefix("status-"))
        {
            let code: u16 = status.parse().unwrap();
            let error =
                web_push::request_builder::parse_response(code.try_into().unwrap(), b"{}".to_vec())
                    .unwrap_err();
            return Err(error.into());
        }

        if subscription.endpoint.ends_with("/expired") {
            return Err(leo_official_service::PushError::Gone);
        }
        if subscription.endpoint.ends_with("/unavailable") {
            self.unavailable_attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            return Err(leo_official_service::PushError::Unavailable);
        }
        if self
            .failures_remaining
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |remaining| remaining.checked_sub(1),
            )
            .is_ok()
        {
            return Err(leo_official_service::PushError::Unavailable);
        }
        self.messages
            .lock()
            .unwrap()
            .push((subscription.endpoint.clone(), payload.clone()));
        Ok(())
    }
}

async fn register(app: &Fixture, cookie: &str, session: &Value, endpoint: &str) -> String {
    let response = request(
        app,
        cookie,
        session,
        Method::POST,
        "/api/account/notifications/subscriptions",
    )
    .json(&subscription(endpoint))
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn invite(relay: &common::RelayedInstallation, email: &str) -> (String, Value) {
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let response = request(
        &relay.app,
        &relay.cookie,
        &relay.session,
        Method::POST,
        &format!("/api/installations/{id}/sharing/invitations"),
    )
    .json(&json!({ "email": email }))
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let invitation: Value = response.json().await.unwrap();
    let (cookie, session) = login(&relay.app, email).await;
    assert_eq!(
        request(
            &relay.app,
            &cookie,
            &session,
            Method::POST,
            &format!(
                "/api/account/invitations/{}/accept",
                invitation["id"].as_str().unwrap()
            )
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::NO_CONTENT
    );
    (cookie, session)
}

async fn chat_run(relay: &common::RelayedInstallation) -> (String, String) {
    let response = request(
        &relay.app,
        &relay.cookie,
        &relay.session,
        Method::POST,
        &format!("{}/chats", relay.base.strip_prefix(&relay.app.url).unwrap()),
    )
    .json(&json!({}))
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut chat: Value = response.json().await.unwrap();
    let chat_id = chat["id"].as_str().unwrap().to_owned();
    let run_id = uuid::Uuid::new_v4().to_string();
    chat["runId"] = run_id.clone().into();
    // The execution adapter supplies a running turn; assertions cross the official API or push provider.
    relay.installation.store.put("chats", chat).await.unwrap();
    let run = json!({
        "id": run_id,
        "taskId": chat_id,
        "status": "running",
        "trigger": "chat",
        "createdAt": leo_agent_manager::config::now(),
    });
    relay
        .installation
        .store
        .transaction(move |db| db.add_run(&run, None))
        .await
        .unwrap();
    (chat_id, run_id)
}

async fn question(relay: &common::RelayedInstallation, run: &str, id: &str) {
    relay
        .installation
        .question_receive(
            run,
            json!({
                "id": id,
                "blocking": true,
                "fields": [{ "id": "choice", "title": "Private question content" }],
            }),
        )
        .await
        .unwrap();
}

async fn wait_pushes(mail: &PushMailbox, count: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if mail.messages.lock().unwrap().len() >= count {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the official service must deliver the installation event through the push provider");
    assert_eq!(mail.messages.lock().unwrap().len(), count);
}

#[tokio::test]
async fn relayed_questions_and_alerts_target_only_current_installation_accounts() {
    let mail = std::sync::Arc::new(PushMailbox::default());
    let relay = common::RelayedInstallation::with_push(mail.clone()).await;
    let (member_cookie, member_session) = invite(&relay, "member@example.test").await;
    let (stranger_cookie, stranger_session) = login(&relay.app, "stranger@example.test").await;
    let owner = "https://fcm.googleapis.com/owner";
    let member = "https://fcm.googleapis.com/member";
    register(&relay.app, &relay.cookie, &relay.session, owner).await;
    register(&relay.app, &member_cookie, &member_session, member).await;
    register(
        &relay.app,
        &stranger_cookie,
        &stranger_session,
        "https://fcm.googleapis.com/stranger",
    )
    .await;
    let (chat, run) = chat_run(&relay).await;
    question(&relay, &run, &"a".repeat(64)).await;
    wait_pushes(&mail, 2).await;
    let sent = mail.messages.lock().unwrap().clone();
    let mut endpoints: Vec<_> = sent.iter().map(|entry| entry.0.as_str()).collect();
    endpoints.sort();
    assert_eq!(endpoints, vec![member, owner]);
    let installation = relay.session["installations"][0]["id"].as_str().unwrap();
    for (_, payload) in sent {
        assert_eq!(payload["installationId"], installation);
        assert_eq!(payload["chatId"], chat);
        assert_eq!(payload["questionId"], "a".repeat(64));
        assert!(!payload.to_string().contains("Private question content"));
    }
    let member_id = member_session["account"]["id"].as_str().unwrap();
    assert_eq!(
        request(
            &relay.app,
            &relay.cookie,
            &relay.session,
            Method::DELETE,
            &format!("/api/installations/{installation}/sharing/members/{member_id}")
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::NO_CONTENT
    );
    question(&relay, &run, &"b".repeat(64)).await;
    wait_pushes(&mail, 3).await;
    assert_eq!(mail.messages.lock().unwrap()[2].0, owner);
    leo_agent_manager::nodes::alerts::raise(
        &relay.installation,
        &run,
        "unavailable",
        "Node unavailable",
        "Open Leo to review.",
    )
    .await
    .unwrap();
    wait_pushes(&mail, 4).await;
    let alert = mail.messages.lock().unwrap()[3].clone();
    assert_eq!(alert.0, owner);
    assert_eq!(alert.1["title"], "Node unavailable");
    assert_eq!(alert.1["installationId"], installation);
    assert!(alert.1["alertId"].is_string());
    relay.close().await;
}

#[tokio::test]
async fn notification_configuration_exposes_only_the_public_operator_key() {
    let app = Fixture::with_push(std::sync::Arc::new(PushMailbox::default())).await;
    let (cookie, session) = login(&app, "owner@example.test").await;
    let response = request(
        &app,
        &cookie,
        &session,
        Method::GET,
        "/api/account/notifications",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({ "publicKey": "fixture-public-key" })
    );
    assert_eq!(
        app.client
            .get(format!("{}/api/account/notifications", app.url))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    app.close().await;
}

#[tokio::test]
async fn device_limits_and_subscription_writes_are_isolated_per_account() {
    let app = Fixture::new().await;
    let (cookie, session) = login(&app, "owner@example.test").await;
    let path = "/api/account/notifications/subscriptions";
    let input = subscription("https://fcm.googleapis.com/device-zero");
    assert_eq!(
        app.client
            .post(format!("{}{path}", app.url))
            .header("origin", &app.url)
            .header("cookie", &cookie)
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    for endpoint in [
        "https://localhost/private",
        "http://fcm.googleapis.com/device",
        "https://fcm.googleapis.com@evil.test/device",
    ] {
        assert_eq!(
            request(&app, &cookie, &session, Method::POST, path)
                .json(&subscription(endpoint))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    for index in 0..49 {
        register(
            &app,
            &cookie,
            &session,
            &format!("https://fcm.googleapis.com/device-{index}"),
        )
        .await;
    }
    let (a, b) = tokio::join!(
        request(&app, &cookie, &session, Method::POST, path)
            .json(&subscription("https://fcm.googleapis.com/final-a"))
            .send(),
        request(&app, &cookie, &session, Method::POST, path)
            .json(&subscription("https://fcm.googleapis.com/final-b"))
            .send(),
    );
    let mut statuses = vec![a.unwrap().status().as_u16(), b.unwrap().status().as_u16()];
    statuses.sort();
    assert_eq!(statuses, vec![200, 409]);
    register(
        &app,
        &cookie,
        &session,
        "https://fcm.googleapis.com/device-0",
    )
    .await;
    let (other_cookie, other_session) = login(&app, "other@example.test").await;
    let id = register(
        &app,
        &other_cookie,
        &other_session,
        "https://fcm.googleapis.com/device-0",
    )
    .await;
    assert_eq!(
        request(
            &app,
            &cookie,
            &session,
            Method::GET,
            &format!("{path}/{id}")
        )
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap(),
        json!({ "registered": false })
    );
    register(
        &app,
        &cookie,
        &session,
        "https://fcm.googleapis.com/replacement",
    )
    .await;
    app.close().await;
}

async fn pause(relay: &mut common::RelayedInstallation) {
    relay.stop.cancel();
    (&mut relay.connector).await.unwrap().unwrap();
}

fn resume(relay: &mut common::RelayedInstallation, router: axum::Router) {
    relay.stop = tokio_util::sync::CancellationToken::new();
    relay.connector = tokio::spawn(leo_agent_manager::relay::connect(
        relay
            .installation
            .config
            .data_dir
            .join("installation-relay"),
        router,
        relay.installation.clone(),
        relay.stop.clone(),
    ));
}

#[tokio::test]
async fn queued_events_exclude_removed_members_after_reconnection() {
    let mail = std::sync::Arc::new(PushMailbox::default());
    let mut relay = common::RelayedInstallation::with_push(mail.clone()).await;
    let (cookie, session) = invite(&relay, "member@example.test").await;
    register(
        &relay.app,
        &relay.cookie,
        &relay.session,
        "https://fcm.googleapis.com/owner",
    )
    .await;
    let device = register(
        &relay.app,
        &cookie,
        &session,
        "https://fcm.googleapis.com/member",
    )
    .await;
    let (_, run) = chat_run(&relay).await;
    pause(&mut relay).await;
    question(&relay, &run, &"c".repeat(64)).await;
    let installation = relay.session["installations"][0]["id"].as_str().unwrap();
    let member = session["account"]["id"].as_str().unwrap();
    assert_eq!(
        request(
            &relay.app,
            &relay.cookie,
            &relay.session,
            Method::DELETE,
            &format!("/api/installations/{installation}/sharing/members/{member}")
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::NO_CONTENT
    );
    let router = leo_agent_manager::http::router(relay.installation.clone())
        .await
        .unwrap();
    resume(&mut relay, router);
    wait_pushes(&mail, 1).await;
    assert_eq!(
        mail.messages.lock().unwrap()[0].0,
        "https://fcm.googleapis.com/owner"
    );
    let registered: Value = request(
        &relay.app,
        &cookie,
        &session,
        Method::GET,
        &format!("/api/account/notifications/subscriptions/{device}"),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(registered, json!({ "registered": true }));
    relay.close().await;
}

#[tokio::test]
async fn detached_installations_cannot_forward_queued_events() {
    let mail = std::sync::Arc::new(PushMailbox::default());
    let mut relay = common::RelayedInstallation::with_push(mail.clone()).await;
    register(
        &relay.app,
        &relay.cookie,
        &relay.session,
        "https://fcm.googleapis.com/owner",
    )
    .await;
    let (_, run) = chat_run(&relay).await;
    pause(&mut relay).await;
    question(&relay, &run, &"d".repeat(64)).await;
    let installation = relay.session["installations"][0]["id"].as_str().unwrap();
    assert_eq!(
        request(
            &relay.app,
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("/api/installations/{installation}/detach")
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::NO_CONTENT
    );
    let router = leo_agent_manager::http::router(relay.installation.clone())
        .await
        .unwrap();
    resume(&mut relay, router);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !relay.connector.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the detached machine credential must stop reconnection");
    assert!(mail.messages.lock().unwrap().is_empty());
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    relay.close().await;
}

#[tokio::test]
async fn transient_push_failures_retry_after_reconnection_and_expired_devices_are_removed() {
    let mail = std::sync::Arc::new(PushMailbox::default());
    mail.failures_remaining
        .store(1, std::sync::atomic::Ordering::SeqCst);
    let mut relay = common::RelayedInstallation::with_push(mail.clone()).await;
    let expired = register(
        &relay.app,
        &relay.cookie,
        &relay.session,
        "https://fcm.googleapis.com/expired",
    )
    .await;
    register(
        &relay.app,
        &relay.cookie,
        &relay.session,
        "https://fcm.googleapis.com/live",
    )
    .await;
    let (_, run) = chat_run(&relay).await;
    question(&relay, &run, &"e".repeat(64)).await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let registered: Value = request(
                &relay.app,
                &relay.cookie,
                &relay.session,
                Method::GET,
                &format!("/api/account/notifications/subscriptions/{expired}"),
            )
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
            if registered["registered"] == false
                && mail
                    .failures_remaining
                    .load(std::sync::atomic::Ordering::SeqCst)
                    == 0
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect(
        "the provider's expired endpoint must be removed while transient delivery remains queued",
    );
    assert!(mail.messages.lock().unwrap().is_empty());
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    pause(&mut relay).await;
    let router = leo_agent_manager::http::router(relay.installation.clone())
        .await
        .unwrap();
    resume(&mut relay, router);
    wait_pushes(&mail, 1).await;
    assert_eq!(
        mail.messages.lock().unwrap()[0].0,
        "https://fcm.googleapis.com/live"
    );
    relay.close().await;
}

#[tokio::test]
async fn a_failing_device_does_not_starve_newer_events_on_healthy_devices() {
    let mail = std::sync::Arc::new(PushMailbox {
        send_delay: std::time::Duration::from_millis(5),
        ..PushMailbox::default()
    });
    let mut relay = common::RelayedInstallation::with_push(mail.clone()).await;
    for endpoint in [
        "https://fcm.googleapis.com/unavailable",
        "https://fcm.googleapis.com/healthy",
    ] {
        register(&relay.app, &relay.cookie, &relay.session, endpoint).await;
    }
    let (_, run) = chat_run(&relay).await;
    pause(&mut relay).await;
    for index in 0..400 {
        question(&relay, &run, &format!("{index:064x}")).await;
    }
    let newest = "f".repeat(64);
    question(&relay, &run, &newest).await;
    let router = leo_agent_manager::http::router(relay.installation.clone())
        .await
        .unwrap();
    resume(&mut relay, router);

    // This is a bounded backlog/fairness check, not a throughput benchmark.
    // Slow hosts still have to deliver every distinct event and suppress retries.
    tokio::time::timeout(std::time::Duration::from_secs(120), async {
        loop {
            let newest_delivered = mail.messages.lock().unwrap().iter()
                .any(|(_, payload)| payload["questionId"] == newest);

            if newest_delivered {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("a newer question must reach a healthy device even when older events cannot be acknowledged: delivered={}, unavailable_attempts={}", mail.messages.lock().unwrap().len(), mail.unavailable_attempts.load(std::sync::atomic::Ordering::SeqCst)));
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while mail
            .unavailable_attempts
            .load(std::sync::atomic::Ordering::SeqCst)
            < 410
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the unavailable device must be retried");

    {
        let messages = mail.messages.lock().unwrap();
        assert_eq!(
            messages.len(),
            401,
            "each question must reach the healthy device exactly once despite retries"
        );
        let unique_questions: std::collections::HashSet<_> = messages
            .iter()
            .map(|(_, payload)| payload["questionId"].as_str().unwrap())
            .collect();
        assert_eq!(unique_questions.len(), 401);
    }

    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    relay.close().await;
}

#[tokio::test]
async fn healthy_devices_are_not_redelivered_after_a_partial_failure_and_tunnel_reconnect() {
    let mail = std::sync::Arc::new(PushMailbox::default());
    let mut relay = common::RelayedInstallation::with_push(mail.clone()).await;
    for endpoint in [
        "https://fcm.googleapis.com/unavailable",
        "https://fcm.googleapis.com/healthy",
    ] {
        register(&relay.app, &relay.cookie, &relay.session, endpoint).await;
    }
    let (_, run) = chat_run(&relay).await;
    question(&relay, &run, &"a".repeat(64)).await;
    wait_pushes(&mail, 1).await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while mail
            .unavailable_attempts
            .load(std::sync::atomic::Ordering::SeqCst)
            < 1
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    for attempt in 2..=3 {
        pause(&mut relay).await;
        let router = leo_agent_manager::http::router(relay.installation.clone())
            .await
            .unwrap();
        resume(&mut relay, router);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while mail
                .unavailable_attempts
                .load(std::sync::atomic::Ordering::SeqCst)
                < attempt
            {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("only the unavailable device should be retried after reconnection");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    assert_eq!(
        mail.messages.lock().unwrap().len(),
        1,
        "a healthy device must not receive an already delivered event again"
    );
    relay.close().await;
}

struct SuspendedPush {
    blocked_endpoint: Option<String>,
    entered: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
    mailbox: PushMailbox,
}

impl Default for SuspendedPush {
    fn default() -> Self {
        Self {
            blocked_endpoint: None,
            entered: tokio::sync::Semaphore::new(0),
            release: tokio::sync::Semaphore::new(0),
            mailbox: PushMailbox::default(),
        }
    }
}

#[async_trait::async_trait]
impl leo_official_service::PushSender for SuspendedPush {
    fn public_key(&self) -> &str {
        "fixture-public-key"
    }

    async fn send(
        &self,
        subscription: &leo_official_service::PushSubscription,
        payload: &Value,
    ) -> Result<(), leo_official_service::PushError> {
        if self
            .blocked_endpoint
            .as_ref()
            .is_none_or(|endpoint| *endpoint == subscription.endpoint)
        {
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
        }
        leo_official_service::PushSender::send(&self.mailbox, subscription, payload).await
    }
}

#[tokio::test]
async fn a_suspended_push_provider_does_not_occupy_the_account_api_pool() {
    let push = std::sync::Arc::new(SuspendedPush::default());
    let relay = common::RelayedInstallation::with_push_and_pool_size(push.clone(), 1).await;
    register(
        &relay.app,
        &relay.cookie,
        &relay.session,
        "https://fcm.googleapis.com/healthy",
    )
    .await;
    let (_, run) = chat_run(&relay).await;
    question(&relay, &run, &"b".repeat(64)).await;
    tokio::time::timeout(std::time::Duration::from_secs(5), push.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();

    let response = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        request(
            &relay.app,
            &relay.cookie,
            &relay.session,
            Method::GET,
            "/api/account/session",
        )
        .send(),
    )
    .await
    .expect("account sessions must stay available while the provider is suspended")
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    push.release.add_permits(1);
    wait_pushes(&push.mailbox, 1).await;
    relay.close().await;
}

#[tokio::test]
async fn permanent_provider_rejections_remove_devices_but_429_and_5xx_remain_retryable() {
    let mail = std::sync::Arc::new(PushMailbox::default());
    let relay = common::RelayedInstallation::with_push(mail.clone()).await;
    let statuses = [400_u16, 401, 403, 404, 410, 413, 422, 429, 500];
    let mut devices = Vec::new();
    for status in statuses {
        let id = register(
            &relay.app,
            &relay.cookie,
            &relay.session,
            &format!("https://fcm.googleapis.com/status-{status}"),
        )
        .await;
        devices.push((status, id));
    }
    register(
        &relay.app,
        &relay.cookie,
        &relay.session,
        "https://fcm.googleapis.com/healthy",
    )
    .await;
    let (_, run) = chat_run(&relay).await;
    question(&relay, &run, &"c".repeat(64)).await;
    wait_pushes(&mail, 1).await;

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let mut correct = true;
            for (status, id) in &devices {
                let registered: Value = request(
                    &relay.app,
                    &relay.cookie,
                    &relay.session,
                    Method::GET,
                    &format!("/api/account/notifications/subscriptions/{id}"),
                )
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
                correct &= registered["registered"] == (*status == 429 || *status >= 500);
            }
            if correct {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("all 4xx except 429 must remove the device, including stale VAPID credentials");
    relay.close().await;
}

#[tokio::test]
async fn removing_a_member_waits_for_an_admitted_send_and_prevents_the_next_send() {
    let endpoint = "https://fcm.googleapis.com/member";
    let push = std::sync::Arc::new(SuspendedPush {
        blocked_endpoint: Some(endpoint.into()),
        ..SuspendedPush::default()
    });
    let relay = common::RelayedInstallation::with_push(push.clone()).await;
    let (member_cookie, member_session) = invite(&relay, "member@example.test").await;
    register(&relay.app, &member_cookie, &member_session, endpoint).await;
    register(
        &relay.app,
        &relay.cookie,
        &relay.session,
        "https://fcm.googleapis.com/owner",
    )
    .await;
    let (_, run) = chat_run(&relay).await;
    question(&relay, &run, &"d".repeat(64)).await;
    tokio::time::timeout(std::time::Duration::from_secs(5), push.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();

    let installation = relay.session["installations"][0]["id"].as_str().unwrap();
    let member = member_session["account"]["id"].as_str().unwrap();
    let removal = request(
        &relay.app,
        &relay.cookie,
        &relay.session,
        Method::DELETE,
        &format!("/api/installations/{installation}/sharing/members/{member}"),
    )
    .send();
    tokio::pin!(removal);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut removal)
            .await
            .is_err(),
        "revocation must serialize with the already admitted send"
    );
    question(&relay, &run, &"e".repeat(64)).await;

    push.release.add_permits(1);
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), &mut removal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    question(&relay, &run, &"f".repeat(64)).await;
    wait_pushes(&push.mailbox, 4).await;

    assert_eq!(
        push.entered.available_permits(),
        0,
        "no new send may be admitted for the removed member"
    );
    assert_eq!(
        push.mailbox
            .messages
            .lock()
            .unwrap()
            .iter()
            .filter(|(recipient, _)| recipient == endpoint)
            .count(),
        1
    );
    let response = request(
        &relay.app,
        &member_cookie,
        &member_session,
        Method::GET,
        &format!("/api/installations/{installation}/api/chats"),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    relay.close().await;
}

#[tokio::test]
async fn failed_push_tasks_release_the_relay_window_for_new_events() {
    let mail = std::sync::Arc::new(PushMailbox::default());
    mail.panics_remaining
        .store(4, std::sync::atomic::Ordering::SeqCst);
    let mut relay = common::RelayedInstallation::with_push(mail.clone()).await;
    register(
        &relay.app,
        &relay.cookie,
        &relay.session,
        "https://fcm.googleapis.com/healthy",
    )
    .await;
    let (_, run) = chat_run(&relay).await;
    pause(&mut relay).await;
    for id in ['a', 'b', 'c', 'd'] {
        question(&relay, &run, &id.to_string().repeat(64)).await;
    }
    let router = leo_agent_manager::http::router(relay.installation.clone())
        .await
        .unwrap();
    resume(&mut relay, router);

    // Concurrent sends may reach the provider in any order. Fill the window
    // with failing sends before adding the event that must get a new slot.
    tokio::time::timeout(Duration::from_secs(5), async {
        while mail
            .panics_remaining
            .load(std::sync::atomic::Ordering::SeqCst)
            != 0
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("all four admitted push tasks should panic");

    let new_question = "f".repeat(64);
    question(&relay, &run, &new_question).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let delivered = mail
                .messages
                .lock()
                .unwrap()
                .iter()
                .any(|(_, payload)| payload["questionId"] == new_question);
            if delivered {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("a new event should be delivered after the failed tasks release the window");

    let deliveries = mail
        .messages
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, payload)| payload["questionId"] == new_question)
        .count();
    assert_eq!(deliveries, 1);
    relay.close().await;
}

#[tokio::test]
async fn android_device_registration_rotates_one_account_device_and_can_be_removed() {
    let app = Fixture::new().await;
    let (cookie, session) = login(&app, "android@example.test").await;
    let path = "/api/account/notifications/android";
    let device = uuid::Uuid::new_v4().to_string();
    let input = json!({ "deviceId": device, "token": "fixture-fcm-token-one" });
    let response = request(&app, &cookie, &session, Method::POST, path)
        .json(&input)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let registration: Value = response.json().await.unwrap();
    let response = request(&app, &cookie, &session, Method::POST, path)
        .json(&json!({ "deviceId": device, "token": "fixture-fcm-token-two" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.json::<Value>().await.unwrap(), registration);
    let id = registration["id"].as_str().unwrap();
    let registered = format!("/api/account/notifications/subscriptions/{id}");
    let (other_cookie, other_session) = login(&app, "other-android@example.test").await;
    let response = request(
        &app,
        &other_cookie,
        &other_session,
        Method::GET,
        &registered,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({ "registered": false })
    );
    let response = request(&app, &cookie, &session, Method::DELETE, &registered)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = request(&app, &cookie, &session, Method::GET, &registered)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({ "registered": false })
    );
    app.close().await;
}

#[tokio::test]
async fn native_push_reuses_current_membership_and_drops_removed_members_immediately() {
    let mail = std::sync::Arc::new(PushMailbox::default());
    let relay = common::RelayedInstallation::with_push(mail.clone()).await;
    let (member_cookie, member_session) = invite(&relay, "native-member@example.test").await;
    for (cookie, session, token) in [
        (&relay.cookie, &relay.session, "fixture-native-owner"),
        (&member_cookie, &member_session, "fixture-native-member"),
    ] {
        let response = request(
            &relay.app,
            cookie,
            session,
            Method::POST,
            "/api/account/notifications/android",
        )
        .json(&json!({ "deviceId": uuid::Uuid::new_v4().to_string(), "token": token }))
        .send()
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    let (chat, run) = chat_run(&relay).await;
    question(&relay, &run, &"c".repeat(64)).await;
    wait_pushes(&mail, 2).await;
    let mut endpoints: Vec<_> = mail
        .messages
        .lock()
        .unwrap()
        .iter()
        .map(|message| message.0.clone())
        .collect();
    endpoints.sort();
    assert_eq!(
        endpoints,
        ["fcm:fixture-native-member", "fcm:fixture-native-owner"]
    );
    let installation = relay.session["installations"][0]["id"].as_str().unwrap();
    for (_, payload) in mail.messages.lock().unwrap().iter() {
        assert_eq!(payload["installationId"], installation);
        assert_eq!(payload["chatId"], chat);
        assert!(!payload.to_string().contains("Private question content"));
    }
    let member = member_session["account"]["id"].as_str().unwrap();
    let response = request(
        &relay.app,
        &relay.cookie,
        &relay.session,
        Method::DELETE,
        &format!("/api/installations/{installation}/sharing/members/{member}"),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    question(&relay, &run, &"d".repeat(64)).await;
    wait_pushes(&mail, 3).await;
    assert_eq!(
        mail.messages.lock().unwrap()[2].0,
        "fcm:fixture-native-owner"
    );
    relay.close().await;
}

#[tokio::test]
async fn one_android_device_receives_both_installations_without_installation_subscriptions() {
    use futures_util::{SinkExt, StreamExt};
    use leo_relay_protocol::{Frame, NotificationEvent, NotificationKind};
    use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

    let mail = std::sync::Arc::new(PushMailbox::default());
    let app = Fixture::with_push(mail.clone()).await;
    let (cookie, session) = login(&app, "native-owner@example.test").await;
    let registered = request(
        &app,
        &cookie,
        &session,
        Method::POST,
        "/api/account/notifications/android",
    )
    .json(&json!({
        "deviceId": uuid::Uuid::new_v4().to_string(),
        "token": "fixture-global-device",
    }))
    .send()
    .await
    .unwrap();
    assert_eq!(registered.status(), StatusCode::OK);
    let mut installations = Vec::new();

    for name in ["Home", "Work"] {
        let code: Value = request(
            &app,
            &cookie,
            &session,
            Method::POST,
            "/api/installations/claim-code",
        )
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        let claim: Value = app
            .client
            .post(format!("{}/api/relay/claim", app.url))
            .json(&json!({
                "code": code["code"],
                "name": name,
                "protocol": 3,
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let id = claim["installationId"].as_str().unwrap();
        installations.push(id.to_owned());
        let mut connect = format!("{}/api/relay/{id}/connect", app.url.replace("http:", "ws:"))
            .into_client_request()
            .unwrap();
        connect.headers_mut().insert(
            "authorization",
            format!("Bearer {}", claim["token"].as_str().unwrap())
                .parse()
                .unwrap(),
        );
        let (mut socket, _) = tokio_tungstenite::connect_async(connect).await.unwrap();
        socket
            .send(Message::Text(
                serde_json::to_string(&Frame::Hello { versions: vec![3] })
                    .unwrap()
                    .into(),
            ))
            .await
            .unwrap();
        let welcome = socket.next().await.unwrap().unwrap();
        assert!(matches!(
            serde_json::from_str::<Frame>(welcome.to_text().unwrap()).unwrap(),
            Frame::Welcome { version: 3 }
        ));
        let event = Frame::Notification(NotificationEvent {
            id: "e".repeat(64),
            chat_id: uuid::Uuid::new_v4().to_string(),
            kind: NotificationKind::Question {
                question_id: "e".repeat(64),
            },
        });
        socket
            .send(Message::Text(serde_json::to_string(&event).unwrap().into()))
            .await
            .unwrap();
        let acknowledgement = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let message = socket.next().await.unwrap().unwrap();
                if message.is_text() {
                    break message;
                }
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            serde_json::from_str::<Frame>(acknowledgement.to_text().unwrap()).unwrap(),
            Frame::NotificationAck {
                delivered: true,
                ..
            }
        ));
        socket.close(None).await.unwrap();
    }

    let messages = mail.messages.lock().unwrap().clone();
    assert_eq!(messages.len(), 2);
    for (endpoint, payload) in &messages {
        assert_eq!(endpoint, "fcm:fixture-global-device");
        assert_eq!(payload["accountId"], session["account"]["id"]);
    }
    assert_eq!(
        messages
            .iter()
            .map(|(_, payload)| payload["installationId"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        installations
    );
    app.close().await;
}
