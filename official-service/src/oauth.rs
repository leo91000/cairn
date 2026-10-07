use super::*;
use axum::{
    extract::{Form, Path, Query},
    response::{Html, Redirect},
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

impl OAuthProvider {
    fn scope(&self) -> &'static str {
        if self.emails_url.is_some() {
            "user:email"
        } else {
            "openid email"
        }
    }

    async fn identity(
        &self,
        client: &reqwest::Client,
        access_token: &str,
    ) -> Result<VerifiedIdentity, ApiError> {
        let identity: Value = client
            .get(&self.userinfo_url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|_| unavailable())?
            .error_for_status()
            .map_err(|_| unavailable())?
            .json()
            .await
            .map_err(|_| unavailable())?;

        let (subject, email) = if self.emails_url.is_some() {
            let subject = identity["id"].as_u64().ok_or_else(rejected)?.to_string();
            let emails: Vec<Value> = client
                .get(self.emails_url.as_ref().ok_or_else(unavailable)?)
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
        Ok(VerifiedIdentity {
            subject,
            email,
            profile: identity,
        })
    }

    async fn revoke_token(
        &self,
        client: &reqwest::Client,
        access_token: &str,
    ) -> Result<(), ApiError> {
        if self.emails_url.is_none() {
            return Ok(());
        }

        // GitHub's app-token endpoint shares the API prefix of /user, including
        // an overridden API prefix in loopback tests. Encode the client ID as a segment.
        let mut url = Url::parse(&self.userinfo_url).map_err(|_| unavailable())?;
        url.set_query(None);
        url.set_fragment(None);
        url.path_segments_mut()
            .map_err(|_| unavailable())?
            .pop_if_empty()
            .pop()
            .push("applications")
            .push(&self.client_id)
            .push("token");

        let response = client
            .delete(url)
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .header(header::ACCEPT, "application/vnd.github+json")
            .json(&json!({ "access_token": access_token }))
            .send()
            .await
            .map_err(|_| unavailable())?;

        if response.status() != StatusCode::NO_CONTENT {
            return Err(unavailable());
        }

        Ok(())
    }
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
    // A logged-in flow is explicit linking and must be protected like any mutation.
    let account_id = if session_token(&headers).is_empty() {
        None
    } else {
        Some(account::confirmed_session(&service, &headers).await?.0)
    };

    consume_limit(&service.pool, &format!("oauth:{}", peer.ip()), 30).await?;

    let state = random_token();
    let browser = random_token();
    let verifier = random_token();
    let mut url = Url::parse(&provider.authorization_url).map_err(|_| unavailable())?;
    let redirect_uri = format!("{}/api/account/oauth/{name}/callback", service.origin);
    url.query_pairs_mut()
        .append_pair("client_id", &provider.client_id)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", provider.scope())
        .append_pair("state", &state)
        .append_pair("code_challenge_method", "S256")
        .append_pair(
            "code_challenge",
            &URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
        );

    let mut transaction = service.pool.begin().await?;
    if let Some(account) = &account_id {
        lock_and_confirm_linking_session(&mut transaction, &headers, account).await?;
    }

    query("INSERT INTO sign_in_challenges (id, kind, browser_digest, account_id, session_digest, state) VALUES ($1, $2, $3, $4, $5, $6)")
        .bind(&state).bind(format!("oauth:{name}")).bind(digest(&browser)).bind(&account_id)
        .bind(digest(session_token(&headers)))
        .bind(serde_json::to_string(&OAuthState { verifier, native }).map_err(|_| unavailable())?)
        .execute(&mut *transaction).await?;

    if native {
        let secret = random_token();
        let launcher = random_token();
        query("INSERT INTO native_oauth_handovers (id, provider, secret_digest, launcher_digest, browser_token, authorization_url, link_account, session_digest) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)")
            .bind(&state).bind(&name).bind(digest(&secret)).bind(digest(&launcher)).bind(&browser)
            .bind(url.as_str()).bind(&account_id).bind(digest(session_token(&headers)))
            .execute(&mut *transaction).await?;
        transaction.commit().await?;

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

    transaction.commit().await?;

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
        StatusCode::SERVICE_UNAVAILABLE,
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
    let mut response = result.unwrap_or_else(|error| match error {
        ApiError::Http(StatusCode::CONFLICT, _) => Redirect::to("/?sign_in_error=oauth_link_required").into_response(),
        ApiError::Http(StatusCode::FORBIDDEN, _) => Redirect::to("/?sign_in_error=oauth_proof").into_response(),
        ApiError::Http(StatusCode::SERVICE_UNAVAILABLE, _) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Html("<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><title>Sign-in provider unavailable</title><h1>Sign-in provider unavailable</h1><p>Please try again or use another sign-in method.</p><a href=\"/?sign_in_error=oauth_unavailable\">Try again</a></html>"),
        ).into_response(),
        _ => Redirect::to("/?sign_in_error=oauth").into_response(),
    });
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
            let mut connection = service.pool.acquire().await?;
            confirmed_native_link(
                &mut connection,
                link_account.as_deref().ok_or_else(rejected)?,
                &session_digest,
            )
            .await?;
        } else {
            let account = methods::authenticated(service, headers, false).await?;
            if link_account.as_deref() != Some(&account.0)
                || session_digest != digest(session_token(headers))
            {
                return Err(rejected());
            }
            let mut connection = service.pool.acquire().await?;
            account::require_recent_proof(&mut connection, headers).await?;
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
    let identity = provider.identity(&client, access_token).await;
    // Attempt cleanup even if identity verification failed. Never create a Leo
    // session when GitHub refuses to revoke the identification token.
    provider.revoke_token(&client, access_token).await?;

    let identity = identity?;

    if state.native {
        return native_confirmation(service, name, &input.state, identity).await;
    }
    let session_response =
        complete_identity(service, name, headers, identity, link_account, None).await?;

    // Provider tokens are deliberately discarded: GitHub identification grants no agent access.
    let mut response = Redirect::to("/").into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        session_response.headers()[header::SET_COOKIE].clone(),
    );

    Ok(response)
}

#[derive(Serialize, Deserialize)]
struct VerifiedIdentity {
    subject: String,
    email: String,
    profile: Value,
}

// The browser which proved the GitHub identity must explicitly authorize the
// Android handover. Possession of a launcher URL and polling secret is not consent.
async fn native_confirmation(
    service: &Service,
    name: &str,
    challenge: &str,
    mut identity: VerifiedIdentity,
) -> Result<Response, ApiError> {
    identity.email = normalized_email(&identity.email).map_err(|_| rejected())?;
    let displayed_email = identity
        .email
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;");

    // GitHub's public profile is unnecessary for its verified-email policy.
    identity.profile = Value::Null;
    let proof = random_token();
    let browser = random_token();
    let updated = query("UPDATE native_oauth_handovers SET pending_identity = $3, confirmation_digest = $4, confirmation_browser_digest = $5 WHERE id = $1 AND provider = $2 AND expires_at > clock_timestamp()")
        .bind(challenge).bind(name)
        .bind(serde_json::to_string(&identity).map_err(|_| unavailable())?)
        .bind(digest(&proof)).bind(digest(&browser)).execute(&service.pool).await?;
    if updated.rows_affected() != 1 {
        return Err(rejected());
    }
    let html = format!(
        "<!doctype html><html lang=fr><meta charset=utf-8><meta name=viewport content='width=device-width'>\
         <title>Autoriser Leo pour Android</title>\
         <h1>Autoriser l’application Leo pour Android</h1>\
         <p>Compte GitHub vérifié : <strong>{displayed_email}</strong></p>\
         <p>Cette autorisation connectera ou liera ce compte à l’application Leo pour Android qui a demandé la connexion.</p>\
         <p role=alert>Ne confirmez pas un lien reçu d’une autre personne. Continuez uniquement si vous venez de demander cette connexion dans Leo sur votre appareil.</p>\
         <form method=post action='/api/account/oauth/github/native/confirm'>\
         <input type=hidden name=challenge value='{challenge}'>\
         <input type=hidden name=proof value='{proof}'>\
         <button type=submit>Autoriser sur cet appareil</button></form>\
         <p>Pour annuler, fermez cet onglet sans autoriser.</p></html>"
    );
    Ok((
        [
            (
                header::SET_COOKIE,
                browser_cookie(service, "leo_native_confirmation", &browser, 300),
            ),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'"
                    .into(),
            ),
            // Browser form POSTs need a non-null Origin for the exact-origin
            // check. Send only the public origin, never the callback's query.
            (header::REFERRER_POLICY, "strict-origin".into()),
        ],
        axum::response::Html(html),
    )
        .into_response())
}

#[derive(Deserialize)]
pub(super) struct NativeConfirmation {
    challenge: String,
    proof: String,
}

pub(super) async fn native_confirm(
    State(service): State<Service>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Form(input): Form<NativeConfirmation>,
) -> Result<Response, ApiError> {
    // The proof is in the page only; its independent HttpOnly cookie binds it
    // to the browser that authenticated. Origin checking is provided by the router.
    let row: Option<(String, Option<String>)> = query_as("UPDATE native_oauth_handovers SET confirmation_digest = NULL, confirmation_browser_digest = NULL WHERE id = $1 AND provider = $2 AND confirmation_digest = $3 AND confirmation_browser_digest = $4 AND pending_identity IS NOT NULL AND expires_at > clock_timestamp() RETURNING pending_identity, link_account")
        .bind(&input.challenge).bind(&name).bind(digest(&input.proof))
        .bind(digest(cookie_token(&headers, "leo_native_confirmation")))
        .fetch_optional(&service.pool).await?;
    let (identity, link_account) = row.ok_or_else(rejected)?;
    let identity = serde_json::from_str(&identity).map_err(|_| rejected())?;
    let mut response = complete_identity(
        &service,
        &name,
        &headers,
        identity,
        link_account,
        Some(&input.challenge),
    )
    .await?;
    response.headers_mut().insert(
        header::SET_COOKIE,
        browser_cookie(&service, "leo_native_confirmation", "", 0)
            .parse()
            .map_err(|_| unavailable())?,
    );
    Ok(response)
}

// The native browser has an independent cookie. Linking must still confirm the
// exact app session recorded when the handover started.
async fn confirmed_native_link(
    connection: &mut sqlx_postgres::PgConnection,
    account_id: &str,
    session_digest: &str,
) -> Result<(), ApiError> {
    let (active,): (bool,) = query_as("SELECT EXISTS (SELECT 1 FROM web_sessions WHERE digest = $1 AND account_id = $2 AND expires_at > clock_timestamp())")
        .bind(session_digest).bind(account_id).fetch_one(&mut *connection).await?;
    if !active {
        return Err(rejected());
    }

    account::require_recent_proof_for_session(connection, session_digest).await
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
    // A linked subject is the identity; a provider's mutable email is only used
    // to attach a new identity. Never move a method to another account.
    let linked_account: Option<(String,)> =
        query_as("SELECT account_id FROM sign_in_methods WHERE kind = $1 AND subject = $2")
            .bind(name)
            .bind(&subject)
            .fetch_optional(&mut *transaction)
            .await?;
    let (account_id, email, new_account) = if let Some((id,)) = linked_account {
        let (account_id, account_email): (String, String) =
            query_as("SELECT id, email FROM leo_accounts WHERE id = $1 FOR NO KEY UPDATE")
                .bind(id)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or_else(rejected)?;
        (account_id, account_email, false)
    } else {
        let created: Option<(String,)> = query_as("INSERT INTO leo_accounts (id, email) VALUES ($1, $2) ON CONFLICT (email) DO NOTHING RETURNING id")
            .bind(uuid::Uuid::new_v4().to_string()).bind(&email).fetch_optional(&mut *transaction).await?;
        if let Some((id,)) = created {
            (id, email, true)
        } else {
            let (id,): (String,) =
                query_as("SELECT id FROM leo_accounts WHERE email = $1 FOR NO KEY UPDATE")
                    .bind(&email)
                    .fetch_one(&mut *transaction)
                    .await?;
            (id, email, false)
        }
    };

    if link_account.as_ref().is_some_and(|id| id != &account_id) {
        return Err(rejected());
    }

    if let Some(linked_account) = link_account.as_ref() {
        if let Some(handover) = native {
            let session: Option<(String,)> = query_as("SELECT session_digest FROM native_oauth_handovers WHERE id = $1 AND link_account = $2 AND expires_at > clock_timestamp()")
                .bind(handover).bind(linked_account).fetch_optional(&mut *transaction).await?;
            let (session_digest,) = session.ok_or_else(rejected)?;
            confirmed_native_link(&mut transaction, linked_account, &session_digest).await?;
        } else if methods::authenticated_on(&mut transaction, headers, false)
            .await?
            .0
            != *linked_account
        {
            return Err(rejected());
        }
        if native.is_none() {
            account::require_recent_proof(&mut transaction, headers).await?;
        }
    }

    // Re-read after serialization: another sign-in or removal may have finished.
    let existing: Option<(String, bool)> = query_as(
        "SELECT account_id, removed FROM sign_in_methods WHERE kind = $1 AND subject = $2",
    )
    .bind(name)
    .bind(&subject)
    .fetch_optional(&mut *transaction)
    .await?;
    if !new_account && existing.is_none() && link_account.is_none() && !authoritative {
        return Err(ApiError::Http(
            StatusCode::CONFLICT,
            "Sign in by email, then link this provider from Sign-in methods",
        ));
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
        let updated = query("UPDATE native_oauth_handovers SET ready_account = $2, method_subject = $3, pending_identity = NULL WHERE id = $1 AND provider = $4 AND expires_at > clock_timestamp()")
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

async fn lock_and_confirm_linking_session(
    connection: &mut sqlx_postgres::PgConnection,
    headers: &HeaderMap,
    account_id: &str,
) -> Result<(), ApiError> {
    query("SELECT id FROM leo_accounts WHERE id = $1 FOR NO KEY UPDATE")
        .bind(account_id)
        .execute(&mut *connection)
        .await?;
    account::confirmed_session_on(connection, headers).await?;

    Ok(())
}

async fn google_start(
    service: &Service,
    peer: SocketAddr,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    let provider = service.oauth.provider("google")?;
    let account = if session_token(headers).is_empty() {
        None
    } else {
        Some(account::confirmed_session(service, headers).await?.0)
    };
    consume_limit(&service.pool, &format!("oauth:{}", peer.ip()), 30).await?;

    let challenge = random_token();
    let browser = random_token();
    let nonce = random_token();
    let mut transaction = service.pool.begin().await?;
    if let Some(account_id) = &account {
        lock_and_confirm_linking_session(&mut transaction, headers, account_id).await?;
    }

    query("INSERT INTO sign_in_challenges (id, kind, browser_digest, account_id, session_digest, state) VALUES ($1, 'google-native', $2, $3, $4, $5)")
        .bind(&challenge).bind(digest(&browser)).bind(account).bind(digest(session_token(headers))).bind(&nonce)
        .execute(&mut *transaction).await?;
    transaction.commit().await?;

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
    if let Some(account) = link_account.as_ref() {
        let confirmed = account::confirmed_session(&service, &headers).await?;
        if confirmed.0 != *account || session != digest(session_token(&headers)) {
            return Err(rejected());
        }
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
    let row: Option<NativeHandover> = query_as("SELECT link_account, session_digest, ready_account, method_subject FROM native_oauth_handovers WHERE id = $1 AND provider = $2 AND secret_digest = $3 AND expires_at > clock_timestamp() FOR UPDATE")
        .bind(&input.challenge).bind(&name).bind(digest(&input.secret)).fetch_optional(&mut *transaction).await?;
    let NativeHandover {
        link_account: link,
        session_digest: session,
        ready_account: account,
        method_subject: subject,
    } = row.ok_or_else(rejected)?;
    if let Some(id) = link.as_ref()
        && (session != digest(session_token(&headers))
            || account::confirmed_session_on(&mut transaction, &headers)
                .await?
                .0
                != *id)
    {
        return Err(rejected());
    }
    let Some(account) = account else {
        return Ok((StatusCode::ACCEPTED, Json(json!({"pending": true}))).into_response());
    };
    let (email,): (String,) =
        query_as("SELECT email FROM leo_accounts WHERE id = $1 FOR NO KEY UPDATE")
            .bind(&account)
            .fetch_one(&mut *transaction)
            .await?;
    if let Some(id) = link.as_ref()
        && account::confirmed_session_on(&mut transaction, &headers)
            .await?
            .0
            != *id
    {
        return Err(rejected());
    }
    let method: Option<(String,)> = query_as("SELECT id FROM sign_in_methods WHERE account_id = $1 AND kind = $2 AND subject = $3 AND NOT removed AND EXISTS(SELECT 1 FROM native_oauth_handovers h WHERE h.id = $4 AND h.expires_at > clock_timestamp())")
        .bind(&account).bind(name).bind(subject).bind(&input.challenge).fetch_optional(&mut *transaction).await?;
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
