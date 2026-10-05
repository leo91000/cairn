use super::*;
use axum::{
    extract::{Path, Query},
    response::Redirect,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
use std::time::Duration;
use url::Url;

#[derive(Clone)]
pub struct OAuthProvider {
    pub client_id: String,
    pub client_secret: String,
    pub authorization_url: String,
    pub token_url: String,
    pub userinfo_url: String,
    pub emails_url: Option<String>,
}

#[derive(Clone, Default)]
pub struct OAuthProviders {
    pub google: Option<OAuthProvider>,
    pub github: Option<OAuthProvider>,
}

impl OAuthProviders {
    fn provider(&self, name: &str) -> Result<&OAuthProvider, ApiError> {
        let configured = match name {
            "google" => &self.google,
            "github" => &self.github,
            _ => &None,
        };
        configured.as_ref().ok_or(ApiError(
            StatusCode::NOT_FOUND,
            "Sign-in provider unavailable",
        ))
    }
}

pub(super) async fn options(State(service): State<Service>) -> Json<Value> {
    Json(json!({
        "passkeys": passkeys::available(&service),
        "google": service.oauth.google.is_some(),
        "github": service.oauth.github.is_some(),
    }))
}

#[derive(Serialize, Deserialize)]
struct OAuthState {
    verifier: String,
}

pub(super) fn browser_cookie(service: &Service, name: &str, token: &str, max_age: u16) -> String {
    let secure = if service.origin.starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    format!("{name}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}")
}

pub(super) async fn start(
    State(service): State<Service>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let provider = service.oauth.provider(&name)?;
    consume_limit(&service.pool, &format!("oauth:{}", peer.ip()), 30).await?;

    // A logged-in flow is explicit linking and must be protected like any mutation.
    let account_id = if session_token(&headers).is_empty() {
        None
    } else {
        Some(methods::authenticated(&service, &headers, true).await?.0)
    };

    let state = random_token();
    let browser = random_token();
    let verifier = random_token();
    let mut url = Url::parse(&provider.authorization_url).map_err(|_| unavailable())?;
    let redirect_uri = format!("{}/api/account/oauth/{name}/callback", service.origin);
    url.query_pairs_mut()
        .append_pair("client_id", &provider.client_id)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("response_type", "code")
        .append_pair(
            "scope",
            if name == "github" {
                "user:email"
            } else {
                "openid email"
            },
        )
        .append_pair("state", &state)
        .append_pair("code_challenge_method", "S256")
        .append_pair(
            "code_challenge",
            &URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
        );

    query("INSERT INTO sign_in_challenges (id, kind, browser_digest, account_id, session_digest, state) VALUES ($1, $2, $3, $4, $5, $6)")
        .bind(&state).bind(format!("oauth:{name}")).bind(digest(&browser)).bind(account_id)
        .bind(digest(session_token(&headers)))
        .bind(serde_json::to_string(&OAuthState { verifier }).map_err(|_| unavailable())?)
        .execute(&service.pool).await?;

    Ok((
        [(
            header::SET_COOKIE,
            browser_cookie(&service, "leo_oauth", &browser, 300),
        )],
        Json(json!({ "url": url.as_str() })),
    )
        .into_response())
}

#[derive(Deserialize)]
pub(super) struct Callback {
    state: String,
    code: Option<String>,
}

fn unavailable() -> ApiError {
    ApiError(
        StatusCode::BAD_GATEWAY,
        "Sign-in provider unavailable. Please try again.",
    )
}

fn rejected() -> ApiError {
    ApiError(
        StatusCode::UNAUTHORIZED,
        "Unable to verify this sign-in. Please try again.",
    )
}

pub(super) async fn callback(
    State(service): State<Service>,
    Path(name): Path<String>,
    headers: HeaderMap,
    query: Result<Query<Callback>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let result = match query {
        Ok(Query(input)) => complete_callback(&service, &name, &headers, input).await,
        Err(_) => Err(rejected()),
    };
    let mut response =
        result.unwrap_or_else(|_| Redirect::to("/?sign_in_error=oauth").into_response());
    response.headers_mut().append(
        header::SET_COOKIE,
        browser_cookie(&service, "leo_oauth", "", 0)
            .parse()
            .expect("static cookie"),
    );
    response
}

async fn complete_callback(
    service: &Service,
    name: &str,
    headers: &HeaderMap,
    input: Callback,
) -> Result<Response, ApiError> {
    let provider = service.oauth.provider(name)?;
    let row: Option<(Option<String>, String, String)> = query_as("DELETE FROM sign_in_challenges WHERE id = $1 AND kind = $2 AND browser_digest = $3 AND expires_at > now() RETURNING account_id, session_digest, state")
        .bind(input.state).bind(format!("oauth:{name}")).bind(digest(cookie_token(headers, "leo_oauth")))
        .fetch_optional(&service.pool).await?;

    let (link_account, session_digest, state) = row.ok_or_else(rejected)?;
    if link_account.is_some() {
        let account = methods::authenticated(service, headers, false).await?;
        if link_account.as_deref() != Some(&account.0)
            || session_digest != digest(session_token(headers))
        {
            return Err(rejected());
        }
    }

    let state: OAuthState = serde_json::from_str(&state).map_err(|_| rejected())?;
    let code = input.code.ok_or_else(rejected)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Leo account sign-in")
        .build()
        .map_err(|_| unavailable())?;

    let token: Value = client
        .post(&provider.token_url)
        .header(header::ACCEPT, "application/json")
        .form(&[
            ("client_id", provider.client_id.as_str()),
            ("client_secret", provider.client_secret.as_str()),
            ("code", code.as_str()),
            ("grant_type", "authorization_code"),
            (
                "redirect_uri",
                &format!("{}/api/account/oauth/{name}/callback", service.origin),
            ),
            ("code_verifier", &state.verifier),
        ])
        .send()
        .await
        .map_err(|_| unavailable())?
        .error_for_status()
        .map_err(|_| unavailable())?
        .json()
        .await
        .map_err(|_| unavailable())?;

    let access_token = token["access_token"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or_else(rejected)?;
    let identity: Value = client
        .get(&provider.userinfo_url)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|_| unavailable())?
        .error_for_status()
        .map_err(|_| unavailable())?
        .json()
        .await
        .map_err(|_| unavailable())?;

    let (subject, email) = if name == "github" {
        let subject = identity["id"].as_u64().ok_or_else(rejected)?.to_string();
        let emails: Vec<Value> = client
            .get(provider.emails_url.as_ref().ok_or_else(unavailable)?)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|_| unavailable())?
            .error_for_status()
            .map_err(|_| unavailable())?
            .json()
            .await
            .map_err(|_| unavailable())?;
        let email = emails
            .iter()
            .find(|item| item["verified"] == true && item["primary"] == true)
            .and_then(|item| item["email"].as_str())
            .ok_or_else(rejected)?
            .to_owned();
        (subject, email)
    } else {
        if identity["email_verified"] != true {
            return Err(rejected());
        }
        let subject = identity["sub"]
            .as_str()
            .filter(|v| !v.is_empty())
            .ok_or_else(rejected)?
            .to_owned();
        let email = identity["email"].as_str().ok_or_else(rejected)?.to_owned();
        (subject, email)
    };

    let email = normalized_email(&email).map_err(|_| rejected())?;
    let domain = email
        .rsplit_once('@')
        .map(|(_, domain)| domain)
        .ok_or_else(rejected)?;
    let authoritative = name == "google"
        && (domain == "gmail.com"
            || identity["hd"]
                .as_str()
                .is_some_and(|hosted| hosted.to_lowercase() == domain));

    let mut transaction = service.pool.begin().await?;
    let created: Option<(String,)> = query_as("INSERT INTO leo_accounts (id, email) VALUES ($1, $2) ON CONFLICT (email) DO NOTHING RETURNING id")
        .bind(uuid::Uuid::new_v4().to_string()).bind(&email).fetch_optional(&mut *transaction).await?;
    let new_account = created.is_some();
    let account_id = if let Some((id,)) = created {
        id
    } else {
        let (id,): (String,) = query_as("SELECT id FROM leo_accounts WHERE email = $1 FOR UPDATE")
            .bind(&email)
            .fetch_one(&mut *transaction)
            .await?;
        id
    };

    if link_account.as_ref().is_some_and(|id| id != &account_id) {
        return Err(rejected());
    }

    let existing: Option<(String, bool)> = query_as(
        "SELECT account_id, removed FROM sign_in_methods WHERE kind = $1 AND subject = $2",
    )
    .bind(name)
    .bind(&subject)
    .fetch_optional(&mut *transaction)
    .await?;
    if !new_account && existing.is_none() && link_account.is_none() && !authoritative {
        return Err(rejected());
    }
    if existing.is_some_and(|(id, removed)| id != account_id || removed && link_account.is_none()) {
        return Err(rejected());
    }

    let linked = query("INSERT INTO sign_in_methods (id, account_id, kind, subject, label) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (kind, subject) DO UPDATE SET removed = false WHERE sign_in_methods.account_id = EXCLUDED.account_id AND (NOT sign_in_methods.removed OR $6)")
        .bind(uuid::Uuid::new_v4().to_string()).bind(&account_id).bind(name).bind(subject).bind(&email)
        .bind(link_account.is_some()).execute(&mut *transaction).await?;
    if linked.rows_affected() != 1 {
        return Err(rejected());
    }

    let session_response = create_session(service, &mut transaction, &account_id, &email).await?;
    transaction.commit().await?;

    // Provider tokens are deliberately discarded: GitHub identification grants no agent access.
    let mut response = Redirect::to("/").into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        session_response.headers()[header::SET_COOKIE].clone(),
    );

    Ok(response)
}
