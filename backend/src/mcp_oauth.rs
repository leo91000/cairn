use crate::{
    auth::{digest, hex_digest, token},
    config::now,
    error::{Error, Result, required},
    mcps::Mcps,
    network,
    service::Service,
    store::merge,
    validation::text,
};
use reqwest::{
    Method,
    header::{HeaderMap, HeaderValue},
};
use serde_json::{Value, json};
use std::collections::HashMap;

async fn get(item: &Value, url: &str) -> Result<Value> {
    let response = network::fetch(
        url,
        Method::GET,
        HeaderMap::new(),
        None,
        item["allowPrivateNetwork"] == true,
    )
    .await?;
    if !(200..300).contains(&response.status) {
        return Err(Error::bad_gateway("OAuth metadata is unavailable."));
    }
    response.json()
}

fn valid_url(item: &Value, value: &str) -> Result<url::Url> {
    let url = url::Url::parse(value).map_err(|_| Error::bad("Unsupported authorization URL."))?;
    if !["http", "https"].contains(&url.scheme())
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || (item["allowPrivateNetwork"] != true && url.scheme() != "https")
    {
        return Err(Error::bad("Unsupported authorization URL."));
    }
    Ok(url)
}

async fn discover(s: &Service, item: &Value) -> Result<Value> {
    let endpoint = valid_url(item, text(item, "url"))?;
    let probe = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "server/discover",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": {
                    "name": "leo-mcp-client",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        }
    });
    let headers = HeaderMap::from_iter([
        (
            "content-type".parse().unwrap(),
            HeaderValue::from_static("application/json"),
        ),
        (
            "accept".parse().unwrap(),
            HeaderValue::from_static("application/json, text/event-stream"),
        ),
        (
            "mcp-protocol-version".parse().unwrap(),
            HeaderValue::from_static("2026-07-28"),
        ),
        (
            "mcp-method".parse().unwrap(),
            HeaderValue::from_static("server/discover"),
        ),
    ]);
    let response = network::fetch(
        endpoint.as_str(),
        Method::POST,
        headers,
        Some(serde_json::to_vec(&probe)?),
        item["allowPrivateNetwork"] == true,
    )
    .await?;
    if response.status != 401 {
        return Err(Error::bad(
            "The server did not request OAuth. Use Test connection or choose no authentication.",
        ));
    }
    let challenge = response
        .headers
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let mut metadata_url = regex::Regex::new(r#"resource_metadata="([^"]+)""#)
        .unwrap()
        .captures(challenge)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_owned())
        .unwrap_or_else(|| {
            format!(
                "{}/.well-known/oauth-protected-resource{}",
                endpoint.origin().ascii_serialization(),
                endpoint.path().trim_end_matches('/')
            )
        });
    let resource = match get(item, &metadata_url).await {
        Ok(resource) => resource,
        Err(error) if challenge.contains("resource_metadata=") => return Err(error),
        Err(_) => {
            metadata_url = format!(
                "{}/.well-known/oauth-protected-resource",
                endpoint.origin().ascii_serialization()
            );
            get(item, &metadata_url).await.map_err(|_| {
                Error::bad_gateway("OAuth protected resource metadata is unavailable.")
            })?
        }
    };
    let resource_url = valid_url(item, text(&resource, "resource"))?;
    if resource_url.origin() != endpoint.origin()
        || !endpoint.path().starts_with(resource_url.path())
        || (endpoint.path() != resource_url.path()
            && !resource_url.path().ends_with('/')
            && !endpoint.path()[resource_url.path().len()..].starts_with('/'))
    {
        return Err(Error::bad(
            "OAuth resource does not match the configured MCP server.",
        ));
    }
    let server = resource["authorization_servers"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .ok_or_else(|| Error::bad("OAuth authorization server is missing."))?;
    let metadata = authorization_metadata(item, server).await?;
    let result = json!({
        "authorizationServerUrl": server,
        "resourceMetadataUrl": metadata_url,
        "resourceMetadata": resource,
        "authorizationServerMetadata": metadata
    });
    s.mcps
        .change_secrets(
            s,
            text(item, "id"),
            json!({
                "discovery": result
            }),
        )
        .await?;
    Ok(result)
}

async fn authorization_metadata(item: &Value, server: &str) -> Result<Value> {
    let issuer = valid_url(item, server)?;
    let suffix = issuer.path().trim_end_matches('/');
    let origin = issuer.origin().ascii_serialization();
    let urls = [
        format!("{origin}/.well-known/oauth-authorization-server{suffix}"),
        format!("{origin}/.well-known/openid-configuration{suffix}"),
        format!("{}{}/.well-known/openid-configuration", origin, suffix),
    ];
    let mut metadata = None;
    for url in urls {
        if let Ok(value) = get(item, &url).await {
            metadata = Some(value);
            break;
        }
    }
    let metadata = required(metadata, "OAuth authorization metadata is unavailable.")?;
    if text(&metadata, "issuer") != server {
        return Err(Error::bad(
            "OAuth authorization server identity does not match discovery.",
        ));
    }
    for key in ["authorization_endpoint", "token_endpoint"] {
        valid_url(item, text(&metadata, key))?;
    }
    if metadata["code_challenge_methods_supported"]
        .as_array()
        .is_some_and(|methods| !methods.iter().any(|m| m == "S256"))
    {
        return Err(Error::bad("This OAuth server does not support S256 PKCE."));
    }
    Ok(metadata)
}

async fn client(s: &Service, item: &Value, discovery: &Value) -> Result<Value> {
    let secret = s.mcps.secrets(s, text(item, "id")).await?;
    if !text(item, "clientId").is_empty() {
        let mut client = json!({
            "client_id": item["clientId"]
        });
        if !text(&secret, "clientSecret").is_empty() {
            client["client_secret"] = secret["clientSecret"].clone();
        }
        return Ok(client);
    }
    if !text(&secret["client"], "client_id").is_empty() {
        return Ok(secret["client"].clone());
    }
    let registration = text(
        &discovery["authorizationServerMetadata"],
        "registration_endpoint",
    );
    if registration.is_empty() {
        return Err(Error::bad(
            "Enter an OAuth client ID registered with this provider.",
        ));
    }
    valid_url(item, registration)?;
    let mut metadata = json!({
        "client_name": "Leo Agent Manager",
        "redirect_uris": [format!("{}/oauth/mcp/callback", s.config.public_url)],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none"
    });
    if !text(item, "scopes").is_empty() {
        metadata["scope"] = item["scopes"].clone();
    }
    let headers = HeaderMap::from_iter([(
        "content-type".parse().unwrap(),
        HeaderValue::from_static("application/json"),
    )]);
    let response = network::fetch(
        registration,
        Method::POST,
        headers,
        Some(serde_json::to_vec(&metadata)?),
        item["allowPrivateNetwork"] == true,
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
    s.mcps
        .change_secrets(
            s,
            text(item, "id"),
            json!({
                "client": client
            }),
        )
        .await?;
    Ok(client)
}

async fn exchange(
    s: &Service,
    item: &Value,
    mut parameters: HashMap<String, String>,
    discovery: &Value,
) -> Result<()> {
    let client = client(s, item, discovery).await?;
    parameters.insert("client_id".into(), text(&client, "client_id").into());
    let mut authorization = None;
    if !text(&client, "client_secret").is_empty() {
        let supported =
            &discovery["authorizationServerMetadata"]["token_endpoint_auth_methods_supported"];
        let method = text(&client, "token_endpoint_auth_method");
        let basic = method == "client_secret_basic"
            || (method.is_empty()
                && supported.as_array().is_some_and(|methods| {
                    methods.iter().any(|m| m == "client_secret_basic")
                        && !methods.iter().any(|m| m == "client_secret_post")
                }));
        if basic {
            use base64::Engine;
            let encode = |value: &str| {
                url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>()
            };
            authorization = Some(format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!(
                    "{}:{}",
                    encode(text(&client, "client_id")),
                    encode(text(&client, "client_secret"))
                ))
            ));
            parameters.remove("client_id");
        } else if method.is_empty() || method == "client_secret_post" {
            parameters.insert(
                "client_secret".into(),
                text(&client, "client_secret").into(),
            );
        } else {
            return Err(Error::bad(
                "Unsupported OAuth client authentication method.",
            ));
        }
    }
    parameters.insert(
        "resource".into(),
        text(&discovery["resourceMetadata"], "resource").to_owned(),
    );
    let endpoint = text(&discovery["authorizationServerMetadata"], "token_endpoint");
    valid_url(item, endpoint)?;
    let mut headers = HeaderMap::from_iter([(
        "content-type".parse().unwrap(),
        HeaderValue::from_static("application/x-www-form-urlencoded"),
    )]);
    if let Some(value) = authorization {
        headers.insert(
            "authorization",
            HeaderValue::from_str(&value)
                .map_err(|_| Error::bad("Invalid OAuth client credentials."))?,
        );
    }
    let response = network::fetch(
        endpoint,
        Method::POST,
        headers,
        Some(
            serde_urlencoded::to_string(&parameters)
                .map_err(Error::internal)?
                .into_bytes(),
        ),
        item["allowPrivateNetwork"] == true,
    )
    .await?;
    if !(200..300).contains(&response.status) {
        return Err(Error::unauthorized("Sign in to connect this server."));
    }
    let mut tokens = response.json()?;
    if text(&tokens, "access_token").is_empty()
        || !text(&tokens, "token_type").eq_ignore_ascii_case("bearer")
    {
        return Err(Error::unauthorized("OAuth returned invalid credentials."));
    }
    if tokens.get("expires_in").is_some()
        && !tokens["expires_in"]
            .as_f64()
            .is_some_and(|n| n.is_finite() && (0. ..=315360000.).contains(&n))
    {
        return Err(Error::unauthorized(
            "OAuth returned an invalid token lifetime.",
        ));
    }
    if parameters
        .get("grant_type")
        .is_some_and(|g| g == "refresh_token")
        && tokens.get("refresh_token").is_none()
    {
        tokens["refresh_token"] = parameters
            .get("refresh_token")
            .cloned()
            .unwrap_or_default()
            .into();
    }
    let expires = tokens["expires_in"]
        .as_f64()
        .map(|n| now() + (n * 1000.) as i64);
    s.mcps
        .change_secrets(
            s,
            text(item, "id"),
            json!({
                "tokens": tokens,
                "tokenExpiresAt": expires
            }),
        )
        .await
}

pub async fn refresh(s: &Service, item: &Value) -> Result<()> {
    let secrets = s.mcps.secrets(s, text(item, "id")).await?;
    let refresh = text(&secrets["tokens"], "refresh_token");
    if refresh.is_empty() {
        return Err(Error::unauthorized("Sign in to connect this server."));
    }
    let discovery = if secrets["discovery"].is_object() {
        secrets["discovery"].clone()
    } else {
        discover(s, item).await?
    };
    exchange(
        s,
        item,
        HashMap::from([
            ("grant_type".into(), "refresh_token".into()),
            ("refresh_token".into(), refresh.into()),
        ]),
        &discovery,
    )
    .await
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
        let mut item = self.get(s, id).await?;
        if item["auth"] != "oauth" || item["transport"] != "http" {
            return Err(Error::bad("This connection does not use OAuth."));
        }
        let connection_id = id.to_owned();
        s.store
            .write(move |db| {
                for (key, pending) in db.keys("mcp-oauth:")? {
                    if pending["connectionId"] == connection_id {
                        db.delete(&key)?;
                    }
                }
                Ok(())
            })
            .await?;
        self.change_secrets(
            s,
            id,
            json!({
                "tokens": null,
                "verifier": null,
                "discovery": null,
                "tokenExpiresAt": null
            }),
        )
        .await?;
        merge(
            &mut item,
            &json!({
                "state": "needs-auth",
                "error": "",
                "checkedAt": null
            }),
        );
        s.store.put("mcps", item.clone()).await?;
        let result = async {
            let discovery = discover(s, &item).await?;
            let client = client(s, &item, &discovery).await?;
            let verifier = token();
            let nonce = token();
            let mut authorization = valid_url(
                &item,
                text(
                    &discovery["authorizationServerMetadata"],
                    "authorization_endpoint",
                ),
            )?;
            authorization.query_pairs_mut().extend_pairs([
                ("response_type", "code"),
                ("client_id", text(&client, "client_id")),
                (
                    "redirect_uri",
                    &format!("{}/oauth/mcp/callback", s.config.public_url),
                ),
                ("code_challenge", &digest(&verifier)),
                ("code_challenge_method", "S256"),
                ("state", &nonce),
                ("resource", text(&discovery["resourceMetadata"], "resource")),
            ]);
            let scope = if text(&item, "scopes").is_empty() {
                discovery["resourceMetadata"]["scopes_supported"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                text(&item, "scopes").into()
            };
            if !scope.is_empty() {
                authorization.query_pairs_mut().append_pair("scope", &scope);
            }
            self.change_secrets(
                s,
                id,
                json!({
                    "verifier": verifier
                }),
            )
            .await?;
            let expires = now() + 600000;
            s.store
                .set(
                    &format!("mcp-oauth:{}", hex_digest(&nonce)),
                    json!({
                        "connectionId": id,
                        "revision": item["revision"],
                        "session": hex_digest(session),
                        "nonce": nonce,
                        "native": native,
                        "expiresAt": expires
                    }),
                    Some(expires),
                )
                .await?;
            Ok(json!({
                "url": authorization.as_str()
            }))
        }
        .await;
        if let Err(error) = &result {
            self.failure(s, item, error).await?;
        }
        result
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
        let key = format!("mcp-oauth:{}", hex_digest(state));
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
                let Some(mut pending) = db.kv(&key)? else {
                    return Ok(false);
                };
                if pending["native"] != true {
                    return Ok(false);
                }
                // First response wins; a replay cannot replace the captured code or extend its TTL.
                if pending["callback"].is_null() {
                    let expires = pending["expiresAt"]
                        .as_i64()
                        .ok_or_else(|| Error::bad("Authorization session expired."))?;
                    pending["callback"] = serde_json::to_value(values)?;
                    db.set(&key, &pending, Some(expires))?;
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
        let pending = s
            .store
            .keys("mcp-oauth:")
            .await?
            .into_iter()
            .map(|(_, v)| v)
            .find(|v| {
                v["native"] == true
                    && v["connectionId"] == id
                    && v["session"] == hex_digest(session)
            });
        let Some(pending) = pending else {
            return Ok(json!({
                "pending": false,
                "result": "expired"
            }));
        };
        if pending["callback"].is_null() {
            return Ok(json!({
                "pending": true
            }));
        }
        let parameters = serde_json::from_value(pending["callback"].clone())?;
        let result = self.callback(s, &parameters, session).await?;
        Ok(json!({
            "pending": false,
            "result": result
        }))
    }

    pub async fn callback(
        &self,
        s: &Service,
        parameters: &HashMap<String, String>,
        session: &str,
    ) -> Result<String> {
        let state = parameters.get("state").map(String::as_str).unwrap_or("");
        let key = format!("mcp-oauth:{}", hex_digest(state));
        let pending = required(
            s.store.kv(&key).await?,
            "Authorization session expired. Start again from MCPs.",
        )?;
        if pending["session"] != hex_digest(session) {
            return Err(Error::bad(
                "Authorization session expired. Start again from MCPs.",
            ));
        }
        let id = text(&pending, "connectionId");
        let _guard = self.lock(id).await;
        let key_copy = key.clone();
        s.store
            .transaction(move |db| {
                if db.kv(&key_copy)?.is_none() {
                    return Err(Error::bad("Authorization has already been completed."));
                }
                db.delete(&key_copy)
            })
            .await?;
        let mut item = self.get(s, id).await?;
        if item["revision"] != pending["revision"] {
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
        let result = async {
            let secrets = self.secrets(s, id).await?;
            let discovery = &secrets["discovery"];
            if !discovery.is_object() || text(&secrets, "verifier").is_empty() {
                return Err(Error::bad("Authorization session expired."));
            }
            let expected = text(&discovery["authorizationServerMetadata"], "issuer");
            if parameters
                .get("iss")
                .is_some_and(|issuer| issuer != expected)
                || (discovery["authorizationServerMetadata"]["authorization_response_iss_parameter_supported"]
                    == true
                    && !parameters.contains_key("iss"))
            {
                return Err(Error::bad(
                    "OAuth callback issuer does not match the authorization server.",
                ));
            }
            // Re-discover the issuer metadata before exchanging the code, binding it to
            // the issuer recorded at redirect time instead of trusting callback input.
            let metadata = authorization_metadata(&item, expected).await?;
            if metadata["issuer"] != discovery["authorizationServerMetadata"]["issuer"] {
                return Err(Error::bad(
                    "OAuth authorization server changed during sign-in.",
                ));
            }
            exchange(
                s,
                &item,
                HashMap::from([
                    ("grant_type".into(), "authorization_code".into()),
                    ("code".into(), code.clone()),
                    (
                        "redirect_uri".into(),
                        format!("{}/oauth/mcp/callback", s.config.public_url),
                    ),
                    ("code_verifier".into(), text(&secrets, "verifier").into()),
                ]),
                discovery,
            )
            .await?;
            self.change_secrets(
                s,
                id,
                json!({
                    "verifier": null
                }
                ),
            )
            .await?;
            let mut client = crate::mcp_client::Client::connect(s, &item).await?;
            let tools = client.discover().await;
            client.close().await;
            tools
        }
        .await;
        match result {
            Ok(tools) => {
                merge(
                    &mut item,
                    &json!({
                        "state": "connected",
                        "tools": tools,
                        "error": "",
                        "checkedAt": now()
                    }),
                );
                s.store.put("mcps", item).await?;
                Ok("connected".into())
            }
            Err(error) => {
                self.failure(s, item, &error).await?;
                Ok("failed".into())
            }
        }
    }
}
