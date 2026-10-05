mod common;

use common::{Fixture, login};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

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
