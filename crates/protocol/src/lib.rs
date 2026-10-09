//! Versioned frames for the authenticated, installation-initiated tunnel.
//! Requests are independent: future streaming frames can share their request ID.
pub mod data_channel;
pub mod direct;

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 4;
pub const MIN_PROTOCOL_VERSION: u16 = 1;
pub const SUPPORTED_VERSIONS: &[u16] = &[PROTOCOL_VERSION, 3, 2, MIN_PROTOCOL_VERSION];
pub const MAX_STREAM_CHUNK: usize = 65_536;
pub const MAX_BODY: usize = 8_000_000;
// Base64 expands by four bytes per three body bytes, plus envelope metadata.
pub const MAX_FRAME: usize = MAX_BODY.div_ceil(3) * 4 + 65_536;
pub const MAX_IN_FLIGHT: usize = 32;
pub const MAX_STREAMS: usize = 24;
pub const MAX_STREAMS_PER_ACCOUNT: usize = 8;
// Public files never borrow authenticated API or live-stream capacity.
pub const MAX_PUBLIC_IN_FLIGHT: usize = 4;
pub const MAX_NOTIFICATION_IN_FLIGHT: usize = 4;
pub const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Owner,
    Member,
}

/// Scheduling eligibility from the beacon authority, never local request authorization.
#[derive(Clone, Serialize, Deserialize)]
pub struct TaskAuthorGrant {
    pub account_id: String,
    pub access_id: String,
}

#[derive(Serialize, Deserialize)]
pub struct TaskAuthorPolicy {
    pub owner: TaskAuthorGrant,
    pub members: Vec<TaskAuthorGrant>,
}

impl TaskAuthorPolicy {
    pub fn grant(&self, account: &str) -> Option<&TaskAuthorGrant> {
        std::iter::once(&self.owner)
            .chain(&self.members)
            .find(|grant| grant.account_id == account)
    }
}

#[derive(Serialize, Deserialize)]
pub struct ApiRequest {
    pub id: String,
    pub account_id: String,
    pub role: Role,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_scopes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_artifact: Option<String>,
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    #[serde(with = "body")]
    pub body: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
pub struct ApiResponse {
    pub id: String,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    #[serde(with = "body")]
    pub body: Vec<u8>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationEvent {
    pub id: String,
    pub chat_id: String,
    #[serde(flatten)]
    pub kind: NotificationKind,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum NotificationKind {
    Question {
        question_id: String,
    },
    Alert {
        alert_id: String,
        title: String,
        body: String,
    },
}

mod body {
    use super::MAX_BODY;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        if encoded.len() > MAX_BODY.div_ceil(3) * 4 {
            return Err(D::Error::custom("Relay body is too large"));
        }

        let bytes = STANDARD.decode(encoded).map_err(D::Error::custom)?;
        if bytes.len() > MAX_BODY {
            return Err(D::Error::custom("Relay body is too large"));
        }

        Ok(bytes)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Frame {
    Hello {
        versions: Vec<u16>,
    },
    Welcome {
        version: u16,
    },
    // Version 4: control plane only, on the authenticated installation tunnel.
    DirectKey {
        public_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stun_url: Option<String>,
    },
    DirectAuthorize {
        id: String,
        authorization: direct::DirectAuthorization,
    },
    DirectRenew {
        id: String,
        authorization: direct::DirectAuthorization,
    },
    DirectAuthorized {
        id: String,
        accepted: bool,
    },
    DirectSignal {
        id: String,
        signal: direct::DirectSignal,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    DirectSignalAck {
        id: String,
        accepted: bool,
    },
    DirectRevoke {
        scope: direct::DirectRevocation,
    },
    Request(ApiRequest),
    Response(ApiResponse),
    // Version 2 only. Credit permits one bounded chunk, independently per stream.
    StreamStart(ApiResponse),
    StreamChunk {
        id: String,
        #[serde(with = "body")]
        body: Vec<u8>,
    },
    StreamEnd {
        id: String,
        failed: bool,
    },
    StreamCredit {
        id: String,
    },
    Cancel {
        id: String,
    },
    // Version 3 only. Pending events stay on the installation until acknowledged.
    Notification(NotificationEvent),
    NotificationAck {
        id: String,
        // Wire name retained: true means the event can leave the installation outbox,
        // including invalid events and installations whose access has been revoked.
        delivered: bool,
    },
}

pub fn negotiate(versions: &[u16]) -> Option<u16> {
    SUPPORTED_VERSIONS
        .iter()
        .copied()
        .find(|version| versions.contains(version))
}

pub fn stream_path(path: &str) -> bool {
    let route = path.split('?').next().unwrap_or("");
    route.ends_with("/stream") || route.starts_with("/api/shared-artifacts/")
}

pub fn request_header(name: &str) -> bool {
    matches!(
        name,
        "content-type"
            | "accept"
            | "range"
            | "if-none-match"
            | "if-modified-since"
            | "last-event-id"
            | "mcp-protocol-version"
            | "mcp-method"
    )
}

pub fn response_header(name: &str) -> bool {
    matches!(
        name,
        "content-type"
            | "content-length"
            | "content-disposition"
            | "etag"
            | "last-modified"
            | "accept-ranges"
            | "content-range"
            | "retry-after"
            | "mcp-protocol-version"
            | "www-authenticate"
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
