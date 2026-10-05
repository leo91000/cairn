//! Official MCP grants. Content and tool execution remain on the installation.
use super::{ApiError, Service, digest, installations, random_token, relay};
use axum::{
    Json,
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx_core::{query::query, query_as::query_as};

type GrantListing = (String, String, Vec<String>, Option<String>, i64, i64);

const SCOPES: [&str; 3] = ["read", "run", "manage"];

async fn owner(service: &Service, installation: &str, account: &str) -> Result<(), ApiError> {
    let owned: Option<(String,)> =
        query_as("SELECT id FROM installations WHERE id = $1 AND owner_id = $2")
            .bind(installation)
            .bind(account)
            .fetch_optional(&service.pool)
            .await?;
    if owned.is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "Installation not found"));
    }

    Ok(())
}

fn valid_scopes(scopes: &[String]) -> Result<(), ApiError> {
    if scopes.is_empty() || scopes.iter().any(|scope| !SCOPES.contains(&scope.as_str())) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "Choose valid MCP scopes"));
    }

    Ok(())
}

#[derive(Deserialize)]
pub(super) struct Personal {
    label: String,
    scopes: Vec<String>,
}

pub(super) async fn personal(
    State(service): State<Service>,
    Path(installation): Path<String>,
    headers: HeaderMap,
    Json(input): Json<Personal>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let account = installations::account(&service, &headers, &Method::POST).await?;
    owner(&service, &installation, &account).await?;
    valid_scopes(&input.scopes)?;
    let label = input.label.trim();
    if label.is_empty() || label.chars().count() > 100 || label.chars().any(char::is_control) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Choose a token name (1–100 characters)",
        ));
    }

    let id = random_token();
    let token = random_token();
    let mut transaction = service.pool.begin().await?;
    query("INSERT INTO mcp_grants (id, account_id, installation_id, label, scopes, expires_at) VALUES ($1, $2, $3, $4, $5, now() + interval '30 days')")
        .bind(&id).bind(&account).bind(&installation).bind(label).bind(&input.scopes)
        .execute(&mut *transaction).await?;
    query("INSERT INTO mcp_tokens (digest, grant_id, kind, scopes, expires_at) VALUES ($1, $2, 'access', $3, now() + interval '30 days')")
        .bind(digest(&token)).bind(&id).bind(&input.scopes).execute(&mut *transaction).await?;
    transaction.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": id, "token": token })),
    ))
}

pub(super) async fn list(
    State(service): State<Service>,
    Path(installation): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<Value>>, ApiError> {
    let account = installations::account(&service, &headers, &Method::GET).await?;
    owner(&service, &installation, &account).await?;
    let rows: Vec<GrantListing> = query_as("SELECT id, label, scopes, client_id, (extract(epoch FROM created_at) * 1000)::bigint, (extract(epoch FROM expires_at) * 1000)::bigint FROM mcp_grants WHERE account_id = $1 AND installation_id = $2 AND expires_at > now() ORDER BY created_at")
        .bind(account).bind(&installation).fetch_all(&service.pool).await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, label, scopes, client, created, expires)| {
                json!({
                    "id": id,
                    "label": label,
                    "scopes": scopes,
                    "clientId": client.unwrap_or_else(|| "personal".into()),
                    "installationId": installation,
                    "createdAt": created,
                    "expiresAt": expires,
                })
            })
            .collect(),
    ))
}

pub(super) async fn revoke(
    State(service): State<Service>,
    Path((installation, grant)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let account = installations::account(&service, &headers, &Method::DELETE).await?;
    owner(&service, &installation, &account).await?;
    query("DELETE FROM mcp_grants WHERE id = $1 AND account_id = $2 AND installation_id = $3")
        .bind(grant)
        .bind(account)
        .bind(installation)
        .execute(&service.pool)
        .await?;
    Ok(Json(json!({ "revoked": true })))
}

pub(super) async fn handle(
    State(service): State<Service>,
    request: Request,
) -> Result<Response, ApiError> {
    let token = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(kind, _)| kind.eq_ignore_ascii_case("bearer"))
        .map_or("", |(_, value)| value);
    let grant: Option<(String, String, Vec<String>)> = query_as("SELECT g.account_id, g.installation_id, t.scopes FROM mcp_tokens t JOIN mcp_grants g ON g.id = t.grant_id JOIN installations i ON i.id = g.installation_id AND i.owner_id = g.account_id WHERE t.digest = $1 AND t.kind = 'access' AND NOT t.used AND t.expires_at > now() AND g.expires_at > now()")
        .bind(digest(token)).fetch_optional(&service.pool).await?;
    let Some((account, installation, scopes)) = grant else {
        let mut response = (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        )
            .into_response();
        response.headers_mut().insert(
            "www-authenticate",
            HeaderValue::from_str(&format!(
                "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource/mcp\"",
                service.origin
            ))
            .unwrap(),
        );
        response
            .headers_mut()
            .insert("cache-control", HeaderValue::from_static("no-store"));
        return Ok(response);
    };
    relay::mcp(&service, &installation, account, scopes, request).await
}

#[derive(Deserialize)]
pub(super) struct Registration {
    #[serde(default = "default_client_name")]
    client_name: String,
    redirect_uris: Vec<String>,
    token_endpoint_auth_method: Option<String>,
    grant_types: Option<Vec<String>>,
    response_types: Option<Vec<String>>,
}

fn default_client_name() -> String {
    "MCP client".into()
}

pub(super) async fn register(
    State(service): State<Service>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Json(input): Json<Registration>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    super::consume_limit(&service.pool, &format!("mcp-register:{}", peer.ip()), 10).await?;
    let valid_redirect = |uri: &String| {
        url::Url::parse(uri).is_ok_and(|url| {
            let loopback = url.scheme() == "http"
                && ["localhost", "127.0.0.1", "[::1]"].contains(&url.host_str().unwrap_or(""));
            uri.len() <= 2048
                && url.host_str().is_some()
                && url.fragment().is_none()
                && url.username().is_empty()
                && url.password().is_none()
                && (url.scheme() == "https" || loopback)
        })
    };
    if !(1..=10).contains(&input.redirect_uris.len())
        || !input.redirect_uris.iter().all(valid_redirect)
        || input
            .token_endpoint_auth_method
            .as_deref()
            .is_some_and(|method| method != "none")
        || input.grant_types.as_ref().is_some_and(|types| {
            types
                .iter()
                .any(|kind| !["authorization_code", "refresh_token"].contains(&kind.as_str()))
        })
        || input
            .response_types
            .as_ref()
            .is_some_and(|types| types.iter().any(|kind| kind != "code"))
        || input.client_name.is_empty()
        || input.client_name.chars().count() > 100
        || input.client_name.chars().any(char::is_control)
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Register a public code-flow client with valid redirect URIs",
        ));
    }

    let id = random_token();
    let mut transaction = service.pool.begin().await?;
    query("LOCK TABLE mcp_clients IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *transaction)
        .await?;
    let (count,): (i64,) = query_as("SELECT count(*) FROM mcp_clients")
        .fetch_one(&mut *transaction)
        .await?;
    if count >= 100 {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Client registration limit reached",
        ));
    }
    query("INSERT INTO mcp_clients (id, name, redirect_uris) VALUES ($1, $2, $3)")
        .bind(&id)
        .bind(&input.client_name)
        .bind(&input.redirect_uris)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "client_id": id,
            "client_name": input.client_name,
            "redirect_uris": input.redirect_uris,
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
        })),
    ))
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

struct Authorization {
    client_id: String,
    name: String,
    redirect_uri: String,
    challenge: String,
    scopes: Vec<String>,
}

async fn authorization(service: &Service, params: &Value) -> Result<Authorization, ApiError> {
    let client_id = text(params, "client_id");
    let client: Option<(String, Vec<String>)> =
        query_as("SELECT name, redirect_uris FROM mcp_clients WHERE id = $1")
            .bind(client_id)
            .fetch_optional(&service.pool)
            .await?;
    let Some((name, redirects)) = client else {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Unknown client or redirect URI",
        ));
    };
    let redirect = text(params, "redirect_uri");
    let challenge = text(params, "code_challenge");
    let resource = text(params, "resource");
    if !redirects.iter().any(|uri| uri == redirect)
        || params["response_type"] != "code"
        || params["code_challenge_method"] != "S256"
        || challenge.len() != 43
        || !challenge
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        || (!resource.is_empty() && resource != format!("{}/mcp", service.origin))
        || text(params, "state").len() > 2048
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Authorization requires an exact redirect, MCP resource and S256 PKCE",
        ));
    }
    let scopes: Vec<String> = match text(params, "scope") {
        "" => vec!["read".into()],
        scopes => scopes.split_whitespace().map(str::to_owned).collect(),
    };
    valid_scopes(&scopes)?;
    Ok(Authorization {
        client_id: client_id.into(),
        name,
        redirect_uri: redirect.into(),
        challenge: challenge.into(),
        scopes,
    })
}

pub(super) async fn authorize(
    State(service): State<Service>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<axum::response::Redirect, ApiError> {
    authorization(&service, &json!(params)).await?;
    let mut destination = url::Url::parse(&format!("{}/authorize", service.origin)).unwrap();
    destination.query_pairs_mut().extend_pairs(params);
    Ok(axum::response::Redirect::temporary(destination.as_str()))
}

pub(super) async fn preview(
    State(service): State<Service>,
    headers: HeaderMap,
    Json(params): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let account = installations::account(&service, &headers, &Method::POST).await?;
    let details = authorization(&service, &params).await?;
    Ok(Json(json!({
        "client": { "client_id": details.client_id, "client_name": details.name },
        "resource": format!("{}/mcp", service.origin),
        "scopes": details.scopes,
        "installations": installations::list(&service.pool, &account, &service.relay).await?,
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Consent {
    parameters: Value,
    installation_id: String,
    approved: bool,
}

pub(super) async fn consent(
    State(service): State<Service>,
    headers: HeaderMap,
    Json(input): Json<Consent>,
) -> Result<Json<Value>, ApiError> {
    let account = installations::account(&service, &headers, &Method::POST).await?;
    super::consume_limit(&service.pool, &format!("mcp-consent:{account}"), 30).await?;
    let details = authorization(&service, &input.parameters).await?;
    let mut redirect = url::Url::parse(&details.redirect_uri).unwrap();
    if let Some(state) = input.parameters["state"].as_str() {
        redirect.query_pairs_mut().append_pair("state", state);
    }
    if !input.approved {
        redirect
            .query_pairs_mut()
            .append_pair("error", "access_denied");
    } else {
        let code = random_token();
        let mut transaction = service.pool.begin().await?;
        let owned: Option<(String,)> =
            query_as("SELECT id FROM installations WHERE id = $1 AND owner_id = $2 FOR SHARE")
                .bind(&input.installation_id)
                .bind(&account)
                .fetch_optional(&mut *transaction)
                .await?;
        if owned.is_none() {
            return Err(ApiError(StatusCode::NOT_FOUND, "Installation not found"));
        }
        query("DELETE FROM mcp_codes WHERE expires_at <= now()")
            .execute(&mut *transaction)
            .await?;
        query("INSERT INTO mcp_codes (digest, account_id, installation_id, client_id, redirect_uri, challenge, scopes, expires_at) VALUES ($1, $2, $3, $4, $5, $6, $7, now() + interval '5 minutes')")
            .bind(digest(&code)).bind(&account).bind(input.installation_id).bind(details.client_id)
            .bind(details.redirect_uri).bind(details.challenge).bind(details.scopes).execute(&mut *transaction).await?;
        transaction.commit().await?;
        redirect.query_pairs_mut().append_pair("code", &code);
    }
    Ok(Json(json!({ "redirect": redirect.as_str() })))
}

struct OAuthError(&'static str);

impl IntoResponse for OAuthError {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, Json(json!({ "error": self.0 }))).into_response()
    }
}

pub(super) async fn exchange(
    State(service): State<Service>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    axum::extract::Form(params): axum::extract::Form<std::collections::HashMap<String, String>>,
) -> Response {
    if let Err(error) =
        super::consume_limit(&service.pool, &format!("mcp-token:{}", peer.ip()), 60).await
    {
        return error.into_response();
    }
    let params = json!(params);
    match exchange_tokens(&service, &params).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn exchange_tokens(service: &Service, params: &Value) -> Result<Response, ApiError> {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use sha2::{Digest, Sha256};
    let mut transaction = service.pool.begin().await?;
    let (grant_id, scopes) = match text(params, "grant_type") {
        "authorization_code" => {
            type Code = (String, String, String, String, String, Vec<String>, String);
            let code: Option<Code> = query_as("SELECT c.account_id, c.installation_id, c.client_id, c.redirect_uri, c.challenge, c.scopes, cl.name FROM mcp_codes c JOIN mcp_clients cl ON cl.id = c.client_id JOIN installations i ON i.id = c.installation_id AND i.owner_id = c.account_id WHERE c.digest = $1 AND c.expires_at > now() FOR UPDATE OF c FOR SHARE OF i")
                .bind(digest(text(params, "code"))).fetch_optional(&mut *transaction).await?;
            let Some((account, installation, client, redirect, challenge, scopes, label)) = code
            else {
                return Ok(OAuthError("invalid_grant").into_response());
            };
            let verifier = text(params, "code_verifier");
            if text(params, "client_id") != client
                || text(params, "redirect_uri") != redirect
                || !(43..=128).contains(&verifier.len())
                || !verifier
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-._~".contains(&byte))
                || URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) != challenge
                || !resource_matches(service, params)
            {
                return Ok(OAuthError("invalid_grant").into_response());
            }
            query("DELETE FROM mcp_codes WHERE digest = $1")
                .bind(digest(text(params, "code")))
                .execute(&mut *transaction)
                .await?;
            let grant = random_token();
            query("INSERT INTO mcp_grants (id, account_id, installation_id, client_id, label, scopes, expires_at) VALUES ($1, $2, $3, $4, $5, $6, now() + interval '30 days')")
                .bind(&grant).bind(account).bind(installation).bind(client).bind(label).bind(&scopes).execute(&mut *transaction).await?;
            (grant, scopes)
        }
        "refresh_token" => {
            let previous: Option<(String, Vec<String>, bool, String)> = query_as("SELECT g.id, t.scopes, t.used, g.client_id FROM mcp_tokens t JOIN mcp_grants g ON g.id = t.grant_id JOIN installations i ON i.id = g.installation_id AND i.owner_id = g.account_id WHERE t.digest = $1 AND t.kind = 'refresh' AND t.expires_at > now() AND g.expires_at > now() FOR UPDATE OF g, t FOR SHARE OF i")
                .bind(digest(text(params, "refresh_token"))).fetch_optional(&mut *transaction).await?;
            let Some((grant, mut scopes, used, client)) = previous else {
                return Ok(OAuthError("invalid_grant").into_response());
            };
            if text(params, "client_id") != client || !resource_matches(service, params) {
                return Ok(OAuthError("invalid_grant").into_response());
            }
            if used {
                query("DELETE FROM mcp_grants WHERE id = $1")
                    .bind(grant)
                    .execute(&mut *transaction)
                    .await?;
                transaction.commit().await?;
                return Ok(OAuthError("invalid_grant").into_response());
            }
            let requested = text(params, "scope");
            if !requested.is_empty() {
                let narrowed: Vec<String> =
                    requested.split_whitespace().map(str::to_owned).collect();
                if narrowed.is_empty() || narrowed.iter().any(|scope| !scopes.contains(scope)) {
                    return Ok(OAuthError("invalid_scope").into_response());
                }
                scopes = narrowed;
            }
            query("UPDATE mcp_tokens SET used = true WHERE digest = $1")
                .bind(digest(text(params, "refresh_token")))
                .execute(&mut *transaction)
                .await?;
            query("UPDATE mcp_grants SET scopes = $2, expires_at = now() + interval '30 days' WHERE id = $1").bind(&grant).bind(&scopes).execute(&mut *transaction).await?;
            (grant, scopes)
        }
        _ => return Ok(OAuthError("unsupported_grant_type").into_response()),
    };
    let access = random_token();
    let refresh = random_token();
    query("INSERT INTO mcp_tokens (digest, grant_id, kind, scopes, expires_at) VALUES ($1, $2, 'access', $3, now() + interval '1 hour'), ($4, $2, 'refresh', $3, now() + interval '30 days')")
        .bind(digest(&access)).bind(grant_id).bind(&scopes).bind(digest(&refresh)).execute(&mut *transaction).await?;
    transaction.commit().await?;
    Ok(Json(json!({
        "access_token": access,
        "refresh_token": refresh,
        "token_type": "Bearer",
        "expires_in": 3600,
        "scope": scopes.join(" "),
    }))
    .into_response())
}

fn resource_matches(service: &Service, params: &Value) -> bool {
    let resource = text(params, "resource");
    resource.is_empty() || resource == format!("{}/mcp", service.origin)
}

pub(super) async fn revoke_token(
    State(service): State<Service>,
    axum::extract::Form(params): axum::extract::Form<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let params = json!(params);
    query("DELETE FROM mcp_grants g USING mcp_tokens t WHERE t.grant_id = g.id AND t.digest = $1 AND g.client_id = $2")
        .bind(digest(text(&params, "token"))).bind(text(&params, "client_id")).execute(&service.pool).await?;
    Ok(Json(json!({})))
}

pub(super) async fn resource_metadata(State(service): State<Service>) -> Json<Value> {
    Json(json!({
        "resource": format!("{}/mcp", service.origin),
        "authorization_servers": [service.origin],
        "scopes_supported": SCOPES,
        "bearer_methods_supported": ["header"],
        "resource_name": "Leo Agent Manager",
    }))
}

pub(super) async fn server_metadata(State(service): State<Service>) -> Json<Value> {
    let origin = &service.origin;
    Json(json!({
        "issuer": origin,
        "authorization_endpoint": format!("{origin}/oauth/authorize"),
        "token_endpoint": format!("{origin}/oauth/token"),
        "registration_endpoint": format!("{origin}/oauth/register"),
        "revocation_endpoint": format!("{origin}/oauth/revoke"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": SCOPES,
    }))
}

pub(super) async fn public_security(request: Request, next: axum::middleware::Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    for (name, value) in [
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("content-security-policy", "default-src 'none'; sandbox"),
        ("referrer-policy", "no-referrer"),
        ("x-robots-tag", "noindex, nofollow"),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    headers.remove("set-cookie");
    response
}
