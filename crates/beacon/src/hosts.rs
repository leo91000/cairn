//! All installation origins belong to Beacon; no user-controlled site may share this domain.
use crate::{ApiError, cookie_token, html::escape_html};
use axum::{
    Router,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use sqlx_core::query_as::query_as;
use sqlx_postgres::PgPool;
use url::Url;

#[derive(Clone)]
pub struct InstallationHosts {
    origin: String,
    root: Option<Url>,
    enabled: bool,
}

#[derive(Clone)]
pub(crate) enum BrowserHost {
    Central,
    Installation { id: String },
}

impl InstallationHosts {
    pub fn new(origin: &str, enabled: bool) -> Result<Self, String> {
        let root = Url::parse(origin).map_err(|_| "Invalid Beacon origin")?;
        let host = root.host_str().ok_or("Invalid Beacon hostname")?;
        if enabled && (root.domain().is_none() || !host.contains('.')) {
            return Err("Installation subdomains require a DNS hostname; use cairn.localhost for local development".into());
        }
        if enabled
            && root.scheme() != "https"
            && !(root.scheme() == "http" && host.ends_with(".localhost"))
        {
            return Err("Installation subdomains require HTTPS (HTTP .localhost is allowed for development)".into());
        }
        if root.path() != "/"
            || root.query().is_some()
            || root.fragment().is_some()
            || !root.username().is_empty()
            || root.password().is_some()
        {
            return Err("Use a Beacon origin without a path or credentials".into());
        }
        Ok(Self {
            origin: root.origin().ascii_serialization(),
            root: Some(root),
            enabled,
        })
    }

    pub(crate) fn disabled(origin: &str) -> Self {
        Self {
            origin: origin.into(),
            root: Url::parse(origin).ok(),
            enabled: false,
        }
    }

    pub(crate) fn origin(&self) -> &str {
        &self.origin
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn shell(&self, html: &str) -> String {
        html.replacen(
            "id=\"app\"",
            &format!(
                "id=\"app\" data-beacon-origin=\"{}\" data-beacon-subdomains=\"{}\"",
                escape_html(&self.origin),
                self.enabled,
            ),
            1,
        )
    }

    /// Apply to the complete server, including the app shell and static files.
    /// The API applies the same guard when used separately by library clients.
    pub fn layer(&self, router: Router, pool: PgPool) -> Router {
        router.layer(middleware::from_fn_with_state((self.clone(), pool), guard))
    }

    fn request_origin(&self, headers: &HeaderMap) -> Option<Url> {
        let root = self.root.as_ref()?;
        let authority = headers.get(header::HOST)?.to_str().ok()?;
        let candidate = Url::parse(&format!("{}://{authority}", root.scheme())).ok()?;
        if candidate.path() != "/"
            || candidate.query().is_some()
            || candidate.fragment().is_some()
            || !candidate.username().is_empty()
            || candidate.password().is_some()
            || candidate.port_or_known_default() != root.port_or_known_default()
        {
            return None;
        }
        Some(candidate)
    }

    pub(crate) fn central(&self, headers: &HeaderMap) -> bool {
        !self.enabled
            || self
                .request_origin(headers)
                .is_some_and(|url| url.origin().ascii_serialization() == self.origin)
    }

    fn slug(&self, origin: &Url) -> Option<String> {
        let suffix = format!(".{}", self.root.as_ref()?.host_str()?);
        let slug = origin.host_str()?.strip_suffix(&suffix)?;
        if slug.is_empty()
            || slug.len() > 63
            || !slug
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            || slug.starts_with('-')
            || slug.ends_with('-')
        {
            return None;
        }
        Some(slug.into())
    }

    pub(crate) fn allows_origin(&self, headers: &HeaderMap) -> bool {
        let expected = if self.enabled {
            self.request_origin(headers)
                .map(|url| url.origin().ascii_serialization())
        } else {
            Some(self.origin.clone())
        };
        if headers.get_all(header::ORIGIN).iter().count() != 1 {
            return false;
        }
        headers
            .get(header::ORIGIN)
            .and_then(|origin| origin.to_str().ok())
            == expected.as_deref()
    }

    pub(crate) fn cookie(&self, token: &str, max_age: i64, domain: bool) -> String {
        let secure = if self.origin.starts_with("https://") {
            "; Secure"
        } else {
            ""
        };
        let scope = if domain
            && self
                .root
                .as_ref()
                .and_then(Url::domain)
                .is_some_and(|host| host.contains('.'))
        {
            format!(
                "; Domain={}",
                self.root.as_ref().and_then(Url::host_str).unwrap_or("")
            )
        } else {
            String::new()
        };
        format!(
            "cairn_session={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}{scope}"
        )
    }

    pub(crate) fn session_cookies(&self, token: &str, max_age: i64) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.append(
            header::SET_COOKIE,
            HeaderValue::from_str(&self.cookie(token, max_age, self.enabled))
                .expect("validated cookie"),
        );
        headers
    }
}

async fn guard(
    State((hosts, pool)): State<(InstallationHosts, PgPool)>,
    mut request: Request,
    next: Next,
) -> Response {
    if (!hosts.enabled
        && !hosts
            .root
            .as_ref()
            .and_then(Url::domain)
            .is_some_and(|host| host.contains('.')))
        || request.uri().path() == "/health"
        || request.extensions().get::<BrowserHost>().is_some()
    {
        return next.run(request).await;
    }
    if let Some(authority) = request.uri().authority() {
        let authority = authority.as_str().to_owned();
        if let Some(host) = request.headers().get(header::HOST) {
            if !host.as_bytes().eq_ignore_ascii_case(authority.as_bytes()) {
                return ApiError::Http(
                    StatusCode::MISDIRECTED_REQUEST,
                    "Conflicting Beacon hostname",
                )
                .into_response();
            }
        } else {
            match HeaderValue::from_str(&authority) {
                Ok(value) => {
                    request.headers_mut().insert(header::HOST, value);
                }
                Err(_) => {
                    return ApiError::Http(
                        StatusCode::MISDIRECTED_REQUEST,
                        "Invalid Beacon hostname",
                    )
                    .into_response();
                }
            }
        }
    }
    let Some(origin) = hosts.request_origin(request.headers()) else {
        return ApiError::Http(StatusCode::MISDIRECTED_REQUEST, "Unknown Beacon hostname")
            .into_response();
    };
    let context = if origin.origin().ascii_serialization() == hosts.origin {
        BrowserHost::Central
    } else {
        let Some(slug) = hosts.slug(&origin) else {
            return ApiError::Http(StatusCode::MISDIRECTED_REQUEST, "Unknown Beacon hostname")
                .into_response();
        };
        let row: Result<Option<(String,)>, _> =
            query_as("SELECT id FROM installations WHERE slug=$1 AND owner_id IS NOT NULL")
                .bind(&slug)
                .fetch_optional(&pool)
                .await;
        let id = match row {
            Ok(Some((id,))) => id,
            Ok(None) => {
                return ApiError::Http(StatusCode::NOT_FOUND, "Installation not found")
                    .into_response();
            }
            Err(error) => return ApiError::from(error).into_response(),
        };
        BrowserHost::Installation { id }
    };
    if let BrowserHost::Installation { id, .. } = &context {
        let path = request.uri().path();
        if !hosts.enabled {
            if matches!(
                *request.method(),
                axum::http::Method::GET | axum::http::Method::HEAD
            ) && !path.starts_with("/api/")
                && !path.starts_with("/oauth/")
                && path != "/mcp"
            {
                let slug = hosts.slug(&origin).expect("validated installation host");
                let mut target = hosts.root.as_ref().expect("configured origin").clone();
                target.set_path(&format!("/installations/{slug}{path}"));
                target.set_query(request.uri().query());
                return axum::response::Redirect::temporary(target.as_str()).into_response();
            }
            return ApiError::Http(
                StatusCode::NOT_FOUND,
                "Use the central Cairn account origin",
            )
            .into_response();
        }
        if let Some(rest) = path.strip_prefix("/api/installations/") {
            let key = rest.split('/').next().unwrap_or("");
            if !matches!(key, "claim-code" | "device-claim") && key != id {
                return ApiError::Http(StatusCode::NOT_FOUND, "Installation not found")
                    .into_response();
            }
        }
        let central_account = path.starts_with("/api/mcp/")
            || path.starts_with("/api/account/passkeys/")
            || path.starts_with("/api/account/oauth/")
            || path.starts_with("/api/account/reauth/")
            || path.starts_with("/api/account/notifications")
            || matches!(
                path,
                "/api/account/email-code"
                    | "/api/account/verify"
                    | "/api/account/methods/remove"
                    | "/api/account/delete"
            );
        if central_account
            || path.starts_with("/api/relay/")
            || path.starts_with("/oauth/")
            || path.starts_with("/api/public/")
            || path == "/mcp"
            || path == "/sw.js"
        {
            return ApiError::Http(
                StatusCode::NOT_FOUND,
                "Use the central Cairn account origin",
            )
            .into_response();
        }
    }
    let transfer = request.uri().path() == "/browser/drafts"
        && matches!(&context, BrowserHost::Installation { .. });
    // Session credentials never travel through a URL or an additional session record.
    // Duplicate cookies with different values must not select an ambiguous identity.
    let cookies = request
        .headers()
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let token = cookie_token(request.headers(), "cairn_session");
    if cookies
        .split(';')
        .filter_map(|part| part.trim().strip_prefix("cairn_session="))
        .any(|other| other != token)
    {
        let mut response = ApiError::Http(
            StatusCode::UNAUTHORIZED,
            "Ambiguous session cookies. Sign in again.",
        )
        .into_response();
        for value in hosts.session_cookies("", 0).get_all(header::SET_COOKIE) {
            response
                .headers_mut()
                .append(header::SET_COOKIE, value.clone());
        }
        return response;
    }
    request.extensions_mut().insert(context);
    let mut response = next.run(request).await;
    if transfer {
        response.headers_mut().remove(header::X_FRAME_OPTIONS);
        response.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_str(&format!("frame-ancestors {}", hosts.origin))
                .expect("validated origin"),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_cookie_preserves_the_native_cookie_contract() {
        let shared = InstallationHosts::new("https://cairn.build", true).unwrap();
        let headers = shared.session_cookies("session-token", 123);
        let cookies: Vec<_> = headers
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect();
        assert_eq!(cookies.len(), 1);
        assert!(cookies[0].starts_with("cairn_session=session-token;"));
        for attribute in [
            "HttpOnly",
            "Secure",
            "SameSite=Lax",
            "Max-Age=123",
            "Domain=cairn.build",
        ] {
            assert!(cookies[0].contains(attribute));
        }
    }
}
