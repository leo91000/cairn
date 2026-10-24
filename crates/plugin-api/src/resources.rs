//! Related resource collections use the same declarative fields as instances.
use crate::{Manifest, short, slug, validate_settings_schema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceCollection {
    pub name: String,
    pub title: String,
    pub singular: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub delegation: bool,
    /// The package's form handler completes this collection's configuration.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub form: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<Value>,
}

pub(crate) fn validate(manifest: &Manifest) -> Result<(), &'static str> {
    if manifest.resources.is_empty() == manifest.permits_capability("resources")
        || manifest.resources.len() > 16
    {
        return Err(
            "Declare resource collections exactly when requesting the resources capability (at most 16).",
        );
    }
    let mut names = HashSet::new();
    for collection in &manifest.resources {
        if !slug(&collection.name, 40)
            || !names.insert(collection.name.as_str())
            || collection.title.trim().is_empty()
            || !short(&collection.title, 100)
            || collection.singular.trim().is_empty()
            || !short(&collection.singular, 60)
        {
            return Err("Plugin manifest has an invalid resource collection.");
        }
        validate_settings_schema(&collection.settings)?;
        if !collection.form
            && collection
                .settings
                .iter()
                .any(|field| field["remote"] == true)
        {
            return Err("Remote choices require the collection's form handler.");
        }
    }
    for collection in &manifest.resources {
        let mut current = collection;
        let mut seen = HashSet::new();
        while let Some(parent) = &current.parent {
            if !seen.insert(parent) {
                return Err("Resource collections cannot have cyclic parents.");
            }
            current = manifest
                .resources
                .iter()
                .find(|collection| &collection.name == parent)
                .ok_or("Resource collection parent is not declared.")?;
        }
    }
    Ok(())
}
