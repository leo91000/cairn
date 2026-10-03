//! Versioned frames for the authenticated, installation-initiated tunnel.
//! Requests are independent: future streaming frames can share their request ID.
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_BODY: usize = 8_000_000;
// JSON byte arrays expand by up to four bytes per byte, plus envelope metadata.
pub const MAX_FRAME: usize = MAX_BODY * 4 + 65_536;
pub const MAX_IN_FLIGHT: usize = 32;

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Owner,
    Member,
}

#[derive(Serialize, Deserialize)]
pub struct ApiRequest {
    pub id: String,
    pub account_id: String,
    pub role: Role,
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
pub struct ApiResponse {
    pub id: String,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Frame {
    Hello { versions: Vec<u16> },
    Welcome { version: u16 },
    Request(ApiRequest),
    Response(ApiResponse),
}

pub fn request_header(name: &str) -> bool {
    matches!(
        name,
        "content-type" | "accept" | "range" | "if-none-match" | "if-modified-since"
    )
}

pub fn response_header(name: &str) -> bool {
    matches!(
        name,
        "content-type"
            | "content-disposition"
            | "etag"
            | "last-modified"
            | "accept-ranges"
            | "content-range"
            | "retry-after"
            | "content-security-policy"
            | "x-content-type-options"
            | "x-frame-options"
            | "referrer-policy"
    )
}

/// Restrict the tunnel to installation API routes, excluding local browser auth.
pub fn api_path(path: &str) -> bool {
    let route = path.split('?').next().unwrap_or("");
    route.starts_with("/api/")
        && !["/api/session", "/api/setup", "/api/login", "/api/logout"].contains(&route)
        && !route.contains('#')
        && !route.contains('\\')
        && !route
            .split('/')
            .any(|segment| segment == "." || segment == "..")
}
