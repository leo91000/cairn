//! `projectSources`: projects the owner imports through a plugin instance. The
//! package's `projects` handler lists and resolves them; Cairn clones the resolved
//! remote with the instance's run credentials and registers the project.
use super::{
    connections::{self, Connection},
    registry::{self, Package},
    run_credentials,
    scope::{self, Scope},
};
use crate::skills::lifecycle::Authority;
use crate::{
    error::{Error, Result},
    service::Service,
    validation::uuid,
};
use cairn_contracts::{
    ImportProjectInput, ListSourceProjectsInput, Project, ProjectId, ProjectSettings, ProjectSource,
};
use serde_json::{Value, json};

const MAX_ITEMS: usize = 200;

fn plain(value: &Value, max: usize) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| value.chars().count() <= max && !value.chars().any(char::is_control))
        .map(str::to_owned)
}

/// A remote compared across spellings: HTTPS form, without `.git` or case.
fn same_remote(a: &str, b: &str) -> bool {
    let key = |raw: &str| {
        crate::project_git::https_remote(raw).map(|url| {
            url.trim_end_matches('/')
                .trim_end_matches(".git")
                .to_ascii_lowercase()
        })
    };
    matches!((key(a), key(b)), (Some(a), Some(b)) if a == b)
}

async fn instance(
    s: &Service,
    package: &Package,
    source: &str,
    instance: &str,
) -> Result<Connection> {
    if package.manifest.project_source(source).is_none() {
        return Err(Error::not_found("This plugin has no such project source."));
    }
    let connection = connections::get(s, &package.manifest.id, instance).await?;
    if !connection.enabled {
        return Err(Error::bad("Enable this instance before importing from it."));
    }
    Ok(connection)
}

/// One item as the owner sees it, or `None` when the package returned nonsense.
fn item(raw: &Value, projects: &[Project], plugin: &str) -> Option<Value> {
    let id = plain(&raw["id"], 200).filter(|id| !id.is_empty())?;
    let name = plain(&raw["name"], 100).filter(|name| !name.is_empty())?;
    let origin = plain(&raw["origin"], 500).unwrap_or_default();
    let badges = raw["badges"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|badge| plain(badge, 40).filter(|badge| !badge.is_empty()))
        .take(4)
        .collect::<Vec<_>>();
    let imported = projects.iter().any(|project| {
        (project
            .source
            .as_ref()
            .is_some_and(|source| source.plugin_id == plugin && source.id == id))
            || (!origin.is_empty() && same_remote(&project.origin, &origin))
    });
    let url = plain(&raw["url"], 500)
        .filter(|raw| url::Url::parse(raw).is_ok_and(|url| url.scheme() == "https"));
    // The name a new project takes, when it differs from what identifies the item.
    let project_name = plain(&raw["projectName"], 100)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| name.clone());
    Some(json!({
        "id": id,
        "name": name,
        "projectName": project_name,
        "description": plain(&raw["description"], 500).unwrap_or_default(),
        "defaultBranch": plain(&raw["defaultBranch"], 200).unwrap_or_default(),
        "badges": badges,
        "updatedAt": plain(&raw["updatedAt"], 40),
        "url": url,
        "imported": imported,
    }))
}

/// `GET /api/plugins/{id}/projects?source=&instance=&page=&query=`.
pub(crate) async fn list(
    s: &Service,
    input: &ListSourceProjectsInput,
    authority: Authority,
) -> Result<Value> {
    let plugin_id = input.plugin_id.as_str();
    let package = registry::active(s, plugin_id).await?;
    let source = input.source.as_str();
    let connection = instance(s, &package, source, &input.instance).await?;
    let authority = authority.instance(s, &connection, &package).await?;
    let page = input.page;
    let search = &input.query;
    if !(1..=10000).contains(&page)
        || search.chars().count() > 200
        || search.chars().any(char::is_control)
    {
        return Err(Error::bad(
            "Use a page from 1 to 10000 and a search of at most 200 characters.",
        ));
    }
    let scope = Scope::Instance(Box::new(connection));
    let payload = json!({
        "op": "list",
        "source": source,
        "page": page,
        "query": search,
        "instance": scope.instances()[0],
    });
    let result = scope::invoke(s, &package, "projects", payload, &scope).await?;
    authority.recheck(s).await?;
    let projects = s.store.projects().await?;
    let raw = result["items"]
        .as_array()
        .filter(|items| items.len() <= MAX_ITEMS)
        .ok_or_else(|| Error::bad_gateway("The plugin returned an invalid project list."))?;
    let items = raw
        .iter()
        .filter_map(|raw| item(raw, &projects, plugin_id))
        .collect::<Vec<_>>();
    let next_page = result["nextPage"]
        .as_i64()
        .filter(|next| *next > i64::from(page) && *next <= 10_000);
    Ok(json!({ "items": items, "nextPage": next_page }))
}

/// `POST /api/projects/import`:
/// `{ pluginId, source, instance, id, name?, description?, baseBranch? }`.
pub(crate) async fn import(
    s: &Service,
    command: &ImportProjectInput,
    authority: Authority,
) -> Result<Value> {
    let _import = s
        .projects
        .import
        .try_lock()
        .map_err(|_| Error::conflict("A project import is already running. Retry shortly."))?;
    let plugin_id = command.plugin_id.as_str();
    let package = registry::active(s, plugin_id).await?;
    let source = command.source.as_str();
    let instance_id = command.instance.as_str();
    uuid(instance_id)?;
    let connection = instance(s, &package, source, instance_id).await?;
    let authority = authority.instance(s, &connection, &package).await?;
    let item_id = plain(&Value::String(command.id.clone()), 200)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Error::bad("Choose a project to import."))?;
    let projects = s.store.projects().await?;
    // Repeat submissions return the registered project, including after a timeout.
    if let Some(project) = projects.iter().find(|project| {
        project
            .source
            .as_ref()
            .is_some_and(|source| source.plugin_id == plugin_id && source.id == item_id)
    }) {
        return Ok(serde_json::to_value(project)?);
    }
    let scope = Scope::Instance(Box::new(connection.clone()));
    let payload = json!({
        "op": "resolve",
        "source": source,
        "id": item_id,
        "instance": scope.instances()[0],
    });
    let resolved = scope::invoke(s, &package, "projects", payload, &scope).await?;
    let origin = plain(&resolved["origin"], 500).unwrap_or_default();
    let served = url::Url::parse(&origin).is_ok_and(|url| {
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url
                .host_str()
                .is_some_and(|host| package.manifest.allows_host(host))
    });
    if !served {
        return Err(Error::bad_gateway(
            "The plugin resolved a remote outside its declared hosts.",
        ));
    }
    if let Some(project) = projects
        .iter()
        .find(|project| same_remote(&project.origin, &origin))
    {
        return Ok(serde_json::to_value(project)?);
    }
    let branch = command
        .base_branch
        .as_deref()
        .filter(|branch| !branch.is_empty())
        .map(str::to_owned)
        .or_else(|| plain(&resolved["defaultBranch"], 200))
        .filter(|branch| !branch.is_empty())
        .unwrap_or_else(|| "main".into());
    let root = s
        .config
        .workspace_roots
        .first()
        .ok_or_else(|| Error::bad("No workspace root is configured."))?;
    let root = crate::skills::workspace(root, &s.config.workspace_roots).await?;
    let name = command
        .name
        .as_ref()
        .filter(|name| !name.trim().is_empty())
        .cloned()
        .or_else(|| plain(&resolved["name"], 100))
        .unwrap_or_else(|| item_id.clone());
    let description = command
        .description
        .clone()
        .or_else(|| plain(&resolved["description"], 500))
        .unwrap_or_default();
    let mut settings: ProjectSettings = serde_json::from_value(json!({
        "name": name, "description": description, "path": root,
        "baseBranch": branch, "sourceMode": "remote",
    }))
    .map_err(|error| Error::bad(error.to_string()))?;
    settings.validate().map_err(Error::bad)?;
    let directory = tempfile::Builder::new()
        .prefix(&format!("{plugin_id}-"))
        .tempdir_in(&root)?;
    let path = directory.path().join("repository");
    authority.recheck(s).await?;
    let credentials = run_credentials::for_instance(s, &package, &connection).await?;
    crate::project_git::import(&s.config, &credentials, &origin, &path, &branch).await?;
    settings.path = path.to_string_lossy().into_owned();
    let source = ProjectSource {
        plugin_id: plugin_id.into(),
        source: source.into(),
        instance: connection.id.clone(),
        id: item_id,
    };
    authority.recheck(s).await?;
    let project = Project {
        id: ProjectId(uuid::Uuid::new_v4()),
        settings,
        origin: crate::service::normalize_origin(&origin),
        source: Some(source),
        revision: crate::config::id(),
        created_at: crate::config::now(),
    };
    // The queued writer owns the clone even if the HTTP/tool future is dropped.
    let saved = s
        .store
        .transaction(move |db| {
            authority.authorize(db)?;
            let saved = db.save_project(&project, None)?;
            let _ = directory.keep();
            Ok(saved)
        })
        .await?;
    Ok(serde_json::to_value(saved)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remotes_compare_across_spellings() {
        assert!(same_remote(
            "git@forge.example:Team/Repo.git",
            "https://forge.example/team/repo"
        ));
        assert!(same_remote(
            "forge.example:team/repo.git",
            "https://forge.example/team/repo.git"
        ));
        assert!(!same_remote(
            "https://forge.example/team/repo.git",
            "https://forge.example/team/other.git"
        ));
        assert!(!same_remote("", ""));
    }

    #[test]
    fn items_are_bounded_and_marked_imported() {
        let project = |origin: &str, source: Option<ProjectSource>| Project {
            id: cairn_contracts::ProjectId(uuid::Uuid::new_v4()),
            settings: ProjectSettings {
                name: "Fixture".into(),
                path: "/fixture".into(),
                description: "".into(),
                base_branch: "main".into(),
                source_mode: "remote".into(),
            },
            origin: origin.into(),
            source,
            revision: "fixture".into(),
            created_at: 0,
        };
        let projects = vec![
            project("forge.example:team/old.git", None),
            project(
                "",
                Some(ProjectSource {
                    plugin_id: "forge".into(),
                    source: "repositories".into(),
                    instance: uuid::Uuid::new_v4().to_string(),
                    id: "team/new".into(),
                }),
            ),
        ];
        let old = item(
            &json!({ "id": "team/old", "name": "old", "origin": "https://forge.example/team/old.git" }),
            &projects,
            "forge",
        )
        .unwrap();
        assert_eq!(old["imported"], true);
        let new = item(
            &json!({ "id": "team/new", "name": "new" }),
            &projects,
            "forge",
        )
        .unwrap();
        assert_eq!(new["imported"], true);
        let other = item(&json!({ "id": "team/x", "name": "x" }), &projects, "forge").unwrap();
        assert_eq!(other["imported"], false);
        assert!(item(&json!({ "id": "", "name": "x" }), &projects, "forge").is_none());
        assert!(item(&json!({ "id": "a\nb", "name": "x" }), &projects, "forge").is_none());
        let unsafe_link = item(
            &json!({ "id": "a", "name": "a", "url": "javascript:alert(1)" }),
            &projects,
            "forge",
        )
        .unwrap();
        assert!(unsafe_link["url"].is_null());
    }
}
