use crate::{
    auth::{digest, hex_digest, token},
    config::now,
    error::{Error, Result, required},
    mcp_client::{Client, MODERN, modern_meta},
    mcps::{
        Mcps, callback_url, cancel_pending,
        record::{ConnectionState, McpSecrets, McpServer, OAuthDiscovery, PendingAuthorization},
    },
    network,
    rpc::jsonrpc::{Frame, Message},
    service::Service,
    validation::text,
};
use base64::Engine;
use reqwest::{
    Method,
    header::{HeaderMap, HeaderValue},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::LazyLock};

/// Pending authorizations expire after ten minutes.
const PENDING_TTL_MS: i64 = 600_000;
const EXPIRED: &str = "Authorization session expired. Start again from MCPs.";

static RESOURCE_METADATA: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r#"resource_metadata="([^"]+)""#).unwrap());

fn pending_key(state: &str) -> String {
    format!("mcp-oauth:{}", hex_digest(state))
}

fn content_type(value: &'static str) -> HeaderMap {
    HeaderMap::from_iter([(
        "content-type".parse().unwrap(),
        HeaderValue::from_static(value),
    )])
}

async fn get(server: &McpServer, url: &str) -> Result<Value> {
    let response = network::fetch(
        url,
        Method::GET,
        HeaderMap::new(),
        None,
        server.allow_private_network,
    )
    .await?;
    if !(200..300).contains(&response.status) {
        return Err(Error::bad_gateway("OAuth metadata is unavailable."));
    }
    response.json()
}

fn valid_url(server: &McpServer, value: &str) -> Result<url::Url> {
    let invalid = || Error::bad("Unsupported authorization URL.");
    let url = url::Url::parse(value).map_err(|_| invalid())?;
    let insecure = !server.allow_private_network && url.scheme() != "https";
    if !["http", "https"].contains(&url.scheme())
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || insecure
    {
        return Err(invalid());
    }
    Ok(url)
}

/// Sends an unauthenticated probe and returns the server's `WWW-Authenticate` challenge.
async fn challenge(server: &McpServer, endpoint: &url::Url) -> Result<String> {
    let probe = Frame::strict(Message::Request {
        id: 1,
        method: "server/discover".into(),
        params: json!({ "_meta": modern_meta() }),
    });
    let mut headers = crate::mcp_client::headers("", MODERN, None)?;
    headers.insert("mcp-method", HeaderValue::from_static("server/discover"));
    let response = network::fetch(
        endpoint.as_str(),
        Method::POST,
        headers,
        Some(serde_json::to_vec(&probe)?),
        server.allow_private_network,
    )
    .await?;
    if response.status != 401 {
        return Err(Error::bad(
            "The server did not request OAuth. Use Test connection or choose no authentication.",
        ));
    }
    Ok(response
        .headers
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned())
}

/// Protected resource metadata and the URL it was read from. Without an explicit
/// challenge URL, the path-specific well-known location falls back to the root one.
async fn protected_resource(
    server: &McpServer,
    endpoint: &url::Url,
    challenge: &str,
) -> Result<(String, Value)> {
    let origin = endpoint.origin().ascii_serialization();
    let advertised = RESOURCE_METADATA
        .captures(challenge)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_owned());
    let metadata_url = advertised.unwrap_or_else(|| {
        format!(
            "{origin}/.well-known/oauth-protected-resource{}",
            endpoint.path().trim_end_matches('/')
        )
    });
    match get(server, &metadata_url).await {
        Ok(resource) => Ok((metadata_url, resource)),
        Err(error) if challenge.contains("resource_metadata=") => Err(error),
        Err(_) => {
            let root = format!("{origin}/.well-known/oauth-protected-resource");
            let resource = get(server, &root).await.map_err(|_| {
                Error::bad_gateway("OAuth protected resource metadata is unavailable.")
            })?;
            Ok((root, resource))
        }
    }
}

/// Whether the advertised resource is the endpoint or one of its path ancestors.
fn resource_covers(resource: &url::Url, endpoint: &url::Url) -> bool {
    let (resource_path, path) = (resource.path(), endpoint.path());
    resource.origin() == endpoint.origin()
        && path.starts_with(resource_path)
        && (path == resource_path
            || resource_path.ends_with('/')
            || path[resource_path.len()..].starts_with('/'))
}

async fn discover(s: &Service, server: &McpServer) -> Result<OAuthDiscovery> {
    let endpoint = valid_url(server, &server.url)?;
    let challenge = challenge(server, &endpoint).await?;
    let (resource_metadata_url, resource) =
        protected_resource(server, &endpoint, &challenge).await?;
    let resource_url = valid_url(server, text(&resource, "resource"))?;
    if !resource_covers(&resource_url, &endpoint) {
        return Err(Error::bad(
            "OAuth resource does not match the configured MCP server.",
        ));
    }
    let authorization_server = resource["authorization_servers"]
        .as_array()
        .and_then(|servers| servers.first())
        .and_then(Value::as_str)
        .ok_or_else(|| Error::bad("OAuth authorization server is missing."))?
        .to_owned();
    let metadata = authorization_metadata(server, &authorization_server).await?;
    let discovery = OAuthDiscovery {
        authorization_server_url: authorization_server,
        resource_metadata_url,
        resource_metadata: resource,
        authorization_server_metadata: metadata,
    };
    let stored = discovery.clone();
    s.mcps
        .update_secrets(s, &server.id, |secrets| secrets.discovery = Some(stored))
        .await?;
    Ok(discovery)
}

async fn authorization_metadata(server: &McpServer, issuer: &str) -> Result<Value> {
    let issuer_url = valid_url(server, issuer)?;
    let suffix = issuer_url.path().trim_end_matches('/');
    let origin = issuer_url.origin().ascii_serialization();
    let urls = [
        format!("{origin}/.well-known/oauth-authorization-server{suffix}"),
        format!("{origin}/.well-known/openid-configuration{suffix}"),
        format!("{origin}{suffix}/.well-known/openid-configuration"),
    ];
    let mut metadata = None;
    for url in urls {
        if let Ok(value) = get(server, &url).await {
            metadata = Some(value);
            break;
        }
    }
    let metadata = required(metadata, "OAuth authorization metadata is unavailable.")?;
    if text(&metadata, "issuer") != issuer {
        return Err(Error::bad(
            "OAuth authorization server identity does not match discovery.",
        ));
    }
    for key in ["authorization_endpoint", "token_endpoint"] {
        valid_url(server, text(&metadata, key))?;
    }
    let lacks_s256 = metadata["code_challenge_methods_supported"]
        .as_array()
        .is_some_and(|methods| !methods.iter().any(|m| m == "S256"));
    if lacks_s256 {
        return Err(Error::bad("This OAuth server does not support S256 PKCE."));
    }
    Ok(metadata)
}

/// OAuth client credentials: configured on the connection or dynamically registered.
struct OAuthClient {
    id: String,
    secret: String,
    auth_method: String,
}

impl OAuthClient {
    fn from_registration(client: &Value) -> Self {
        Self {
            id: text(client, "client_id").to_owned(),
            secret: text(client, "client_secret").to_owned(),
            auth_method: text(client, "token_endpoint_auth_method").to_owned(),
        }
    }
}

#[derive(Serialize)]
struct ClientRegistration<'a> {
    client_name: &'static str,
    redirect_uris: [String; 1],
    grant_types: [&'static str; 2],
    response_types: [&'static str; 1],
    token_endpoint_auth_method: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<&'a str>,
}

async fn register(s: &Service, server: &McpServer, endpoint: &str) -> Result<Value> {
    valid_url(server, endpoint)?;
    let registration = ClientRegistration {
        client_name: "Leo Agent Manager",
        redirect_uris: [callback_url(s)],
        grant_types: ["authorization_code", "refresh_token"],
        response_types: ["code"],
        token_endpoint_auth_method: "none",
        scope: Some(server.scopes.as_str()).filter(|scopes| !scopes.is_empty()),
    };
    let response = network::fetch(
        endpoint,
        Method::POST,
        content_type("application/json"),
        Some(serde_json::to_vec(&registration)?),
        server.allow_private_network,
    )
    .await?;
    if !(200..300).contains(&response.status) {
        return Err(Error::bad("OAuth client registration failed."));
    }
    let client = response.json()?;
    if text(&client, "client_id").is_empty() {
        return Err(Error::bad(
            "OAuth server returned invalid client information.",
        ));
    }
    Ok(client)
}

async fn client(
    s: &Service,
    server: &McpServer,
    discovery: &OAuthDiscovery,
) -> Result<OAuthClient> {
    let secrets = s.mcps.server_secrets(s, &server.id).await?;
    if !server.client_id.is_empty() {
        return Ok(OAuthClient {
            id: server.client_id.clone(),
            secret: secrets.client_secret().to_owned(),
            auth_method: String::new(),
        });
    }
    if let Some(client) = secrets
        .client
        .as_ref()
        .filter(|c| !text(c, "client_id").is_empty())
    {
        return Ok(OAuthClient::from_registration(client));
    }
    let endpoint = discovery.server("registration_endpoint");
    if endpoint.is_empty() {
        return Err(Error::bad(
            "Enter an OAuth client ID registered with this provider.",
        ));
    }
    let client = register(s, server, endpoint).await?;
    let registered = OAuthClient::from_registration(&client);
    s.mcps
        .update_secrets(s, &server.id, |secrets| secrets.client = Some(client))
        .await?;
    Ok(registered)
}

/// Applies the client's token endpoint authentication: HTTP Basic credentials
/// (returned as an `Authorization` value) or a `client_secret` form parameter.
fn authenticate_client(
    client: &OAuthClient,
    discovery: &OAuthDiscovery,
    parameters: &mut HashMap<String, String>,
) -> Result<Option<String>> {
    if client.secret.is_empty() {
        return Ok(None);
    }
    let method = client.auth_method.as_str();
    let supported =
        &discovery.authorization_server_metadata["token_endpoint_auth_methods_supported"];
    let only_basic = supported.as_array().is_some_and(|methods| {
        methods.iter().any(|m| m == "client_secret_basic")
            && !methods.iter().any(|m| m == "client_secret_post")
    });
    if method == "client_secret_basic" || (method.is_empty() && only_basic) {
        let encode = |value: &str| {
            url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>()
        };
        let credentials = format!("{}:{}", encode(&client.id), encode(&client.secret));
        parameters.remove("client_id");
        let encoded = base64::engine::general_purpose::STANDARD.encode(credentials);
        return Ok(Some(format!("Basic {encoded}")));
    }
    if method.is_empty() || method == "client_secret_post" {
        parameters.insert("client_secret".into(), client.secret.clone());
        return Ok(None);
    }
    Err(Error::bad(
        "Unsupported OAuth client authentication method.",
    ))
}

fn validate_tokens(tokens: &Value) -> Result<()> {
    if text(tokens, "access_token").is_empty()
        || !text(tokens, "token_type").eq_ignore_ascii_case("bearer")
    {
        return Err(Error::unauthorized("OAuth returned invalid credentials."));
    }
    let valid_lifetime = tokens["expires_in"]
        .as_f64()
        .is_some_and(|n| n.is_finite() && (0. ..=315_360_000.).contains(&n));
    if tokens.get("expires_in").is_some() && !valid_lifetime {
        return Err(Error::unauthorized(
            "OAuth returned an invalid token lifetime.",
        ));
    }
    Ok(())
}

async fn exchange(
    s: &Service,
    server: &McpServer,
    mut parameters: HashMap<String, String>,
    discovery: &OAuthDiscovery,
) -> Result<()> {
    let client = client(s, server, discovery).await?;
    parameters.insert("client_id".into(), client.id.clone());
    let authorization = authenticate_client(&client, discovery, &mut parameters)?;
    parameters.insert("resource".into(), discovery.resource().to_owned());
    let endpoint = discovery.server("token_endpoint");
    valid_url(server, endpoint)?;
    let mut headers = content_type("application/x-www-form-urlencoded");
    if let Some(value) = authorization {
        let value = HeaderValue::from_str(&value)
            .map_err(|_| Error::bad("Invalid OAuth client credentials."))?;
        headers.insert("authorization", value);
    }
    let body = serde_urlencoded::to_string(&parameters).map_err(Error::internal)?;
    let response = network::fetch(
        endpoint,
        Method::POST,
        headers,
        Some(body.into_bytes()),
        server.allow_private_network,
    )
    .await?;
    if !(200..300).contains(&response.status) {
        return Err(Error::unauthorized("Sign in to connect this server."));
    }
    let mut tokens = response.json()?;
    validate_tokens(&tokens)?;
    // Servers may omit an unchanged refresh token from a refresh response.
    let refreshing = parameters
        .get("grant_type")
        .is_some_and(|grant| grant == "refresh_token");
    if refreshing && tokens.get("refresh_token").is_none() {
        let previous = parameters.get("refresh_token").cloned().unwrap_or_default();
        tokens["refresh_token"] = previous.into();
    }
    let expires = tokens["expires_in"]
        .as_f64()
        .map(|seconds| now() + (seconds * 1000.) as i64);
    s.mcps
        .update_secrets(s, &server.id, |secrets| {
            secrets.tokens = Some(tokens);
            secrets.token_expires_at = expires;
        })
        .await
}

pub async fn refresh(s: &Service, server: &McpServer) -> Result<()> {
    let secrets = s.mcps.server_secrets(s, &server.id).await?;
    let refresh = secrets.refresh_token();
    if refresh.is_empty() {
        return Err(Error::unauthorized("Sign in to connect this server."));
    }
    let discovery = match secrets.discovery.clone() {
        Some(discovery) => discovery,
        None => discover(s, server).await?,
    };
    let parameters = HashMap::from([
        ("grant_type".into(), "refresh_token".into()),
        ("refresh_token".into(), refresh.into()),
    ]);
    exchange(s, server, parameters, &discovery).await
}

/// The authorization endpoint URL with PKCE, state, resource and scope parameters.
fn authorization_url(
    s: &Service,
    server: &McpServer,
    discovery: &OAuthDiscovery,
    client: &OAuthClient,
    verifier: &str,
    nonce: &str,
) -> Result<url::Url> {
    let mut url = valid_url(server, discovery.server("authorization_endpoint"))?;
    url.query_pairs_mut().extend_pairs([
        ("response_type", "code"),
        ("client_id", &client.id),
        ("redirect_uri", &callback_url(s)),
        ("code_challenge", &digest(verifier)),
        ("code_challenge_method", "S256"),
        ("state", nonce),
        ("resource", discovery.resource()),
    ]);
    let scope = if server.scopes.is_empty() {
        discovery.resource_metadata["scopes_supported"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        server.scopes.clone()
    };
    if !scope.is_empty() {
        url.query_pairs_mut().append_pair("scope", &scope);
    }
    Ok(url)
}

#[derive(Serialize)]
struct NativeCallback {
    pending: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<String>,
}

impl Mcps {
    pub async fn connect(&self, s: &Service, id: &str, session: &str) -> Result<Value> {
        self.connect_mode(s, id, session, false).await
    }

    pub async fn connect_native(&self, s: &Service, id: &str, session: &str) -> Result<Value> {
        self.connect_mode(s, id, session, true).await
    }

    async fn connect_mode(
        &self,
        s: &Service,
        id: &str,
        session: &str,
        native: bool,
    ) -> Result<Value> {
        let _guard = self.lock(id).await;
        let mut server = self.server(s, id).await?;
        if !server.uses_oauth() || !server.is_http() {
            return Err(Error::bad("This connection does not use OAuth."));
        }
        let connection_id = id.to_owned();
        s.store
            .write(move |db| cancel_pending(db, &connection_id))
            .await?;
        self.update_secrets(s, id, McpSecrets::clear_authorization)
            .await?;
        server.reset_state(ConnectionState::NeedsAuth);
        self.put_server(s, &server).await?;
        let result = self.authorize(s, &server, session, native).await;
        if let Err(error) = &result {
            self.failure(s, server, error).await?;
        }
        result
    }

    /// Starts an authorization and returns the URL the user signs in at.
    async fn authorize(
        &self,
        s: &Service,
        server: &McpServer,
        session: &str,
        native: bool,
    ) -> Result<Value> {
        let discovery = discover(s, server).await?;
        let client = client(s, server, &discovery).await?;
        let verifier = token();
        let nonce = token();
        let url = authorization_url(s, server, &discovery, &client, &verifier, &nonce)?;
        self.update_secrets(s, &server.id, |secrets| secrets.verifier = Some(verifier))
            .await?;
        let expires = now() + PENDING_TTL_MS;
        let pending = PendingAuthorization {
            connection_id: server.id.clone(),
            revision: Some(server.revision),
            session: hex_digest(session),
            nonce: nonce.clone(),
            native,
            expires_at: Some(expires),
            callback: None,
        };
        s.store
            .set(
                &pending_key(&nonce),
                serde_json::to_value(pending)?,
                Some(expires),
            )
            .await?;
        Ok(json!({ "url": url.as_str() }))
    }

    /// The browser may return a code, but only the initiating authenticated native
    /// session can exchange it. No session cookie or token is transferred to the browser.
    pub async fn capture_native_callback(
        &self,
        s: &Service,
        parameters: &HashMap<String, String>,
    ) -> Result<bool> {
        let Some(state) = parameters.get("state").filter(|v| v.len() <= 1000) else {
            return Ok(false);
        };
        let key = pending_key(state);
        let values: HashMap<String, String> = parameters
            .iter()
            .filter(|(key, _)| ["state", "code", "iss", "error"].contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        if values.values().any(|v| v.len() > 10000) {
            return Err(Error::bad("Invalid OAuth callback."));
        }
        s.store
            .transaction(move |db| {
                let Some(pending) = db.kv(&key)? else {
                    return Ok(false);
                };
                let mut pending = PendingAuthorization::deserialize(&pending)?;
                if !pending.native {
                    return Ok(false);
                }
                // First response wins; a replay cannot replace the captured code or extend its TTL.
                if pending.callback.is_none() {
                    let expires = pending
                        .expires_at
                        .ok_or_else(|| Error::bad("Authorization session expired."))?;
                    pending.callback = Some(values);
                    db.set(&key, &serde_json::to_value(&pending)?, Some(expires))?;
                }
                Ok(true)
            })
            .await
    }

    pub async fn finish_native_callback(
        &self,
        s: &Service,
        id: &str,
        session: &str,
    ) -> Result<Value> {
        let session_digest = hex_digest(session);
        let pending = s
            .store
            .keys("mcp-oauth:")
            .await?
            .into_iter()
            .filter_map(|(_, value)| PendingAuthorization::deserialize(&value).ok())
            .find(|pending| {
                pending.native && pending.connection_id == id && pending.session == session_digest
            });
        let status = match pending {
            None => NativeCallback {
                pending: false,
                result: Some("expired".into()),
            },
            Some(PendingAuthorization { callback: None, .. }) => NativeCallback {
                pending: true,
                result: None,
            },
            Some(PendingAuthorization {
                callback: Some(parameters),
                ..
            }) => NativeCallback {
                pending: false,
                result: Some(self.callback(s, &parameters, session).await?),
            },
        };
        Ok(serde_json::to_value(status)?)
    }

    pub async fn callback(
        &self,
        s: &Service,
        parameters: &HashMap<String, String>,
        session: &str,
    ) -> Result<String> {
        let state = parameters.get("state").map_or("", String::as_str);
        let key = pending_key(state);
        let pending = required(s.store.kv(&key).await?, EXPIRED)?;
        let pending = PendingAuthorization::deserialize(&pending)?;
        if pending.session != hex_digest(session) {
            return Err(Error::bad(EXPIRED));
        }
        let id = pending.connection_id.as_str();
        let _guard = self.lock(id).await;
        s.store
            .transaction(move |db| {
                if db.kv(&key)?.is_none() {
                    return Err(Error::bad("Authorization has already been completed."));
                }
                db.delete(&key)
            })
            .await?;
        let server = self.server(s, id).await?;
        if pending.revision != Some(server.revision) {
            return Err(Error::bad(
                "Connection settings changed. Start authorization again.",
            ));
        }
        if parameters.contains_key("error") {
            return Ok("denied".into());
        }
        let code = parameters
            .get("code")
            .filter(|code| !code.is_empty() && code.len() <= 10000)
            .ok_or_else(|| Error::bad("Missing authorization code."))?;
        let tools = self.complete(s, &server, parameters, code).await;
        let outcome = if tools.is_ok() { "connected" } else { "failed" };
        self.record_discovery(s, server, &tools).await?;
        Ok(outcome.into())
    }

    /// Exchanges the authorization code, then reads the now-authorized tool catalog.
    async fn complete(
        &self,
        s: &Service,
        server: &McpServer,
        parameters: &HashMap<String, String>,
        code: &str,
    ) -> Result<Vec<Value>> {
        let secrets = self.server_secrets(s, &server.id).await?;
        let discovery = match &secrets.discovery {
            Some(discovery) if !secrets.verifier().is_empty() => discovery,
            _ => return Err(Error::bad("Authorization session expired.")),
        };
        let expected = discovery.server("issuer");
        let issuer_required =
            discovery.server_flag("authorization_response_iss_parameter_supported");
        let issuer_mismatch = parameters
            .get("iss")
            .is_some_and(|issuer| issuer != expected)
            || (issuer_required && !parameters.contains_key("iss"));
        if issuer_mismatch {
            return Err(Error::bad(
                "OAuth callback issuer does not match the authorization server.",
            ));
        }
        // Re-discover the issuer metadata before exchanging the code, binding it to
        // the issuer recorded at redirect time instead of trusting callback input.
        let metadata = authorization_metadata(server, expected).await?;
        if metadata["issuer"] != discovery.authorization_server_metadata["issuer"] {
            return Err(Error::bad(
                "OAuth authorization server changed during sign-in.",
            ));
        }
        let parameters = HashMap::from([
            ("grant_type".into(), "authorization_code".into()),
            ("code".into(), code.to_owned()),
            ("redirect_uri".into(), callback_url(s)),
            ("code_verifier".into(), secrets.verifier().to_owned()),
        ]);
        exchange(s, server, parameters, discovery).await?;
        self.update_secrets(s, &server.id, |secrets| secrets.verifier = None)
            .await?;
        Client::list_tools(s, server).await
    }
}
