//! Skills imported from a plugin's skill source, and updates from it. The package's
//! `skills` handler resolves a location to a version and lists a folder's files; Cairn
//! downloads them from the package's declared hosts with the instance's credential,
//! picks the skill folder, and compares versions. See docs/SKILLS.md.
use super::{Choice, bundle::Change, check_sizes, choose, diff, imported_name, visible};
use crate::{
    config::now,
    error::{Error, Result},
    plugins::{
        capabilities::target,
        connections::{self, Connection, credential_key},
        registry::{self, Package},
        scope::{self, Scope},
    },
    service::Service,
    validation::{text, uuid},
};
use futures_util::{StreamExt, TryStreamExt};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, time::Duration};

const PARALLEL_DOWNLOADS: usize = 6;
// Files whose upstream changes include line diffs; others list only their status.
const DIFF_FILES: usize = 60;
const DIFF_BYTES: u64 = 1_000_000;
const MAX_FILES: usize = 2000;
// File replacement and source metadata form one lifecycle operation. Downloads
// happen before this lock so detach and deletion can invalidate a pending update.
static MUTATIONS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn key(scope: &str, skill: &str) -> String {
    format!("skill-source:{scope}/{skill}")
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

fn join(parent: &str, child: &str) -> String {
    match (parent.is_empty(), child.is_empty()) {
        (true, _) => child.to_owned(),
        (_, true) => parent.to_owned(),
        _ => format!("{parent}/{child}"),
    }
}

fn plain(value: &Value, max: usize) -> Option<String> {
    value
        .as_str()
        .filter(|value| value.chars().count() <= max && !value.chars().any(char::is_control))
        .map(str::to_owned)
}

fn invalid() -> Error {
    Error::bad_gateway("The plugin returned an invalid skill source response.")
}

/// The package and instance a source record or import names.
struct Origin {
    package: Package,
    source: String,
    connection: Connection,
}

async fn origin(s: &Service, plugin: &str, source: &str, instance: Option<&str>) -> Result<Origin> {
    let package = registry::active(s, plugin)
        .await
        .map_err(|_| Error::bad("The plugin of this skill source is not active."))?;
    if package.manifest.skill_source(source).is_none() {
        return Err(Error::bad("This plugin has no such skill source."));
    }
    let connection = match instance.filter(|id| !id.is_empty()) {
        Some(id) => {
            uuid(id)?;
            connections::get(s, plugin, id).await.ok()
        }
        None => None,
    };
    // A removed instance falls back to another enabled instance of the same plugin.
    let connection = match connection.filter(|connection| connection.enabled) {
        Some(connection) => connection,
        None => {
            let mut instances = s
                .store
                .plugin_instances()
                .await?
                .into_iter()
                .filter(|connection| connection.plugin_id == plugin && connection.enabled)
                .collect::<Vec<_>>();
            instances.sort_by(|a, b| a.name.cmp(&b.name));
            instances.into_iter().next().ok_or_else(|| {
                Error::bad(format!(
                    "Connect an enabled {} instance to use this skill source.",
                    package.manifest.name
                ))
            })?
        }
    };
    Ok(Origin {
        package,
        source: source.to_owned(),
        connection,
    })
}

async fn call(s: &Service, origin: &Origin, payload: Value) -> Result<Value> {
    let scope = Scope::Instance(Box::new(origin.connection.clone()));
    let mut payload = payload;
    payload["source"] = origin.source.clone().into();
    payload["instance"] = scope.instances()[0].clone();
    scope::invoke(s, &origin.package, "skills", payload, &scope).await
}

/// A version of the source: what the package resolved.
struct Version {
    location: Value,
    version: String,
    folder: String,
    label: String,
    url: Option<String>,
    message: String,
    date: String,
}

async fn resolve(s: &Service, origin: &Origin, location: &Value) -> Result<Version> {
    let result = call(s, origin, json!({ "op": "resolve", "location": location })).await?;
    let version = plain(&result["version"], 200)
        .filter(|version| !version.is_empty())
        .ok_or_else(invalid)?;
    let location = result
        .get("location")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| location.clone());
    if serde_json::to_string(&location)?.len() > 4096 {
        return Err(invalid());
    }
    Ok(Version {
        location,
        version,
        folder: folder(&plain(&result["folder"], 500).unwrap_or_default())?.to_owned(),
        label: plain(&result["label"], 200).unwrap_or_default(),
        url: plain(&result["url"], 1000)
            .filter(|raw| url::Url::parse(raw).is_ok_and(|url| url.scheme() == "https")),
        message: plain(&result["message"], 300).unwrap_or_default(),
        date: plain(&result["date"], 40).unwrap_or_default(),
    })
}

/// A file of a source folder, with a path relative to that folder.
struct File {
    path: String,
    size: u64,
    digest: String,
    executable: bool,
    link: bool,
    download: Value,
}

/// Visible files below `folder` at `version`, and whether the listing is partial.
async fn files(
    s: &Service,
    origin: &Origin,
    location: &Value,
    version: &str,
    folder: &str,
) -> Result<(Vec<File>, bool)> {
    let result = call(
        s,
        origin,
        json!({ "op": "files", "location": location, "version": version, "folder": folder }),
    )
    .await?;
    let listed = result["files"]
        .as_array()
        .filter(|files| files.len() <= MAX_FILES)
        .ok_or_else(invalid)?;
    let mut files = Vec::new();
    for raw in listed {
        let path = plain(&raw["path"], 1000).ok_or_else(invalid)?;
        super::relative(&path).map_err(|_| invalid())?;
        if !visible(&path) {
            continue;
        }
        files.push(File {
            path,
            size: raw["size"].as_u64().ok_or_else(invalid)?,
            digest: plain(&raw["digest"], 200).ok_or_else(invalid)?,
            executable: raw["executable"] == true,
            link: raw["link"] == true,
            download: raw["download"].clone(),
        });
    }
    Ok((files, result["truncated"] == true))
}

/// Downloads one file from the package's declared hosts. The host adds the instance's
/// credential with the manifest's rule; the package only names the URL.
async fn fetch(s: &Service, origin: &Origin, file: &File) -> Result<Vec<u8>> {
    let manifest = &origin.package.manifest;
    let url = target(s, manifest, text(&file.download, "url"), "https")?;
    let mut request = s.plugins.http.get(url).timeout(Duration::from_secs(30));
    if let Some(headers) = file.download["headers"].as_object() {
        if headers.len() > 4 {
            return Err(invalid());
        }
        for (name, value) in headers {
            let name = name.to_ascii_lowercase();
            let value = value.as_str().unwrap_or("");
            let allowed =
                matches!(name.as_str(), "accept" | "user-agent") || name.starts_with("x-");
            if !allowed || value.len() > 200 || value.chars().any(char::is_control) {
                return Err(invalid());
            }
            request = request.header(name, value);
        }
    }
    if let Some(rule) = &manifest.credential
        && let Some(token) = s
            .vault
            .get(&credential_key(&origin.connection.id))
            .await?
            .map(|value| text(&value, "token").to_owned())
            .filter(|token| !token.is_empty())
    {
        request = request.header(rule.header.as_str(), format!("{}{token}", rule.prefix));
    }
    let response = request
        .send()
        .await
        .map_err(|_| Error::bad_gateway("The skill source is unavailable."))?;
    match response.status().as_u16() {
        200 => {}
        404 => return Err(Error::not_found("A file of this skill source is missing.")),
        _ => return Err(Error::bad_gateway("The skill source refused a download.")),
    }
    // Listed sizes decide what is downloaded; each body stays within a skill file.
    let limit = super::bundle::FILE_BYTES as usize;
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| Error::bad_gateway("A download was interrupted."))?;
        if bytes.len() + chunk.len() > limit {
            return Err(Error::too_large(format!(
                "Skill files are limited to {} MiB each.",
                super::bundle::FILE_BYTES >> 20
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Downloads the files of a skill folder as writes.
async fn download(s: &Service, origin: &Origin, files: Vec<File>) -> Result<Vec<Change>> {
    if let Some(link) = files.iter().find(|file| file.link) {
        return Err(Error::bad(format!(
            "Skills must not contain symbolic links: {}",
            link.path
        )));
    }
    check_sizes(files.iter().map(|file| file.size))?;
    futures_util::stream::iter(files.into_iter().map(|file| async move {
        Ok::<_, Error>(Change::Write {
            bytes: fetch(s, origin, &file).await?,
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
        "pluginId": record["pluginId"],
        "source": record["source"],
        "instance": record["instance"],
        "label": record["label"],
        "location": record["location"],
        "url": record["url"],
        "folder": record["folder"],
        "version": record["version"],
        "importedAt": record["importedAt"],
        "modified": record["revision"] != revision,
    })
}

async fn record(s: &Service, scope: &str, skill: &str) -> Result<Value> {
    s.store
        .kv(&key(scope, skill))
        .await?
        .ok_or_else(|| Error::not_found("This skill has no source."))
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

/// Imports a new skill: `{ pluginId, skillSource, instance?, location, folder? }`.
pub async fn import(
    s: &Service,
    scope: &str,
    input: &Value,
    project: Option<&Path>,
) -> Result<Value> {
    let plugin = text(input, "pluginId");
    let origin = origin(
        s,
        plugin,
        text(input, "skillSource"),
        input["instance"].as_str(),
    )
    .await?;
    let location = input
        .get("location")
        .filter(|value| value.is_object())
        .cloned()
        .ok_or_else(|| Error::bad("Describe where the skill is."))?;
    let fields = origin
        .package
        .manifest
        .skill_source(&origin.source)
        .map(|source| source.fields.clone())
        .unwrap_or_default();
    let location = crate::plugins::connections::settings_with(&fields, &location)?;
    let resolved = resolve(s, &origin, &location).await?;
    let requested = match input["folder"].as_str().map(str::trim) {
        Some(value) if !value.is_empty() => folder(value)?.to_owned(),
        _ => resolved.folder.clone(),
    };
    let (files, truncated) = files(
        s,
        &origin,
        &resolved.location,
        &resolved.version,
        &requested,
    )
    .await?;
    if truncated {
        return Err(Error::bad(
            "This folder is too large to search. Enter the skill folder path.",
        ));
    }
    let label = if resolved.label.is_empty() {
        origin.package.manifest.name.clone()
    } else {
        resolved.label.clone()
    };
    let chosen = match choose(files.iter().map(|file| file.path.as_str()), None, &label)? {
        Choice::Candidates(candidates) => {
            let candidates = candidates
                .iter()
                .map(|candidate| join(&requested, candidate))
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
    let changes = download(s, &origin, selected).await?;
    let skill = imported_name(&changes)?;
    let _mutation = MUTATIONS.lock().await;
    let saved = s.skills.apply(&skill, None, &changes, project).await?;
    let mut bundle = super::bundle_response(scope, &skill, &saved);
    let record = json!({
        "pluginId": plugin,
        "source": origin.source,
        "instance": origin.connection.id,
        "location": resolved.location,
        "folder": join(&requested, &chosen),
        "version": resolved.version,
        "label": label,
        "url": resolved.url,
        "importedAt": now(),
    });
    remember(s, scope, &skill, record, &mut bundle).await?;
    s.store
        .audit(
            "skill.imported",
            json!({ "scope": scope, "name": skill, "plugin": plugin }),
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
    let _mutation = MUTATIONS.lock().await;
    s.store.delete(&key(scope, skill)).await
}

/// Deletes the skill files and source together, excluding import/update commits.
pub async fn remove(s: &Service, scope: &str, skill: &str, project: Option<&Path>) -> Result<()> {
    let _mutation = MUTATIONS.lock().await;
    s.skills.remove(skill, project).await?;
    s.store.delete(&key(scope, skill)).await
}

async fn content(s: &Service, origin: &Origin, file: Option<&File>) -> Result<Option<Vec<u8>>> {
    match file {
        Some(file) if file.size <= DIFF_BYTES && !file.link => {
            Ok(Some(fetch(s, origin, file).await?))
        }
        Some(_) => Ok(None),
        None => Ok(Some(Vec::new())),
    }
}

async fn recorded_origin(s: &Service, record: &Value) -> Result<Origin> {
    origin(
        s,
        text(record, "pluginId"),
        text(record, "source"),
        record["instance"].as_str(),
    )
    .await
}

/// Compares the recorded version with the latest version of the source.
pub async fn status(
    s: &Service,
    scope: &str,
    skill: &str,
    project: Option<&Path>,
) -> Result<Value> {
    let record = record(s, scope, skill).await?;
    let bundle = s.skills.bundle(scope, skill, project).await?;
    let origin = recorded_origin(s, &record).await?;
    let folder = text(&record, "folder");
    let latest = resolve(s, &origin, &record["location"]).await?;
    let mut result = json!({
        "source": public(&record, text(&bundle, "revision")),
        "latest": { "version": latest.version, "message": latest.message, "date": latest.date },
        "current": latest.version == text(&record, "version"),
        "changes": [],
    });
    if result["current"] == true {
        return Ok(result);
    }

    let (old, _) = files(
        s,
        &origin,
        &record["location"],
        text(&record, "version"),
        folder,
    )
    .await?;
    let (new, _) = files(s, &origin, &record["location"], &latest.version, folder).await?;
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
            (Some(a), Some(b)) if a.digest != b.digest || a.executable != b.executable => {
                "modified"
            }
            _ => continue,
        };
        let mut change = json!({ "path": path, "status": status, "binary": false });
        let content_changed = before.is_none_or(|a| after.is_none_or(|b| a.digest != b.digest));
        if changes.len() < DIFF_FILES && content_changed {
            let (a, b) = (
                content(s, &origin, before).await?,
                content(s, &origin, after).await?,
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

/// Replaces the skill with its source folder at `version`.
pub async fn update(
    s: &Service,
    scope: &str,
    skill: &str,
    input: &Value,
    project: Option<&Path>,
) -> Result<Value> {
    let mut record = record(s, scope, skill).await?;
    let version = plain(&input["version"], 200)
        .filter(|version| !version.is_empty())
        .ok_or_else(|| Error::bad("Choose the version to update to."))?;
    let base = text(input, "base");
    let origin = recorded_origin(s, &record).await?;
    let folder = text(&record, "folder").to_owned();
    let (files, truncated) = files(s, &origin, &record["location"], &version, &folder).await?;
    if truncated {
        return Err(Error::bad(
            "The source listed only part of this folder, so it cannot be updated safely.",
        ));
    }
    let changes = download(s, &origin, files).await?;
    let _mutation = MUTATIONS.lock().await;
    if s.store.kv(&key(scope, skill)).await?.as_ref() != Some(&record) {
        return Err(Error::conflict(
            "The skill source changed during this update. Reload the skill and try again.",
        ));
    }
    let saved = s.skills.replace(skill, base, &changes, project).await?;

    record["version"] = version.into();
    record["instance"] = origin.connection.id.clone().into();
    let mut bundle = super::bundle_response(scope, skill, &saved);
    remember(s, scope, skill, record, &mut bundle).await?;
    s.store
        .audit(
            "skill.updated",
            json!({ "scope": scope, "name": skill, "plugin": origin.package.manifest.id }),
        )
        .await?;
    Ok(bundle)
}
