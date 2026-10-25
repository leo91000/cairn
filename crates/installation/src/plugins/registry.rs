//! Package registry. Bundled packages ship inside the binary. Installed packages live
//! in `<data>/plugins/<id>/<version>/` and run only once the owner approves that exact
//! manifest and every Luau file; any later change on disk needs a new approval.
pub mod owner;

#[cfg(test)]
mod measurement;

use crate::{
    config::now,
    error::{Error, Result},
    service::Service,
    store::Db,
    validation::text,
};
use cairn_plugin_api::{
    CAPABILITIES, MAX_MODULES, MAX_SOURCE_BYTES, Manifest, Sources, module_path,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::OnceLock,
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

/// The exact approval which selected these loaded files. A delayed configuration
/// write must not survive uninstall/reinstallation of otherwise identical files.
#[derive(Clone)]
pub(crate) struct ApprovedPackage {
    pub package: Package,
    record: Option<Value>,
}

/// Durable proof of the precise approval which admitted an unfinished operation.
/// Hash the complete record so restoring identical source files cannot revive it.
#[derive(Clone, Deserialize, Serialize, PartialEq)]
pub(crate) struct Approval {
    plugin: String,
    record_digest: Option<String>,
}

impl Approval {
    fn from_record(plugin: &str, record: Option<&Value>) -> Self {
        Self {
            plugin: plugin.to_owned(),
            record_digest: record.map(|value| crate::auth::hex_digest(&value.to_string())),
        }
    }

    /// Retain native execution's current approval before any remote authority
    /// lookup. Local checks can then fence replacement in the same transaction.
    pub(crate) fn capture_in(db: &Db<'_>, plugin: &str, digest: &str) -> Result<Self> {
        let changed = || Error::forbidden("The source package approval changed.");
        let record = if let Some(package) = builtin(plugin) {
            if package.digest != digest {
                return Err(changed());
            }
            None
        } else {
            let record = db
                .get(KIND, &format!("package:{plugin}"))?
                .ok_or_else(changed)?;
            if record["approvals"][text(&record, "active")] != digest {
                return Err(changed());
            }
            Some(record)
        };
        Ok(Self::from_record(plugin, record.as_ref()))
    }

    /// Remote authority checks can outlive an on-disk edit as well as an approval
    /// replacement. Check both after the last remote lookup; retain `recheck_in`
    /// at the commit boundary to fence subsequent database changes atomically.
    pub(crate) async fn recheck(&self, s: &Service, digest: &str) -> Result<()> {
        let current = approved(s, &self.plugin).await?;
        if current.snapshot().record_digest != self.record_digest {
            return Err(Error::conflict(
                "The package approval changed. Start the operation again.",
            ));
        }
        if current.package.digest != digest {
            return Err(Error::forbidden("The source package changed."));
        }
        Ok(())
    }

    pub(crate) fn recheck_in(&self, db: &Db<'_>) -> Result<()> {
        if let Some(expected) = &self.record_digest {
            let current = db.get(KIND, &format!("package:{}", self.plugin))?;
            let digest = current.map(|value| crate::auth::hex_digest(&value.to_string()));
            if digest.as_ref() != Some(expected) {
                return Err(Error::conflict(
                    "The package approval changed. Start the operation again.",
                ));
            }
        }
        Ok(())
    }
}

impl ApprovedPackage {
    pub(crate) fn snapshot(&self) -> Approval {
        Approval::from_record(&self.package.manifest.id, self.record.as_ref())
    }

    pub(crate) async fn recheck(&self, s: &Service) -> Result<()> {
        self.snapshot().recheck(s, &self.package.digest).await
    }

    pub(crate) fn recheck_in(&self, db: &Db<'_>) -> Result<()> {
        if let Some(expected) = &self.record
            && db
                .get(KIND, &format!("package:{}", self.package.manifest.id))?
                .as_ref()
                != Some(expected)
        {
            return Err(Error::conflict(
                "The package approval changed. Retry with its current version.",
            ));
        }
        Ok(())
    }
}

/// What the owner approves: the manifest and every Luau file, by path.
pub(crate) fn digest(manifest: &Manifest, source: &Sources) -> Result<String> {
    let files = source.files().collect::<BTreeMap<_, _>>();
    Ok(crate::auth::hex_digest(&format!(
        "cairn-package\n{}\n{}",
        serde_json::to_string(manifest)?,
        serde_json::to_string(&files)?
    )))
}

pub(crate) async fn require_digest(s: &Service, plugin: &str, digest: &str) -> Result<()> {
    if active(s, plugin).await?.digest != digest {
        return Err(Error::forbidden(
            "The plugin worker's package approval changed.",
        ));
    }
    Ok(())
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
    // Embedded bytes cannot change before a process restart. Parsing and hashing
    // them on every capability check can delay a transport worker's heartbeats.
    static PACKAGES: OnceLock<Vec<Package>> = OnceLock::new();
    PACKAGES
        .get_or_init(|| {
            BUILTIN
                .iter()
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
                .collect()
        })
        .iter()
        .find(|package| package.manifest.id == id)
        .cloned()
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

/// A broken package is local to its admitted work, rather than an internal
/// scheduler failure. Retain other I/O diagnostics as temporary unavailability;
/// database failures keep their separate internal-error semantics.
fn package_io<T>(result: std::io::Result<T>) -> Result<T> {
    use std::io::ErrorKind;

    result.map_err(|error| match error.kind() {
        ErrorKind::NotFound => {
            Error::not_found("A plugin package file is missing. Review it in Plugins.")
        }
        ErrorKind::InvalidData | ErrorKind::NotADirectory => {
            Error::bad("Plugin package files must contain valid UTF-8 in regular directories.")
        }
        _ => Error::unavailable(format!("Cannot read the plugin package: {error}")),
    })
}

fn read_limited(path: &Path, limit: u64) -> Result<String> {
    let metadata = package_io(std::fs::symlink_metadata(path))?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(Error::bad(
            "Plugin files must be regular files within size limits.",
        ));
    }
    package_io(std::fs::read_to_string(path))
}

/// Reads one installed version. Its directory names must match the manifest.
fn read_version(s: &Service, id: &str, version: &str) -> Result<Package> {
    read_version_at(&root(s), id, version)
}

fn read_version_at(root: &Path, id: &str, version: &str) -> Result<Package> {
    if !segment(id) || !segment(version) {
        return Err(Error::not_found("Plugin version not found."));
    }
    let directory = root.join(id).join(version);
    if !package_io(std::fs::symlink_metadata(&directory))?.is_dir() {
        return Err(Error::not_found("Plugin version not found."));
    }
    let json = directory.join("manifest.json");
    let toml = directory.join("manifest.toml");
    let (format, path) = match (
        package_io(json.try_exists())?,
        package_io(toml.try_exists())?,
    ) {
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
    let directory_entries = package_io(std::fs::read_dir(directory))?;
    let mut entries = package_io(directory_entries.collect::<std::io::Result<Vec<_>>>())?
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
        let metadata = package_io(std::fs::symlink_metadata(&path))?;
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

fn entries(path: &Path) -> Result<Vec<String>> {
    let directory = match std::fs::read_dir(path) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return package_io(Err(error)),
    };
    let mut names = Vec::new();
    for entry in directory {
        let entry = package_io(entry)?;
        if package_io(entry.file_type())?.is_dir()
            && let Ok(name) = entry.file_name().into_string()
            && segment(&name)
        {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
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
    Ok(approved(s, id).await?.package)
}

pub(crate) async fn approved(s: &Service, id: &str) -> Result<ApprovedPackage> {
    if let Some(package) = builtin(id) {
        return Ok(ApprovedPackage {
            package,
            record: None,
        });
    }
    let record = record(s, id).await?;
    let version = record["active"]
        .as_str()
        .ok_or_else(|| Error::forbidden("Approve this plugin before connecting it."))?;
    // File reads and bounded manifest/regex validation are synchronous. Run
    // them outside nested form/credential futures, whose caller retains its
    // original approval and rechecks it at effects and commit.
    let directory = root(s);
    let plugin = id.to_owned();
    let selected = version.to_owned();
    let package =
        tokio::task::spawn_blocking(move || read_version_at(&directory, &plugin, &selected))
            .await
            .map_err(Error::internal)?
            .map_err(|error| {
                if matches!(error.status, 400 | 413) {
                    Error::forbidden("This plugin package is invalid. Review it in Plugins.")
                } else {
                    error
                }
            })?;
    if record["approvals"][version] != package.digest {
        return Err(Error::forbidden(
            "This plugin changed since it was approved. Review it in Plugins.",
        ));
    }
    let approved = ApprovedPackage {
        package,
        record: Some(record),
    };
    let approval = approved.snapshot();
    s.store.read(move |db| approval.recheck_in(db)).await?;
    Ok(approved)
}

/// Every package that can run now: bundled ones and approved active installed versions.
/// A package that fails to load is left out, as it cannot run either.
pub async fn all_active(s: &Service) -> Result<Vec<Package>> {
    let mut packages = BUILTIN
        .iter()
        .map(|(id, _, _, _)| builtin(id).expect("bundled plugin"))
        .collect::<Vec<_>>();
    for id in entries(&root(s))? {
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
    // Preserve the legacy complete response while clients migrate to bounded
    // owner reads. Both contracts share package selection and approval semantics.
    for (id, _, _, _) in BUILTIN {
        packages.push(owner::legacy(s, id).await?);
    }
    for id in entries(&root(s))? {
        if !BUILTIN.iter().any(|(name, _, _, _)| *name == id) {
            packages.push(owner::legacy(s, &id).await?);
        }
    }
    Ok(json!({
        "packageReads": true,
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
    owner: &super::operations::Owner,
) -> Result<Value> {
    if builtin(id).is_some() {
        return Err(Error::bad("Built-in plugins are updated with Cairn."));
    }
    owner.recheck(s).await?;
    let package = read_version(s, id, version)?;
    let record = record(s, id).await?;
    if let Some(reviewed_digest) = reviewed_digest {
        if package.digest != reviewed_digest {
            return Err(Error::conflict(
                "This version changed while you reviewed it. Reload Plugins and review it again.",
            ));
        }
    } else if record["approvals"][version] != package.digest {
        return Err(Error::forbidden(
            "Approve this version before activating it.",
        ));
    }
    // Approval has been checked before any installer is allowed network or file effects.
    super::program_installation::prepare(s, &package, Some(owner)).await?;
    owner.recheck(s).await?;
    let current = read_version(s, id, version)?;
    if current.digest != package.digest {
        return Err(Error::conflict(
            "The plugin changed during program preparation. Review it again.",
        ));
    }
    let approve = reviewed_digest.is_some();
    let digest = package.digest.clone();
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
            // Merge into the live record in the same transaction: preparation
            // must not overwrite approvals committed while it was waiting.
            let key = format!("package:{plugin}");
            let mut record = db
                .get(KIND, &key)?
                .unwrap_or_else(|| json!({"id": key, "pluginId": plugin, "approvals": {}}));
            if approve {
                record["approvals"][&version] = json!(digest);
            } else if record["approvals"][&version] != digest {
                return Err(Error::forbidden("This version is no longer approved."));
            }
            record["active"] = json!(version);
            record["revision"] = json!(crate::config::id());
            record["updatedAt"] = json!(now());
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
    account: &str,
) -> Result<Value> {
    let owner = super::operations::Owner::for_account(s, account).await?;
    if builtin(id).is_some() {
        return set_active(s, id, version, reviewed_digest, &owner).await;
    }
    let package = read_version(s, id, version)?;
    if !package
        .manifest
        .programs
        .iter()
        .any(|program| !program.installer.is_empty())
    {
        return set_active(s, id, version, reviewed_digest, &owner).await;
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
        set_active(&service, &id, &version, digest.as_deref(), &owner).await
    })
}

/// Records the owner's approval of this exact version and runs it.
pub async fn approve(s: &Service, id: &str, version: &str, digest: &str) -> Result<Value> {
    let owner = super::operations::Owner::current(s).await?;
    set_active(s, id, version, Some(digest), &owner).await
}

/// Switches to another previously approved version, e.g. to roll back an update.
pub async fn activate(s: &Service, id: &str, version: &str) -> Result<Value> {
    let owner = super::operations::Owner::current(s).await?;
    set_active(s, id, version, None, &owner).await
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
            super::setup::forget_plugin_in(db, &plugin)?;
            db.audit("plugin.package.uninstalled", &json!({ "plugin": plugin }))
        })
        .await?;
    s.plugins.changed.notify_one();
    Ok(json!({ "deleted": true }))
}
