use crate::mcp_client::Authorization;
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
use serde::Serialize;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::LazyLock};

/// Pending authorizations expire after ten minutes.
const PENDING_TTL_MS: i64 = 600_000;
const EXPIRED: &str = "Authorization session expired. Start again from MCPs.";

static RESOURCE_METADATA: LazyLock<regex::bytes::Regex> = LazyLock::new(|| {
    // An absolute metadata URL requires quotes in an auth parameter. Read the
    // raw field and consume other quoted strings as a whole.
    regex::bytes::Regex::new(
        r#"(?-u)"(?:[^"\\]|\\.)*"|(?:^|[ \t,])(?i:resource_metadata)[ \t]*=[ \t]*"((?:[^"\\]|\\.)*)""#,
    )
    .unwrap()
});

fn pending_key(state: &str) -> String {
    hex_digest(state)
}

fn content_type(value: &'static str) -> HeaderMap {
    HeaderMap::from_iter([(
        "content-type".parse().unwrap(),
        HeaderValue::from_static(value),
    )])
}

async fn get(
    _s: &Service,
    server: &McpServer,
    url: &str,
    authorize: Authorization,
) -> Result<Value> {
    authorize.recheck().await?;
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

fn advertised_metadata(headers: &HeaderMap) -> Result<Option<String>> {
    let quoted = headers
        .get_all("www-authenticate")
        .iter()
        .flat_map(|value| RESOURCE_METADATA.captures_iter(value.as_bytes()))
        .find_map(|capture| capture.get(1));
    let Some(quoted) = quoted else {
        return Ok(None);
    };

    let mut url = Vec::with_capacity(quoted.len());
    let mut escaped = false;
    for &byte in quoted.as_bytes() {
        if byte == b'\\' && !escaped {
            escaped = true;
            continue;
        }
        url.push(byte);
        escaped = false;
    }
    String::from_utf8(url)
        .map(Some)
        .map_err(|_| Error::bad("Unsupported authorization URL."))
}

/// Probes without credentials for a resource metadata challenge.
async fn probe(
    server: &McpServer,
    endpoint: &url::Url,
    authorize: Authorization,
) -> Result<Option<String>> {
    authorize.recheck().await?;
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
    // Discovery can be public even when individual tools require OAuth.
    if !(200..300).contains(&response.status) && response.status != 401 {
        return Err(Error::bad(
            "The server did not request OAuth. Use Test connection or choose no authentication.",
        ));
    }
    advertised_metadata(&response.headers)
}

/// Protected resource metadata and the URL it was read from. Without an explicit
/// challenge URL, the path-specific well-known location falls back to the root one.
async fn protected_resource(
    s: &Service,
    server: &McpServer,
    endpoint: &url::Url,
    advertised: Option<String>,
    authorize: Authorization,
) -> Result<(String, Value)> {
    let origin = &endpoint[..url::Position::BeforePath];
    let path_and_query = &endpoint[url::Position::BeforePath..];
    let root = format!("{origin}/.well-known/oauth-protected-resource");
    let has_advertised_url = advertised.is_some();
    let resource_suffix = if has_advertised_url {
        // An advertised document must identify the actual probe URL.
        path_and_query
    } else if endpoint.path() == "/" {
        // Use the authority-only form for a derived root well-known URL.
        &path_and_query[1..]
    } else {
        path_and_query
    };
    let metadata_url = advertised.unwrap_or_else(|| format!("{root}{resource_suffix}"));
    let (metadata_url, resource, expected_path_and_query) =
        match get(s, server, &metadata_url, authorize.clone()).await {
            Ok(resource) => (metadata_url, resource, resource_suffix),
            Err(error) if has_advertised_url => return Err(error),
            Err(_) => {
                let resource = get(s, server, &root, authorize.clone())
                    .await
                    .map_err(|_| {
                        Error::bad_gateway("OAuth protected resource metadata is unavailable.")
                    })?;
                (root, resource, "")
            }
        };

    let resource_id = text(&resource, "resource");
    valid_url(server, resource_id)?;
    // Bind metadata to the resource used to locate this document (RFC 9728 §3.3).
    if resource_id.strip_prefix(origin) != Some(expected_path_and_query) {
        return Err(Error::bad(
            "OAuth resource does not match the configured MCP server.",
        ));
    }
    Ok((metadata_url, resource))
}

async fn discover(
    s: &Service,
    server: &McpServer,
    authorize: Authorization,
) -> Result<OAuthDiscovery> {
    let endpoint = valid_url(server, &server.url)?;
    let advertised = probe(server, &endpoint, authorize.clone()).await?;
    let (resource_metadata_url, resource) =
        protected_resource(s, server, &endpoint, advertised, authorize.clone()).await?;
    let authorization_server = resource["authorization_servers"]
        .as_array()
        .and_then(|servers| servers.first())
        .and_then(Value::as_str)
        .ok_or_else(|| Error::bad("OAuth authorization server is missing."))?
        .to_owned();
    let metadata =
        authorization_metadata(s, server, &authorization_server, authorize.clone()).await?;
    let discovery = OAuthDiscovery {
        authorization_server_url: authorization_server,
        resource_metadata_url,
        resource_metadata: resource,
        authorization_server_metadata: metadata,
    };
    let stored = discovery.clone();
    authorize.recheck().await?;
    s.mcps
        .update_secrets_live(
            s,
            &server.id,
            |secrets| secrets.discovery = Some(stored),
            authorize,
        )
        .await?;
    Ok(discovery)
}

async fn authorization_metadata(
    s: &Service,
    server: &McpServer,
    issuer: &str,
    authorize: Authorization,
) -> Result<Value> {
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
        if let Ok(value) = get(s, server, &url, authorize.clone()).await {
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

async fn register(
    s: &Service,
    server: &McpServer,
    endpoint: &str,
    authorize: Authorization,
) -> Result<Value> {
    authorize.recheck().await?;
    valid_url(server, endpoint)?;
    let registration = ClientRegistration {
        client_name: "Cairn",
        redirect_uris: [callback_url(s).await?],
        grant_types: ["authorization_code", "refresh_token"],
        response_types: ["code"],
        token_endpoint_auth_method: "none",
        scope: Some(server.scopes.as_str()).filter(|scopes| !scopes.is_empty()),
    };
    authorize.recheck().await?;
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
    authorize: Authorization,
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
    let client = register(s, server, endpoint, authorize.clone()).await?;
    let registered = OAuthClient::from_registration(&client);
    authorize.recheck().await?;
    s.mcps
        .update_secrets_live(
            s,
            &server.id,
            |secrets| secrets.client = Some(client),
            authorize,
        )
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
    authorize: Authorization,
) -> Result<()> {
    let client = client(s, server, discovery, authorize.clone()).await?;
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
    authorize.recheck().await?;
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
    authorize.recheck().await?;
    s.mcps
        .update_secrets_live(
            s,
            &server.id,
            |secrets| {
                secrets.tokens = Some(tokens);
                secrets.token_expires_at = expires;
            },
            authorize,
        )
        .await
}

pub(crate) async fn refresh_authorized(
    s: &Service,
    server: &McpServer,
    authorize: Authorization,
) -> Result<()> {
    let secrets = s.mcps.server_secrets(s, &server.id).await?;
    let refresh = secrets.refresh_token();
    if refresh.is_empty() {
        return Err(Error::unauthorized("Sign in to connect this server."));
    }
    let discovery = match secrets.discovery.clone() {
        Some(discovery) => discovery,
        None => discover(s, server, authorize.clone()).await?,
    };
    let parameters = HashMap::from([
        ("grant_type".into(), "refresh_token".into()),
        ("refresh_token".into(), refresh.into()),
    ]);
    exchange(s, server, parameters, &discovery, authorize).await
}

/// The authorization endpoint URL with PKCE, state, resource and scope parameters.
async fn authorization_url(
    s: &Service,
    server: &McpServer,
    discovery: &OAuthDiscovery,
    client: &OAuthClient,
    verifier: &str,
    nonce: &str,
) -> Result<url::Url> {
    let redirect_uri = callback_url(s).await?;
    let mut url = valid_url(server, discovery.server("authorization_endpoint"))?;
    url.query_pairs_mut().extend_pairs([
        ("response_type", "code"),
        ("client_id", &client.id),
        ("redirect_uri", &redirect_uri),
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

use cairn_contracts::McpConnectionStatus as NativeCallback;

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
        let discovery = discover(s, server, Authorization::new(s, |_| Ok(()))).await?;
        let client = client(s, server, &discovery, Authorization::new(s, |_| Ok(()))).await?;
        let verifier = token();
        let nonce = token();
        let url = authorization_url(s, server, &discovery, &client, &verifier, &nonce).await?;
        self.update_secrets(s, &server.id, |secrets| secrets.verifier = Some(verifier))
            .await?;
        let expires = now() + PENDING_TTL_MS;
        let pending = PendingAuthorization {
            connection_id: server.id.clone(),
            revision: server.revision,
            session: hex_digest(session),
            nonce: nonce.clone(),
            native,
            expires_at: expires,
            callback: None,
        };
        let vault = s.vault.clone();
        s.store
            .transaction(move |db| {
                db.save_pending_mcp_oauth(&pending_key(&nonce), &pending, &vault)
            })
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
        let vault = s.vault.clone();
        s.store
            .transaction(move |db| {
                let Some(mut pending) = db.pending_mcp_oauth(&key, &vault)? else {
                    return Ok(false);
                };
                if !pending.native {
                    return Ok(false);
                }
                // First response wins; a replay cannot replace the captured code or extend its TTL.
                if pending.callback.is_none() {
                    pending.callback = Some(values);
                    db.save_pending_mcp_oauth(&key, &pending, &vault)?;
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
        let id = id.to_owned();
        let vault = s.vault.clone();
        let pending = s
            .store
            .read(move |db| db.pending_mcp_session(&id, &session_digest, &vault))
            .await?;
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
        let vault = s.vault.clone();
        let pending = required(
            s.store
                .read({
                    let key = key.clone();
                    let vault = vault.clone();
                    move |db| db.pending_mcp_oauth(&key, &vault)
                })
                .await?,
            EXPIRED,
        )?;
        if pending.session != hex_digest(session) {
            return Err(Error::bad(EXPIRED));
        }
        let id = pending.connection_id.as_str();
        let _guard = self.lock(id).await;
        s.store
            .transaction(move |db| {
                if db.pending_mcp_oauth(&key, &vault)?.is_none() {
                    return Err(Error::bad("Authorization has already been completed."));
                }
                db.delete_pending_mcp_oauth(&key)
            })
            .await?;
        let server = self.server(s, id).await?;
        if pending.revision != server.revision {
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
        let metadata =
            authorization_metadata(s, server, expected, Authorization::new(s, |_| Ok(()))).await?;
        if metadata["issuer"] != discovery.authorization_server_metadata["issuer"] {
            return Err(Error::bad(
                "OAuth authorization server changed during sign-in.",
            ));
        }
        let parameters = HashMap::from([
            ("grant_type".into(), "authorization_code".into()),
            ("code".into(), code.to_owned()),
            ("redirect_uri".into(), callback_url(s).await?),
            ("code_verifier".into(), secrets.verifier().to_owned()),
        ]);
        exchange(
            s,
            server,
            parameters,
            discovery,
            Authorization::new(s, |_| Ok(())),
        )
        .await?;
        self.update_secrets(s, &server.id, |secrets| secrets.verifier = None)
            .await?;
        Client::list_tools(s, server).await
    }
}
