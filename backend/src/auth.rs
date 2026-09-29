use crate::{
    config::now,
    error::{Error, Result},
    store::{Db, Store, merge},
    validation::text,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tokio::sync::Semaphore;

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
                db.set(
                    "admin",
                    &json!({
                        "salt": salt,
                        "hash": hash
                    }),
                    None,
                )
            })
            .await
    }

    pub async fn login(&self, password: &str) -> Result<Value> {
        if password.len() > 800 {
            return Err(Error::bad("Password is too long."));
        }
        let admin = self
            .store
            .kv("admin")
            .await?
            .ok_or_else(|| Error::unauthorized("Complete setup first."))?;
        let hash = self.password(password, text(&admin, "salt")).await?;
        if !safe_equal(&hash, text(&admin, "hash")) {
            return Err(Error::unauthorized("Incorrect password."));
        }
        self.session().await
    }

    pub async fn session(&self) -> Result<Value> {
        let value = token();
        let session = json!({
            "csrf": token(),
            "createdAt": now()
        });
        self.store
            .set(
                &format!("session:{}", digest(&value)),
                session.clone(),
                Some(now() + 7 * 86400000),
            )
            .await?;
        let mut session = session;
        session["value"] = value.into();
        Ok(session)
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
            .ok_or_else(|| Error::bad("Provide 1–10 redirect URIs."))?;
        if uris.is_empty() || uris.len() > 10 {
            return Err(Error::bad("Provide 1–10 redirect URIs."));
        }
        for uri in uris {
            let url = url::Url::parse(uri.as_str().unwrap_or(""))
                .map_err(|_| Error::bad("Invalid redirect URI."))?;
            if url.fragment().is_some()
                || !url.username().is_empty()
                || url.password().is_some()
                || (url.scheme() != "https"
                    && !(url.scheme() == "http"
                        && ["localhost", "127.0.0.1", "[::1]"]
                            .contains(&url.host_str().unwrap_or(""))))
            {
                return Err(Error::bad(
                    "Redirect URIs must use HTTPS (HTTP is allowed for loopback clients).",
                ));
            }
        }
        let client = json!({
            "client_id": token(),
            "client_name": input["client_name"]
                .as_str()
                .unwrap_or("MCP client")
                .chars()
                .take(100)
                .collect::<String>(),
            "redirect_uris": uris,
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"]
        });
        self.store
            .transaction(move |db| {
                if db.keys("client:")?.len() >= 100 {
                    return Err(Error::too_many_requests(
                        "Client registration limit reached.",
                    ));
                }
                db.set(
                    &format!("client:{}", text(&client, "client_id")),
                    &client,
                    None,
                )?;
                Ok(client)
            })
            .await
    }

    pub async fn authorization(&self, params: &Value) -> Result<Value> {
        let client = self
            .store
            .kv(&format!("client:{}", text(params, "client_id")))
            .await?
            .ok_or_else(|| Error::bad("Unknown client or redirect URI."))?;
        if !client["redirect_uris"]
            .as_array()
            .is_some_and(|uris| uris.contains(&params["redirect_uri"]))
        {
            return Err(Error::bad("Unknown client or redirect URI."));
        }
        let challenge = text(params, "code_challenge");
        if params["response_type"] != "code"
            || params["code_challenge_method"] != "S256"
            || challenge.len() != 43
            || !challenge
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(Error::bad(
                "Authorization requires code flow with S256 PKCE.",
            ));
        }
        let resource = params["resource"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{}/mcp", self.public_url));
        if resource != format!("{}/mcp", self.public_url) {
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
        Ok(json!({
            "client": client,
            "resource": resource,
            "scopes": scopes
        }))
    }

    pub async fn consent(&self, params: Value, approved: bool) -> Result<String> {
        let details = self.authorization(&params).await?;
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
        self.store
            .set(
                &format!("code:{}", digest(&code)),
                json!({
                    "clientId": details["client"]["client_id"],
                    "redirectUri": params["redirect_uri"],
                    "challenge": params["code_challenge"],
                    "resource": details["resource"],
                    "scopes": details["scopes"],
                    "label": details["client"]["client_name"]
                }),
                Some(now() + 300000),
            )
            .await?;
        redirect.query_pairs_mut().append_pair("code", &code);
        Ok(redirect.to_string())
    }

    pub async fn exchange(&self, params: Value) -> Result<Value> {
        self.store
            .transaction(move |db| match text(&params, "grant_type") {
                "authorization_code" => {
                    let key = format!("code:{}", digest(text(&params, "code")));
                    let code = db.kv(&key)?.ok_or_else(|| {
                        Error::oauth(
                            "invalid_grant",
                            "Invalid or expired authorization code, verifier, or resource.",
                        )
                    })?;
                    let verifier = text(&params, "code_verifier");
                    if params["client_id"] != code["clientId"]
                        || params["redirect_uri"] != code["redirectUri"]
                        || !(43..=128).contains(&verifier.len())
                        || digest(verifier) != text(&code, "challenge")
                        || (!text(&params, "resource").is_empty()
                            && params["resource"] != code["resource"])
                    {
                        return Err(Error::oauth(
                            "invalid_grant",
                            "Invalid or expired authorization code, verifier, or resource.",
                        ));
                    }
                    db.delete(&key)?;
                    issue(
                        db,
                        json!({
                            "clientId": code["clientId"],
                            "resource": code["resource"],
                            "scopes": code["scopes"],
                            "label": code["label"],
                            "family": token()
                        }
                        ),
                    )
                }
                "refresh_token" => {
                    let key = format!("refresh:{}", digest(text(&params, "refresh_token")));
                    let mut previous = db
                        .kv(&key)?
                        .ok_or_else(|| Error::oauth("invalid_grant", "Invalid refresh token."))?;
                    if previous["used"] == true {
                        revoke(db, text(&previous, "family"))?;
                        return Ok(json!({
                            "_oauthError": "Refresh token reuse detected. Reconnect this client."
                        }
                        ));
                    }
                    if params["client_id"] != previous["clientId"]
                        || (!text(&params, "resource").is_empty()
                            && params["resource"] != previous["resource"])
                    {
                        return Err(Error::oauth("invalid_grant", "Invalid refresh token."));
                    }
                    if !text(&params, "scope").is_empty() {
                        let scopes = text(&params, "scope")
                            .split(' ')
                            .filter(|s| !s.is_empty())
                            .map(|s| Value::String(s.into()))
                            .collect::<Vec<_>>();
                        if scopes.iter().any(|scope| {
                            !previous["scopes"]
                                .as_array()
                                .is_some_and(|allowed| allowed.contains(scope))
                        }) {
                            return Err(Error::oauth(
                                "invalid_scope",
                                "Refresh cannot add permissions.",
                            ));
                        }
                        previous["scopes"] = scopes.into();
                    }
                    let mut used = previous.clone();
                    used["used"] = true.into();
                    db.set(&key, &used, used["expiresAt"].as_i64())?;
                    issue(db, previous)
                }
                _ => Err(Error::oauth(
                    "unsupported_grant_type",
                    "Unsupported grant type.",
                )),
            })
            .await
            .and_then(|value| {
                if let Some(error) = value["_oauthError"].as_str() {
                    Err(Error::oauth("invalid_grant", error))
                } else {
                    Ok(value)
                }
            })
    }

    pub async fn verify(&self, value: &str, scope: Option<&str>) -> Result<Value> {
        let grant = self
            .store
            .kv(&format!("access:{}", digest(value)))
            .await?
            .ok_or_else(|| Error::unauthorized("A valid MCP access token is required."))?;
        if grant["resource"] != format!("{}/mcp", self.public_url) {
            return Err(Error::unauthorized("A valid MCP access token is required."));
        }
        if let Some(scope) = scope
            && !grant["scopes"]
                .as_array()
                .is_some_and(|scopes| scopes.contains(&Value::String(scope.into())))
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
        let family = token();
        let grant = json!({
            "clientId": "personal",
            "label": label.chars().take(100).collect::<String>(),
            "scopes": scopes,
            "resource": format!("{}/mcp", self.public_url),
            "family": family,
            "expiresAt": now() + 30 * 86400000_i64
        });
        self.store
            .transaction(move |db| {
                db.set(
                    &format!("access:{}", digest(&value)),
                    &grant,
                    grant["expiresAt"].as_i64(),
                )?;
                db.set(
                    &format!("grant:{family}"),
                    &grant,
                    grant["expiresAt"].as_i64(),
                )?;
                Ok(json!({
                    "token": value,
                    "expiresAt": grant["expiresAt"]
                }))
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

fn valid_scopes(scopes: &[&str]) -> Result<()> {
    if scopes.is_empty()
        || scopes
            .iter()
            .any(|s| !["read", "run", "manage"].contains(s))
    {
        return Err(Error::bad("Unsupported scope."));
    }
    Ok(())
}

fn issue(db: &Db<'_>, grant: Value) -> Result<Value> {
    let access = token();
    let refresh = token();
    let mut access_grant = grant.clone();
    access_grant["expiresAt"] = (now() + 3600000).into();
    db.set(
        &format!("access:{}", digest(&access)),
        &access_grant,
        Some(now() + 3600000),
    )?;
    let mut refresh_grant = grant.clone();
    merge(
        &mut refresh_grant,
        &json!({
            "expiresAt": now() + 30 * 86400000_i64
        }),
    );
    db.set(
        &format!("refresh:{}", digest(&refresh)),
        &refresh_grant,
        Some(now() + 30 * 86400000_i64),
    )?;
    let mut record = grant.clone();
    record["createdAt"] = now().into();
    db.set(
        &format!("grant:{}", text(&grant, "family")),
        &record,
        Some(now() + 30 * 86400000_i64),
    )?;
    Ok(json!({
        "access_token": access,
        "refresh_token": refresh,
        "token_type": "Bearer",
        "expires_in": 3600,
        "scope": grant["scopes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" ")
    }))
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
