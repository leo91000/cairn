use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallationRole {
    Owner,
    Member,
}

/// Trusted request context from the authenticated relay, never an HTTP header
/// or stored authorship.
#[derive(Clone)]
pub struct InstallationIdentity {
    pub role: InstallationRole,
    pub account_id: String,
    pub mcp_scopes: Option<Vec<String>>,
    pub public_artifact: Option<String>,
    oauth_binding: String,
}

impl InstallationIdentity {
    /// Only trusted in-process callers (the authenticated tunnel) may
    /// attach this context to a request with its verified Leo account identifier.
    /// Ordinary HTTP clients cannot supply it.
    pub fn trusted(role: InstallationRole, account_id: &str) -> Self {
        Self {
            role,
            account_id: account_id.to_owned(),
            mcp_scopes: None,
            public_artifact: None,
            oauth_binding: format!("leo-account:{account_id}"),
        }
    }

    pub(crate) fn oauth_binding(&self) -> &str {
        &self.oauth_binding
    }
}

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
