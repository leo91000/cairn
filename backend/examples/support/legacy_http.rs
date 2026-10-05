//! Historical OAuth and token journeys, confined to the browser fixture.
use super::*;
use axum::http::StatusCode;

pub async fn handle(service: &Arc<Service>, request: &mut Request) -> Result<Option<Response>> {
    let path = request.uri().path().to_owned();
    if path == "/mcp" {
        let bearer = request
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        let grant = auth(service).verify(bearer, None).await?;
        let mut identity = InstallationIdentity::trusted(InstallationRole::Owner, "fixture-owner");
        identity.mcp_scopes = Some(serde_json::from_value(grant["scopes"].clone())?);
        request.extensions_mut().insert(identity);
        *request.uri_mut() = "/api/mcp".parse().unwrap();
        return Ok(None);
    }
    if path.starts_with("/oauth/")
        || path.starts_with("/.well-known/")
        || path.starts_with("/api/tokens")
        || path.starts_with("/api/oauth/")
    {
        let owned = std::mem::replace(request, Request::new(axum::body::Body::empty()));
        if path.starts_with("/oauth/") {
            return Ok(Some(oauth(service, owned).await?));
        }
        if path.starts_with("/.well-known/") {
            return Ok(Some(metadata(service, owned).await?.into_response()));
        }
        let input = Input::read(owned).await?;
        let session = auth(service)
            .read(&cookie(&input.headers))
            .await?
            .ok_or_else(|| Error::unauthorized("Please sign in."))?;
        if input.method != "GET"
            && !safe_equal(
                text(&session, "csrf"),
                input
                    .headers
                    .get("x-csrf-token")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or(""),
            )
        {
            return Err(Error::forbidden("Invalid CSRF token."));
        }
        let parts = path
            .trim_start_matches("/api/")
            .split('/')
            .collect::<Vec<_>>();
        let result = match (input.method.as_str(), parts.as_slice()) {
            ("GET", ["tokens"]) => service
                .store
                .keys("grant:")
                .await?
                .into_iter()
                .map(|(_, mut value)| {
                    value["id"] = value["family"].clone();
                    value
                })
                .collect::<Vec<_>>()
                .into(),
            ("POST", ["tokens"]) => {
                let scopes = input.body["scopes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap())
                    .collect();
                auth(service)
                    .personal(input.string("label", 100)?, scopes)
                    .await?
            }
            ("DELETE", ["tokens", id]) => {
                auth(service).revoke(id).await?;
                json!({"revoked": true})
            }
            ("POST", ["oauth", "preview"]) => auth(service).authorization(&input.body).await?,
            ("POST", ["oauth", "consent"]) => {
                json!({ "redirect": auth(service).consent(input.body["parameters"].clone(), input.boolean("approved")?).await? })
            }
            _ => return Err(Error::not_found("Not found")),
        };
        return Ok(Some(Json(result).into_response()));
    }
    if path.starts_with("/api/public/installations/") {
        let token = path.rsplit('/').next().unwrap();
        let mut identity = InstallationIdentity::trusted(InstallationRole::Member, "");
        identity.public_artifact = Some(token.into());
        request.extensions_mut().insert(identity);
        request.headers_mut().remove(header::ORIGIN);
        *request.uri_mut() = format!("/api/shared-artifacts/{token}").parse().unwrap();
    }
    Ok(None)
}

fn native_callback_page() -> Response {
    let headers = [
        (header::CONTENT_TYPE, "text/html; charset=utf-8"),
        (header::CACHE_CONTROL, "no-store"),
        (header::REFERRER_POLICY, "no-referrer"),
        (
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; frame-ancestors 'none'",
        ),
    ];
    let page = "<!doctype html><html lang=fr><meta name=viewport content='width=device-width,initial-scale=1'><title>Leo</title><h1>Revenez dans Leo</h1><p>Fermez cet onglet pour terminer la connexion dans l’application Android.</p></html>";
    (headers, page).into_response()
}

async fn oauth(service: &Arc<Service>, mut request: Request) -> Result<Response> {
    let session = auth(service).read(&cookie(request.headers())).await?;
    request
        .extensions_mut()
        .insert(InstallationIdentity::trusted(
            InstallationRole::Owner,
            session
                .as_ref()
                .map_or("fixture-owner", |value| text(value, "csrf")),
        ));
    let input = Input::read(request).await?;
    let auth = auth(service);
    if input.method == "GET" && input.path == "/oauth/mcp/callback" {
        if service
            .mcps
            .capture_native_callback(service, &input.query)
            .await?
        {
            return Ok(native_callback_page());
        }
        let result = if input.identity.is_some() {
            service
                .mcps
                .callback(
                    service,
                    &input.query,
                    &format!(
                        "leo-account:{}",
                        text(
                            &auth
                                .read(&cookie(&input.headers))
                                .await?
                                .unwrap_or_default(),
                            "csrf"
                        )
                    ),
                )
                .await
                .unwrap_or_else(|_| "expired".into())
        } else {
            "expired".into()
        };
        let mut response =
            axum::response::Redirect::temporary(&format!("/mcps?oauth={result}")).into_response();
        response
            .headers_mut()
            .insert("referrer-policy", HeaderValue::from_static("no-referrer"));
        return Ok(response);
    }
    let result = match (input.method.as_str(), input.path.as_str()) {
        ("POST", "/oauth/register") => {
            return Ok(
                (StatusCode::CREATED, Json(auth.register(input.body).await?)).into_response(),
            );
        }
        ("GET", "/oauth/authorize") => {
            let parameters = serde_json::to_value(&input.query)?;
            auth.authorization(&parameters).await?;
            let location = format!(
                "/authorize?{}",
                serde_urlencoded::to_string(&input.query).map_err(Error::internal)?
            );
            return Ok(axum::response::Redirect::temporary(&location).into_response());
        }
        ("POST", "/oauth/token") => auth.exchange(input.body).await?,
        ("POST", "/oauth/revoke") => {
            auth.revoke_token(
                input.string("token", 10000)?,
                input.string("client_id", 200)?,
            )
            .await?;
            json!({})
        }
        _ => return Err(Error::not_found("Not found")),
    };
    Ok(Json(result).into_response())
}

async fn metadata(service: &Service, request: Request) -> Result<Json<Value>> {
    if request.method() != "GET" {
        return Err(Error::method_not_allowed("Method not allowed."));
    }
    let url = &service.config.public_url;
    let scopes = ["read", "run", "manage"];
    match request.uri().path() {
        "/.well-known/oauth-protected-resource" | "/.well-known/oauth-protected-resource/mcp" => {
            Ok(Json(json!({
                "resource": format!("{url}/mcp"),
                "authorization_servers": [url],
                "scopes_supported": scopes,
                "bearer_methods_supported": ["header"],
                "resource_name": "Leo Agent Manager",
            })))
        }
        "/.well-known/oauth-authorization-server" => Ok(Json(json!({
            "issuer": url,
            "authorization_endpoint": format!("{url}/oauth/authorize"),
            "token_endpoint": format!("{url}/oauth/token"),
            "registration_endpoint": format!("{url}/oauth/register"),
            "revocation_endpoint": format!("{url}/oauth/revoke"),
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["none"],
            "scopes_supported": scopes,
        }))),
        _ => Err(Error::not_found("Not found")),
    }
}
