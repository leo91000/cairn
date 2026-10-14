//! Skills imported from GitHub through the installation's GitHub connection,
//! and updates from their source. See docs/SKILLS.md.
use super::{Choice, bundle::Change, check_sizes, choose, diff, imported_name, visible};
use crate::{
    config::now,
    error::{Error, Result},
    github_projects::repository,
    process::{bounded_output, codex_environment, command},
    service::Service,
    validation::text,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{StreamExt, TryStreamExt};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, time::Duration};

const RESPONSE_BYTES: usize = 4_000_000;
// A recursive tree of a large repository, or a 4 MiB file in base64.
const LARGE_RESPONSE_BYTES: usize = 16_000_000;
const PARALLEL_DOWNLOADS: usize = 6;
// Files whose upstream changes include line diffs; others list only their status.
const DIFF_FILES: usize = 60;
const DIFF_BYTES: u64 = 1_000_000;

fn key(scope: &str, skill: &str) -> String {
    format!("skill-source:{scope}/{skill}")
}

async fn github(s: &Service, endpoint: &str, limit: usize) -> Result<Value> {
    let args = ["api", "--hostname", "github.com", endpoint].map(str::to_owned);
    let output = bounded_output(
        command(
            &s.config.gh_bin,
            &args,
            &codex_environment(&s.config, &s.config.home.join(".codex")),
            None,
        ),
        Duration::from_secs(30),
        limit,
    )
    .await
    .map_err(|_| {
        Error::bad_gateway(
            "GitHub is unavailable. Check the GitHub connection in Connections and retry.",
        )
    })?;
    if !output.success {
        if output.stderr.contains("HTTP 404") || output.stdout.contains("\"Not Found\"") {
            return Err(Error::not_found(
                "GitHub could not find this repository, ref or folder.",
            ));
        }
        return Err(Error::bad_gateway(
            "Could not access GitHub. Check the GitHub connection and repository permissions in Connections, then retry.",
        ));
    }
    serde_json::from_str(&output.stdout).map_err(|_| Error::bad_gateway("Invalid GitHub response."))
}

fn reference(value: &str) -> Result<&str> {
    let valid = !value.is_empty()
        && value.len() <= 200
        && !value.contains("..")
        && !value.starts_with('/')
        && !value.ends_with('/')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b));
    if !valid {
        return Err(Error::bad("Choose a branch, tag or commit."));
    }
    Ok(value)
}

fn folder(value: &str) -> Result<&str> {
    let value = value.trim().trim_matches('/');
    if !value.is_empty() {
        super::relative(value)?;
        if !visible(value) {
            return Err(Error::bad("The skill folder cannot be hidden."));
        }
    }
    Ok(value)
}

fn sha(value: &str) -> Result<&str> {
    if value.len() != 40 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::bad("Invalid commit."));
    }
    Ok(value)
}

fn join(parent: &str, child: &str) -> String {
    match (parent.is_empty(), child.is_empty()) {
        (true, _) => child.to_owned(),
        (_, true) => parent.to_owned(),
        _ => format!("{parent}/{child}"),
    }
}

struct Commit {
    sha: String,
    tree: String,
    message: String,
    date: String,
}

async fn commit(s: &Service, repo: &str, reference: &str) -> Result<Commit> {
    let encoded = reference.replace('/', "%2F");
    let value = github(
        s,
        &format!("repos/{repo}/commits/{encoded}"),
        RESPONSE_BYTES,
    )
    .await?;
    let commit = Commit {
        sha: text(&value, "sha").to_owned(),
        tree: value["commit"]["tree"]["sha"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        message: text(&value["commit"], "message")
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned(),
        date: value["commit"]["committer"]["date"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    };
    sha(&commit.sha)?;
    sha(&commit.tree)?;
    Ok(commit)
}

/// A blob of a repository folder, with a path relative to that folder.
struct File {
    path: String,
    sha: String,
    size: u64,
    executable: bool,
    link: bool,
}

/// Visible files below `folder` in the tree `root`, and whether GitHub
/// truncated the listing.
async fn files(s: &Service, repo: &str, root: &str, folder: &str) -> Result<(Vec<File>, bool)> {
    let mut tree = root.to_owned();
    for segment in folder.split('/').filter(|segment| !segment.is_empty()) {
        let listing = github(
            s,
            &format!("repos/{repo}/git/trees/{tree}"),
            LARGE_RESPONSE_BYTES,
        )
        .await?;
        tree = listing["tree"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|entry| entry["path"] == segment && entry["type"] == "tree")
            .and_then(|entry| entry["sha"].as_str())
            .ok_or_else(|| Error::bad(format!("{folder} is not a folder in {repo}.")))?
            .to_owned();
    }
    let listing = github(
        s,
        &format!("repos/{repo}/git/trees/{tree}?recursive=1"),
        LARGE_RESPONSE_BYTES,
    )
    .await?;
    let files = listing["tree"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| entry["type"] == "blob" && visible(text(entry, "path")))
        .map(|entry| File {
            path: text(entry, "path").to_owned(),
            sha: text(entry, "sha").to_owned(),
            size: entry["size"].as_u64().unwrap_or_default(),
            executable: entry["mode"] == "100755",
            link: entry["mode"] == "120000",
        })
        .collect();
    Ok((files, listing["truncated"] == true))
}

async fn blob(s: &Service, repo: &str, blob: &str) -> Result<Vec<u8>> {
    let value = github(
        s,
        &format!("repos/{repo}/git/blobs/{}", sha(blob)?),
        LARGE_RESPONSE_BYTES,
    )
    .await?;
    let content = text(&value, "content")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>();
    STANDARD
        .decode(content)
        .map_err(|_| Error::bad_gateway("Invalid GitHub response."))
}

/// Downloads the files of a skill folder as writes.
async fn download(s: &Service, repo: &str, files: Vec<File>) -> Result<Vec<Change>> {
    if let Some(link) = files.iter().find(|file| file.link) {
        return Err(Error::bad(format!(
            "Skills must not contain symbolic links: {}",
            link.path
        )));
    }
    check_sizes(files.iter().map(|file| file.size))?;
    futures_util::stream::iter(files.into_iter().map(|file| async move {
        Ok::<_, Error>(Change::Write {
            bytes: blob(s, repo, &file.sha).await?,
            path: file.path,
            executable: Some(file.executable),
        })
    }))
    .buffer_unordered(PARALLEL_DOWNLOADS)
    .try_collect()
    .await
}

/// The source as clients see it, with `modified` against the current files.
fn public(record: &Value, revision: &str) -> Value {
    json!({
        "type": "github",
        "repository": record["repository"],
        "path": record["path"],
        "ref": record["ref"],
        "commit": record["commit"],
        "importedAt": record["importedAt"],
        "modified": record["revision"] != revision,
    })
}

async fn record(s: &Service, scope: &str, skill: &str) -> Result<Value> {
    s.store
        .kv(&key(scope, skill))
        .await?
        .ok_or_else(|| Error::not_found("This skill has no GitHub source."))
}

async fn remember(
    s: &Service,
    scope: &str,
    skill: &str,
    mut record: Value,
    bundle: &mut Value,
) -> Result<()> {
    record["revision"] = bundle["revision"].clone();
    s.store
        .set(&key(scope, skill), record.clone(), None)
        .await?;
    bundle["source"] = public(&record, text(bundle, "revision"));
    Ok(())
}

/// Imports a new skill from `repository` at `ref` (default branch when empty).
pub async fn import(
    s: &Service,
    scope: &str,
    input: &Value,
    project: Option<&Path>,
) -> Result<Value> {
    let repo = repository(text(input, "repository").trim())?;
    let reference = match input["ref"].as_str().map(str::trim) {
        Some(value) if !value.is_empty() => reference(value)?.to_owned(),
        _ => {
            let details = github(s, &format!("repos/{repo}"), RESPONSE_BYTES).await?;
            reference(text(&details, "default_branch"))?.to_owned()
        }
    };
    let requested = folder(input["path"].as_str().unwrap_or_default())?;
    let commit = commit(s, repo, &reference).await?;
    let (files, truncated) = files(s, repo, &commit.tree, requested).await?;
    if truncated {
        return Err(Error::bad(
            "This folder is too large to search. Enter the skill folder path.",
        ));
    }

    let chosen = match choose(files.iter().map(|file| file.path.as_str()), None, repo)? {
        Choice::Candidates(candidates) => {
            let candidates = candidates
                .iter()
                .map(|candidate| join(requested, candidate))
                .collect::<Vec<_>>();
            return Ok(json!({ "imported": null, "candidates": candidates }));
        }
        Choice::Folder(folder) => folder,
    };
    let prefix = join(&chosen, "");
    let selected = files
        .into_iter()
        .filter_map(|mut file| {
            file.path = if prefix.is_empty() {
                file.path
            } else {
                file.path.strip_prefix(&format!("{prefix}/"))?.to_owned()
            };
            Some(file)
        })
        .collect();
    let changes = download(s, repo, selected).await?;
    let skill = imported_name(&changes)?;
    let saved = s.skills.apply(&skill, None, &changes, project).await?;
    let mut bundle = super::bundle_response(scope, &skill, &saved);
    let record = json!({
        "type": "github",
        "repository": repo,
        "path": join(requested, &chosen),
        "ref": reference,
        "commit": commit.sha,
        "importedAt": now(),
    });
    remember(s, scope, &skill, record, &mut bundle).await?;
    s.store
        .audit(
            "skill.imported",
            json!({ "scope": scope, "name": skill, "repository": repo }),
        )
        .await?;
    Ok(json!({ "imported": bundle, "candidates": [] }))
}

/// Adds `source` to listed skills.
pub async fn attach(s: &Service, skills: &mut [Value]) -> Result<()> {
    let records = s
        .store
        .keys_as::<Value>("skill-source:")
        .await?
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    for skill in skills {
        let record = records.get(&key(text(skill, "scope"), text(skill, "name")));
        skill["source"] = match record {
            Some(record) => {
                let directory = Path::new(text(skill, "path"))
                    .parent()
                    .unwrap_or(Path::new(""));
                match super::bundle::read_all(directory).await {
                    Ok((revision, _)) => public(record, &revision),
                    Err(_) => Value::Null,
                }
            }
            None => Value::Null,
        };
    }
    Ok(())
}

pub async fn source(s: &Service, scope: &str, skill: &str, bundle: &mut Value) -> Result<()> {
    if let Some(record) = s.store.kv(&key(scope, skill)).await? {
        bundle["source"] = public(&record, text(bundle, "revision"));
    }
    Ok(())
}

pub async fn forget(s: &Service, scope: &str, skill: &str) -> Result<()> {
    s.store.delete(&key(scope, skill)).await
}

async fn content(s: &Service, repo: &str, file: Option<&File>) -> Result<Option<Vec<u8>>> {
    match file {
        Some(file) if file.size <= DIFF_BYTES && !file.link => {
            Ok(Some(blob(s, repo, &file.sha).await?))
        }
        Some(_) => Ok(None),
        None => Ok(Some(Vec::new())),
    }
}

/// Compares the recorded commit with the latest commit of the source ref.
pub async fn status(
    s: &Service,
    scope: &str,
    skill: &str,
    project: Option<&Path>,
) -> Result<Value> {
    let record = record(s, scope, skill).await?;
    let bundle = s.skills.bundle(scope, skill, project).await?;
    let repo = repository(text(&record, "repository"))?;
    let folder = text(&record, "path");
    let latest = commit(s, repo, reference(text(&record, "ref"))?).await?;
    let mut result = json!({
        "source": public(&record, text(&bundle, "revision")),
        "latest": { "commit": latest.sha, "message": latest.message, "date": latest.date },
        "current": latest.sha == text(&record, "commit"),
        "changes": [],
    });
    if result["current"] == true {
        return Ok(result);
    }

    let previous = commit(s, repo, sha(text(&record, "commit"))?).await?;
    let (old, _) = files(s, repo, &previous.tree, folder).await?;
    let (new, _) = files(s, repo, &latest.tree, folder)
        .await
        .map_err(|error| {
            if error.status == 400 {
                Error::bad("The skill folder no longer exists in the latest commit.")
            } else {
                error
            }
        })?;
    let old = old
        .into_iter()
        .map(|file| (file.path.clone(), file))
        .collect::<BTreeMap<_, _>>();
    let new = new
        .into_iter()
        .map(|file| (file.path.clone(), file))
        .collect::<BTreeMap<_, _>>();

    let mut paths = old.keys().chain(new.keys()).cloned().collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    let mut changes = Vec::new();
    for path in paths {
        let (before, after) = (old.get(&path), new.get(&path));
        let status = match (before, after) {
            (None, Some(_)) => "added",
            (Some(_), None) => "removed",
            (Some(a), Some(b)) if a.sha != b.sha || a.executable != b.executable => "modified",
            _ => continue,
        };
        let mut change = json!({ "path": path, "status": status, "binary": false });
        if changes.len() < DIFF_FILES && before.is_none_or(|a| after.is_none_or(|b| a.sha != b.sha))
        {
            let (a, b) = (
                content(s, repo, before).await?,
                content(s, repo, after).await?,
            );
            if let (Some(a), Some(b)) = (a, b) {
                match (String::from_utf8(a), String::from_utf8(b)) {
                    (Ok(a), Ok(b)) if !a.contains('\0') && !b.contains('\0') => {
                        if let Some(lines) = diff::lines(&a, &b) {
                            change["lines"] = lines.into();
                        }
                    }
                    _ => change["binary"] = true.into(),
                }
            }
        }
        changes.push(change);
    }
    result["changes"] = changes.into();
    Ok(result)
}

/// Replaces the skill with its source folder at `commit`.
pub async fn update(
    s: &Service,
    scope: &str,
    skill: &str,
    input: &Value,
    project: Option<&Path>,
) -> Result<Value> {
    let mut record = record(s, scope, skill).await?;
    let target = sha(text(input, "commit"))?;
    let base = text(input, "base");
    let repo = repository(text(&record, "repository"))?.to_owned();
    let folder = text(&record, "path").to_owned();
    let commit = commit(s, &repo, target).await?;
    let (files, truncated) = files(s, &repo, &commit.tree, &folder).await?;
    if truncated {
        return Err(Error::bad(
            "GitHub listed only part of this folder, so it cannot be updated safely.",
        ));
    }
    let changes = download(s, &repo, files).await?;
    let saved = s.skills.replace(skill, base, &changes, project).await?;

    record["commit"] = commit.sha.into();
    let mut bundle = super::bundle_response(scope, skill, &saved);
    remember(s, scope, skill, record, &mut bundle).await?;
    s.store
        .audit(
            "skill.updated",
            json!({ "scope": scope, "name": skill, "repository": repo }),
        )
        .await?;
    Ok(bundle)
}
