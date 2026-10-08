use super::*;
use webauthn_rs::prelude::*;
use webauthn_rs_proto::ResidentKeyRequirement;

fn rejected() -> ApiError {
    ApiError::Http(
        StatusCode::UNAUTHORIZED,
        "Unable to verify this passkey. Please try again.",
    )
}

fn webauthn(service: &Service) -> Result<Webauthn, ApiError> {
    let origin = Url::parse(&service.origin).map_err(|_| rejected())?;
    let host = origin.domain().ok_or(ApiError::Http(
        StatusCode::BAD_REQUEST,
        "Passkeys require a hostname or localhost",
    ))?;
    let mut builder = WebauthnBuilder::new(host, &origin).map_err(|_| rejected())?;
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    for certificate in &service.oauth.android_certificates {
        let native_origin = Url::parse(&format!(
            "android:apk-key-hash:{}",
            URL_SAFE_NO_PAD.encode(certificate)
        ))
        .map_err(|_| rejected())?;
        builder = builder.append_allowed_origin(&native_origin);
    }
    builder.rp_name("Leo").build().map_err(|_| rejected())
}

pub(super) async fn assetlinks(State(service): State<Service>) -> Json<Value> {
    let fingerprints: Vec<String> = service
        .oauth
        .android_certificates
        .iter()
        .map(|certificate| {
            certificate
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect::<Vec<_>>()
                .join(":")
        })
        .collect();
    if fingerprints.is_empty() {
        return Json(json!([]));
    }
    Json(json!([{
        "relation": ["delegate_permission/common.get_login_creds"],
        "target": {
            "namespace": "android_app",
            "package_name": "dev.leo.manager",
            "sha256_cert_fingerprints": fingerprints,
        },
    }]))
}

pub(super) fn available(service: &Service) -> bool {
    Url::parse(&service.origin)
        .ok()
        .is_some_and(|url| url.domain().is_some())
}

async fn store_challenge<T: serde::Serialize>(
    service: &Service,
    kind: &str,
    browser: &str,
    account_id: Option<&str>,
    session: Option<&str>,
    state: &T,
) -> Result<String, ApiError> {
    let id = random_token();
    query("INSERT INTO sign_in_challenges (id, kind, browser_digest, account_id, session_digest, state) VALUES ($1, $2, $3, $4, $5, $6)")
        .bind(&id).bind(kind).bind(digest(browser)).bind(account_id).bind(session.map(digest))
        .bind(serde_json::to_string(state).map_err(|_| rejected())?).execute(&service.pool).await?;

    Ok(id)
}

async fn take_challenge(
    service: &Service,
    id: &str,
    kind: &str,
    browser: &str,
) -> Result<(Option<String>, Option<String>, String), ApiError> {
    let row: Option<(Option<String>, Option<String>, String)> = query_as("DELETE FROM sign_in_challenges WHERE id = $1 AND kind = $2 AND browser_digest = $3 AND expires_at > now() RETURNING account_id, session_digest, state")
        .bind(id).bind(kind).bind(digest(browser)).fetch_optional(&service.pool).await?;
    row.ok_or_else(rejected)
}

async fn account_passkeys(service: &Service, account_id: &str) -> Result<Vec<Passkey>, ApiError> {
    let rows: Vec<(String,)> = query_as("SELECT credential FROM sign_in_methods WHERE account_id = $1 AND kind = 'passkey' AND NOT removed")
        .bind(account_id).fetch_all(&service.pool).await?;
    rows.into_iter()
        .map(|(json,)| serde_json::from_str(&json).map_err(|_| rejected()))
        .collect()
}

pub(super) async fn register_start(
    State(service): State<Service>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let (account_id, email) = account::confirmed_session(&service, &headers).await?;
    consume_limit(
        &service.pool,
        &format!("passkey-registration:{account_id}"),
        10,
    )
    .await?;

    let passkeys = account_passkeys(&service, &account_id).await?;
    if passkeys.len() >= 20 {
        return Err(ApiError::Http(
            StatusCode::CONFLICT,
            "Remove a passkey before adding another",
        ));
    }

    let exclude = passkeys.iter().map(|key| key.cred_id().clone()).collect();
    let (mut options, state) = webauthn(&service)?
        .start_passkey_registration(
            uuid::Uuid::parse_str(&account_id).map_err(|_| rejected())?,
            &email,
            &email,
            Some(exclude),
        )
        .map_err(|_| rejected())?;

    // Discoverable sign-in needs the browser to retain a resident credential.
    if let Some(selection) = options.public_key.authenticator_selection.as_mut() {
        selection.require_resident_key = true;
        selection.resident_key = Some(ResidentKeyRequirement::Required);
    }

    let token = session_token(&headers);
    let challenge = store_challenge(
        &service,
        "passkey-registration",
        token,
        Some(&account_id),
        Some(token),
        &state,
    )
    .await?;

    Ok(Json(json!({
        "challenge": challenge,
        "options": options,
    })))
}

#[derive(Deserialize)]
pub(super) struct Registration {
    challenge: String,
    credential: RegisterPublicKeyCredential,
    label: String,
}

pub(super) async fn register_finish(
    State(service): State<Service>,
    headers: HeaderMap,
    Json(input): Json<Registration>,
) -> Result<StatusCode, ApiError> {
    let (account_id, _) = methods::authenticated(&service, &headers, true).await?;

    let token = session_token(&headers);
    let (owner, session, state) =
        take_challenge(&service, &input.challenge, "passkey-registration", token).await?;
    if owner.as_deref() != Some(&account_id) || session.as_deref() != Some(digest(token).as_str()) {
        return Err(rejected());
    }

    let label = input.label.trim();
    if label.is_empty() || label.len() > 80 || label.chars().any(char::is_control) {
        return Err(ApiError::Http(
            StatusCode::BAD_REQUEST,
            "Enter a passkey name (1–80 characters)",
        ));
    }

    let state: PasskeyRegistration = serde_json::from_str(&state).map_err(|_| rejected())?;
    let passkey = webauthn(&service)?
        .finish_passkey_registration(&input.credential, &state)
        .map_err(|_| rejected())?;
    let subject = hex::encode(passkey.cred_id().as_ref());

    let mut transaction = service.pool.begin().await?;
    query("SELECT id FROM leo_accounts WHERE id = $1 FOR UPDATE")
        .bind(&account_id)
        .execute(&mut *transaction)
        .await?;
    // A stale session cannot mint the passkey used to manufacture a fresh proof.
    // Recheck after the account lock, since the independent proof may expire.
    account::confirmed_session_on(&mut transaction, &headers).await?;

    let (count,): (i64,) = query_as("SELECT count(*) FROM sign_in_methods WHERE account_id = $1 AND kind = 'passkey' AND NOT removed")
        .bind(&account_id).fetch_one(&mut *transaction).await?;
    if count >= 20 {
        return Err(ApiError::Http(
            StatusCode::CONFLICT,
            "Remove a passkey before adding another",
        ));
    }

    let result = query("INSERT INTO sign_in_methods (id, account_id, kind, subject, label, credential) VALUES ($1, $2, 'passkey', $3, $4, $5) ON CONFLICT (kind, subject) DO UPDATE SET removed = false, credential = EXCLUDED.credential, label = EXCLUDED.label WHERE sign_in_methods.removed AND sign_in_methods.account_id = EXCLUDED.account_id")
        .bind(uuid::Uuid::new_v4().to_string()).bind(account_id).bind(subject).bind(label)
        .bind(serde_json::to_string(&passkey).map_err(|_| rejected())?).execute(&mut *transaction).await?;
    if result.rows_affected() != 1 {
        return Err(ApiError::Http(
            StatusCode::CONFLICT,
            "Passkey already registered",
        ));
    }

    transaction.commit().await?;

    Ok(StatusCode::CREATED)
}

pub(super) async fn login_start(
    State(service): State<Service>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Result<Response, ApiError> {
    consume_limit(
        &service.pool,
        &format!(
            "passkey-login:{}",
            super::network::rate_limit_address(peer.ip())
        ),
        30,
    )
    .await?;

    // Every browser gets the same options, without looking up an email or exposing IDs.
    let (options, state) = webauthn(&service)?
        .start_discoverable_authentication()
        .map_err(|_| rejected())?;

    let browser = random_token();
    let challenge =
        store_challenge(&service, "passkey-login", &browser, None, None, &state).await?;

    Ok((
        [(
            header::SET_COOKIE,
            oauth::browser_cookie(&service, "leo_passkey", &browser, 300),
        )],
        Json(json!({
            "challenge": challenge,
            "options": options,
        })),
    )
        .into_response())
}

pub(super) async fn reauth_start(
    State(service): State<Service>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let (account, _) = methods::authenticated(&service, &headers, true).await?;
    consume_limit(&service.pool, &format!("passkey-reauth:{account}"), 10).await?;
    let (options, state) = webauthn(&service)?
        .start_discoverable_authentication()
        .map_err(|_| rejected())?;
    let token = session_token(&headers);
    let challenge = store_challenge(
        &service,
        "passkey-reauth",
        token,
        Some(&account),
        Some(token),
        &state,
    )
    .await?;
    Ok(Json(json!({
        "challenge": challenge,
        "options": options,
    })))
}

pub(super) async fn reauth_finish(
    State(service): State<Service>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<Authentication>,
) -> Result<Response, ApiError> {
    methods::authenticated(&service, &headers, true).await?;
    complete_login(
        &service,
        peer,
        &headers,
        input,
        ProofPurpose::ConfirmSession,
    )
    .await
}

#[derive(Deserialize)]
pub(super) struct Authentication {
    challenge: String,
    credential: PublicKeyCredential,
}

pub(super) async fn login_finish(
    State(service): State<Service>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    input: Result<Json<Authentication>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let result = match input {
        Ok(Json(input)) => {
            complete_login(&service, peer, &headers, input, ProofPurpose::SignIn).await
        }
        Err(_) => Err(ApiError::Http(
            StatusCode::BAD_REQUEST,
            "Invalid passkey proof",
        )),
    };
    let mut response = match result {
        Ok(response) => response,
        Err(error) => error.into_response(),
    };
    response.headers_mut().append(
        header::SET_COOKIE,
        oauth::browser_cookie(&service, "leo_passkey", "", 0)
            .parse()
            .expect("static cookie"),
    );
    response
}

async fn complete_login(
    service: &Service,
    peer: SocketAddr,
    headers: &HeaderMap,
    input: Authentication,
    purpose: ProofPurpose,
) -> Result<Response, ApiError> {
    consume_limit(
        &service.pool,
        &format!(
            "passkey-login:{}",
            super::network::rate_limit_address(peer.ip())
        ),
        30,
    )
    .await?;

    let (expected_owner, state) = match purpose {
        ProofPurpose::ConfirmSession => {
            let (owner, bound_session, state) = take_challenge(
                service,
                &input.challenge,
                "passkey-reauth",
                session_token(headers),
            )
            .await?;
            if bound_session.as_deref() != Some(digest(session_token(headers)).as_str()) {
                return Err(rejected());
            }
            (Some(owner.ok_or_else(rejected)?), state)
        }
        ProofPurpose::SignIn => {
            let (_, _, state) = take_challenge(
                service,
                &input.challenge,
                "passkey-login",
                cookie_token(headers, "leo_passkey"),
            )
            .await?;
            (None, state)
        }
    };
    let state: DiscoverableAuthentication = serde_json::from_str(&state).map_err(|_| rejected())?;
    let webauthn = webauthn(service)?;
    let (owner, credential_id) = webauthn
        .identify_discoverable_authentication(&input.credential)
        .map_err(|_| rejected())?;
    let account_id = owner.to_string();
    if expected_owner
        .as_deref()
        .is_some_and(|expected| expected != account_id)
    {
        return Err(rejected());
    }

    let mut transaction = service.pool.begin().await?;
    let row: Option<(String,)> =
        query_as("SELECT email FROM leo_accounts WHERE id = $1 FOR UPDATE")
            .bind(&account_id)
            .fetch_optional(&mut *transaction)
            .await?;
    let (email,) = row.ok_or_else(rejected)?;

    // Validate against the current credential and counter, including revocation while pending.
    let row: Option<(String, String)> = query_as("SELECT id, credential FROM sign_in_methods WHERE account_id = $1 AND kind = 'passkey' AND NOT removed AND subject = $2 FOR UPDATE")
        .bind(&account_id).bind(hex::encode(credential_id)).fetch_optional(&mut *transaction).await?;
    let (id, credential) = row.ok_or_else(rejected)?;
    let mut passkey: Passkey = serde_json::from_str(&credential).map_err(|_| rejected())?;
    let result = webauthn
        .finish_discoverable_authentication(
            &input.credential,
            state,
            &[DiscoverableKey::from(&passkey)],
        )
        .map_err(|_| rejected())?;
    passkey.update_credential(&result);

    query("UPDATE sign_in_methods SET credential = $1 WHERE id = $2")
        .bind(serde_json::to_string(&passkey).map_err(|_| rejected())?)
        .bind(id)
        .execute(&mut *transaction)
        .await?;

    let response = match purpose {
        ProofPurpose::ConfirmSession => {
            account::confirm_identity(&mut transaction, headers).await?;
            StatusCode::NO_CONTENT.into_response()
        }
        ProofPurpose::SignIn => {
            create_session(
                service,
                &mut transaction,
                &account_id,
                &email,
                headers,
                SessionProof::Passkey,
            )
            .await?
        }
    };

    transaction.commit().await?;

    Ok(response)
}
