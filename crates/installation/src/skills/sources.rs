//! Skills imported from a plugin's skill source, and updates from it. The package's
//! `skills` handler resolves a location to a version and lists a folder's files; Cairn
//! downloads them from the package's declared hosts with the instance's credential,
//! picks the skill folder, and compares versions. See docs/SKILLS.md.
use super::lifecycle::Authority;
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
use cairn_contracts::{SkillSource, SkillSourceChange, SkillSourceStatus, SkillSourceVersion};
use futures_util::{StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, time::Duration};

const PARALLEL_DOWNLOADS: usize = 6;
// Files whose upstream changes include line diffs; others list only their status.
const DIFF_FILES: usize = 60;
const DIFF_BYTES: u64 = 1_000_000;
const DIFF_RESULT_BYTES: usize = 128 * 1024;
const MAX_FILES: usize = 2000;

pub(crate) fn key(scope: &str, skill: &str) -> String {
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
    authority: Authority,
}

async fn origin(
    s: &Service,
    plugin: &str,
    source: &str,
    instance: Option<&str>,
    authority: &Authority,
) -> Result<Origin> {
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
    let authority = authority.instance(s, &connection, &package).await?;
    Ok(Origin {
        package,
        source: source.to_owned(),
        connection,
        authority,
    })
}

async fn call(s: &Service, origin: &Origin, payload: Value) -> Result<Value> {
    origin.authority.check_local(s).await?;
    let scope = Scope::Instance(Box::new(origin.connection.clone()));
    let mut payload = payload;
    payload["source"] = origin.source.clone().into();
    payload["instance"] = scope.instances()[0].clone();
    let result = scope::invoke(s, &origin.package, "skills", payload, &scope).await?;
    origin.authority.check_local(s).await?;
    Ok(result)
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
    let fields = &origin
        .package
        .manifest
        .skill_source(&origin.source)
        .ok_or_else(invalid)?
        .fields;
    let location = connections::settings_with(fields, &location).map_err(|_| invalid())?;
    if serde_json::to_string(&location)?.len() > 4096 {
        return Err(invalid());
    }
    Ok(Version {
        location,
        version,
        folder: folder(&plain(&result["folder"], 500).unwrap_or_default())?.to_owned(),
        label: plain(&result["label"], 200).unwrap_or_default(),
        url: plain(&result["url"], 1000).filter(|raw| {
            url::Url::parse(raw).is_ok_and(|url| {
                url.scheme() == "https" && url.username().is_empty() && url.password().is_none()
            })
        }),
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
        super::bundle::path(&path)?;
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
    origin.authority.check_local(s).await?;
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
    let limit = file.size.min(super::bundle::FILE_BYTES) as usize;
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| Error::bad_gateway("A download was interrupted."))?;
        if bytes.len() + chunk.len() > limit {
            return Err(Error::too_large(format!(
                "The source file exceeds its declared size or the {} MiB file limit.",
                super::bundle::FILE_BYTES >> 20
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    origin.authority.check_local(s).await?;
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

// Same persisted KV document as before B3. Only the plugin-defined location is
// extensible; lifecycle decisions use named fields and the bundle revision.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceRecord {
    plugin_id: String,
    source: String,
    instance: String,
    location: BTreeMap<String, Value>,
    folder: String,
    version: String,
    label: String,
    url: Option<String>,
    imported_at: i64,
    revision: String,
}

/// The source as clients see it, with `modified` against the current files.
fn public(record: &SourceRecord, revision: &str) -> SkillSource {
    SkillSource {
        plugin_id: record.plugin_id.clone(),
        source: record.source.clone(),
        instance: Some(record.instance.clone()),
        label: record.label.clone(),
        location: record.location.clone(),
        url: record.url.clone(),
        folder: record.folder.clone(),
        version: record.version.clone(),
        imported_at: record.imported_at,
        modified: record.revision != revision,
    }
}

async fn record(s: &Service, scope: &str, skill: &str) -> Result<SourceRecord> {
    s.store
        .kv_as::<SourceRecord>(&key(scope, skill))
        .await?
        .ok_or_else(|| Error::not_found("This skill has no source."))
}

/// Imports a new skill: `{ pluginId, skillSource, instance?, location, folder? }`.
pub(crate) async fn import(
    s: &Service,
    scope: &str,
    input: &cairn_contracts::ImportSkillInput,
    project: Option<&Path>,
    authority: Authority,
) -> Result<Value> {
    let cairn_contracts::ImportSkillInput::Plugin {
        plugin_id,
        skill_source,
        instance,
        location,
        folder: requested_folder,
        ..
    } = input
    else {
        return Err(Error::bad("Choose a plugin skill source."));
    };
    let plugin = plugin_id.as_str();
    let origin = origin(s, plugin, skill_source, instance.as_deref(), &authority).await?;
    let location = serde_json::to_value(location)?;
    let fields = origin
        .package
        .manifest
        .skill_source(&origin.source)
        .map(|source| source.fields.clone())
        .unwrap_or_default();
    let location = crate::plugins::connections::settings_with(&fields, &location)?;
    if serde_json::to_vec(&location)?.len() > 4096 {
        return Err(Error::too_large("The skill location is too large."));
    }
    let resolved = resolve(s, &origin, &location).await?;
    let requested = match requested_folder.as_deref().map(str::trim) {
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
            origin.authority.recheck(s).await?;
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
    let mut record = SourceRecord {
        plugin_id: plugin.to_owned(),
        source: origin.source.clone(),
        instance: origin.connection.id.clone(),
        location: serde_json::from_value(resolved.location)?,
        folder: join(&requested, &chosen),
        version: resolved.version,
        label,
        url: resolved.url,
        imported_at: now(),
        revision: String::new(),
    };
    let _mutation = s.skills.mutations.lock().await;
    origin.authority.recheck(s).await?;
    let source_key = key(scope, &skill);
    let auth = origin.authority.clone();
    let mut publication_record = record.clone();
    let saved = s
        .skills
        .apply_authorized(
            &skill,
            None,
            &changes,
            project,
            Some((
                s.store.clone(),
                Box::new(move |db, saved| {
                    auth.authorize(db)?;
                    publication_record.revision = saved.revision.clone();
                    db.set_as(&source_key, &publication_record, None)
                }),
            )),
        )
        .await?;
    let mut bundle = super::bundle_response(scope, &skill, &saved);
    record.revision = saved.revision.clone();
    bundle["source"] = serde_json::to_value(public(&record, &saved.revision))?;
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
        .keys_as::<SourceRecord>("skill-source:")
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
                    Ok((revision, _)) => serde_json::to_value(public(record, &revision))?,
                    Err(_) => Value::Null,
                }
            }
            None => Value::Null,
        };
    }
    Ok(())
}

pub async fn source(s: &Service, scope: &str, skill: &str, bundle: &mut Value) -> Result<()> {
    if let Some(record) = s.store.kv_as::<SourceRecord>(&key(scope, skill)).await? {
        bundle["source"] = serde_json::to_value(public(&record, text(bundle, "revision")))?;
    }
    Ok(())
}

pub(crate) async fn forget(
    s: &Service,
    scope: &str,
    skill: &str,
    authority: Authority,
) -> Result<()> {
    super::name(skill)?;
    let _mutation = s.skills.mutations.lock().await;
    authority.recheck(s).await?;
    let key = key(scope, skill);
    s.store
        .transaction(move |db| {
            authority.authorize(db)?;
            db.delete(&key)
        })
        .await
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

async fn recorded_origin(
    s: &Service,
    record: &SourceRecord,
    authority: &Authority,
) -> Result<Origin> {
    origin(
        s,
        &record.plugin_id,
        &record.source,
        Some(record.instance.as_str()),
        authority,
    )
    .await
}

/// Compares the recorded version with the latest version of the source.
pub(crate) async fn status(
    s: &Service,
    scope: &str,
    skill: &str,
    project: Option<&Path>,
    authority: Authority,
) -> Result<SkillSourceStatus> {
    super::name(skill)?;
    let record = record(s, scope, skill).await?;
    let bundle = s.skills.bundle(scope, skill, project).await?;
    let origin = recorded_origin(s, &record, &authority).await?;
    let folder = &record.folder;
    let location = serde_json::to_value(&record.location)?;
    let latest = resolve(s, &origin, &location).await?;
    let mut result = SkillSourceStatus {
        source: public(&record, text(&bundle, "revision")),
        latest: SkillSourceVersion {
            version: latest.version.clone(),
            message: latest.message,
            date: latest.date,
        },
        current: latest.version == record.version,
        changes: Vec::new(),
    };
    if result.current {
        return Ok(result);
    }

    let (old, _) = files(s, &origin, &location, &record.version, folder).await?;
    let (new, _) = files(s, &origin, &location, &latest.version, folder).await?;
    check_sizes(old.iter().map(|file| file.size))?;
    check_sizes(new.iter().map(|file| file.size))?;
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
    let mut diff_bytes = 0;
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
        let mut change = SkillSourceChange {
            path,
            status: status.to_owned(),
            binary: false,
            lines: None,
        };
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
                            let bytes = serde_json::to_vec(&lines)?.len();
                            if diff_bytes + bytes <= DIFF_RESULT_BYTES {
                                diff_bytes += bytes;
                                change.lines = Some(lines);
                            }
                        }
                    }
                    _ => change.binary = true,
                }
            }
        }
        changes.push(change);
    }
    result.changes = changes;
    Ok(result)
}

/// Replaces the skill with its source folder at `version`.
pub(crate) async fn update(
    s: &Service,
    scope: &str,
    skill: &str,
    input: &cairn_contracts::UpdateSkillSourceInput,
    project: Option<&Path>,
    authority: Authority,
) -> Result<Value> {
    super::name(skill)?;
    let mut record = record(s, scope, skill).await?;
    let version = input.version.clone();
    if version.is_empty() || version.chars().count() > 200 || version.chars().any(char::is_control)
    {
        return Err(Error::bad("Choose the version to update to."));
    }
    let base = input.base.as_str();
    let origin = recorded_origin(s, &record, &authority).await?;
    let folder = record.folder.clone();
    let location = serde_json::to_value(&record.location)?;
    let (files, truncated) = files(s, &origin, &location, &version, &folder).await?;
    if truncated {
        return Err(Error::bad(
            "The source listed only part of this folder, so it cannot be updated safely.",
        ));
    }
    let changes = download(s, &origin, files).await?;
    let _mutation = s.skills.mutations.lock().await;
    if s.store
        .kv_as::<SourceRecord>(&key(scope, skill))
        .await?
        .as_ref()
        != Some(&record)
    {
        return Err(Error::conflict(
            "The skill source changed during this update. Reload the skill and try again.",
        ));
    }
    let source_key = key(scope, skill);
    let expected = record.clone();
    record.version = version;
    record.instance = origin.connection.id.clone();
    let mut publication_record = record.clone();
    let auth = origin.authority.clone();
    auth.recheck(s).await?;
    let root = s.skills.root(project).await?;
    let saved = super::bundle::save_authorized(&root, skill, super::bundle::Base::Replace(base), &changes, Some((s.store.clone(), Box::new(move |db, saved| {
        auth.authorize(db)?;
        if db.kv_as::<SourceRecord>(&source_key)?.as_ref() != Some(&expected) {
            return Err(Error::conflict("The skill source changed during this update. Reload the skill and try again."));
        }
        publication_record.revision = saved.revision.clone();
        db.set_as(&source_key, &publication_record, None)
    })))).await?;
    let mut bundle = super::bundle_response(scope, skill, &saved);
    record.revision = saved.revision.clone();
    bundle["source"] = serde_json::to_value(public(&record, &saved.revision))?;
    s.store
        .audit(
            "skill.updated",
            json!({ "scope": scope, "name": skill, "plugin": origin.package.manifest.id }),
        )
        .await?;
    Ok(bundle)
}
