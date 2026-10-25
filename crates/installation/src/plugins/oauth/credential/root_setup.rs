//! Root OAuth for expiring setup; it cannot be used as a live-account flow.
use super::*;

pub(super) async fn recheck(s: &Service, target: &Arc<Target>) -> Result<()> {
    let expected = target.setup_snapshot().ok_or_else(expired)?;
    recheck_snapshot(s, expected, target.approved()).await?;
    let target = target.clone();
    s.store.read(move |db| target.current(db)).await
}

pub(super) async fn recheck_snapshot(
    s: &Service,
    expected: &SetupDraft,
    approved: &registry::ApprovedPackage,
) -> Result<()> {
    let current = super::super::super::setup::load(s, &expected.id).await?;
    if serde_json::to_value(current)? != serde_json::to_value(expected)? {
        return Err(expired());
    }
    approved.recheck(s).await
}

pub(crate) async fn setup_token(
    s: &Service,
    draft: &SetupDraft,
    approved: &registry::ApprovedPackage,
    owner: &super::super::super::operations::Owner,
    management: Option<&super::super::super::management::Authority>,
) -> Result<Option<String>> {
    let mut connection = draft.connection();
    connection.revision.clone_from(&draft.revision);
    access_token_with(
        s,
        &connection,
        Caller::for_management(management),
        Some(ConfigurationRefresh {
            owner,
            approved,
            setup: Some(draft),
        }),
    )
    .await
}

pub(crate) async fn start_setup(
    s: &Service,
    plugin: &str,
    draft: &str,
    account: &str,
    input: &Value,
    binding: &str,
) -> Result<Value> {
    let target = Target::setup(s, plugin, draft, account).await?;
    let snapshot = target.setup_snapshot().ok_or_else(expired)?;
    if input["revision"] != snapshot.revision {
        return Err(Error::conflict("Setup changed. Save before signing in."));
    }
    if input
        .as_object()
        .is_none_or(|fields| fields.keys().any(|key| key != "revision"))
    {
        return Err(Error::bad("Save the setup settings before signing in."));
    }
    start_for(
        s,
        plugin,
        &json!({"settings": snapshot.settings}),
        binding,
        Some(target),
    )
    .await
}

pub(crate) async fn setup_credential(
    s: &Service,
    plugin: &str,
    flow_id: &str,
    draft: &SetupDraft,
    settings: &Value,
    secrets: &Map<String, Value>,
) -> Result<Value> {
    let flow = {
        let mut flows = s.plugins.oauth_flows.flows.lock().unwrap();
        Flows::prune(&mut flows);
        flows.get(flow_id).cloned().ok_or_else(expired)?
    };
    let target = flow.setup.as_ref().ok_or_else(expired)?;
    if flow.plugin != plugin
        || flow.state != "completed"
        || target
            .setup_snapshot()
            .map(serde_json::to_value)
            .transpose()?
            != Some(serde_json::to_value(draft)?)
    {
        return Err(expired());
    }
    recheck(s, target).await?;
    let oauth = target
        .manifest()
        .credential
        .as_ref()
        .and_then(|credential| credential.oauth.as_ref())
        .ok_or_else(expired)?;
    if flow.settings[&oauth.client_id_setting] != settings[&oauth.client_id_setting]
        || oauth
            .scope_settings
            .keys()
            .any(|key| flow.settings[key] != settings[key])
        || oauth.client_secret_setting.as_ref().is_some_and(|key| {
            secrets.get(key).and_then(Value::as_str) != flow.client_secret.as_deref()
        })
    {
        return Err(Error::bad(
            "The OAuth settings changed. Save the setup and sign in again.",
        ));
    }
    flow.credential.ok_or_else(expired)
}
