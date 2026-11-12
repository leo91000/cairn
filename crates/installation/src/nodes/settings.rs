//! Runtime deadlines shared by HTTP and both management tools. Legacy publication
//! settings stay in the database until an explicit save, but are never exposed.
use crate::{
    error::{Error, Result},
    store::Db,
};
use cairn_contracts::NodeRuntimeSettings;
use serde_json::Value;

const KEY: &str = "node-backup-settings";

pub(crate) fn view(db: &Db<'_>) -> Result<NodeRuntimeSettings> {
    let mut value = serde_json::to_value(NodeRuntimeSettings::default())?;

    if let Some(saved) = db.kv(KEY)? {
        for (name, field) in value.as_object_mut().unwrap() {
            if let Some(saved) = saved.get(name) {
                *field = saved.clone();
            }
        }
    }

    Ok(serde_json::from_value(value)?)
}

pub(crate) fn save(db: &Db<'_>, settings: NodeRuntimeSettings) -> Result<NodeRuntimeSettings> {
    settings.validate().map_err(Error::bad)?;

    let value: Value = serde_json::to_value(&settings)?;
    db.set(KEY, &value, None)?;
    db.audit("node.runtime.configured", &value)?;

    Ok(settings)
}
