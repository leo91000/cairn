//! How the owner connects an instance: what it takes, announced in the catalogue, and
//! the ordered steps the web and Android setup assistants walk through. Packages
//! without `[setup]` steps get the same assistant with steps derived from their
//! credential, settings, delegation and agent access.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// What connecting an instance takes, shown before the owner starts.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Effort {
    /// A sign-in on the service's own page.
    Signin,
    /// A token or key created on the service, then pasted.
    Token,
    /// An application the owner registers with the service first, e.g. an OAuth client.
    App,
}

/// What a step asks for. Settings steps show declared fields; the others are drawn
/// by Cairn from the credential, the delegation and the agent grants.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum StepKind {
    #[default]
    Settings,
    Credential,
    People,
    Agents,
}

/// A value Cairn knows and the owner copies into the service, such as the OAuth
/// callback URL.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CopyValue {
    CallbackUrl,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Risk {
    Low,
    Medium,
    High,
}

/// Something the owner does on the service's site, ticked off in the assistant.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Task {
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// The service's page for this task. HTTPS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The link's label, e.g. "Open API Library".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy: Option<CopyValue>,
}

/// A named combination of the step's boolean settings, e.g. "Read only".
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Preset {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<Risk>,
    /// Settings turned on. The step's other boolean settings are turned off.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Step {
    pub id: String,
    #[serde(default)]
    pub kind: StepKind,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Keys of the settings this step shows, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<String>,
    /// The same values serve every instance, e.g. one OAuth client for every mailbox.
    /// A new instance can reuse them from another one (`sharedFrom`): the host copies
    /// them, secrets included, without sending secrets to the client.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shared: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checklist: Vec<Task>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub presets: Vec<Preset>,
    /// A consequence to know before going on, e.g. a provider's token expiry.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub warning: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Setup {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    /// What the owner needs before connecting, in one or two sentences.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    /// A string setting naming a new instance once known, e.g. the verified mailbox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_from: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<Step>,
}

fn step(id: &str, kind: StepKind, title: String, settings: Vec<String>) -> Step {
    Step {
        id: id.to_owned(),
        kind,
        title,
        description: String::new(),
        settings,
        shared: false,
        checklist: Vec::new(),
        presets: Vec::new(),
        warning: String::new(),
    }
}

fn https_link(raw: &str) -> bool {
    raw.len() <= 500
        && url::Url::parse(raw).is_ok_and(|url| {
            url.scheme() == "https"
                && url.username().is_empty()
                && url.password().is_none()
                && url.host_str().is_some()
        })
}

impl super::Manifest {
    /// Settings the owner sees, in manifest order.
    fn visible_settings(&self) -> Vec<String> {
        self.settings
            .iter()
            .filter(|field| field["hidden"] != true)
            .filter_map(|field| field["key"].as_str().map(str::to_owned))
            .collect()
    }

    /// The setup the owner follows: the declared steps, or steps derived from the
    /// package's credential, settings, delegation and agent access.
    pub fn setup(&self) -> Setup {
        let declared = self.setup.clone().unwrap_or_default();
        let credential = self.credential.as_ref();
        let effort = declared.effort.or_else(|| match credential {
            Some(credential) if credential.oauth.is_some() => Some(Effort::App),
            Some(credential) if credential.device_flow.is_some() => Some(Effort::Signin),
            Some(_) => Some(Effort::Token),
            None if self
                .settings
                .iter()
                .any(|field| field["type"] == "secret" && field["required"] == true) =>
            {
                Some(Effort::Token)
            }
            None => None,
        });
        if !declared.steps.is_empty() {
            return Setup { effort, ..declared };
        }

        let mut steps = Vec::new();
        let settings = self.visible_settings();
        let settings_step = (!settings.is_empty())
            .then(|| step("settings", StepKind::Settings, "Settings".into(), settings));
        let credential_step = credential.map(|credential| {
            let title = if credential.oauth.is_some() || credential.device_flow.is_some() {
                format!("Sign in with {}", self.name)
            } else {
                credential.label.clone()
            };
            step("credential", StepKind::Credential, title, Vec::new())
        });
        // An OAuth sign-in needs its client settings first; other forms read the
        // credential to offer their choices.
        if credential.is_some_and(|credential| credential.oauth.is_some()) {
            steps.extend(settings_step);
            steps.extend(credential_step);
        } else {
            steps.extend(credential_step);
            steps.extend(settings_step);
        }
        if self.permits_capability("conversations.submit") {
            steps.push(step(
                "people",
                StepKind::People,
                "Who can talk to it".into(),
                Vec::new(),
            ));
        }
        if self.agent_access.is_some() {
            steps.push(step(
                "agents",
                StepKind::Agents,
                "Agents".into(),
                Vec::new(),
            ));
        }
        Setup {
            effort,
            steps,
            ..declared
        }
    }

    /// Keys of the settings a new instance may reuse from another one.
    pub fn shared_settings(&self) -> Vec<&str> {
        self.setup
            .iter()
            .flat_map(|setup| &setup.steps)
            .filter(|step| step.shared)
            .flat_map(|step| step.settings.iter().map(String::as_str))
            .collect()
    }

    pub(super) fn validate_setup(&self) -> Result<(), &'static str> {
        let Some(setup) = &self.setup else {
            return Ok(());
        };
        let invalid = "Plugin manifest has an invalid setup.";
        if !super::short(&setup.summary, 300) {
            return Err(invalid);
        }
        if let Some(key) = &setup.name_from
            && !self
                .settings
                .iter()
                .any(|field| field["key"] == key.as_str() && field["type"] == "string")
        {
            return Err(invalid);
        }
        if setup.steps.is_empty() {
            return Ok(());
        }
        if setup.steps.len() > 8 {
            return Err(invalid);
        }

        let field = |key: &str| self.settings.iter().find(|field| field["key"] == key);
        let mut ids = Vec::new();
        let mut shown = Vec::new();
        let mut kinds = Vec::new();
        for (index, step) in setup.steps.iter().enumerate() {
            let valid_text = super::slug(&step.id, 40)
                && !ids.contains(&step.id.as_str())
                && !step.title.trim().is_empty()
                && super::short(&step.title, 60)
                && !step.title.contains('\n')
                && super::short(&step.description, 400)
                && super::short(&step.warning, 300);
            ids.push(&step.id);
            if !valid_text {
                return Err(invalid);
            }

            // Cairn draws each host-owned step once, where the package asks for it.
            let host_owned = step.kind != StepKind::Settings;
            if host_owned && kinds.contains(&step.kind) {
                return Err(invalid);
            }
            kinds.push(step.kind);
            let allowed = match step.kind {
                StepKind::Settings => !step.settings.is_empty(),
                StepKind::Credential => self.credential.is_some(),
                StepKind::People => self.permits_capability("conversations.submit"),
                // Grants are offered once the instance exists: on its last step.
                StepKind::Agents => self.agent_access.is_some() && index == setup.steps.len() - 1,
            };
            if !allowed
                || (host_owned && !step.settings.is_empty())
                || (step.shared && step.kind != StepKind::Settings)
                || step.settings.len() > 30
            {
                return Err(invalid);
            }

            for key in &step.settings {
                let Some(field) = field(key) else {
                    return Err(invalid);
                };
                if field["hidden"] == true || shown.contains(&key.as_str()) {
                    return Err(invalid);
                }
                shown.push(key);
            }

            if step.checklist.len() > 8 {
                return Err(invalid);
            }
            let oauth = self
                .credential
                .as_ref()
                .is_some_and(|credential| credential.oauth.is_some());
            for task in &step.checklist {
                if task.title.trim().is_empty()
                    || !super::short(&task.title, 100)
                    || task.title.contains('\n')
                    || !super::short(&task.text, 300)
                    || task.url.as_deref().is_some_and(|url| !https_link(url))
                    || task.link.as_deref().is_some_and(|link| {
                        task.url.is_none()
                            || link.trim().is_empty()
                            || !super::short(link, 40)
                            || link.contains('\n')
                    })
                    || (task.copy == Some(CopyValue::CallbackUrl) && !oauth)
                {
                    return Err(invalid);
                }
            }

            if step.presets.len() > 6 {
                return Err(invalid);
            }
            let mut presets = Vec::new();
            for preset in &step.presets {
                let switches = preset.values.keys().all(|key| {
                    step.settings.contains(key)
                        && field(key).is_some_and(|field| field["type"] == "boolean")
                });
                if !super::slug(&preset.id, 40)
                    || presets.contains(&preset.id.as_str())
                    || preset.title.trim().is_empty()
                    || !super::short(&preset.title, 60)
                    || preset.title.contains('\n')
                    || !super::short(&preset.description, 200)
                    || !switches
                {
                    return Err(invalid);
                }
                presets.push(&preset.id);
            }
        }

        // Declared steps replace the derived ones, so they cover the whole instance.
        let missing_step = (self.credential.is_some() && !kinds.contains(&StepKind::Credential))
            || (self.permits_capability("conversations.submit")
                && !kinds.contains(&StepKind::People))
            || (self.agent_access.is_some() && !kinds.contains(&StepKind::Agents));
        let missing_setting = self
            .visible_settings()
            .iter()
            .any(|key| !shown.contains(&key.as_str()));
        if missing_step || missing_setting {
            return Err(
                "Plugin setup steps must cover the credential, every visible setting, the delegation and agent access.",
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Manifest;
    use serde_json::{Value, json};

    fn oauth() -> Value {
        json!({
            "id": "mail", "name": "Mail", "version": "1", "apiVersion": 2,
            "capabilities": ["http"], "hosts": ["api.mail.example", "auth.mail.example"],
            "credential": {
                "label": "Authorization", "header": "authorization", "prefix": "Bearer ",
                "oauth": {
                    "authorizationUrl": "https://auth.mail.example/authorize",
                    "tokenUrl": "https://auth.mail.example/token",
                    "clientIdSetting": "clientId", "clientSecretSetting": "clientSecret",
                    "scopeSettings": { "allowSend": ["mail.send"] }
                }
            },
            "settings": [
                { "key": "clientId", "title": "Client ID", "type": "string", "required": true },
                { "key": "clientSecret", "title": "Client secret", "type": "secret", "required": true },
                { "key": "email", "title": "Email", "type": "string", "hidden": true },
                { "key": "allowSend", "title": "Send", "type": "boolean", "risk": "medium" }
            ],
            "agentAccess": {}
        })
    }

    fn steps() -> Value {
        json!({
            "effort": "app", "summary": "Create an app once.", "nameFrom": "email",
            "steps": [
                {
                    "id": "app", "title": "App", "settings": ["clientId", "clientSecret"], "shared": true,
                    "checklist": [{ "title": "Create a client", "url": "https://console.mail.example/clients", "link": "Open", "copy": "callbackUrl" }]
                },
                {
                    "id": "permissions", "title": "Permissions", "settings": ["allowSend"],
                    "presets": [{ "id": "read", "title": "Read only" }, { "id": "send", "title": "Send", "risk": "medium", "values": { "allowSend": true } }]
                },
                { "id": "connect", "kind": "credential", "title": "Sign in" },
                { "id": "agents", "kind": "agents", "title": "Agents" }
            ]
        })
    }

    fn parse(value: Value) -> Result<Manifest, &'static str> {
        let manifest: Manifest = serde_json::from_value(value).map_err(|_| "parse")?;
        manifest.validate()?;
        Ok(manifest)
    }

    #[test]
    fn derived_steps_follow_what_each_credential_needs_first() {
        let manifest = parse(oauth()).unwrap();
        let setup = manifest.setup();
        assert_eq!(setup.effort, Some(Effort::App));
        let kinds = setup.steps.iter().map(|s| s.kind).collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [StepKind::Settings, StepKind::Credential, StepKind::Agents]
        );
        assert_eq!(
            setup.steps[0].settings,
            ["clientId", "clientSecret", "allowSend"]
        );
        assert_eq!(setup.steps[1].title, "Sign in with Mail");

        let bot = parse(json!({
            "id": "bot", "name": "Bot", "version": "1", "apiVersion": 2,
            "capabilities": ["http", "conversations.submit"], "hosts": ["api.bot.example"],
            "credential": { "label": "Bot token", "header": "authorization" },
            "settings": [{ "key": "channel", "title": "Channel", "type": "string" }]
        }))
        .unwrap();
        let setup = bot.setup();
        assert_eq!(setup.effort, Some(Effort::Token));
        let kinds = setup.steps.iter().map(|s| s.kind).collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [StepKind::Credential, StepKind::Settings, StepKind::People]
        );
        assert_eq!(setup.steps[0].title, "Bot token");

        let vault = parse(json!({
            "id": "vault", "name": "Vault", "version": "1", "apiVersion": 2, "capabilities": [],
            "settings": [{ "key": "token", "title": "Token", "type": "secret", "required": true }]
        }))
        .unwrap();
        assert_eq!(vault.setup().effort, Some(Effort::Token));
        assert!(vault.shared_settings().is_empty());
    }

    #[test]
    fn declared_steps_cover_the_instance_and_share_only_settings() {
        let mut value = oauth();
        value["setup"] = steps();
        let manifest = parse(value.clone()).unwrap();
        assert_eq!(manifest.setup().steps.len(), 4);
        assert_eq!(manifest.setup().name_from.as_deref(), Some("email"));
        assert_eq!(manifest.shared_settings(), ["clientId", "clientSecret"]);

        let toml_value: Manifest = toml::from_str(&toml::to_string(&manifest).unwrap()).unwrap();
        assert_eq!(toml_value.setup, manifest.setup);

        let invalid = [
            ("/setup/steps/1/settings", json!(["allowSend", "clientId"])),
            ("/setup/steps/1/settings", json!(["email"])),
            ("/setup/steps/1/settings", json!(["missing"])),
            ("/setup/steps/2/shared", json!(true)),
            ("/setup/steps/2/settings", json!(["allowSend"])),
            ("/setup/steps/3/kind", json!("people")),
            ("/setup/steps/1/id", json!("app")),
            ("/setup/steps/1/id", json!("Not a slug")),
            ("/setup/steps/1/title", json!("")),
            (
                "/setup/steps/0/checklist/0/url",
                json!("http://console.mail.example"),
            ),
            (
                "/setup/steps/0/checklist/0/url",
                json!("javascript:alert(1)"),
            ),
            (
                "/setup/steps/1/presets/1/values",
                json!({ "clientId": true }),
            ),
            ("/setup/steps/1/presets/1/id", json!("read")),
            ("/setup/nameFrom", json!("allowSend")),
            ("/setup/effort", json!("magic")),
        ];
        for (pointer, replacement) in invalid {
            let mut changed = value.clone();
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            changed.pointer_mut(parent).unwrap()[key] = replacement;
            assert!(parse(changed).is_err(), "{pointer}");
        }

        // Every part of the instance keeps a step: removing one is refused.
        for index in [0, 1, 2, 3] {
            let mut changed = value.clone();
            changed["setup"]["steps"]
                .as_array_mut()
                .unwrap()
                .remove(index);
            assert!(parse(changed).is_err(), "step {index}");
        }
        // Agents are granted on the last step only.
        let mut changed = value.clone();
        changed["setup"]["steps"].as_array_mut().unwrap().swap(2, 3);
        assert!(parse(changed).is_err());
        // A callback URL only exists for OAuth.
        let mut changed = value;
        changed["credential"]["oauth"] = Value::Null;
        assert!(parse(changed).is_err());
    }

    #[test]
    fn derived_steps_match_the_clients_fallback_corpus() {
        let corpus: Value =
            serde_json::from_str(include_str!("../../../tests/fixtures/plugin-setup.json"))
                .unwrap();
        for case in corpus["derivation"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let setup = parse(case["manifest"].clone())
                .unwrap_or_else(|error| panic!("{name}: {error}"))
                .setup();
            assert_eq!(json!(setup.effort), case["effort"], "{name}");
            assert_eq!(json!(setup.steps), case["steps"], "{name}");
        }
    }

    #[test]
    fn risk_only_qualifies_switches() {
        let mut value = oauth();
        value["settings"][0]["risk"] = json!("low");
        assert!(parse(value.clone()).is_err());
        value["settings"][0]["risk"] = Value::Null;
        value["settings"][3]["risk"] = json!("extreme");
        assert!(parse(value).is_err());
    }
}
