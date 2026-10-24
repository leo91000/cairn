use crate::{Manifest, short, slug};
use serde::{Deserialize, Serialize};

/// Public endpoint authenticated by a signature over the original request bytes.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Webhook {
    pub name: String,
    pub signature: WebhookSignature,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<WebhookSetup>,
}

/// Signed JSON challenges accepted before activation. Every selector must match.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookSetup {
    #[serde(rename = "match")]
    pub selectors: std::collections::BTreeMap<String, String>,
}

/// HMAC-SHA256, encoded as a hexadecimal digest with an optional prefix.
/// Only the host reads `secret`; the package receives the verified request.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookSignature {
    pub secret: String,
    pub header: String,
    #[serde(default)]
    pub prefix: String,
    /// Literal text with exactly one `{{body}}` and, when configured, `{{timestamp}}`.
    pub template: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<WebhookTimestamp>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookTimestamp {
    pub header: String,
    /// Unix seconds. The signed timestamp must be within this distance of host time.
    pub max_age_seconds: u16,
}

pub(crate) fn validate(manifest: &Manifest) -> Result<(), &'static str> {
    if manifest.webhooks.is_empty() == manifest.permits_capability("webhooks") {
        return Err("Plugin manifest must list endpoints exactly when it requests webhooks.");
    }
    if manifest.webhooks.len() > 8 {
        return Err("Plugin manifest declares too many public endpoints.");
    }
    let mut names = Vec::new();
    for webhook in &manifest.webhooks {
        let signature = &webhook.signature;
        let timestamp = signature.timestamp.as_ref();
        let template = &signature.template;
        let bound_timestamp = template.matches("{{timestamp}}").count();
        let remaining = template
            .replace("{{body}}", "")
            .replace("{{timestamp}}", "");
        if !slug(&webhook.name, 40)
            || names.contains(&webhook.name.as_str())
            || !manifest
                .secret_settings()
                .any(|key| key == signature.secret)
            || !slug(&signature.header, 80)
            || !short(&signature.prefix, 32)
            || signature
                .prefix
                .bytes()
                .any(|byte| !byte.is_ascii_graphic())
            || !short(template, 128)
            || template.chars().any(char::is_control)
            || template.matches("{{body}}").count() != 1
            || bound_timestamp != usize::from(timestamp.is_some())
            || remaining.contains(['{', '}'])
            || timestamp.is_some_and(|rule| {
                !slug(&rule.header, 80)
                    || rule.header == signature.header
                    || !(1..=600).contains(&rule.max_age_seconds)
            })
        {
            return Err("Plugin manifest has an invalid public endpoint signature rule.");
        }
        if let Some(setup) = &webhook.setup
            && (setup.selectors.is_empty()
                || setup.selectors.len() > 8
                || setup.selectors.iter().any(|(pointer, value)| {
                    !pointer.starts_with('/')
                        || !short(pointer, 128)
                        || pointer.chars().any(char::is_control)
                        || !short(value, 256)
                        || value.chars().any(char::is_control)
                        || pointer
                            .split('~')
                            .skip(1)
                            .any(|part| !part.starts_with(['0', '1']))
                }))
        {
            return Err("Declare one to eight JSON pointer selectors for setup challenges.");
        }
        names.push(webhook.name.as_str());
    }
    Ok(())
}
