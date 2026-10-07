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
    /// Production defaults to Google’s fixed JWKS endpoint.
    pub google_jwks_url: Option<String>,
    pub android_certificates: Vec<[u8; 32]>,
}

impl OAuthProviders {
    fn provider(&self, name: &str) -> Result<&OAuthProvider, ApiError> {
        let configured = match name {
            "google" => &self.google,
            "github" => &self.github,
            _ => &None,
        };
        configured.as_ref().ok_or(ApiError::Http(
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
    #[serde(default)]
    native: bool,
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
    input: Option<Json<NativeStart>>,
) -> Result<Response, ApiError> {
    let provider = service.oauth.provider(&name)?;
    let native = input.is_some_and(|Json(input)| input.native);
    if native && name == "google" {
        return google_start(&service, peer, &headers).await;
    }
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
        .bind(&state).bind(format!("oauth:{name}")).bind(digest(&browser)).bind(&account_id)
        .bind(digest(session_token(&headers)))
        .bind(serde_json::to_string(&OAuthState { verifier, native }).map_err(|_| unavailable())?)
        .execute(&service.pool).await?;

    if native {
        let secret = random_token();
        let launcher = random_token();
        query("INSERT INTO native_oauth_handovers (id, provider, secret_digest, launcher_digest, browser_token, authorization_url, link_account, session_digest) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)")
            .bind(&state).bind(&name).bind(digest(&secret)).bind(digest(&launcher)).bind(&browser)
            .bind(url.as_str()).bind(&account_id).bind(digest(session_token(&headers)))
            .execute(&service.pool).await?;
        let mut launch = Url::parse(&format!(
            "{}/api/account/oauth/{name}/native/browser",
            service.origin
        ))
        .map_err(|_| unavailable())?;
        launch
            .query_pairs_mut()
            .append_pair("challenge", &state)
            .append_pair("token", &launcher);
        return Ok(Json(json!({
            "url": launch.as_str(),
            "challenge": state,
            "secret": secret,
        }))
        .into_response());
    }

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
    ApiError::Http(
        StatusCode::BAD_GATEWAY,
        "Sign-in provider unavailable. Please try again.",
    )
}

fn rejected() -> ApiError {
    ApiError::Http(
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
        .bind(&input.state).bind(format!("oauth:{name}")).bind(digest(cookie_token(headers, "leo_oauth")))
        .fetch_optional(&service.pool).await?;

    let (link_account, session_digest, state) = row.ok_or_else(rejected)?;
    let state: OAuthState = serde_json::from_str(&state).map_err(|_| rejected())?;
    if link_account.is_some() {
        if state.native {
            let active: Option<(String,)> = query_as(
                "SELECT account_id FROM web_sessions WHERE digest = $1 AND expires_at > now()",
            )
            .bind(&session_digest)
            .fetch_optional(&service.pool)
            .await?;
            if active.map(|(id,)| id) != link_account {
                return Err(rejected());
            }
        } else {
            let account = methods::authenticated(service, headers, false).await?;
            if link_account.as_deref() != Some(&account.0)
                || session_digest != digest(session_token(headers))
            {
                return Err(rejected());
            }
        }
    }

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

    let session_response = complete_identity(
        service,
        name,
        headers,
        VerifiedIdentity {
            subject,
            email,
            profile: identity,
        },
        link_account,
        state.native.then_some(input.state.as_str()),
    )
    .await?;
    if state.native {
        return Ok(session_response);
    }

    // Provider tokens are deliberately discarded: GitHub identification grants no agent access.
    let mut response = Redirect::to("/").into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        session_response.headers()[header::SET_COOKIE].clone(),
    );

    Ok(response)
}

struct VerifiedIdentity {
    subject: String,
    email: String,
    profile: Value,
}

// Both transports apply exactly the same verified-email and removed-method policy.
async fn complete_identity(
    service: &Service,
    name: &str,
    headers: &HeaderMap,
    identity: VerifiedIdentity,
    link_account: Option<String>,
    native: Option<&str>,
) -> Result<Response, ApiError> {
    let VerifiedIdentity {
        subject,
        email,
        profile,
    } = identity;
    let email = normalized_email(&email).map_err(|_| rejected())?;
    let domain = email
        .rsplit_once('@')
        .map(|(_, domain)| domain)
        .ok_or_else(rejected)?;
    let authoritative = name == "google"
        && (domain == "gmail.com"
            || profile["hd"]
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
        .bind(uuid::Uuid::new_v4().to_string()).bind(&account_id).bind(name).bind(&subject).bind(&email)
        .bind(link_account.is_some()).execute(&mut *transaction).await?;
    if linked.rows_affected() != 1 {
        return Err(rejected());
    }

    if let Some(id) = native {
        let updated = query("UPDATE native_oauth_handovers SET ready_account = $2, method_subject = $3 WHERE id = $1 AND provider = $4 AND expires_at > now()")
            .bind(id).bind(&account_id).bind(&subject).bind(name).execute(&mut *transaction).await?;
        if updated.rows_affected() != 1 {
            return Err(rejected());
        }
        transaction.commit().await?;
        return Ok(axum::response::Html("<!doctype html><html lang=fr><meta name=viewport content='width=device-width'><title>Leo</title><p>Connexion réussie. Revenez dans l’application Leo.</p></html>").into_response());
    }

    let session_response = create_session(
        service,
        &mut transaction,
        &account_id,
        &email,
        headers,
        SessionProof::OAuth,
    )
    .await?;
    transaction.commit().await?;

    Ok(session_response)
}

#[derive(Deserialize)]
pub(super) struct NativeStart {
    #[serde(default)]
    native: bool,
}

async fn google_start(
    service: &Service,
    peer: SocketAddr,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    consume_limit(&service.pool, &format!("oauth:{}", peer.ip()), 30).await?;
    let provider = service.oauth.provider("google")?;
    let account = if session_token(headers).is_empty() {
        None
    } else {
        Some(methods::authenticated(service, headers, true).await?.0)
    };
    let challenge = random_token();
    let browser = random_token();
    let nonce = random_token();
    query("INSERT INTO sign_in_challenges (id, kind, browser_digest, account_id, session_digest, state) VALUES ($1, 'google-native', $2, $3, $4, $5)")
        .bind(&challenge).bind(digest(&browser)).bind(account).bind(digest(session_token(headers))).bind(&nonce)
        .execute(&service.pool).await?;
    Ok((
        [(
            header::SET_COOKIE,
            browser_cookie(service, "leo_oauth", &browser, 300),
        )],
        Json(json!({
            "challenge": challenge,
            "nonce": nonce,
            "clientId": provider.client_id,
        })),
    )
        .into_response())
}

#[derive(Deserialize)]
pub(super) struct GoogleCredential {
    challenge: String,
    credential: String,
}

pub(super) async fn google_credential(
    State(service): State<Service>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(input): Json<GoogleCredential>,
) -> Result<Response, ApiError> {
    if name != "google" || input.credential.len() > 16_384 {
        return Err(rejected());
    }
    let provider = service.oauth.provider("google")?;
    let row: Option<(Option<String>, String, String)> = query_as("DELETE FROM sign_in_challenges WHERE id = $1 AND kind = 'google-native' AND browser_digest = $2 AND expires_at > now() RETURNING account_id, session_digest, state")
        .bind(&input.challenge).bind(digest(cookie_token(&headers, "leo_oauth"))).fetch_optional(&service.pool).await?;
    let (link_account, session, nonce) = row.ok_or_else(rejected)?;
    if let Some(account) = link_account.as_ref()
        && (methods::authenticated(&service, &headers, true).await?.0 != *account
            || session != digest(session_token(&headers)))
    {
        return Err(rejected());
    }
    use jwt_simple::prelude::*;
    let metadata = Token::decode_metadata(&input.credential).map_err(|_| rejected())?;
    let kid = metadata.key_id().ok_or_else(rejected)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| unavailable())?;
    let keys: Value = client
        .get(
            service
                .oauth
                .google_jwks_url
                .as_deref()
                .unwrap_or("https://www.googleapis.com/oauth2/v3/certs"),
        )
        .send()
        .await
        .map_err(|_| unavailable())?
        .error_for_status()
        .map_err(|_| unavailable())?
        .json()
        .await
        .map_err(|_| unavailable())?;
    let key = keys["keys"]
        .as_array()
        .and_then(|keys| {
            keys.iter()
                .find(|key| key["kid"] == kid && key["kty"] == "RSA" && key["alg"] == "RS256")
        })
        .ok_or_else(rejected)?;
    let n = URL_SAFE_NO_PAD
        .decode(key["n"].as_str().ok_or_else(rejected)?)
        .map_err(|_| rejected())?;
    let e = URL_SAFE_NO_PAD
        .decode(key["e"].as_str().ok_or_else(rejected)?)
        .map_err(|_| rejected())?;
    let claims = RS256PublicKey::from_components(&n, &e)
        .map_err(|_| rejected())?
        .verify_token::<Value>(
            &input.credential,
            Some(VerificationOptions {
                allowed_issuers: Some(HashSet::from_strings(&[
                    "https://accounts.google.com",
                    "accounts.google.com",
                ])),
                allowed_audiences: Some(HashSet::from_strings(&[&provider.client_id])),
                required_nonce: Some(nonce),
                time_tolerance: Some(jwt_simple::prelude::Duration::from_secs(30)),
                ..Default::default()
            }),
        )
        .map_err(|_| rejected())?;
    if claims.expires_at.is_none()
        || claims.issued_at.is_none()
        || claims.custom["email_verified"] != true
    {
        return Err(rejected());
    }
    let subject = claims
        .subject
        .filter(|value| !value.is_empty())
        .ok_or_else(rejected)?;
    let email = claims.custom["email"]
        .as_str()
        .ok_or_else(rejected)?
        .to_owned();
    complete_identity(
        &service,
        "google",
        &headers,
        VerifiedIdentity {
            subject,
            email,
            profile: claims.custom,
        },
        link_account,
        None,
    )
    .await
}

#[derive(Deserialize)]
pub(super) struct NativeLaunch {
    challenge: String,
    token: String,
}

pub(super) async fn native_browser(
    State(service): State<Service>,
    Path(name): Path<String>,
    Query(input): Query<NativeLaunch>,
) -> Result<Response, ApiError> {
    let mut transaction = service.pool.begin().await?;
    let row: Option<(String, String)> = query_as("SELECT browser_token, authorization_url FROM native_oauth_handovers WHERE id = $1 AND provider = $2 AND launcher_digest = $3 AND expires_at > now() FOR UPDATE")
        .bind(&input.challenge).bind(name).bind(digest(&input.token)).fetch_optional(&mut *transaction).await?;
    let (browser, authorization) = row.ok_or_else(rejected)?;
    query("UPDATE native_oauth_handovers SET launcher_digest = NULL, browser_token = NULL WHERE id = $1")
        .bind(&input.challenge).execute(&mut *transaction).await?;
    transaction.commit().await?;
    let mut response = Redirect::to(&authorization).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        browser_cookie(&service, "leo_oauth", &browser, 300)
            .parse()
            .map_err(|_| unavailable())?,
    );
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

#[derive(Deserialize)]
pub(super) struct NativeFinish {
    challenge: String,
    secret: String,
}

struct NativeHandover {
    link_account: Option<String>,
    session_digest: String,
    ready_account: Option<String>,
    method_subject: Option<String>,
}

impl<'r> sqlx_core::from_row::FromRow<'r, sqlx_postgres::PgRow> for NativeHandover {
    fn from_row(row: &'r sqlx_postgres::PgRow) -> Result<Self, sqlx_core::error::Error> {
        use sqlx_core::row::Row;
        Ok(Self {
            link_account: row.try_get("link_account")?,
            session_digest: row.try_get("session_digest")?,
            ready_account: row.try_get("ready_account")?,
            method_subject: row.try_get("method_subject")?,
        })
    }
}

pub(super) async fn native_finish(
    State(service): State<Service>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(input): Json<NativeFinish>,
) -> Result<Response, ApiError> {
    let mut transaction = service.pool.begin().await?;
    let row: Option<NativeHandover> = query_as("SELECT link_account, session_digest, ready_account, method_subject FROM native_oauth_handovers WHERE id = $1 AND provider = $2 AND secret_digest = $3 AND expires_at > now() FOR UPDATE")
        .bind(&input.challenge).bind(&name).bind(digest(&input.secret)).fetch_optional(&mut *transaction).await?;
    let NativeHandover {
        link_account: link,
        session_digest: session,
        ready_account: account,
        method_subject: subject,
    } = row.ok_or_else(rejected)?;
    if let Some(id) = link.as_ref()
        && (session != digest(session_token(&headers))
            || methods::authenticated_on(&mut transaction, &headers, true)
                .await?
                .0
                != *id)
    {
        return Err(rejected());
    }
    let Some(account) = account else {
        return Ok((StatusCode::ACCEPTED, Json(json!({"pending": true}))).into_response());
    };
    let (email,): (String,) = query_as("SELECT email FROM leo_accounts WHERE id = $1 FOR UPDATE")
        .bind(&account)
        .fetch_one(&mut *transaction)
        .await?;
    if let Some(id) = link.as_ref()
        && methods::authenticated_on(&mut transaction, &headers, true)
            .await?
            .0
            != *id
    {
        return Err(rejected());
    }
    let method: Option<(String,)> = query_as("SELECT id FROM sign_in_methods WHERE account_id = $1 AND kind = $2 AND subject = $3 AND NOT removed")
        .bind(&account).bind(name).bind(subject).fetch_optional(&mut *transaction).await?;
    if method.is_none() {
        return Err(rejected());
    }
    query("DELETE FROM native_oauth_handovers WHERE id = $1")
        .bind(&input.challenge)
        .execute(&mut *transaction)
        .await?;
    let response = create_session(
        &service,
        &mut transaction,
        &account,
        &email,
        &headers,
        SessionProof::OAuth,
    )
    .await?;
    transaction.commit().await?;
    Ok(response)
}
