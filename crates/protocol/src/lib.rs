//! Versioned frames for the authenticated, installation-initiated tunnel.
//! Requests are independent: future streaming frames can share their request ID.
pub mod data_channel;
pub mod direct;
pub mod nodes;
pub mod slugs;

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 6;
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
// A node channel carries raw HTTP bytes in binary WebSocket messages.
pub const MAX_NODE_CHANNEL_MESSAGE: usize = 1 << 20;
pub const MAX_NODE_CHANNELS: usize = 256;
// One address cannot take every channel of an installation.
pub const MAX_NODE_CHANNELS_PER_ADDRESS: usize = 64;

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

#[derive(Clone, Serialize, Deserialize)]
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
    pub mcp_credential_digest: Option<String>,
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
    /// The only members to notify besides the owner, who is always notified. Absent from
    /// older installations and for events meant for everyone; older Beacons ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub members: Option<Vec<String>>,
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
        version: u16,
    },
    Welcome {
        version: u16,
    },
    // Control plane only, on the authenticated installation tunnel.
    DirectKey {
        public_key: String,
        stun_url: String,
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
    // Terminal refusal on an authenticated DataChannel; no application request was admitted.
    UpdateRequired {
        message: String,
    },
    Request(ApiRequest),
    Response(ApiResponse),
    // Credit permits one bounded chunk, independently per stream.
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
    // Pending events stay on the installation until acknowledged.
    Notification(NotificationEvent),
    NotificationAck {
        id: String,
        // Wire name retained: true means the event can leave the installation outbox,
        // including invalid events and installations whose access has been revoked.
        delivered: bool,
    },
    // Beacon asks the installation to accept a node channel by
    // opening its own authenticated WebSocket for this ID. The node and the
    // manager speak TLS inside it, so Beacon carries bytes it cannot read.
    NodeChannel {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        address: Option<String>,
    },
    // Beacon forwards a node's WebRTC offer, with every
    // candidate already gathered, so the node can reach the manager directly.
    NodeOffer {
        id: String,
        sdp: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        address: Option<String>,
    },
    // The installation's complete answer, or no SDP to refuse the offer.
    NodeAnswer {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sdp: Option<String>,
    },
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
            | "accept-encoding"
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
            | "content-encoding"
            | "vary"
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
