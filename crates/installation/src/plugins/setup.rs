//! Expiring, owner-bound configuration before an instance acquires any authority.
//! Setup records are separate from live connections and never start a worker.
use super::{connections, registry};
use crate::{
    config::{id, now},
    error::{Error, Result},
    service::Service,
    store::Db,
    validation::{text, uuid},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const PREFIX: &str = "plugin-setup:";
const LIFETIME: i64 = 86_400_000;

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetupDraft {
    pub id: String,
    pub plugin_id: String,
    pub name: String,
    pub settings: Value,
    pub secret_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub credential: bool,
    pub revision: String,
    pub digest: String,
    pub owner_account: String,
    pub owner_access_id: String,
    pub created_at: i64,
    pub expires_at: i64,
}

impl SetupDraft {
    fn recheck_in(&self, db: &Db<'_>) -> Result<()> {
        let saved = db.kv(&format!("{PREFIX}{}", self.id))?;
        if self.expires_at <= now() || saved.as_ref() != Some(&serde_json::to_value(self)?) {
            return Err(missing());
        }
        Ok(())
    }

    pub fn connection(&self) -> connections::Connection {
        let mut connection = connections::Connection::draft(
            &self.id,
            &self.plugin_id,
            self.settings.clone(),
            self.secret_keys.clone(),
        );
        connection.name = self.name.clone();
        connection
    }

    fn public(&self, available: bool) -> Value {
        json!({
            "id": self.id,
            "pluginId": self.plugin_id,
            "name": self.name,
            "status": if available { "setup" } else { "review_needed" },
            "settings": self.settings,
            "secretKeys": self.secret_keys,
            "credential": self.credential,
            "revision": self.revision,
            "createdAt": self.created_at,
            "expiresAt": self.expires_at,
        })
    }
}

fn missing() -> Error {
    Error::not_found("Setup draft is unavailable or expired.")
}

async fn owner(s: &Service) -> Result<cairn_protocol::TaskAuthorGrant> {
    crate::relay::task_author_policy(s)
        .await?
        .map(|policy| policy.owner)
        .ok_or_else(|| Error::forbidden("The installation has no verified owner."))
}

/// Public challenges have no account identity; the draft still belongs to the
/// same live installation owner and exactly the approved package.
async fn stored(s: &Service, draft: &str) -> Result<SetupDraft> {
    uuid(draft)?;
    let value = s
        .store
        .kv(&format!("{PREFIX}{draft}"))
        .await?
        .ok_or_else(missing)?;
    let draft: SetupDraft = serde_json::from_value(value)?;
    if draft.expires_at <= now() {
        return Err(missing());
    }
    Ok(draft)
}

pub(crate) async fn lookup(s: &Service, draft: &str) -> Result<SetupDraft> {
    let draft = stored(s, draft).await?;
    approved_package(s, &draft).await?;
    Ok(draft)
}

async fn approved_package(s: &Service, draft: &SetupDraft) -> Result<registry::ApprovedPackage> {
    let approved = registry::approved(s, &draft.plugin_id)
        .await
        .map_err(|_| missing())?;
    if approved.package.digest != draft.digest {
        return Err(missing());
    }
    Ok(approved)
}

pub async fn load(s: &Service, draft: &str) -> Result<SetupDraft> {
    let draft = stored(s, draft).await?;
    let approved = approved_package(s, &draft).await?;
    let current = owner(s).await?;
    if current.account_id != draft.owner_account || current.access_id != draft.owner_access_id {
        return Err(missing());
    }
    // Owner verification is network I/O. Retire the held snapshot if setup or
    // approval changed while that response was in flight, including expiry.
    s.store
        .read(move |db| {
            approved.recheck_in(db).map_err(|_| missing())?;
            draft.recheck_in(db)?;
            Ok(draft)
        })
        .await
}

async fn owned(s: &Service, plugin: &str, draft: &str, account: &str) -> Result<SetupDraft> {
    let draft = stored(s, draft).await?;
    let current = owner(s).await?;
    if draft.plugin_id != plugin
        || draft.owner_account != account
        || current.account_id != draft.owner_account
        || current.access_id != draft.owner_access_id
    {
        return Err(missing());
    }
    s.store
        .read(move |db| {
            draft.recheck_in(db)?;
            Ok(draft)
        })
        .await
}

pub async fn list(s: &Service, plugin: &str, account: &str) -> Result<Value> {
    let current = owner(s).await?;
    if current.account_id != account {
        return Err(Error::forbidden(
            "Only the installation owner can list setup drafts.",
        ));
    }
    let digest = registry::active(s, plugin)
        .await
        .ok()
        .map(|package| package.digest);
    let mut drafts = s
        .store
        .keys_as::<SetupDraft>(PREFIX)
        .await?
        .into_iter()
        .map(|(_, draft)| draft)
        .filter(|draft| {
            draft.plugin_id == plugin
                && draft.expires_at > now()
                && draft.owner_account == account
                && draft.owner_access_id == current.access_id
        })
        .collect::<Vec<_>>();
    drafts.sort_by(|a, b| a.id.cmp(&b.id));
    let items = drafts
        .iter()
        .map(|draft| draft.public(digest.as_deref() == Some(&draft.digest)))
        .collect::<Vec<_>>();
    Ok(json!({"items": items, "nextCursor": null}))
}

pub async fn get(s: &Service, plugin: &str, draft: &str, account: &str) -> Result<Value> {
    let draft = owned(s, plugin, draft, account).await?;
    let available = registry::active(s, plugin)
        .await
        .is_ok_and(|package| package.digest == draft.digest);
    Ok(draft.public(available))
}

pub async fn form(
    s: &Service,
    plugin: &str,
    draft: &str,
    account: &str,
    input: &Value,
) -> Result<Value> {
    let draft = owned(s, plugin, draft, account).await?;
    if input["revision"].as_str() != Some(&draft.revision) {
        return Err(Error::conflict("Setup changed. Reload its form."));
    }
    let approved = approved_package(s, &draft).await?;
    super::forms::setup_form(s, &approved.package, draft, input).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    name: String,
    #[serde(default)]
    settings: Value,
    #[serde(default)]
    secrets: Value,
    #[serde(default)]
    token: Value,
    revision: Option<String>,
}

pub async fn save(
    s: &Service,
    plugin: &str,
    account: &str,
    input: &Value,
    existing: Option<&str>,
) -> Result<Value> {
    let input: Input =
        serde_json::from_value(input.clone()).map_err(|_| Error::bad("Invalid setup draft."))?;
    let approved = registry::approved(s, plugin).await?;
    let package = &approved.package;
    // Snapshot before fetching owner authority. Cleanup must not remove drafts
    // written under a replacement owner's proof while that response was in flight.
    let setup_snapshot = s.store.keys(PREFIX).await?;
    let current_owner = owner(s).await?;
    if current_owner.account_id != account {
        return Err(Error::forbidden(
            "Only the installation owner can configure a draft.",
        ));
    }
    let old = match existing {
        Some(draft) => Some(owned(s, plugin, draft, account).await?),
        None => None,
    };
    if old
        .as_ref()
        .is_some_and(|draft| draft.digest != package.digest)
    {
        return Err(Error::conflict(
            "The package changed. Discard this draft and start setup again.",
        ));
    }
    if old.as_ref().map(|d| &d.revision) != input.revision.as_ref() {
        return Err(Error::conflict("Setup changed. Reload before saving."));
    }
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(Error::bad("Enter a setup name."));
    }
    // Reuse the instance validators while allowing fields not entered yet.
    let mut partial = package.manifest.clone();
    for field in &mut partial.settings {
        field["required"] = json!(false);
    }
    let settings = connections::settings(&partial, &input.settings)?;
    let draft_id = old.as_ref().map_or_else(id, |d| d.id.clone());
    let token = super::forms::typed_credential(&package.manifest, &input.token)?;
    let credential_key = connections::credential_key(&draft_id);
    let saved_credential = s.vault.get(&credential_key).await?;
    let credential = token.is_some() || saved_credential.is_some();
    let encrypted_credential = token
        .map(|token| s.vault.encrypt(&credential_key, &json!({"token": token})))
        .transpose()?;
    let saved = connections::secret_values(s, &draft_id).await?;
    let fields = connections::secrets(&partial, &input.secrets, &saved)?;
    let expires_at = old
        .as_ref()
        .map_or_else(|| now() + LIFETIME, |d| d.expires_at);
    let draft = SetupDraft {
        id: draft_id.clone(),
        plugin_id: plugin.to_owned(),
        name: name.to_owned(),
        settings,
        secret_keys: fields.keys().cloned().collect(),
        credential,
        revision: id(),
        digest: package.digest.clone(),
        owner_account: account.to_owned(),
        owner_access_id: current_owner.access_id,
        created_at: old.as_ref().map_or_else(now, |d| d.created_at),
        expires_at,
    };
    let secret_key = connections::secrets_key(&draft_id);
    let encrypted = s.vault.encrypt(&secret_key, &json!({"fields": fields}))?;
    let result = draft.public(true);
    s.store
        .transaction(move |db| {
            approved.recheck_in(db)?;
            if db.keys(PREFIX)? != setup_snapshot {
                return Err(Error::conflict("Setup changed. Reload before saving."));
            }
            let key = format!("{PREFIX}{draft_id}");
            let actual = db.kv(&key)?;
            if actual != old.as_ref().map(|d| serde_json::to_value(d).unwrap()) {
                return Err(Error::conflict("Setup changed. Reload before saving."));
            }
            if draft.expires_at <= now() {
                return Err(missing());
            }
            // A replaced owner's drafts can never regain their old access proof.
            // Retire them together with their encrypted secrets before applying quota.
            let mut current_count = 0;
            for (_, previous) in db.keys_as::<SetupDraft>(PREFIX)? {
                if previous.owner_account != draft.owner_account
                    || previous.owner_access_id != draft.owner_access_id
                {
                    erase(db, &previous)?;
                } else {
                    current_count += 1;
                }
            }
            if old.is_none() && current_count >= 32 {
                return Err(Error::too_many_requests(
                    "Finish or discard an existing setup draft.",
                ));
            }
            db.set(&key, &serde_json::to_value(&draft)?, Some(expires_at))?;
            if let Some(encrypted) = encrypted_credential {
                db.set(
                    &format!("mcp-secret:{credential_key}"),
                    &encrypted,
                    Some(expires_at),
                )?;
            }
            db.set(
                &format!("mcp-secret:{secret_key}"),
                &encrypted,
                Some(expires_at),
            )?;
            db.audit(
                "plugin.setup.saved",
                &json!({ "pluginId": draft.plugin_id, "draftId": draft.id }),
            )
        })
        .await?;
    Ok(result)
}

pub async fn remove(
    s: &Service,
    plugin: &str,
    draft: &str,
    account: &str,
    input: &Value,
) -> Result<Value> {
    let draft = owned(s, plugin, draft, account).await?;
    if text(input, "revision") != draft.revision {
        return Err(Error::conflict("Setup changed. Reload before discarding."));
    }
    s.store
        .transaction(move |db| {
            let key = format!("{PREFIX}{}", draft.id);
            if db.kv(&key)?.as_ref() != Some(&serde_json::to_value(&draft)?) {
                return Err(Error::conflict("Setup changed. Reload before discarding."));
            }
            erase(db, &draft)?;
            db.audit(
                "plugin.setup.deleted",
                &json!({ "pluginId": draft.plugin_id, "draftId": draft.id }),
            )?;
            Ok(json!({ "deleted": true }))
        })
        .await
}

fn erase(db: &Db<'_>, draft: &SetupDraft) -> Result<()> {
    db.delete(&format!("{PREFIX}{}", draft.id))?;
    db.delete(&format!(
        "mcp-secret:{}",
        connections::credential_key(&draft.id)
    ))?;
    db.delete(&format!(
        "mcp-secret:{}",
        connections::secrets_key(&draft.id)
    ))
}

pub(crate) fn forget_plugin_in(db: &Db<'_>, plugin: &str) -> Result<()> {
    for (_, draft) in db.keys_as::<SetupDraft>(PREFIX)? {
        if draft.plugin_id == plugin {
            erase(db, &draft)?;
        }
    }
    Ok(())
}
