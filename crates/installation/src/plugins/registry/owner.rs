//! Owner reads separate catalogue presentation, package configuration and exact
//! version review. Lists never embed every version's manifest or source files.
use super::{
    BUILTIN, Package, builtin, entries, package_io, read_version, record, root, segment, summary,
};
use crate::{
    error::{Error, Result},
    plugins::paging,
    service::Service,
    validation::text,
};
use cairn_plugin_api::{CAPABILITIES, Manifest};
use serde_json::{Value, json};
use std::path::PathBuf;

struct Entry {
    id: String,
    builtin: bool,
    status: &'static str,
    active: Option<String>,
    manifest: Option<Manifest>,
    version_count: usize,
    versions: Vec<Value>,
}

impl Entry {
    fn detail(&self) -> Value {
        json!({
            "id": self.id,
            "builtin": self.builtin,
            "status": self.status,
            "active": self.active,
            "manifest": self.manifest,
            "versionCount": self.version_count,
        })
    }

    fn card(&self) -> Value {
        let presentation = self.manifest.as_ref().map(|manifest| {
            let resource_delegation = manifest.resources.iter().any(|resource| resource.delegation);
            json!({
                "name": manifest.name,
                "description": manifest.description,
                "version": manifest.version,
                "brand": manifest.brand,
                "instances": manifest.instances,
                "capabilities": manifest.capabilities,
                "needsWorker": manifest.needs_worker(),
                "delegates": manifest.permits_capability("conversations.submit") && !resource_delegation,
                "agentAccess": manifest.agent_access.is_some(),
                "contributions": {
                    "commands": manifest.commands.len(),
                    "tools": manifest.tools.len(),
                    "instructions": !manifest.instructions.is_empty(),
                    "view": manifest.instance_view,
                    "test": manifest.instance_test,
                    "form": manifest.instance_form,
                    "collections": manifest.resources.len(),
                },
            })
        });
        json!({
            "id": self.id,
            "builtin": self.builtin,
            "status": self.status,
            "active": self.active,
            "presentation": presentation,
            "versionCount": self.version_count,
        })
    }
}

fn package_directory(s: &Service, id: &str) -> Result<PathBuf> {
    if !segment(id) {
        return Err(Error::not_found("Plugin package not found."));
    }
    let directory = root(s).join(id);
    if !package_io(std::fs::symlink_metadata(&directory))?.is_dir() {
        return Err(Error::not_found("Plugin package not found."));
    }
    Ok(directory)
}

fn cursor(value: Option<&str>) -> Result<()> {
    if value.is_some_and(|value| !segment(value)) {
        return Err(Error::bad("Use a package ID or version as the cursor."));
    }
    Ok(())
}

/// Do not retain all manifests when building a compact card. Approval state and
/// the chosen display manifest nevertheless match the complete legacy catalogue.
async fn inspect(s: &Service, id: &str, complete_versions: bool) -> Result<Entry> {
    if let Some(package) = builtin(id) {
        return Ok(Entry {
            id: id.to_owned(),
            builtin: true,
            status: "active",
            active: Some(package.manifest.version.clone()),
            versions: if complete_versions {
                vec![summary(&package)]
            } else {
                Vec::new()
            },
            manifest: Some(package.manifest),
            version_count: 1,
        });
    }
    let directory = package_directory(s, id)?;
    let record = record(s, id).await?;
    let active = text(&record, "active");
    let names = entries(&directory)?;
    let mut versions = Vec::new();
    let mut current = None;
    let mut latest = None;
    let mut pending = false;
    for version in &names {
        match read_version(s, id, version) {
            Ok(package) => {
                let approved = record["approvals"][version] == package.digest;
                pending |= !approved;
                if approved && active == version {
                    current = Some(package.manifest.clone());
                }
                if complete_versions {
                    let mut item = summary(&package);
                    item["approved"] = json!(approved);
                    versions.push(item);
                }
                latest = Some(package.manifest);
            }
            Err(error) if complete_versions => {
                versions.push(json!({ "version": version, "error": error.message }));
            }
            Err(_) => {}
        }
    }
    let active = current.as_ref().map(|manifest| manifest.version.clone());
    let manifest = current.or(latest);
    let status = match (active.is_some(), pending, manifest.is_some()) {
        (_, _, false) => "invalid",
        (true, false, _) => "active",
        (true, true, _) => "update",
        (false, _, _) => "needs_approval",
    };
    Ok(Entry {
        id: id.to_owned(),
        builtin: false,
        status,
        active,
        manifest,
        version_count: names.len(),
        versions,
    })
}

pub(super) async fn legacy(s: &Service, id: &str) -> Result<Value> {
    let entry = inspect(s, id, true).await?;
    let mut value = entry.detail();
    value.as_object_mut().unwrap().remove("versionCount");
    value["versions"] = json!(entry.versions);
    Ok(value)
}

/// Sorted, count- and byte-bounded cards. A cursor is the last package ID, not a
/// manifest digest: changing approval or live state does not invalidate later pages.
pub async fn list(s: &Service, after: Option<&str>) -> Result<Value> {
    cursor(after)?;
    let mut names = entries(&root(s))?;
    names.extend(BUILTIN.iter().map(|(id, _, _, _)| (*id).to_owned()));
    names.sort();
    names.dedup();
    let mut cards = Vec::new();
    for id in names
        .iter()
        .filter(|id| after.is_none_or(|after| id.as_str() > after))
        .take(paging::MAX_ITEMS + 1)
    {
        cards.push(inspect(s, id, false).await?.card());
    }
    let metadata = json!({
        "packageReads": true,
        "setupDrafts": true,
        "capabilities": CAPABILITIES
            .iter()
            .map(|(name, description)| json!({ "name": name, "description": description }))
            .collect::<Vec<_>>(),
    });
    let budget = paging::MAX_BYTES - serde_json::to_vec(&metadata)?.len();
    let mut page = paging::ordered_page(cards, "id", paging::MAX_ITEMS, budget)?;
    page.as_object_mut()
        .unwrap()
        .extend(metadata.as_object().unwrap().clone());
    Ok(page)
}

/// The selected display manifest for configuring one package, without its history.
pub async fn detail(s: &Service, id: &str) -> Result<Value> {
    Ok(inspect(s, id, false).await?.detail())
}

fn version_value(package: &Package, record: &Value, full: bool) -> Value {
    let approved =
        package.builtin || record["approvals"][&package.manifest.version] == package.digest;
    let mut value = if full {
        summary(package)
    } else {
        json!({
            "version": package.manifest.version,
            "digest": package.digest,
            "fileCount": package.source.files().count(),
            "sourceBytes": package.source.size(),
        })
    };
    value["approved"] = json!(approved);
    value
}

/// A page contains review metadata only. Exact selected and active manifests are
/// separate reads; an invalid local version remains visible with its diagnostic.
pub async fn versions(s: &Service, id: &str, after: Option<&str>) -> Result<Value> {
    cursor(after)?;
    let bundled = builtin(id);
    let names = if let Some(package) = &bundled {
        vec![package.manifest.version.clone()]
    } else {
        entries(&package_directory(s, id)?)?
    };
    let record = if bundled.is_some() {
        Value::Null
    } else {
        record(s, id).await?
    };
    let mut versions = Vec::new();
    for name in names
        .iter()
        .filter(|name| after.is_none_or(|after| name.as_str() > after))
        .take(paging::MAX_ITEMS + 1)
    {
        let package = bundled
            .clone()
            .map_or_else(|| read_version(s, id, name), Ok);
        versions.push(match package {
            Ok(package) => version_value(&package, &record, false),
            Err(error) => json!({ "version": name, "error": error.message }),
        });
    }
    paging::ordered_page(versions, "version", paging::MAX_ITEMS, paging::MAX_BYTES)
}

/// Review these exact bytes and pass their digest to the existing approval route.
/// This read grants no execution authority and does not replace approval checks.
pub async fn version(s: &Service, id: &str, version: &str) -> Result<Value> {
    if !segment(version) {
        return Err(Error::not_found("Plugin version not found."));
    }
    if let Some(package) = builtin(id) {
        return if package.manifest.version == version {
            Ok(version_value(&package, &Value::Null, true))
        } else {
            Err(Error::not_found("Plugin version not found."))
        };
    }
    let directory = package_directory(s, id)?;
    if !entries(&directory)?.iter().any(|name| name == version) {
        return Err(Error::not_found("Plugin version not found."));
    }
    let record = record(s, id).await?;
    Ok(match read_version(s, id, version) {
        Ok(package) => version_value(&package, &record, true),
        Err(error) => json!({ "version": version, "error": error.message }),
    })
}
