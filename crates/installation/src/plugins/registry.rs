//! Package registry. Bundled packages ship inside the binary. Installed packages live
//! in `<data>/plugins/<id>/<version>/` and run only once the owner approves that exact
//! manifest and source; any later change on disk needs a new approval.
use crate::{
    config::now,
    error::{Error, Result},
    service::Service,
    validation::text,
};
use cairn_plugin_api::{CAPABILITIES, MAX_SOURCE_BYTES, Manifest};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const KIND: &str = "plugin-packages";

const BUILTIN: &[(&str, &str, &str)] = &[
    (
        "onepassword",
        include_str!("../../../../plugins/onepassword/manifest.json"),
        include_str!("../../../../plugins/onepassword/main.luau"),
    ),
    (
        "discord",
        include_str!("../../../../plugins/discord/manifest.json"),
        include_str!("../../../../plugins/discord/main.luau"),
    ),
];

/// Path segments of the management API; never usable as package IDs.
const RESERVED: &[&str] = &["packages"];

#[derive(Clone)]
pub struct Package {
    pub manifest: Manifest,
    pub source: String,
    pub builtin: bool,
    pub digest: String,
}

fn digest(manifest: &Manifest, source: &str) -> Result<String> {
    Ok(crate::auth::hex_digest(&format!(
        "{}\n{source}",
        serde_json::to_string(manifest)?
    )))
}

fn package(manifest: &str, source: &str, builtin: bool) -> Result<Package> {
    let manifest: Manifest =
        serde_json::from_str(manifest).map_err(|_| Error::bad("Invalid plugin manifest."))?;
    manifest.validate().map_err(Error::bad)?;
    if source.len() > MAX_SOURCE_BYTES {
        return Err(Error::too_large("Plugin source is too large."));
    }
    if !builtin
        && (RESERVED.contains(&manifest.id.as_str())
            || BUILTIN.iter().any(|(id, _, _)| *id == manifest.id))
    {
        return Err(Error::bad("This plugin ID belongs to a built-in plugin."));
    }
    Ok(Package {
        digest: digest(&manifest, source)?,
        manifest,
        source: source.to_owned(),
        builtin,
    })
}

pub fn builtin(id: &str) -> Option<Package> {
    BUILTIN
        .iter()
        .find(|(name, _, _)| *name == id)
        .map(|(_, manifest, source)| package(manifest, source, true).expect("bundled plugin"))
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
    let manifest = read_limited(&directory.join("manifest.json"), 65_536)?;
    let source = read_limited(&directory.join("main.luau"), MAX_SOURCE_BYTES as u64)?;
    let package = package(&manifest, &source, false)?;
    if package.manifest.id != id || package.manifest.version != version {
        return Err(Error::bad(
            "Plugin folder names must match the manifest ID and version.",
        ));
    }
    Ok(package)
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

fn summary(package: &Package) -> Value {
    json!({
        "version": package.manifest.version,
        "digest": package.digest,
        "manifest": package.manifest,
    })
}

/// Everything the owner reviews: requested capabilities, hosts and approval state.
pub async fn catalog(s: &Service) -> Result<Value> {
    let mut packages = Vec::new();
    for (id, _, _) in BUILTIN {
        let package = builtin(id).expect("bundled plugin");
        packages.push(json!({
            "id": id,
            "builtin": true,
            "status": "active",
            "active": package.manifest.version,
            "manifest": package.manifest,
            "versions": [summary(&package)],
        }));
    }
    for id in entries(&root(s)) {
        if BUILTIN.iter().any(|(name, _, _)| *name == id) {
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
        let manifest = current
            .map(|item| item["manifest"].clone())
            .or_else(|| latest.map(|package| json!(package.manifest)));
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

/// Writes an uploaded package version to disk. It stays inactive until approved.
pub async fn install(s: &Service, input: &Value) -> Result<Value> {
    let manifest = serde_json::to_string(&input["manifest"])?;
    let source = text(input, "source");
    let package = package(&manifest, source, false)?;
    if !segment(&package.manifest.version) {
        return Err(Error::bad("Use a simple version such as 1.2.0."));
    }
    // Compile the source in a real worker so broken packages never reach approval.
    let session = s
        .plugins
        .host
        .start(&s.plugins.executable()?, &package.manifest, source)
        .await
        .map_err(|_| {
            Error::bad("The plugin source did not load. It must return a table of handlers.")
        })?;
    session.stop().await;
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
    std::fs::write(staging.path().join("main.luau"), source)?;
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
