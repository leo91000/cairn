//! Owner-only repository browsing and imports using the shared GitHub connection.
use crate::{
    error::{Error, Result},
    process::{bounded_output, codex_environment, command},
    service::Service,
    validation::{parse, text},
};
use serde_json::{Value, json};
use std::time::Duration;

async fn github(s: &Service, endpoint: &str) -> Result<Value> {
    let args = ["api", "--hostname", "github.com", endpoint].map(str::to_owned);
    let output = bounded_output(
        command(
            &s.config.gh_bin,
            &args,
            &codex_environment(&s.config, &s.config.home.join(".codex")),
            None,
        ),
        Duration::from_secs(20),
        4_000_000,
    )
    .await
    .map_err(|_| {
        Error::new(
            502,
            "GitHub is unavailable. Check the GitHub connection in Connections and retry.",
        )
    })?;
    if !output.success {
        return Err(Error::new(
            502,
            "Could not access GitHub. Check the GitHub connection and repository \
                permissions in Connections, then retry.",
        ));
    }
    serde_json::from_str(&output.stdout).map_err(|_| Error::new(502, "Invalid GitHub response."))
}

fn repository(raw: &str) -> Result<&str> {
    let parts = raw.split('/').collect::<Vec<_>>();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || part.len() > 100
                || *part == "."
                || *part == ".."
                || !part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        })
    {
        return Err(Error::bad("Choose a GitHub repository using owner/name."));
    }
    Ok(raw)
}

fn origin(repo: &str) -> String {
    format!("https://github.com/{repo}.git")
}

fn matches_origin(raw: &str, repo: &str) -> bool {
    let path = raw
        .strip_prefix("https://github.com/")
        .or_else(|| raw.strip_prefix("http://github.com/"))
        .or_else(|| raw.strip_prefix("ssh://github.com/"))
        .or_else(|| raw.strip_prefix("ssh://git@github.com/"))
        .or_else(|| raw.strip_prefix("git@github.com:"))
        .or_else(|| raw.strip_prefix("github.com:"));
    path.is_some_and(|path| path.trim_end_matches(".git").eq_ignore_ascii_case(repo))
}

async fn existing(s: &Service, repo: &str) -> Result<Option<Value>> {
    Ok(s.store
        .list("projects")
        .await?
        .into_iter()
        .find(|p| matches_origin(text(p, "origin"), repo)))
}

pub async fn list(s: &Service, page: i64) -> Result<Value> {
    let values = github(s, &format!("user/repos?per_page=100&page={page}&sort=pushed&direction=desc&affiliation=owner,collaborator,organization_member")).await?;
    let values = values
        .as_array()
        .ok_or_else(|| Error::new(502, "Invalid GitHub repository list."))?;
    let projects = s.store.list("projects").await?;
    let repos = values
        .iter()
        .filter(|r| repository(text(r, "full_name")).is_ok())
        .map(|r| {
            let full_name = text(r, "full_name");
            json!({
                "fullName": full_name,
                "name": text(r, "name"),
                "description": text(r, "description"),
                "defaultBranch": text(r, "default_branch"),
                "private": r["private"] == true,
                "archived": r["archived"] == true,
                "fork": r["fork"] == true,
                "owner": text(&r["owner"], "login"),
                "language": text(r, "language"),
                "stars": r["stargazers_count"].as_u64().unwrap_or(0),
                "pushedAt": text(r, "pushed_at"),
                "imported": projects
                    .iter()
                    .any(|p| matches_origin(text(p, "origin"), full_name))
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "repositories": repos,
        "nextPage": if values.len() == 100 {
            Some(page + 1)
        } else {
            None
        }
    }))
}

pub async fn import(s: &Service, input: Value) -> Result<Value> {
    let repo = repository(text(&input, "repository"))?;
    // Repeat submissions return the registered project, including after a client timeout.
    if let Some(project) = existing(s, repo).await? {
        return Ok(project);
    }
    let metadata = github(s, &format!("repos/{repo}")).await?;
    let branch = input
        .get("baseBranch")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| text(&metadata, "default_branch"));
    let root = s
        .config
        .workspace_roots
        .first()
        .ok_or_else(|| Error::bad("No workspace root is configured."))?;
    let root = crate::skills::workspace(root, &s.config.workspace_roots).await?;
    let mut project = parse(
        "project",
        json!({
            "name": input.get("name").unwrap_or(&metadata["name"]),
            "description": input
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_else(|| text(&metadata, "description")),
            "path": root,
            "baseBranch": branch,
            "sourceMode": "remote"
        }),
    )?;
    let directory = tempfile::Builder::new()
        .prefix("github-")
        .tempdir_in(root)?;
    let path = directory.path().join("repository");
    crate::project_git::import(&s.config, &origin(repo), &path, branch).await?;
    project["path"] = path.to_string_lossy().into_owned().into();
    // The API serializes imports through registration to avoid concurrent duplicates.
    let saved = s.project(project, None).await?;
    let _ = directory.keep();
    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_existing_ssh_and_https_origins() {
        for raw in [
            "https://github.com/Team/Repo.git",
            "github.com:Team/Repo.git",
            "ssh://github.com/Team/Repo.git",
        ] {
            assert!(matches_origin(raw, "team/repo"));
        }
        assert!(!matches_origin(
            "https://other.example/team/repo.git",
            "team/repo"
        ));
    }

    #[test]
    fn accepts_only_repository_coordinates() {
        for invalid in [
            "",
            "../repo",
            "owner/..",
            "owner/repo/extra",
            "https://github.com/a/b",
            "a/b?x=y",
            "a/b%2fc",
            "a/b\n",
        ] {
            assert!(repository(invalid).is_err(), "{invalid}");
        }
        assert!(repository("octocat/hello-world.js").is_ok());
    }
}
