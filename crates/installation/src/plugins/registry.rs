//! Package registry. Bundled packages ship inside the binary. Installed packages live
//! in `<data>/plugins/<id>/<version>/` and run only once the owner approves that exact
//! manifest and every Luau file; any later change on disk needs a new approval.
use crate::{
    config::now,
    error::{Error, Result},
    service::Service,
    validation::text,
};
use cairn_plugin_api::{
    CAPABILITIES, MAX_MODULES, MAX_SOURCE_BYTES, Manifest, Sources, module_path,
};
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

const KIND: &str = "plugin-packages";

/// `plugins/<id>/`: manifest and Luau files, embedded by `build.rs`.
type Bundled = (
    &'static str,
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
);

const BUILTIN: &[Bundled] = include!(concat!(env!("OUT_DIR"), "/bundled_plugins.rs"));

/// Path segments of the management API; never usable as package IDs.
const RESERVED: &[&str] = &["packages", "oauth"];

#[derive(Clone)]
pub struct Package {
    pub manifest: Manifest,
    pub source: Sources,
    pub builtin: bool,
    pub digest: String,
}

/// What the owner approves: the manifest and every Luau file, by path.
fn digest(manifest: &Manifest, source: &Sources) -> Result<String> {
    let files = source.files().collect::<BTreeMap<_, _>>();
    Ok(crate::auth::hex_digest(&format!(
        "cairn-package\n{}\n{}",
        serde_json::to_string(manifest)?,
        serde_json::to_string(&files)?
    )))
}

fn package(format: &str, manifest: &str, source: Sources, builtin: bool) -> Result<Package> {
    let manifest = Manifest::parse(format, manifest).map_err(Error::bad)?;
    source.validate().map_err(|message| {
        if source.size() > MAX_SOURCE_BYTES {
            Error::too_large(message)
        } else {
            Error::bad(message)
        }
    })?;
    if manifest
        .conditions
        .iter()
        .any(|condition| jsonschema::validator_for(&condition.input_schema).is_err())
    {
        return Err(Error::bad(
            "A plugin condition has an invalid input schema.",
        ));
    }
    for program in &manifest.programs {
        if !program.installer.is_empty() && !source.modules.contains_key(&program.installer) {
            return Err(Error::bad(
                "A program installer must name a Luau file in the package.",
            ));
        }
    }
    if !builtin
        && (RESERVED.contains(&manifest.id.as_str())
            || BUILTIN.iter().any(|(id, _, _, _)| *id == manifest.id))
    {
        return Err(Error::bad("This plugin ID belongs to a built-in plugin."));
    }
    Ok(Package {
        digest: digest(&manifest, &source)?,
        manifest,
        source,
        builtin,
    })
}

pub fn builtin(id: &str) -> Option<Package> {
    BUILTIN
        .iter()
        .find(|(name, _, _, _)| *name == id)
        .map(|(_, format, manifest, files)| {
            let mut source = Sources::default();
            for (path, text) in *files {
                if *path == "main.luau" {
                    source.main = (*text).to_owned();
                } else {
                    source
                        .modules
                        .insert((*path).to_owned(), (*text).to_owned());
                }
            }
            package(format, manifest, source, true).expect("bundled plugin")
        })
}

/// Files of a package as the owner reviews them: path and size, `main.luau` first.
pub fn files(source: &Sources) -> Value {
    json!(
        source
            .files()
            .map(|(path, text)| json!({ "path": path, "bytes": text.len() }))
            .collect::<Vec<_>>()
    )
}

fn root(s: &Service) -> PathBuf {
    s.config.data_dir.join("plugins")
}

fn segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && !value.starts_with('.')
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'+'))
}

fn read_limited(path: &Path, limit: u64) -> Result<String> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(Error::bad(
            "Plugin files must be regular files within size limits.",
        ));
    }
    std::fs::read_to_string(path).map_err(Into::into)
}

/// Reads one installed version. Its directory names must match the manifest.
fn read_version(s: &Service, id: &str, version: &str) -> Result<Package> {
    if !segment(id) || !segment(version) {
        return Err(Error::not_found("Plugin version not found."));
    }
    let directory = root(s).join(id).join(version);
    if !std::fs::symlink_metadata(&directory).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(Error::not_found("Plugin version not found."));
    }
    let json = directory.join("manifest.json");
    let toml = directory.join("manifest.toml");
    let (format, path) = match (json.try_exists()?, toml.try_exists()?) {
        (true, false) => ("json", json),
        (false, true) => ("toml", toml),
        _ => {
            return Err(Error::bad(
                "A package must contain exactly one manifest.json or manifest.toml.",
            ));
        }
    };
    let manifest = read_limited(&path, 65_536)?;
    let mut source = Sources {
        main: read_limited(&directory.join("main.luau"), MAX_SOURCE_BYTES as u64)?,
        modules: BTreeMap::new(),
    };
    read_modules(&directory, "", &mut source)?;
    let package = package(format, &manifest, source, false)?;
    if package.manifest.id != id || package.manifest.version != version {
        return Err(Error::bad(
            "Plugin folder names must match the manifest ID and version.",
        ));
    }
    Ok(package)
}

/// Every other `.luau` file of an installed version, as a module. Links, odd names
/// and oversized packages are refused rather than skipped.
fn read_modules(directory: &Path, prefix: &str, source: &mut Sources) -> Result<()> {
    let invalid = || Error::bad("Plugin files must be like lib/name.luau, without links.");
    let mut entries = std::fs::read_dir(directory)?
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(invalid)?;
        let relative = format!("{prefix}{name}");
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            if prefix.matches('/').count() >= 3 {
                return Err(invalid());
            }
            read_modules(&path, &format!("{relative}/"), source)?;
        } else if relative == "main.luau" || !name.ends_with(".luau") {
            continue;
        } else if !metadata.is_file() || !module_path(&relative) {
            return Err(invalid());
        } else {
            if source.modules.len() >= MAX_MODULES {
                return Err(Error::bad("Plugin package has too many files."));
            }
            let text = read_limited(&path, MAX_SOURCE_BYTES as u64)?;
            source.modules.insert(relative, text);
        }
    }
    Ok(())
}

fn entries(path: &Path) -> Vec<String> {
    let mut names = std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| segment(name))
        .collect::<Vec<_>>();
    names.sort();
    names
}

async fn record(s: &Service, id: &str) -> Result<Value> {
    Ok(s.store
        .get(KIND, &format!("package:{id}"))
        .await?
        .unwrap_or_else(
            || json!({ "id": format!("package:{id}"), "pluginId": id, "approvals": {} }),
        ))
}

/// The package a connection runs: bundled, or the approved active installed version.
pub async fn active(s: &Service, id: &str) -> Result<Package> {
    if let Some(package) = builtin(id) {
        return Ok(package);
    }
    let record = record(s, id).await?;
    let version = record["active"]
        .as_str()
        .ok_or_else(|| Error::forbidden("Approve this plugin before connecting it."))?;
    let package = read_version(s, id, version)?;
    if record["approvals"][version] != package.digest {
        return Err(Error::forbidden(
            "This plugin changed since it was approved. Review it in Plugins.",
        ));
    }
    Ok(package)
}

/// Every package that can run now: bundled ones and approved active installed versions.
/// A package that fails to load is left out, as it cannot run either.
pub async fn all_active(s: &Service) -> Result<Vec<Package>> {
    let mut packages = BUILTIN
        .iter()
        .map(|(id, _, _, _)| builtin(id).expect("bundled plugin"))
        .collect::<Vec<_>>();
    for id in entries(&root(s)) {
        if BUILTIN.iter().any(|(name, _, _, _)| *name == id) {
            continue;
        }
        if let Ok(package) = active(s, &id).await {
            packages.push(package);
        }
    }
    Ok(packages)
}

fn summary(package: &Package) -> Value {
    json!({
        "version": package.manifest.version,
        "digest": package.digest,
        "manifest": package.manifest,
        "setup": package.manifest.setup(),
        "files": files(&package.source),
    })
}

/// Everything the owner reviews: requested capabilities, hosts and approval state.
pub async fn catalog(s: &Service) -> Result<Value> {
    let mut packages = Vec::new();
    for (id, _, _, _) in BUILTIN {
        let package = builtin(id).expect("bundled plugin");
        packages.push(json!({
            "id": id,
            "builtin": true,
            "status": "active",
            "active": package.manifest.version,
            "manifest": package.manifest,
            "setup": package.manifest.setup(),
            "versions": [summary(&package)],
        }));
    }
    for id in entries(&root(s)) {
        if BUILTIN.iter().any(|(name, _, _, _)| *name == id) {
            continue;
        }
        let record = record(s, &id).await?;
        let active = record["active"].as_str().unwrap_or("");
        let mut versions = Vec::new();
        let mut latest = None;
        for version in entries(&root(s).join(&id)) {
            match read_version(s, &id, &version) {
                Ok(package) => {
                    let mut item = summary(&package);
                    item["approved"] = json!(record["approvals"][&version] == package.digest);
                    versions.push(item);
                    latest = Some(package);
                }
                Err(error) => versions.push(json!({ "version": version, "error": error.message })),
            }
        }
        let current = versions
            .iter()
            .find(|item| item["version"] == active && item["approved"] == true);
        let (manifest, setup) = match (current, latest) {
            (Some(item), _) => (Some(item["manifest"].clone()), item["setup"].clone()),
            (None, Some(package)) => (
                Some(json!(package.manifest)),
                json!(package.manifest.setup()),
            ),
            (None, None) => (None, Value::Null),
        };
        let pending = versions
            .iter()
            .any(|item| item["approved"] == false && item["error"].is_null());
        packages.push(json!({
            "id": id,
            "builtin": false,
            "status": match (current.is_some(), pending, manifest.is_some()) {
                (_, _, false) => "invalid",
                (true, false, _) => "active",
                (true, true, _) => "update",
                (false, _, _) => "needs_approval",
            },
            "active": current.map(|item| item["version"].clone()),
            "manifest": manifest,
            "setup": setup,
            "versions": versions,
        }));
    }
    Ok(json!({
        "packages": packages,
        "capabilities": CAPABILITIES
            .iter()
            .map(|(name, text)| json!({ "name": name, "description": text }))
            .collect::<Vec<_>>(),
    }))
}

/// Writes an uploaded package version to disk: `{ manifest, source, modules? }` where
/// `modules` maps paths such as `lib/rest.luau` to their text. It stays inactive until
/// approved.
pub async fn install(s: &Service, input: &Value) -> Result<Value> {
    let format = input["manifestFormat"].as_str().unwrap_or("json");
    let manifest = match &input["manifest"] {
        Value::String(source) => source.clone(),
        Value::Object(_) if format == "json" => serde_json::to_string(&input["manifest"])?,
        _ => {
            return Err(Error::bad(
                "Supply a JSON manifest object or JSON/TOML text with manifestFormat.",
            ));
        }
    };
    let empty = Map::new();
    let modules = match &input["modules"] {
        Value::Null => &empty,
        Value::Object(modules) => modules,
        _ => return Err(Error::bad("Package modules must map file paths to text.")),
    };
    let mut source = Sources {
        main: text(input, "source").to_owned(),
        modules: BTreeMap::new(),
    };
    for (path, text) in modules {
        let text = text
            .as_str()
            .ok_or_else(|| Error::bad("Package modules must map file paths to text."))?;
        source.modules.insert(path.clone(), text.to_owned());
    }
    let package = package(format, &manifest, source, false)?;
    if !segment(&package.manifest.version) {
        return Err(Error::bad("Use a simple version such as 1.2.0."));
    }
    // Compile the source in a real worker so broken packages never reach approval.
    let session = s
        .plugins
        .host
        .start(&s.plugins.executable()?, &package.manifest, &package.source)
        .await
        .map_err(|_| {
            Error::bad("The plugin source did not load. main.luau must return a table of handlers, and every require must name a package file.")
        })?;
    session.stop().await;
    for program in package
        .manifest
        .programs
        .iter()
        .filter(|program| !program.installer.is_empty())
    {
        let session = s
            .plugins
            .host
            .start(
                &s.plugins.executable()?,
                &package.manifest,
                super::program_installation::source(&package, program),
            )
            .await
            .map_err(|_| {
                Error::bad("Program installers must load and export resolve and install functions.")
            })?;
        session.stop().await;
    }
    let directory = root(s)
        .join(&package.manifest.id)
        .join(&package.manifest.version);
    if let Ok(existing) = read_version(s, &package.manifest.id, &package.manifest.version) {
        if existing.digest == package.digest {
            return Ok(summary(&existing));
        }
        return Err(Error::conflict(
            "This version is already installed with different content. Increase the version.",
        ));
    }
    let parent = root(s).join(&package.manifest.id);
    std::fs::create_dir_all(&parent)?;
    let staging = tempfile::tempdir_in(&parent)?;
    std::fs::write(
        staging.path().join("manifest.json"),
        serde_json::to_vec_pretty(&package.manifest)?,
    )?;
    for (path, text) in package.source.files() {
        let target = staging.path().join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(target, text)?;
    }
    std::fs::rename(staging.keep(), &directory)?;
    let id = package.manifest.id.clone();
    let version = package.manifest.version.clone();
    s.store
        .transaction(move |db| {
            db.audit(
                "plugin.package.installed",
                &json!({ "plugin": id, "version": version }),
            )
        })
        .await?;
    Ok(summary(&package))
}

async fn set_active(
    s: &Service,
    id: &str,
    version: &str,
    reviewed_digest: Option<&str>,
) -> Result<Value> {
    if builtin(id).is_some() {
        return Err(Error::bad("Built-in plugins are updated with Cairn."));
    }
    let package = read_version(s, id, version)?;
    let mut record = record(s, id).await?;
    if let Some(reviewed_digest) = reviewed_digest {
        if package.digest != reviewed_digest {
            return Err(Error::conflict(
                "This version changed while you reviewed it. Reload Plugins and review it again.",
            ));
        }
        record["approvals"][version] = json!(package.digest);
    } else if record["approvals"][version] != package.digest {
        return Err(Error::forbidden(
            "Approve this version before activating it.",
        ));
    }
    // Approval has been checked before any installer is allowed network or file effects.
    super::program_installation::prepare(s, &package).await?;
    let current = read_version(s, id, version)?;
    if current.digest != package.digest {
        return Err(Error::conflict(
            "The plugin changed during program preparation. Review it again.",
        ));
    }
    // Preparation can take time; preserve approvals written by concurrent requests.
    record = self::record(s, id).await?;
    if reviewed_digest.is_some() {
        record["approvals"][version] = json!(package.digest);
    } else if record["approvals"][version] != package.digest {
        return Err(Error::forbidden("This version is no longer approved."));
    }
    record["active"] = json!(version);
    record["updatedAt"] = json!(now());
    let (plugin, action) = (
        id.to_owned(),
        if reviewed_digest.is_some() {
            "approved"
        } else {
            "activated"
        },
    );
    let version = version.to_owned();
    s.store
        .transaction(move |db| {
            db.put(KIND, &record)?;
            db.audit(
                &format!("plugin.package.{action}"),
                &json!({ "plugin": plugin, "version": version }),
            )
        })
        .await?;
    s.plugins.changed.notify_one();
    Ok(summary(&package))
}

/// Installer-backed activation is tracked without holding a relay request open.
/// Ordinary packages keep the existing synchronous response contract.
pub async fn activate_request(
    s: &std::sync::Arc<Service>,
    id: &str,
    version: &str,
    reviewed_digest: Option<&str>,
) -> Result<Value> {
    if builtin(id).is_some() {
        return set_active(s, id, version, reviewed_digest).await;
    }
    let package = read_version(s, id, version)?;
    if !package
        .manifest
        .programs
        .iter()
        .any(|program| !program.installer.is_empty())
    {
        return set_active(s, id, version, reviewed_digest).await;
    }
    if reviewed_digest.is_some_and(|digest| digest != package.digest) {
        return Err(Error::conflict(
            "This version changed while you reviewed it. Reload Plugins.",
        ));
    }
    let (service, id, version, digest) = (
        s.clone(),
        id.to_owned(),
        version.to_owned(),
        reviewed_digest.map(str::to_owned),
    );
    s.plugins.operations.start(s.clone(), async move {
        set_active(&service, &id, &version, digest.as_deref()).await
    })
}

/// Records the owner's approval of this exact version and runs it.
pub async fn approve(s: &Service, id: &str, version: &str, digest: &str) -> Result<Value> {
    set_active(s, id, version, Some(digest)).await
}

/// Switches to another previously approved version, e.g. to roll back an update.
pub async fn activate(s: &Service, id: &str, version: &str) -> Result<Value> {
    set_active(s, id, version, None).await
}

pub async fn uninstall(s: &Service, id: &str) -> Result<Value> {
    if builtin(id).is_some() || !segment(id) {
        return Err(Error::bad("Built-in plugins can't be uninstalled."));
    }
    let used = s
        .store
        .list(super::connections::KIND)
        .await?
        .iter()
        .any(|connection| connection["pluginId"] == id);
    if used {
        return Err(Error::conflict("Remove this plugin's connections first."));
    }
    let directory = root(s).join(id);
    if directory.exists() {
        std::fs::remove_dir_all(&directory)?;
    }
    let plugin = id.to_owned();
    s.store
        .transaction(move |db| {
            db.remove(KIND, &format!("package:{plugin}"))?;
            db.audit("plugin.package.uninstalled", &json!({ "plugin": plugin }))
        })
        .await?;
    s.plugins.changed.notify_one();
    Ok(json!({ "deleted": true }))
}
