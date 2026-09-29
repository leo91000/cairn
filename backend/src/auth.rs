use crate::{
    config::now,
    error::{Error, Result},
    store::{Db, Store},
    validation::text,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tokio::sync::Semaphore;

const DAY_MS: i64 = 86_400_000;
const SESSION_TTL: i64 = 7 * DAY_MS;
const CODE_TTL: i64 = 300_000;
const ACCESS_TTL: i64 = 3_600_000;
const REFRESH_TTL: i64 = 30 * DAY_MS;
const PERSONAL_TTL: i64 = 30 * DAY_MS;
const MAX_CLIENTS: usize = 100;
const SCOPES: [&str; 3] = ["read", "run", "manage"];
const INVALID_CODE: &str = "Invalid or expired authorization code, verifier, or resource.";
const INVALID_REFRESH: &str = "Invalid refresh token.";

pub fn token() -> String {
    let mut bytes = [0; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn digest(value: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(value.as_bytes()))
}

pub fn hex_digest(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

pub fn safe_equal(a: &str, b: &str) -> bool {
    bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

/// The single administrator password, stored as `admin`.
#[derive(Serialize, Deserialize)]
struct Admin {
    salt: String,
    hash: String,
}

/// Browser session, stored as `session:{digest}`; `value` is only returned once.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Session {
    csrf: String,
    created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
}

/// Dynamically registered OAuth client (RFC 7591), stored as `client:{client_id}`.
#[derive(Clone, Serialize, Deserialize)]
struct OAuthClient {
    client_id: String,
    client_name: String,
    redirect_uris: Vec<String>,
    token_endpoint_auth_method: String,
    grant_types: Vec<String>,
    response_types: Vec<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Serialize)]
struct Authorization {
    client: OAuthClient,
    resource: String,
    scopes: Vec<String>,
}

/// Single-use authorization code, stored as `code:{digest}`.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthorizationCode {
    client_id: String,
    redirect_uri: String,
    challenge: String,
    resource: String,
    scopes: Vec<String>,
    label: String,
}

/// Token grant shared by an access token, its refresh token and the `grant:`
/// listing. Tokens of one `family` are revoked together.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Grant {
    client_id: String,
    resource: String,
    scopes: Vec<String>,
    label: String,
    family: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created_at: Option<i64>,
    /// A rotated refresh token; presenting it again revokes the family.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    used: bool,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Serialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    token_type: &'static str,
    expires_in: i64,
    scope: String,
}

/// The reuse revocation must commit, so it is reported after the transaction.
enum Exchange {
    Issued(TokenResponse),
    RefreshReused,
}

#[derive(Clone)]
pub struct Auth {
    pub store: Store,
    pub public_url: String,
    hash_slots: Arc<Semaphore>,
}

impl Auth {
    pub fn new(store: Store, public_url: String) -> Self {
        Self {
            store,
            public_url,
            hash_slots: Arc::new(Semaphore::new(2)),
        }
    }
    fn resource(&self) -> String {
        format!("{}/mcp", self.public_url)
    }
    async fn password(&self, password: &str, salt: &str) -> Result<String> {
        let permit = self
            .hash_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(Error::internal)?;
        let (password, salt) = (password.to_owned(), salt.to_owned());
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut out = [0; 64];
            scrypt::scrypt(
                password.as_bytes(),
                salt.as_bytes(),
                &scrypt::Params::new(14, 8, 1, 64).map_err(Error::internal)?,
                &mut out,
            )
            .map_err(Error::internal)?;
            Ok(hex::encode(out))
        })
        .await
        .map_err(Error::internal)?
    }

    pub async fn setup(&self, password: &str) -> Result<()> {
        if !(12..=200).contains(&password.chars().count()) {
            return Err(Error::bad("Use a password between 12 and 200 characters."));
        }
        if self.store.kv("admin").await?.is_some() {
            return Err(Error::conflict("Setup is already complete."));
        }
        let salt = token();
        let hash = self.password(password, &salt).await?;
        self.store
            .transaction(move |db| {
                if db.kv("admin")?.is_some() {
                    return Err(Error::conflict("Setup is already complete."));
                }
                db.set_as("admin", &Admin { salt, hash }, None)
            })
            .await
    }

    pub async fn login(&self, password: &str) -> Result<Value> {
        if password.len() > 800 {
            return Err(Error::bad("Password is too long."));
        }
        let admin = self
            .store
            .kv_as::<Admin>("admin")
            .await?
            .ok_or_else(|| Error::unauthorized("Complete setup first."))?;
        let hash = self.password(password, &admin.salt).await?;
        if !safe_equal(&hash, &admin.hash) {
            return Err(Error::unauthorized("Incorrect password."));
        }
        self.session().await
    }

    pub async fn session(&self) -> Result<Value> {
        let value = token();
        let mut session = Session {
            csrf: token(),
            created_at: now(),
            value: None,
        };
        self.store
            .set(
                &format!("session:{}", digest(&value)),
                serde_json::to_value(&session)?,
                Some(now() + SESSION_TTL),
            )
            .await?;
        session.value = Some(value);
        Ok(serde_json::to_value(session)?)
    }

    pub async fn read(&self, value: &str) -> Result<Option<Value>> {
        if value.is_empty() {
            return Ok(None);
        }
        self.store.kv(&format!("session:{}", digest(value))).await
    }

    pub async fn logout(&self, value: &str) -> Result<()> {
        self.store
            .delete(&format!("session:{}", digest(value)))
            .await
    }

    pub async fn register(&self, input: Value) -> Result<Value> {
        let uris = input["redirect_uris"]
            .as_array()
            .filter(|uris| (1..=10).contains(&uris.len()))
            .ok_or_else(|| Error::bad("Provide 1–10 redirect URIs."))?
            .iter()
            .map(|uri| redirect_uri(uri.as_str().unwrap_or("")))
            .collect::<Result<Vec<_>>>()?;
        let client = OAuthClient {
            client_id: token(),
            client_name: input["client_name"]
                .as_str()
                .unwrap_or("MCP client")
                .chars()
                .take(100)
                .collect(),
            redirect_uris: uris,
            token_endpoint_auth_method: "none".into(),
            grant_types: vec!["authorization_code".into(), "refresh_token".into()],
            response_types: vec!["code".into()],
            extra: Map::new(),
        };
        self.store
            .transaction(move |db| {
                if db.keys("client:")?.len() >= MAX_CLIENTS {
                    return Err(Error::too_many_requests(
                        "Client registration limit reached.",
                    ));
                }
                db.set_as(&format!("client:{}", client.client_id), &client, None)?;
                Ok(serde_json::to_value(client)?)
            })
            .await
    }
    async fn authorize(&self, params: &Value) -> Result<Authorization> {
        let client = self
            .store
            .kv_as::<OAuthClient>(&format!("client:{}", text(params, "client_id")))
            .await?
            .ok_or_else(|| Error::bad("Unknown client or redirect URI."))?;
        if !client
            .redirect_uris
            .iter()
            .any(|uri| params["redirect_uri"] == *uri)
        {
            return Err(Error::bad("Unknown client or redirect URI."));
        }
        let challenge = text(params, "code_challenge");
        if params["response_type"] != "code"
            || params["code_challenge_method"] != "S256"
            || !valid_challenge(challenge)
        {
            return Err(Error::bad(
                "Authorization requires code flow with S256 PKCE.",
            ));
        }
        let resource = params["resource"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map_or_else(|| self.resource(), str::to_owned);
        if resource != self.resource() {
            return Err(Error::bad("Resource does not match this MCP server."));
        }
        let scopes = params["scope"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or("read")
            .split(' ')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        valid_scopes(&scopes)?;
        Ok(Authorization {
            client,
            resource,
            scopes: scopes.into_iter().map(str::to_owned).collect(),
        })
    }
    pub async fn authorization(&self, params: &Value) -> Result<Value> {
        Ok(serde_json::to_value(self.authorize(params).await?)?)
    }

    pub async fn consent(&self, params: Value, approved: bool) -> Result<String> {
        let details = self.authorize(&params).await?;
        let mut redirect = url::Url::parse(text(&params, "redirect_uri"))
            .map_err(|_| Error::bad("Invalid redirect URI"))?;
        if let Some(state) = params["state"].as_str() {
            redirect.query_pairs_mut().append_pair("state", state);
        }
        if !approved {
            redirect
                .query_pairs_mut()
                .append_pair("error", "access_denied");
            return Ok(redirect.to_string());
        }
        let code = token();
        let record = AuthorizationCode {
            client_id: details.client.client_id,
            redirect_uri: text(&params, "redirect_uri").to_owned(),
            challenge: text(&params, "code_challenge").to_owned(),
            resource: details.resource,
            scopes: details.scopes,
            label: details.client.client_name,
        };
        self.store
            .set_as(
                &format!("code:{}", digest(&code)),
                record,
                Some(now() + CODE_TTL),
            )
            .await?;
        redirect.query_pairs_mut().append_pair("code", &code);
        Ok(redirect.to_string())
    }

    pub async fn exchange(&self, params: Value) -> Result<Value> {
        let exchange = self
            .store
            .transaction(move |db| match text(&params, "grant_type") {
                "authorization_code" => exchange_code(db, &params).map(Exchange::Issued),
                "refresh_token" => refresh(db, &params),
                _ => Err(Error::oauth(
                    "unsupported_grant_type",
                    "Unsupported grant type.",
                )),
            })
            .await?;
        match exchange {
            Exchange::Issued(tokens) => Ok(serde_json::to_value(tokens)?),
            Exchange::RefreshReused => Err(Error::oauth(
                "invalid_grant",
                "Refresh token reuse detected. Reconnect this client.",
            )),
        }
    }

    pub async fn verify(&self, value: &str, scope: Option<&str>) -> Result<Value> {
        let grant = self
            .store
            .kv(&format!("access:{}", digest(value)))
            .await?
            .filter(|grant| grant["resource"] == self.resource())
            .ok_or_else(|| Error::unauthorized("A valid MCP access token is required."))?;
        if let Some(scope) = scope
            && !grant["scopes"]
                .as_array()
                .is_some_and(|scopes| scopes.iter().any(|s| s == scope))
        {
            return Err(Error::forbidden(format!("The {scope} scope is required.")));
        }
        Ok(grant)
    }

    pub async fn personal(&self, label: &str, scopes: Vec<&str>) -> Result<Value> {
        if label.trim().is_empty() || label.len() > 400 {
            return Err(Error::bad("Choose a token name and valid scopes."));
        }
        valid_scopes(&scopes)?;
        let value = token();
        let expires_at = now() + PERSONAL_TTL;
        let grant = Grant {
            client_id: "personal".into(),
            resource: self.resource(),
            scopes: scopes.into_iter().map(str::to_owned).collect(),
            label: label.chars().take(100).collect(),
            family: token(),
            expires_at: Some(expires_at),
            created_at: None,
            used: false,
            extra: Map::new(),
        };
        self.store
            .transaction(move |db| {
                let expires = Some(expires_at);
                db.set_as(&format!("access:{}", digest(&value)), &grant, expires)?;
                db.set_as(&format!("grant:{}", grant.family), &grant, expires)?;
                Ok(json!({ "token": value, "expiresAt": expires_at }))
            })
            .await
    }

    pub async fn revoke(&self, family: &str) -> Result<()> {
        let family = family.to_owned();
        self.store.transaction(move |db| revoke(db, &family)).await
    }

    pub async fn revoke_token(&self, value: &str, client_id: &str) -> Result<()> {
        for prefix in ["access:", "refresh:"] {
            if let Some(grant) = self.store.kv(&format!("{prefix}{}", digest(value))).await?
                && grant["clientId"] == client_id
            {
                self.revoke(text(&grant, "family")).await?;
            }
        }
        Ok(())
    }
}

/// HTTPS only, except plain HTTP to loopback clients; no fragment or credentials.
fn redirect_uri(uri: &str) -> Result<String> {
    let url = url::Url::parse(uri).map_err(|_| Error::bad("Invalid redirect URI."))?;
    let loopback_http = url.scheme() == "http"
        && ["localhost", "127.0.0.1", "[::1]"].contains(&url.host_str().unwrap_or(""));
    if url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || (url.scheme() != "https" && !loopback_http)
    {
        return Err(Error::bad(
            "Redirect URIs must use HTTPS (HTTP is allowed for loopback clients).",
        ));
    }
    Ok(uri.to_owned())
}

/// A base64url SHA-256 PKCE challenge.
fn valid_challenge(challenge: &str) -> bool {
    challenge.len() == 43
        && challenge
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn valid_scopes(scopes: &[&str]) -> Result<()> {
    if scopes.is_empty() || scopes.iter().any(|s| !SCOPES.contains(s)) {
        return Err(Error::bad("Unsupported scope."));
    }
    Ok(())
}

/// A resource is optional on token requests but must match when present.
fn resource_matches(params: &Value, resource: &str) -> bool {
    text(params, "resource").is_empty() || params["resource"] == resource
}

fn exchange_code(db: &Db<'_>, params: &Value) -> Result<TokenResponse> {
    let key = format!("code:{}", digest(text(params, "code")));
    let code = db
        .kv_as::<AuthorizationCode>(&key)?
        .ok_or_else(|| Error::oauth("invalid_grant", INVALID_CODE))?;
    let verifier = text(params, "code_verifier");
    if params["client_id"] != code.client_id.as_str()
        || params["redirect_uri"] != code.redirect_uri.as_str()
        || !(43..=128).contains(&verifier.len())
        || digest(verifier) != code.challenge
        || !resource_matches(params, &code.resource)
    {
        return Err(Error::oauth("invalid_grant", INVALID_CODE));
    }
    db.delete(&key)?;
    let grant = Grant {
        client_id: code.client_id,
        resource: code.resource,
        scopes: code.scopes,
        label: code.label,
        family: token(),
        expires_at: None,
        created_at: None,
        used: false,
        extra: Map::new(),
    };
    issue(db, &grant)
}

fn refresh(db: &Db<'_>, params: &Value) -> Result<Exchange> {
    let key = format!("refresh:{}", digest(text(params, "refresh_token")));
    let mut previous = db
        .kv_as::<Grant>(&key)?
        .ok_or_else(|| Error::oauth("invalid_grant", INVALID_REFRESH))?;
    if previous.used {
        revoke(db, &previous.family)?;
        return Ok(Exchange::RefreshReused);
    }
    if params["client_id"] != previous.client_id.as_str()
        || !resource_matches(params, &previous.resource)
    {
        return Err(Error::oauth("invalid_grant", INVALID_REFRESH));
    }
    let requested = text(params, "scope");
    if !requested.is_empty() {
        let scopes = requested
            .split(' ')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if scopes.iter().any(|scope| !previous.scopes.contains(scope)) {
            return Err(Error::oauth(
                "invalid_scope",
                "Refresh cannot add permissions.",
            ));
        }
        previous.scopes = scopes;
    }
    let used = Grant {
        used: true,
        ..previous.clone()
    };
    db.set_as(&key, &used, used.expires_at)?;
    issue(db, &previous).map(Exchange::Issued)
}

fn issue(db: &Db<'_>, grant: &Grant) -> Result<TokenResponse> {
    let access = token();
    let refresh = token();
    let access_expires = now() + ACCESS_TTL;
    let access_grant = Grant {
        expires_at: Some(access_expires),
        ..grant.clone()
    };
    db.set_as(
        &format!("access:{}", digest(&access)),
        &access_grant,
        Some(access_expires),
    )?;
    let refresh_grant = Grant {
        expires_at: Some(now() + REFRESH_TTL),
        ..grant.clone()
    };
    db.set_as(
        &format!("refresh:{}", digest(&refresh)),
        &refresh_grant,
        Some(now() + REFRESH_TTL),
    )?;
    let record = Grant {
        created_at: Some(now()),
        ..grant.clone()
    };
    db.set_as(
        &format!("grant:{}", grant.family),
        &record,
        Some(now() + REFRESH_TTL),
    )?;
    Ok(TokenResponse {
        access_token: access,
        refresh_token: refresh,
        token_type: "Bearer",
        expires_in: ACCESS_TTL / 1000,
        scope: grant.scopes.join(" "),
    })
}

fn revoke(db: &Db<'_>, family: &str) -> Result<()> {
    for prefix in ["access:", "refresh:"] {
        for (key, value) in db.keys(prefix)? {
            if value["family"] == family {
                db.delete(&key)?;
            }
        }
    }
    db.delete(&format!("grant:{family}"))
}
