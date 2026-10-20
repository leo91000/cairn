//! `runCredentials`: what granted agents' runs receive from plugin instances, rendered
//! by the host from the manifest's templates. Scripts never see the values. Grants are
//! live (ADR 0051): every preparation and resumption renders them again, so a
//! withdrawn grant blanks the files it wrote.
use super::{
    connections::{self, Connection, credential_key, secret_values},
    registry::{self, Package},
};
use crate::{error::Result, service::Service, validation::text};
use cairn_plugin_api::render;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Git credentials for one HTTPS remote prefix.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct GitAccess {
    pub url: String,
    pub username: String,
    pub password: String,
}

impl GitAccess {
    /// Whether a remote URL is served under this prefix.
    pub fn covers(&self, remote: &str) -> bool {
        remote == self.url
            || remote
                .strip_prefix(&self.url)
                .is_some_and(|rest| rest.starts_with('/'))
    }

    /// A line of a Git credential-store file.
    pub fn store_line(&self) -> Option<String> {
        let mut url = url::Url::parse(&self.url).ok()?;
        url.set_username(&self.username).ok()?;
        url.set_password(Some(&self.password)).ok()?;
        Some(url.to_string().trim_end_matches('/').to_owned())
    }
}

/// What one run receives from the instances its agent is granted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunCredentials {
    /// Home-relative path and rendered content.
    pub files: Vec<(String, String)>,
    pub git: Vec<GitAccess>,
    /// Name and email for commits, when the installation has no global identity.
    pub identity: Option<(String, String)>,
    /// Credentials for the host's own fetches of the run's projects, by project ID.
    pub fetch: std::collections::BTreeMap<String, Vec<GitAccess>>,
    /// Credential and secret values of the granted instances, masked in run output.
    pub secrets: Vec<String>,
}

impl RunCredentials {
    pub fn fetch_for(&self, project: &Value) -> &[GitAccess] {
        self.fetch
            .get(text(project, "id"))
            .map_or(&[], Vec::as_slice)
    }
}

struct Rendered {
    files: Vec<(String, String)>,
    git: Vec<GitAccess>,
    identity: Option<(String, String)>,
    secrets: Vec<String>,
}

async fn values(s: &Service, connection: &Connection) -> Result<(Option<String>, Value)> {
    let credential = s
        .vault
        .get(&credential_key(&connection.id))
        .await?
        .map(|value| text(&value, "token").to_owned())
        .filter(|token| !token.is_empty());
    let secrets = secret_values(s, &connection.id).await?;
    Ok((credential, Value::Object(secrets)))
}

/// Renders one instance. A template naming a missing value leaves its item out.
async fn render_instance(
    s: &Service,
    package: &Package,
    connection: &Connection,
) -> Result<Rendered> {
    let Some(run) = &package.manifest.run_credentials else {
        return Ok(Rendered {
            files: Vec::new(),
            git: Vec::new(),
            identity: None,
            secrets: Vec::new(),
        });
    };
    let (credential, secrets) = values(s, connection).await?;
    let secrets = secrets.as_object().cloned().unwrap_or_default();
    let fill = |template: &str| {
        render(
            template,
            credential.as_deref(),
            &secrets,
            &connection.settings,
        )
    };
    let files = run
        .files
        .iter()
        .filter_map(|file| Some((file.path.clone(), fill(&file.content)?)))
        .collect();
    let git = run
        .git
        .iter()
        .filter_map(|git| {
            Some(GitAccess {
                url: git.url.clone(),
                username: fill(&git.username)?,
                password: fill(&git.password)?,
            })
        })
        .filter(|git| !git.password.is_empty())
        .collect();
    let identity = run.git_identity.as_ref().and_then(|identity| {
        let name = fill(&identity.name)?.trim().to_owned();
        let email = fill(&identity.email)?.trim().to_owned();
        (!name.is_empty() && !email.is_empty()).then_some((name, email))
    });
    let values = credential
        .iter()
        .cloned()
        .chain(
            secrets
                .values()
                .filter_map(Value::as_str)
                .map(str::to_owned),
        )
        .filter(|value| value.len() >= 8)
        .collect();
    Ok(Rendered {
        files,
        git,
        identity,
        secrets: values,
    })
}

/// Enabled instances of packages declaring run credentials, by package, sorted by name.
async fn instances(s: &Service) -> Result<Vec<(Package, Vec<Connection>)>> {
    let mut connections = s
        .store
        .list(connections::KIND)
        .await?
        .into_iter()
        .filter_map(|value| serde_json::from_value::<Connection>(value).ok())
        .filter(|connection| connection.enabled)
        .collect::<Vec<_>>();
    connections.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    let mut output = Vec::new();
    for package in registry::all_active(s).await? {
        if package.manifest.run_credentials.is_none() {
            continue;
        }
        let owned = connections
            .iter()
            .filter(|connection| connection.plugin_id == package.manifest.id)
            .cloned()
            .collect::<Vec<_>>();
        if !owned.is_empty() {
            output.push((package, owned));
        }
    }
    Ok(output)
}

/// Run credentials for `agent_id`: one granted instance per package (the first by
/// name), and the host's fetch credentials for each of `projects`.
pub async fn for_run(s: &Service, agent_id: &str, projects: &[Value]) -> Result<RunCredentials> {
    let mut credentials = RunCredentials::default();
    let mut fetch_order = Vec::new();
    for (package, owned) in instances(s).await? {
        let granted = owned.iter().find(|connection| connection.grants(agent_id));
        if let Some(connection) = granted {
            let rendered = render_instance(s, &package, connection).await?;
            // Packages never overwrite each other's files: the first one keeps a path.
            for (path, content) in rendered.files {
                if !credentials.files.iter().any(|(known, _)| *known == path) {
                    credentials.files.push((path, content));
                }
            }
            credentials.secrets.extend(rendered.secrets);
            credentials.git.extend(rendered.git.iter().cloned());
            if credentials.identity.is_none() {
                credentials.identity = rendered.identity;
            }
            fetch_order.push((connection.id.clone(), rendered.git));
        }
        for connection in owned
            .iter()
            .filter(|c| Some(c.id.as_str()) != granted.map(|g| g.id.as_str()))
        {
            let rendered = render_instance(s, &package, connection).await?;
            fetch_order.push((connection.id.clone(), rendered.git));
        }
    }
    for project in projects {
        let access = fetch_access(project, &fetch_order);
        if !access.is_empty() {
            credentials
                .fetch
                .insert(text(project, "id").to_owned(), access);
        }
    }
    Ok(credentials)
}

/// The project's own instance first, then the agent's, then any other enabled one.
fn fetch_access(project: &Value, order: &[(String, Vec<GitAccess>)]) -> Vec<GitAccess> {
    let origin = text(project, "origin");
    let https = crate::project_git::https_remote(origin);
    let source = text(&project["source"], "instance");
    let mut output = Vec::new();
    let preferred = order.iter().filter(|(id, _)| id == source);
    let others = order.iter().filter(|(id, _)| id != source);
    for (_, access) in preferred.chain(others) {
        for git in access {
            let serves = origin.is_empty() || https.as_deref().is_some_and(|url| git.covers(url));
            if serves && !output.contains(git) {
                output.push(git.clone());
            }
        }
    }
    output
}

/// Credentials for the host's own fetch of one project, outside any run: imports
/// and project seeds opened by an agent.
pub async fn fetch(s: &Service, agent_id: Option<&str>, project: &Value) -> Result<Vec<GitAccess>> {
    let credentials = for_run(s, agent_id.unwrap_or(""), std::slice::from_ref(project)).await?;
    Ok(credentials.fetch_for(project).to_vec())
}

/// Credentials of one instance for a remote, e.g. to clone a project it resolved.
pub async fn for_instance(
    s: &Service,
    package: &Package,
    connection: &Connection,
) -> Result<Vec<GitAccess>> {
    Ok(render_instance(s, package, connection).await?.git)
}

/// Whether the agent may hold another instance of this package: runs receive the
/// credentials of one instance per package.
pub fn one_per_package(manifest: &cairn_plugin_api::Manifest) -> bool {
    manifest
        .run_credentials
        .as_ref()
        .is_some_and(|run| !run.files.is_empty() || !run.git.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn access(url: &str, password: &str) -> GitAccess {
        GitAccess {
            url: url.into(),
            username: "x-access-token".into(),
            password: password.into(),
        }
    }

    #[test]
    fn prefixes_cover_whole_path_segments() {
        let git = access("https://forge.example", "t");
        assert!(git.covers("https://forge.example/team/repo.git"));
        assert!(git.covers("https://forge.example"));
        assert!(!git.covers("https://forge.example.evil/team/repo.git"));
        assert!(!git.covers("git@forge.example:team/repo.git"));
        let scoped = access("https://forge.example/team", "t");
        assert!(scoped.covers("https://forge.example/team/repo.git"));
        assert!(!scoped.covers("https://forge.example/teammate/repo.git"));
    }

    #[test]
    fn store_lines_escape_credentials() {
        let line = access("https://forge.example", "p@ss:w/rd")
            .store_line()
            .unwrap();
        assert_eq!(line, "https://x-access-token:p%40ss%3Aw%2Frd@forge.example");
    }

    #[test]
    fn the_project_instance_comes_first() {
        let order = vec![
            (
                "agent".to_owned(),
                vec![access("https://forge.example", "agent")],
            ),
            (
                "owner".to_owned(),
                vec![access("https://forge.example", "owner")],
            ),
            (
                "other".to_owned(),
                vec![access("https://other.example", "other")],
            ),
        ];
        let project = json!({
            "id": "p", "origin": "forge.example:team/repo.git",
            "source": { "instance": "owner" },
        });
        let selected = fetch_access(&project, &order);
        assert_eq!(
            selected
                .iter()
                .map(|g| g.password.as_str())
                .collect::<Vec<_>>(),
            ["owner", "agent"]
        );
        let local = json!({ "id": "p", "origin": "" });
        assert_eq!(fetch_access(&local, &order).len(), 3);
    }
}
