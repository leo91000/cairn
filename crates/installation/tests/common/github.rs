//! A local GitHub for the bundled GitHub package: plugin HTTP for its declared hosts
//! is routed here. Answers come from `github-state.json` beside the responses folder,
//! then from files named after endpoints (`/`, `?`, `=` and `%` as `_`), and every
//! request is recorded with the token it carried.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use cairn_installation::{plugins::connections, service::Service, validation::text};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::task::JoinHandle;

pub const USER_TOKEN: &str = "gho_fixture_device_token_1234567890";

pub struct Github {
    pub origin: String,
    pub calls: Arc<Mutex<Vec<Value>>>,
    root: PathBuf,
    _server: JoinHandle<()>,
}

impl Github {
    /// Serves answers kept under `root` (`github-state.json`, `github/…`).
    pub async fn start(root: &Path) -> Self {
        std::fs::create_dir_all(root.join("github")).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let state = (root.to_owned(), calls.clone());
        let app = Router::new().fallback(move |request: Request<Body>| {
            let (root, calls) = state.clone();
            async move { answer(&root, &calls, request).await }
        });
        let (origin, server) = super::serve_locally(app).await;
        Self {
            origin,
            calls,
            root: root.to_owned(),
            _server: server,
        }
    }

    /// Routes a service's plugin HTTP here and runs its workers from the built binary.
    pub fn attach(&self, s: &Service) {
        s.plugins.route_http_to(Some(self.origin.clone()));
        s.plugins
            .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    }

    pub fn calls(&self) -> Vec<Value> {
        self.calls.lock().unwrap().clone()
    }

    /// Calls other than the account checks instance forms make.
    pub fn api_calls(&self) -> Vec<Value> {
        self.calls()
            .into_iter()
            .filter(|call| call["path"] != "user")
            .collect()
    }

    pub fn state(&self, value: &Value) {
        std::fs::write(self.root.join("github-state.json"), value.to_string()).unwrap();
    }

    /// Answers `endpoint` (as `repos/x/y?recursive=1`) with `value`.
    pub fn respond(&self, endpoint: &str, value: &Value) {
        let name = endpoint.replace(['/', '?', '=', '%'], "_");
        std::fs::write(self.root.join("github").join(name), value.to_string()).unwrap();
    }
}

/// A GitHub instance saved through the package's own form, granted to `agents`.
pub async fn instance(s: &Service, name: &str, token: &str, agents: &[&str]) -> String {
    super::relay_fixture::claimed(s).await.unwrap();
    let saved = connections::save(
        s,
        "github",
        &json!({ "name": name, "enabled": true, "token": token, "agentIds": agents }),
        None,
    )
    .await
    .unwrap();
    text(&saved, "id").to_owned()
}

fn state(root: &Path) -> Value {
    std::fs::read(root.join("github-state.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| json!({}))
}

fn json_response(status: StatusCode, value: &Value) -> Response {
    (
        status,
        [
            ("content-type", "application/json"),
            ("x-oauth-scopes", "repo, workflow, read:org"),
            ("x-ratelimit-remaining", "4999"),
        ],
        value.to_string(),
    )
        .into_response()
}

fn not_found() -> Response {
    json_response(StatusCode::NOT_FOUND, &json!({ "message": "Not Found" }))
}

async fn answer(root: &Path, calls: &Mutex<Vec<Value>>, request: Request<Body>) -> Response {
    let method = request.method().to_string();
    let uri = request.uri().clone();
    let path = uri.path().trim_start_matches('/').to_owned();
    let full = uri.path_and_query().map_or(path.clone(), |value| {
        value.as_str().trim_start_matches('/').to_owned()
    });
    let headers = request.headers().clone();
    let token = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim_start_matches("Bearer ").to_owned());
    let bytes = to_bytes(request.into_body(), 1_000_000)
        .await
        .unwrap_or_default();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    calls.lock().unwrap().push(json!({
        "method": method,
        "path": full,
        "token": token,
        "body": if body.is_null() { json!(String::from_utf8_lossy(&bytes)) } else { body.clone() },
        "accept": headers.get("accept").and_then(|value| value.to_str().ok()),
        "userAgent": headers.get("user-agent").and_then(|value| value.to_str().ok()),
    }));
    let state = state(root);

    if let Some(gate) = state.get("gate")
        && gate["path"] == full.as_str()
    {
        std::fs::write(text(gate, "started"), "").unwrap();
        while !Path::new(text(gate, "released")).exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    while state["waitForRelease"] == true && !root.join("github-release").exists() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    if let Some(delay) = state["delay"].as_u64().filter(|delay| *delay > 0) {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    if state["error"] == true {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "fixture-secret-must-not-leak",
        )
            .into_response();
    }

    // Device sign-in.
    if path == "login/device/code" {
        return json_response(
            StatusCode::OK,
            &json!({
                "device_code": "fixture-device-code",
                "user_code": "WDJB-MJHT",
                "verification_uri": "https://github.com/login/device",
                "expires_in": 900,
                "interval": 1,
            }),
        );
    }
    if path == "login/oauth/access_token" {
        let answer = match state["device"].as_str() {
            Some("approved") => {
                json!({ "access_token": USER_TOKEN, "token_type": "bearer", "scope": "repo,workflow,read:org" })
            }
            Some("denied") => json!({ "error": "access_denied" }),
            _ => json!({ "error": "authorization_pending" }),
        };
        return json_response(StatusCode::OK, &answer);
    }

    let file = root
        .join("github")
        .join(full.replace(['/', '?', '=', '%'], "_"));
    if let Ok(contents) = std::fs::read(&file) {
        let value: Value = serde_json::from_slice(&contents).unwrap();
        let raw = headers
            .get("accept")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|accept| accept.contains("raw"));
        if raw && value["encoding"] == "base64" {
            let content = text(&value, "content").replace(['\n', ' '], "");
            return (StatusCode::OK, STANDARD.decode(content).unwrap()).into_response();
        }
        return json_response(StatusCode::OK, &value);
    }

    if path == "user" {
        if token
            .as_deref()
            .is_some_and(|token| token.starts_with("revoked"))
        {
            return json_response(
                StatusCode::UNAUTHORIZED,
                &json!({ "message": "Bad credentials" }),
            );
        }
        let user = state
            .get("user")
            .cloned()
            .unwrap_or_else(|| json!({ "login": "octo", "id": 583231, "name": "Octo Cat" }));
        return json_response(StatusCode::OK, &user);
    }
    if path == "user/repos" {
        return json_response(StatusCode::OK, state.get("repos").unwrap_or(&json!([])));
    }
    if path == "graphql" && state.get("pr").is_some() {
        if state["graphqlError"] == true {
            return json_response(
                StatusCode::OK,
                &json!({ "errors": [{ "message": "fixture-secret-must-not-leak" }] }),
            );
        }
        let query = text(&body, "query");
        let cursor = body["variables"]["cursor"].as_str();
        let pr = match cursor {
            Some(cursor) => state["pages"][cursor].clone(),
            None if !query.contains("contexts(") => state["finalPr"].clone(),
            None => state["pr"].clone(),
        };
        let pr = if pr.is_null() {
            state["pr"].clone()
        } else {
            pr
        };
        return json_response(
            StatusCode::OK,
            &json!({ "data": { "repository": { "pullRequest": pr } } }),
        );
    }
    if path.contains("/rules/branches/") {
        let page = uri
            .query()
            .and_then(|query| {
                query
                    .split('&')
                    .find_map(|pair| pair.strip_prefix("page="))
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "1".into());
        let earlier = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| text(call, "path").contains("/rules/branches/"))
            .count();
        let rules = state["rulePages"]
            .get(&page)
            .cloned()
            .or_else(|| {
                (earlier > 1)
                    .then(|| state["rulesAfter"].clone())
                    .filter(|v| !v.is_null())
            })
            .or_else(|| state.get("rules").cloned())
            .unwrap_or_else(|| json!([]));
        return json_response(StatusCode::OK, &rules);
    }
    if path.contains("/actions/runs/") && state.get("response").is_some() {
        return json_response(StatusCode::OK, &state["response"]);
    }
    if let Some(name) = path.strip_prefix("repos/")
        && name.matches('/').count() == 1
        && let Some(repository) = state["repositories"].get(name)
    {
        return json_response(StatusCode::OK, repository);
    }
    not_found()
}
