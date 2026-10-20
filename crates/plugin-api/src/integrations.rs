//! What a package adds to Cairn's own features: a sign-in the host runs for the
//! owner, credentials the host renders into granted agents' runs, projects to import,
//! conditions conversations can wait for and sources skills are imported from.
//! Scripts only describe and answer; the host keeps every credential.
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// OAuth 2.0 device authorization grant (RFC 8628), run by the host for the owner.
/// The token obtained becomes the instance's credential; scripts never see it.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceFlow {
    /// Where the host asks for a device and user code. HTTPS on a declared host.
    pub authorization_url: String,
    /// Where the host polls for the token. HTTPS on a declared host.
    pub token_url: String,
    /// The public client ID of the package publisher's OAuth application.
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
}

/// A file Cairn writes in the home of each run whose agent is granted an instance.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunFile {
    /// Relative to the run's home, e.g. `.config/tool/hosts.yml`.
    pub path: String,
    /// A template: `{{credential}}`, `{{secrets.KEY}}` and `{{settings.KEY}}`.
    pub content: String,
}

/// Git credentials for one HTTPS remote prefix, kept in a store file Cairn owns.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitCredential {
    /// `https://host` or `https://host/path`, on a declared host.
    pub url: String,
    pub username: String,
    pub password: String,
}

/// The commit author of runs, when the installation has no global Git identity.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitIdentity {
    pub name: String,
    pub email: String,
}

/// Rendered by the host for granted agents, when their run is prepared or resumed, and
/// for its own Git fetches of matching remotes. Removed when the run ends or the grant
/// is withdrawn.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunCredentials {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<RunFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub git: Vec<GitCredential>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_identity: Option<GitIdentity>,
}

/// Where the owner imports projects from: the `projects` handler lists and resolves them.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectSource {
    pub name: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// A condition granted agents can wait for with `wait_for`, observed by the `condition`
/// handler. Its type is `<plugin>_<name>`, like a tool name.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Condition {
    pub name: String,
    pub description: String,
    /// Properties besides `type` (and `projectId` with `project`).
    pub input_schema: Value,
    /// Pinned to one of the run's projects: Cairn checks it is authorized and passes it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub project: bool,
}

/// Where the owner imports skills from: the `skills` handler resolves a location and
/// lists its files, which the host downloads.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillSource {
    pub name: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// The location form, like instance settings: `string`, `url` or `text` fields.
    pub fields: Vec<Value>,
}

/// `.config/tool/hosts.yml`: visible segments of letters, digits, `.`, `_` and `-`.
pub fn home_path(path: &str) -> bool {
    let segments = path.split('/').collect::<Vec<_>>();
    // Provider state, skills and Cairn's own files stay under the host's control.
    let reserved = [
        ".codex",
        ".claude",
        ".claude.json",
        ".agents",
        ".gitconfig",
        ".cairn",
        ".config/cairn",
        ".ssh",
    ];
    path.len() <= 200
        && segments.len() <= 6
        && segments.iter().all(|segment| {
            !segment.is_empty()
                && *segment != "."
                && *segment != ".."
                && segment
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
        })
        && !reserved
            .iter()
            .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
}

/// Placeholders a template may use. Anything else between braces is refused.
fn template(value: &str, max: usize) -> bool {
    if value.len() > max || value.contains('\0') {
        return false;
    }
    let mut rest = value;
    while let Some(start) = rest.find("{{") {
        let Some(end) = rest[start..].find("}}") else {
            return false;
        };
        let name = &rest[start + 2..start + end];
        let key = |prefix: &str| {
            name.strip_prefix(prefix).is_some_and(|key| {
                !key.is_empty() && key.len() <= 40 && key.bytes().all(|c| c.is_ascii_alphanumeric())
            })
        };
        if name != "credential" && !key("secrets.") && !key("settings.") {
            return false;
        }
        rest = &rest[start + end + 2..];
    }
    true
}

/// Renders a template. Values with control characters are refused: they could break
/// out of the file's syntax.
pub fn render(
    template: &str,
    credential: Option<&str>,
    secrets: &serde_json::Map<String, Value>,
    settings: &Value,
) -> Option<String> {
    let mut output = String::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let end = rest[start..].find("}}")?;
        output.push_str(&rest[..start]);
        let name = &rest[start + 2..start + end];
        let value = if name == "credential" {
            credential.map(str::to_owned)
        } else if let Some(key) = name.strip_prefix("secrets.") {
            secrets.get(key).and_then(Value::as_str).map(str::to_owned)
        } else if let Some(key) = name.strip_prefix("settings.") {
            match &settings[key] {
                Value::String(value) => Some(value.clone()),
                Value::Number(value) => Some(value.to_string()),
                Value::Bool(value) => Some(value.to_string()),
                _ => None,
            }
        } else {
            None
        }?;
        if value.chars().any(char::is_control) {
            return None;
        }
        output.push_str(&value);
        rest = &rest[start + end + 2..];
    }
    output.push_str(rest);
    Some(output)
}

fn https_on(manifest: &super::Manifest, raw: &str) -> bool {
    url::Url::parse(raw).is_ok_and(|url| {
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url
                .host_str()
                .is_some_and(|host| manifest.allows_host(host))
    })
}

fn name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 40
        && value.starts_with(|c: char| c.is_ascii_lowercase())
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

impl super::Manifest {
    /// `wait_for` type of a condition: `<plugin>_<name>`, with `-` as `_`.
    pub fn condition_type(&self, condition: &Condition) -> String {
        format!("{}_{}", self.id.replace('-', "_"), condition.name)
    }

    /// The condition this package declares under a `wait_for` type, if any.
    pub fn condition_of_type(&self, kind: &str) -> Option<&Condition> {
        self.conditions
            .iter()
            .find(|condition| self.condition_type(condition) == kind)
    }

    pub fn project_source(&self, name: &str) -> Option<&ProjectSource> {
        self.project_sources
            .iter()
            .find(|source| source.name == name)
    }

    pub fn skill_source(&self, name: &str) -> Option<&SkillSource> {
        self.skill_sources.iter().find(|source| source.name == name)
    }

    /// Secret patterns compiled for masking run output.
    pub fn secret_regexes(&self) -> Vec<regex::Regex> {
        self.secret_patterns
            .iter()
            .filter_map(|source| secret_pattern(source))
            .collect()
    }

    pub(super) fn validate_integrations(&self) -> Result<(), &'static str> {
        let flow = self
            .credential
            .as_ref()
            .and_then(|credential| credential.device_flow.as_ref());
        if let Some(flow) = flow {
            let scopes_ok = flow.scopes.len() <= 20
                && flow.scopes.iter().all(|scope| {
                    !scope.is_empty()
                        && scope.len() <= 100
                        && scope.bytes().all(|c| c.is_ascii_graphic() && c != b'"')
                });
            let client_ok = !flow.client_id.is_empty()
                && flow.client_id.len() <= 200
                && flow
                    .client_id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'));
            if !https_on(self, &flow.authorization_url)
                || !https_on(self, &flow.token_url)
                || !client_ok
                || !scopes_ok
            {
                return Err("Plugin manifest has an invalid device sign-in.");
            }
        }
        if self.secret_patterns.len() > 8
            || self
                .secret_patterns
                .iter()
                .any(|source| secret_pattern(source).is_none())
        {
            return Err("Plugin manifest has invalid secret patterns.");
        }
        if let Some(run) = &self.run_credentials {
            if self.agent_access.is_none() {
                return Err("Plugin run credentials need agentAccess.");
            }
            let mut paths = Vec::new();
            for file in &run.files {
                if !home_path(&file.path)
                    || paths.contains(&file.path.as_str())
                    || !template(&file.content, 8192)
                {
                    return Err("Plugin manifest has an invalid run file.");
                }
                paths.push(&file.path);
            }
            let git_ok = run.git.iter().all(|git| {
                https_on(self, &git.url)
                    && !git.url.ends_with('/')
                    && template(&git.username, 200)
                    && template(&git.password, 200)
            });
            let identity_ok = run.git_identity.as_ref().is_none_or(|identity| {
                template(&identity.name, 200) && template(&identity.email, 200)
            });
            if run.files.len() > 8 || run.git.len() > 4 || !git_ok || !identity_ok {
                return Err("Plugin manifest has invalid run credentials.");
            }
        }
        let mut names = Vec::new();
        for source in &self.project_sources {
            if !name(&source.name)
                || names.contains(&source.name.as_str())
                || source.title.trim().is_empty()
                || !super::short(&source.title, 60)
                || !super::short(&source.description, 300)
            {
                return Err("Plugin manifest has an invalid project source.");
            }
            names.push(&source.name);
        }
        let mut names = Vec::new();
        for source in &self.skill_sources {
            let fields_ok = !source.fields.is_empty()
                && source.fields.len() <= 8
                && source
                    .fields
                    .iter()
                    .all(|field| matches!(field["type"].as_str(), Some("string" | "url" | "text")))
                && super::validate_settings_schema(&source.fields).is_ok();
            if !name(&source.name)
                || names.contains(&source.name.as_str())
                || source.title.trim().is_empty()
                || !super::short(&source.title, 60)
                || !super::short(&source.description, 300)
                || !fields_ok
            {
                return Err("Plugin manifest has an invalid skill source.");
            }
            names.push(&source.name);
        }
        if !self.conditions.is_empty() && self.agent_access.is_none() {
            return Err("Plugin conditions need agentAccess.");
        }
        if self.project_sources.len() > 4 || self.skill_sources.len() > 4 {
            return Err("Plugin manifest declares too many sources.");
        }
        if self.conditions.len() > 8 {
            return Err("Plugin manifest declares too many conditions.");
        }
        let mut names = Vec::new();
        for condition in &self.conditions {
            let properties = condition.input_schema["properties"].as_object();
            let reserved = properties.is_some_and(|properties| {
                properties.contains_key("type") || properties.contains_key("projectId")
            });
            let schema = serde_json::to_string(&condition.input_schema).unwrap_or_default();
            if !name(&condition.name)
                || names.contains(&condition.name.as_str())
                || condition.description.trim().is_empty()
                || !super::short(&condition.description, 2000)
                || condition.input_schema["type"] != "object"
                || reserved
                || schema.len() > 8192
            {
                return Err("Plugin manifest has an invalid condition.");
            }
            names.push(&condition.name);
        }
        Ok(())
    }
}

fn secret_pattern(source: &str) -> Option<regex::Regex> {
    if source.is_empty() || source.len() > 200 {
        return None;
    }
    let regex = regex::RegexBuilder::new(source)
        .size_limit(1 << 20)
        .build()
        .ok()?;
    // A pattern matching nothing specific would erase ordinary output.
    (!regex.is_match("") && !regex.is_match("a") && !regex.is_match("the")).then_some(regex)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Manifest;
    use serde_json::json;

    fn manifest(extra: Value) -> Result<Manifest, &'static str> {
        let mut value = json!({
            "id": "forge", "name": "Forge", "version": "1", "apiVersion": 2,
            "capabilities": ["http"], "hosts": ["forge.example", "api.forge.example"],
            "credential": { "label": "Token", "header": "authorization", "prefix": "Bearer " },
            "agentAccess": {},
        });
        for (key, item) in extra.as_object().unwrap() {
            if key == "deviceFlow" {
                value["credential"]["deviceFlow"] = item.clone();
            } else {
                value[key] = item.clone();
            }
        }
        let manifest: Manifest = serde_json::from_value(value).map_err(|_| "parse")?;
        manifest.validate()?;
        Ok(manifest)
    }

    #[test]
    fn device_sign_in_uses_declared_https_hosts() {
        let flow = json!({
            "authorizationUrl": "https://forge.example/login/device/code",
            "tokenUrl": "https://forge.example/login/oauth/access_token",
            "clientId": "Ov23liAbc",
            "scopes": ["repo", "read:user"],
        });
        manifest(json!({ "deviceFlow": flow.clone() })).unwrap();
        for (key, value) in [
            ("authorizationUrl", json!("http://forge.example/device")),
            ("tokenUrl", json!("https://elsewhere.example/token")),
            ("tokenUrl", json!("https://forge.example/token?x=1")),
            ("clientId", json!("bad id")),
            ("scopes", json!(["has space"])),
        ] {
            let mut invalid = flow.clone();
            invalid[key] = value;
            assert!(manifest(json!({ "deviceFlow": invalid })).is_err(), "{key}");
        }
    }

    #[test]
    fn run_credentials_stay_in_the_home_and_use_known_placeholders() {
        let run = json!({
            "files": [{ "path": ".config/forge/hosts.yml", "content": "token: {{credential}}\nuser: {{settings.login}}\n" }],
            "git": [{ "url": "https://forge.example", "username": "x-access-token", "password": "{{credential}}" }],
            "gitIdentity": { "name": "{{settings.name}}", "email": "{{settings.email}}" },
        });
        manifest(json!({ "runCredentials": run.clone() })).unwrap();
        for path in [
            "../escape",
            "/etc/passwd",
            ".codex/auth.json",
            ".gitconfig",
            ".config/cairn/x",
            "a//b",
            "a/./b",
        ] {
            let mut invalid = run.clone();
            invalid["files"][0]["path"] = json!(path);
            assert!(
                manifest(json!({ "runCredentials": invalid })).is_err(),
                "{path}"
            );
        }
        for content in ["{{env.HOME}}", "{{credential", "{{secrets.}}"] {
            let mut invalid = run.clone();
            invalid["files"][0]["content"] = json!(content);
            assert!(
                manifest(json!({ "runCredentials": invalid })).is_err(),
                "{content}"
            );
        }
        let mut invalid = run.clone();
        invalid["git"][0]["url"] = json!("https://undeclared.example");
        assert!(manifest(json!({ "runCredentials": invalid })).is_err());
        let mut without_access = run;
        without_access["files"] = json!([]);
        let value = json!({
            "id": "forge", "name": "Forge", "version": "1", "apiVersion": 2,
            "capabilities": [], "runCredentials": without_access,
        });
        let parsed: Manifest = serde_json::from_value(value).unwrap();
        assert!(parsed.validate().is_err());
    }

    #[test]
    fn templates_render_known_values_without_control_characters() {
        let secrets = json!({ "pat": "s3cret" });
        let settings = json!({ "login": "octo", "id": 42, "bad": "a\nb" });
        let secrets = secrets.as_object().unwrap();
        assert_eq!(
            render(
                "{{credential}}/{{secrets.pat}}/{{settings.login}}/{{settings.id}}",
                Some("tok"),
                secrets,
                &settings
            )
            .as_deref(),
            Some("tok/s3cret/octo/42")
        );
        assert!(render("{{settings.bad}}", None, secrets, &settings).is_none());
        assert!(render("{{credential}}", None, secrets, &settings).is_none());
        assert!(render("{{settings.missing}}", None, secrets, &settings).is_none());
    }

    #[test]
    fn conditions_and_sources_are_named_and_typed() {
        let condition = json!({
            "name": "pr_checks", "description": "Required checks of a pull request.",
            "inputSchema": { "type": "object", "properties": { "prNumber": { "type": "integer" } } },
            "project": true,
        });
        let parsed = manifest(json!({ "conditions": [condition.clone()] })).unwrap();
        assert_eq!(
            parsed.condition_type(&parsed.conditions[0]),
            "forge_pr_checks"
        );
        assert!(parsed.condition_of_type("forge_pr_checks").is_some());
        let mut reserved = condition.clone();
        reserved["inputSchema"]["properties"]["projectId"] = json!({ "type": "string" });
        assert!(manifest(json!({ "conditions": [reserved] })).is_err());
        let mut named = condition;
        named["name"] = json!("Bad-Name");
        assert!(manifest(json!({ "conditions": [named] })).is_err());

        manifest(
            json!({ "projectSources": [{ "name": "repositories", "title": "Repositories" }] }),
        )
        .unwrap();
        let skill = json!({
            "name": "repository", "title": "Repository",
            "fields": [{ "key": "repository", "title": "Repository", "type": "string", "required": true }],
        });
        manifest(json!({ "skillSources": [skill.clone()] })).unwrap();
        let mut secret = skill;
        secret["fields"][0]["type"] = json!("secret");
        assert!(manifest(json!({ "skillSources": [secret] })).is_err());
    }

    #[test]
    fn secret_patterns_must_be_specific() {
        manifest(json!({ "secretPatterns": ["\\bfrg_[A-Za-z0-9]{20,}\\b"] })).unwrap();
        for pattern in [".*", "a", "[", ""] {
            assert!(
                manifest(json!({ "secretPatterns": [pattern] })).is_err(),
                "{pattern}"
            );
        }
    }
}
