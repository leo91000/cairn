//! Control-plane metadata only; application payload frames never enter signaling.
use crate::Role;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};

pub const DIRECT_VERSION: u16 = 4;
pub const DIRECT_TTL: u64 = 180;
pub const MAX_DIRECT_CONNECTIONS: usize = 32;
pub const MAX_DIRECT_PER_ACCOUNT: usize = 8;
pub const MAX_SIGNAL: usize = 16_384;
pub const SIGNING_CONTEXT: &[u8] = b"leo-direct-authorization-v4\0";

pub fn until_expiry(deadline: u64) -> std::time::Duration {
    (UNIX_EPOCH + std::time::Duration::from_secs(deadline))
        .duration_since(SystemTime::now())
        .unwrap_or_default()
}

pub fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectClaims {
    pub connection_id: String,
    pub installation_id: String,
    pub account_id: String,
    pub role: Role,
    pub session_id: String,
    pub generation: u64,
    pub fingerprint: String,
    pub expires_at: u64,
    pub nonce: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectAuthorization {
    pub claims: DirectClaims,
    pub signature: String,
}

pub fn signing_bytes(claims: &DirectClaims) -> Vec<u8> {
    let mut bytes = SIGNING_CONTEXT.to_vec();
    bytes.extend(serde_json::to_vec(claims).expect("serializable direct claims"));
    bytes
}

/// A SHA-256 DTLS certificate fingerprint in canonical SDP form.
pub fn valid_fingerprint(fingerprint: &str) -> bool {
    let Some(digest) = fingerprint.strip_prefix("sha-256 ") else {
        return false;
    };
    let octets: Vec<_> = digest.split(':').collect();
    octets.len() == 32
        && octets.iter().all(|octet| {
            octet.len() == 2
                && octet
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_lowercase())
        })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DirectSignal {
    Offer {
        sdp: String,
    },
    Answer {
        sdp: String,
    },
    Candidate {
        candidate: String,
        sdp_mid: Option<String>,
        sdp_m_line_index: Option<u16>,
    },
}

impl DirectSignal {
    pub fn valid(&self) -> bool {
        match self {
            Self::Offer { sdp } | Self::Answer { sdp } => {
                sdp.len() <= MAX_SIGNAL
                    && sdp.starts_with("v=0\r\n")
                    && sdp.lines().all(sdp_line)
                    && sdp.lines().any(|line| line.starts_with("m=application "))
                    && sdp.lines().any(|line| {
                        line.strip_prefix("a=fingerprint:")
                            .is_some_and(valid_fingerprint)
                    })
            }
            Self::Candidate {
                candidate,
                sdp_mid,
                sdp_m_line_index,
            } => {
                (candidate.is_empty()
                    || (candidate.starts_with("candidate:")
                        && candidate.len() <= 1024
                        && candidate.bytes().all(|b| b.is_ascii_graphic() || b == b' ')))
                    && sdp_mid.as_ref().is_none_or(|mid| {
                        mid.len() <= 32
                            && mid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    })
                    && sdp_m_line_index.is_none_or(|index| index == 0)
            }
        }
    }

    pub fn matches_client_fingerprint(&self, fingerprint: &str) -> bool {
        match self {
            Self::Offer { sdp } | Self::Answer { sdp } => {
                let fingerprints: Vec<_> = sdp
                    .lines()
                    .filter_map(|line| line.strip_prefix("a=fingerprint:"))
                    .collect();
                !fingerprints.is_empty() && fingerprints.iter().all(|value| *value == fingerprint)
            }
            Self::Candidate { .. } => true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
pub enum DirectRevocation {
    Account { account_id: String, generation: u64 },
    Session { session_id: String },
    Installation,
}

/// Installation verification state, initialized exclusively by its authenticated tunnel.
pub struct DirectVerifier {
    installation: String,
    public_key: Vec<u8>,
    generations: HashMap<String, u64>,
    nonces: HashMap<String, u64>,
    sessions: HashMap<String, u64>,
}

impl DirectVerifier {
    pub fn new(installation: String, public_key: &str) -> Result<Self, &'static str> {
        let public_key = URL_SAFE_NO_PAD
            .decode(public_key)
            .map_err(|_| "Invalid signing key")?;
        if public_key.len() != 32 {
            return Err("Invalid signing key");
        }
        Ok(Self {
            installation,
            public_key,
            generations: HashMap::new(),
            nonces: HashMap::new(),
            sessions: HashMap::new(),
        })
    }

    pub fn verify(
        &mut self,
        authorization: &DirectAuthorization,
    ) -> Result<DirectClaims, &'static str> {
        let now = unix_time();
        self.nonces.retain(|_, deadline| *deadline > now);
        self.sessions.retain(|_, deadline| *deadline > now);
        let claims = &authorization.claims;
        let signature = URL_SAFE_NO_PAD
            .decode(&authorization.signature)
            .map_err(|_| "Invalid signature")?;
        UnparsedPublicKey::new(&ED25519, &self.public_key)
            .verify(&signing_bytes(claims), &signature)
            .map_err(|_| "Invalid signature")?;
        if claims.installation_id != self.installation
            || claims.expires_at <= now
            || claims.expires_at > now + DIRECT_TTL
            || !valid_fingerprint(&claims.fingerprint)
            || claims.connection_id.is_empty()
            || claims.account_id.is_empty()
            || claims.session_id.is_empty()
            || claims.nonce.is_empty()
            || claims.generation != *self.generations.get(&claims.account_id).unwrap_or(&0)
            || self.sessions.contains_key(&claims.session_id)
        {
            return Err("Invalid direct authorization");
        }
        if self.nonces.contains_key(&claims.nonce) {
            return Err("Replayed authorization");
        }
        if self.nonces.len() >= 4096 {
            return Err("Authorization verification busy");
        }
        self.nonces.insert(claims.nonce.clone(), claims.expires_at);
        Ok(claims.clone())
    }

    pub fn revoke(&mut self, scope: &DirectRevocation) {
        match scope {
            DirectRevocation::Account {
                account_id,
                generation,
            } => {
                self.generations.insert(account_id.clone(), *generation);
            }
            DirectRevocation::Session { session_id } => {
                self.sessions
                    .insert(session_id.clone(), unix_time() + DIRECT_TTL);
            }
            DirectRevocation::Installation => {
                self.public_key.clear();
            }
        }
    }
}

fn token(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'-' | b'_'))
}

fn sdp_line(line: &str) -> bool {
    if matches!(
        line,
        "v=0"
            | "s=-"
            | "t=0 0"
            | "a=ice-options:trickle"
            | "a=end-of-candidates"
            | "a=setup:actpass"
            | "a=setup:active"
            | "a=setup:passive"
            | "a=sendrecv"
            | "a=msid-semantic: WMS"
            | "a=msid-semantic: WMS *"
    ) {
        return true;
    }
    if let Some(value) = line.strip_prefix("a=fingerprint:") {
        return valid_fingerprint(value);
    }
    if let Some(value) = line.strip_prefix("a=candidate:") {
        return valid_candidate(&format!("candidate:{value}"));
    }
    for prefix in ["a=mid:", "a=ice-ufrag:", "a=ice-pwd:"] {
        if let Some(value) = line.strip_prefix(prefix) {
            return token(value, 256);
        }
    }
    for prefix in ["a=sctp-port:", "a=max-message-size:"] {
        if let Some(value) = line.strip_prefix(prefix) {
            return value.parse::<u32>().is_ok();
        }
    }
    if let Some(value) = line.strip_prefix("a=group:BUNDLE ") {
        return value.split(' ').all(|mid| token(mid, 32));
    }
    let words: Vec<_> = line.split(' ').collect();
    match words.as_slice() {
        ["o=-", session, version, "IN", family, address] => {
            session.parse::<u64>().is_ok()
                && version.parse::<u64>().is_ok()
                && valid_address(family, address)
        }
        ["c=IN", family, address] => valid_address(family, address),
        ["m=application", port, "UDP/DTLS/SCTP", "webrtc-datachannel"] => {
            port.parse::<u16>().is_ok()
        }
        _ => false,
    }
}

fn valid_address(family: &str, value: &str) -> bool {
    match value.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(_)) => family == "IP4",
        Ok(std::net::IpAddr::V6(_)) => family == "IP6",
        _ => false,
    }
}

fn valid_candidate(candidate: &str) -> bool {
    if candidate.is_empty() {
        return true;
    }
    if candidate.len() > 1024 {
        return false;
    }
    let Some(value) = candidate.strip_prefix("candidate:") else {
        return false;
    };
    let words: Vec<_> = value.split(' ').collect();
    if words.len() < 8 {
        return false;
    }
    let address = |value: &str| {
        value.parse::<std::net::IpAddr>().is_ok()
            || value
                .strip_suffix(".local")
                .is_some_and(|name| token(name, 64))
    };
    token(words[0], 32)
        && words[1] == "1"
        && matches!(words[2], "udp" | "UDP" | "tcp" | "TCP")
        && words[3].parse::<u32>().is_ok()
        && address(words[4])
        && words[5].parse::<u16>().is_ok()
        && words[6] == "typ"
        && matches!(words[7], "host" | "srflx" | "prflx" | "relay")
        && words[8..].chunks(2).all(|pair| match pair {
            ["raddr", value] => address(value),
            ["rport", value] => value.parse::<u16>().is_ok(),
            ["generation" | "network-id" | "network-cost", value] => value.parse::<u32>().is_ok(),
            ["ufrag", value] => token(value, 256),
            ["tcptype", value] => matches!(*value, "active" | "passive" | "so"),
            _ => false,
        })
}
