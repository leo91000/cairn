//! Approved OAuth providers. Only the host sees token endpoint responses.
use crate::{Manifest, short, slug};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OAuthTokenResponse {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

fn scope_parameter() -> String {
    "scope".into()
}

fn scope_separator() -> String {
    " ".into()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OAuth {
    pub name: String,
    pub title: String,
    pub collection: String,
    pub authorization_url: String,
    pub token_url: String,
    /// Instance setting names, not literal credentials.
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub pkce: bool,
    #[serde(default = "scope_parameter")]
    pub scope_parameter: String,
    #[serde(default = "scope_separator")]
    pub scope_separator: String,
    pub scopes: Vec<String>,
    pub token: OAuthTokenResponse,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token_response: Option<OAuthTokenResponse>,
    /// Nonsecret identity fields only; leaves must be `id` or `name`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

fn pointer(value: &str) -> bool {
    value.len() <= 200
        && value.starts_with('/')
        && value.split('/').skip(1).count() <= 8
        && value.split('/').skip(1).all(|part| {
            !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
}

fn endpoint(manifest: &Manifest, value: &str) -> bool {
    let Some(rest) = value.strip_prefix("https://") else {
        return false;
    };
    let Some((host, path)) = rest.split_once('/') else {
        return false;
    };
    value.len() <= 500
        && manifest.allows_host(host)
        && crate::hostname(host)
        && !path.is_empty()
        && !path.contains(['?', '#', '\\'])
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-'))
        && !path.split('/').any(|part| matches!(part, "." | ".."))
}

pub(crate) fn validate(manifest: &Manifest) -> Result<(), &'static str> {
    if manifest.oauth.is_empty() == manifest.permits_capability("oauth") || manifest.oauth.len() > 8
    {
        return Err("Declare at most eight OAuth providers exactly when requesting oauth.");
    }
    let mut names = HashSet::new();
    for provider in &manifest.oauth {
        let collection = manifest
            .resources
            .iter()
            .find(|c| c.name == provider.collection)
            .ok_or("OAuth must target a declared resource collection.")?;
        let client_id = manifest
            .settings
            .iter()
            .any(|f| f["key"] == provider.client_id && f["type"] == "string");
        let client_secret = provider.client_secret.as_ref().is_none_or(|key| {
            manifest
                .settings
                .iter()
                .any(|f| f["key"] == *key && f["type"] == "secret")
        });
        if !slug(&provider.name, 40)
            || !names.insert(&provider.name)
            || provider.title.trim().is_empty()
            || !short(&provider.title, 100)
            || !endpoint(manifest, &provider.authorization_url)
            || !endpoint(manifest, &provider.token_url)
            || !client_id
            || !client_secret
            || (!provider.pkce && provider.client_secret.is_none())
            || !matches!(provider.scope_separator.as_str(), " " | ",")
            || provider.scope_parameter.is_empty()
            || provider.scope_parameter.len() > 40
            || !provider.scope_parameter.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            })
            || [
                "response_type",
                "client_id",
                "client_secret",
                "redirect_uri",
                "state",
                "code",
                "grant_type",
                "code_challenge",
                "code_challenge_method",
                "code_verifier",
            ]
            .contains(&provider.scope_parameter.as_str())
        {
            return Err("Invalid OAuth provider, endpoints or client settings.");
        }
        let mut scopes = HashSet::new();
        if provider.scopes.is_empty()
            || provider.scopes.len() > 128
            || !provider.scopes.iter().all(|scope| {
                !scope.is_empty()
                    && scope.len() <= 100
                    && scope.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b':' | b'.' | b'_' | b'-')
                    })
                    && scopes.insert(scope)
            })
        {
            return Err("OAuth scopes must be distinct bounded names.");
        }
        for response in
            std::iter::once(&provider.token).chain(provider.refresh_token_response.iter())
        {
            let paths = std::iter::once(&response.access_token)
                .chain(response.refresh_token.iter())
                .chain(response.expires_in.iter())
                .chain(response.scope.iter());
            let mut seen = HashSet::new();
            if !paths
                .into_iter()
                .all(|path| pointer(path) && seen.insert(path))
            {
                return Err("OAuth token response fields must be distinct JSON pointers.");
            }
        }
        if provider.metadata.len() > 8
            || !provider.metadata.iter().all(|(key, path)| {
                collection
                    .settings
                    .iter()
                    .any(|f| f["key"] == *key && f["type"] == "string")
                    && pointer(path)
                    && matches!(path.rsplit('/').next(), Some("id" | "name"))
                    && !path.split('/').any(|part| {
                        part.contains("token")
                            || part.contains("secret")
                            || part.contains("credential")
                    })
                    && std::iter::once(&provider.token)
                        .chain(provider.refresh_token_response.iter())
                        .all(|response| {
                            std::iter::once(&response.access_token)
                                .chain(response.refresh_token.iter())
                                .all(|secret| {
                                    path != secret
                                        && !path.starts_with(&format!("{secret}/"))
                                        && !secret.starts_with(&format!("{path}/"))
                                })
                        })
            })
        {
            return Err("OAuth metadata may capture only nonsecret identity strings.");
        }
    }
    Ok(())
}
