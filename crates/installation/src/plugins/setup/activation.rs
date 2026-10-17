//! The transition from expiring owner configuration to a live instance is one
//! transaction. Preparation may perform I/O; it grants no operational authority.
use super::*;
use crate::plugins::resources::target::Target;

pub(crate) async fn activate(
    s: &Service,
    plugin: &str,
    draft_id: &str,
    account: &str,
    input: &Value,
) -> Result<Value> {
    let target = Target::setup(s, plugin, draft_id, account).await?;
    let draft = target.setup_snapshot().ok_or_else(missing)?.clone();
    if input["revision"] != draft.revision {
        return Err(Error::conflict("Setup changed. Reload before activating."));
    }
    let choices = input
        .as_object()
        .filter(|choices| {
            choices.keys().all(|key| {
                [
                    "revision",
                    "enabled",
                    "agentIds",
                    "agentId",
                    "projectId",
                    "accessMode",
                    "userIds",
                    "people",
                    "maxMessagesPerHour",
                    "maxPerPersonPerHour",
                    "directOrigin",
                ]
                .contains(&key.as_str())
            })
        })
        .ok_or_else(|| Error::bad("Invalid activation fields."))?;
    let mut values = Value::Object(choices.clone());
    values["name"] = json!(draft.name);
    values["settings"] = draft.settings.clone();
    let check = target.clone();
    let resources = s
        .store
        .read(move |db| {
            check.current(db)?;
            check.list(db)
        })
        .await?;
    let prepared = connections::prepare(
        s,
        target.package(),
        &values,
        connections::SaveSource::Setup(&draft),
    )
    .await?;
    // The final form may make network requests. It must not finish a former
    // owner's setup or publish stale resources after any intervening edit.
    let current = super::load(s, draft_id).await?;
    if serde_json::to_value(&current)? != serde_json::to_value(&draft)?
        || prepared.connection.owner_access_id.as_deref() != Some(&draft.owner_access_id)
    {
        return Err(Error::conflict(
            "Setup authority changed. Reload before activating.",
        ));
    }
    let vault = s.vault.clone();
    let result = s
        .store
        .transaction(move |db| {
            target.current(db)?;
            let connection = prepared.connection.clone();
            let result = prepared.persist_in(db)?;
            super::super::resources::activate_setup_in(
                db,
                &vault,
                &target,
                &resources,
                &connection,
            )?;
            for key in [
                connections::credential_key(&draft.id),
                connections::secrets_key(&draft.id),
            ] {
                let stored = format!("mcp-secret:{key}");
                if let Some(encrypted) = db.kv(&stored)? {
                    db.set(&stored, &encrypted, None)?;
                }
            }
            // Consume only metadata: ordinary discard would delete promoted secrets
            // and OAuth along with the setup resource tree.
            target.current(db)?;
            db.delete(&format!("{PREFIX}{}", draft.id))?;
            db.audit(
                "plugin.setup.activated",
                &json!({"pluginId": draft.plugin_id, "draftId": draft.id}),
            )?;
            Ok(result)
        })
        .await?;
    s.plugins.changed.notify_one();
    Ok(result)
}
