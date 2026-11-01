//! Explicit host grants and bounded delegation from authenticated human requests.
use crate::{
    error::{Error, Result},
    service::{Service, policy},
    store::Db,
    validation::text,
};
use serde_json::{Value, json};

const MAX_AUTHORITY_DEPTH: usize = 16;

tokio::task_local! { pub static CALLER: Option<String>; }

tokio::task_local! { pub(crate) static PARENT: String; }

tokio::task_local! { pub(crate) static DELEGATION: Origin; }

#[derive(Clone)]
pub(crate) struct PluginOrigin {
    pub connection: String,
    pub revision: String,
    pub author: String,
}

#[derive(Clone, Default)]
pub(crate) enum Origin {
    Human(String),
    Run(String),
    DelegatedTask(String),
    Plugin(PluginOrigin),
    Stored(Value),
    #[default]
    Unknown,
}

impl Origin {
    pub(crate) fn known(&self) -> bool {
        !matches!(self, Self::Unknown)
    }
}

pub(crate) fn origin() -> Origin {
    if let Ok(parent) = PARENT.try_with(Clone::clone) {
        return Origin::Run(parent);
    }
    if let Ok(origin) = DELEGATION.try_with(Clone::clone) {
        return origin;
    }
    CALLER
        .try_with(Clone::clone)
        .ok()
        .flatten()
        .map_or(Origin::Unknown, Origin::Human)
}

pub fn caller() -> Option<String> {
    CALLER.try_with(Clone::clone).ok().flatten()
}

pub fn native(agent: &Value) -> bool {
    policy(agent)["backend"] == "native"
}

/// A profile or an unplaced snapshot may authorize either execution kind.
/// Once placed, the run snapshot always records its concrete backend.
pub fn supports(agent: &Value, backend: &str) -> bool {
    let access = policy(agent);
    access["backend"] == "auto"
        || access["backend"] == backend
        || backend == "firecracker" && access["backend"].is_null()
}

pub fn node_native(node: &Value) -> bool {
    node["capabilities"]["backend"] == "native"
}

pub fn principals(run: &Value) -> &Value {
    &run["nativePrincipals"]
}

fn explicit(list: &Value, id: &str) -> bool {
    list.as_array()
        .is_some_and(|values| values.iter().any(|value| value == id))
}

fn current_account(db: &Db<'_>, account: &str) -> Result<bool> {
    let authority = db.kv("native-authority")?.unwrap_or_default();
    Ok(authority["observedAt"]
        .as_i64()
        .is_some_and(|at| (0..=60_000).contains(&(crate::config::now() - at)))
        && authority["accounts"].get(account).is_some())
}

/// These fields are written only after authenticated input parsing. Unknown
/// historical or unapproved plugin provenance never becomes a native grant.
pub(crate) fn provenance(db: &Db<'_>, source: &Origin) -> Result<Value> {
    let unknown = || {
        json!({
            "nativePrincipals": [],
            "nativeAuthorityNodes": [],
            "nativeProvenanceUnknown": true,
        })
    };
    match source {
        Origin::Unknown => Ok(unknown()),
        Origin::Human(account) => {
            let nodes = db
                .nodes_boundary()?
                .into_iter()
                .filter(|node| {
                    node_native(node)
                        && node["nativeEnabled"] == true
                        && node["revoked"] != true
                        && explicit(&node["nativeAccounts"], account)
                })
                .map(|node| node["id"].clone())
                .collect::<Vec<_>>();
            Ok(json!({
                "nativePrincipals": [account],
                "nativeAuthorityNodes": nodes,
                "nativeProvenanceUnknown": false,
            }))
        }
        Origin::Plugin(origin) => {
            let value = crate::error::required(
                db.plugin_instance(&origin.connection)?,
                "Plugin connection no longer exists.",
            )?;
            let connection = value;
            crate::plugins::connections::authorize(db, &connection)?;
            if connection.revision != origin.revision
                || !crate::plugins::connections::external_id(&origin.author)
                || !connection.allows(&origin.author)
                || connection
                    .native_authority
                    .as_ref()
                    .is_none_or(|grant| grant.native_provenance_unknown)
            {
                return Err(Error::forbidden(
                    "This plugin has no current native delegation.",
                ));
            }
            let mut grant = provenance(
                db,
                &Origin::Stored(serde_json::to_value(connection.native_authority)?),
            )?;
            let mut plugins = grant["nativePluginGrants"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            plugins.push(json!({
                "connectionId": origin.connection,
                "revision": origin.revision,
                "authorId": origin.author,
            }));
            grant["nativePluginGrants"] = plugins.into();
            Ok(grant)
        }
        Origin::Run(id) | Origin::DelegatedTask(id) => {
            let Some(run) = db.run(id)? else {
                return Ok(unknown());
            };
            if run["nativeProvenanceUnknown"] == true
                || principals(&run).as_array().is_none_or(Vec::is_empty)
            {
                return Ok(unknown());
            }
            Ok(json!({
                "nativePrincipals": run["nativePrincipals"],
                "nativeAuthorityNodes": run["nativeAuthorityNodes"],
                "nativeParentRunId": id,
                "nativeParentAccess": if matches!(source, Origin::DelegatedTask(_)) {
                    "delegation"
                } else {
                    "installation"
                },
                "nativeProvenanceUnknown": false,
            }))
        }
        Origin::Stored(value) => {
            if value["nativeProvenanceUnknown"] != false {
                return Ok(unknown());
            }
            Ok(json!({
                "nativePrincipals": value["nativePrincipals"],
                "nativeAuthorityNodes": value["nativeAuthorityNodes"],
                "nativeParentRunId": value["nativeParentRunId"],
                "nativeParentAccess": value["nativeParentAccess"],
                "nativePluginGrants": value["nativePluginGrants"],
                "nativeProvenanceUnknown": false,
            }))
        }
    }
}

pub(crate) fn apply(record: &mut Value, grant: &Value) {
    for key in [
        "nativePrincipals",
        "nativeAuthorityNodes",
        "nativeParentRunId",
        "nativeParentAccess",
        "nativeProvenanceUnknown",
        "nativePluginGrants",
    ] {
        record[key] = grant[key].clone();
    }
}

/// Scheduled commitments retain their author and every directing editor. An
/// edit cannot replace an earlier person's host authority with its own.
pub(crate) fn task_grant(
    db: &Db<'_>,
    task: &Value,
    previous: Option<&Value>,
    source: &Origin,
) -> Result<Value> {
    let agent = db
        .agent_snapshot(text(task, "agentId"))?
        .unwrap_or_default();
    let mut grant = provenance(db, source)?;
    let mut people = principals(&grant).as_array().cloned().unwrap_or_default();
    if let Some(previous) = previous {
        merge_plugin_grants(&mut grant, previous);
        for person in principals(previous).as_array().into_iter().flatten() {
            if !people.contains(person) {
                people.push(person.clone());
            }
        }
        if previous["nativeProvenanceUnknown"] != false {
            grant["nativeProvenanceUnknown"] = true.into();
        }
        if previous["nativeParentRunId"].is_string() {
            if grant["nativeParentRunId"].is_string()
                && grant["nativeParentRunId"] != previous["nativeParentRunId"]
            {
                if native(&agent) {
                    return Err(Error::forbidden(
                        "A task cannot combine unrelated native delegation grants.",
                    ));
                }
                grant["nativeProvenanceUnknown"] = true.into();
            }
            grant["nativeParentRunId"] = previous["nativeParentRunId"].clone();
            grant["nativeParentAccess"] = previous["nativeParentAccess"].clone();
            let nodes = grant["nativeAuthorityNodes"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            grant["nativeAuthorityNodes"] = nodes
                .into_iter()
                .filter(|node| {
                    node.as_str()
                        .is_some_and(|node| explicit(&previous["nativeAuthorityNodes"], node))
                })
                .collect::<Vec<_>>()
                .into();
        }
    }
    if let Some(author) = task["authorId"].as_str()
        && !people.iter().any(|person| person == author)
    {
        people.push(author.into());
    }
    grant["nativePrincipals"] = people.into();
    authorize_origin(db, &agent, &Origin::Stored(grant))
}

/// Admission and renewal follow current grants all the way to the human origin.
/// Parent node limits are a ceiling; descendants cannot add a node or caller.
pub fn allows(db: &Db<'_>, run: &Value, node: &Value) -> Result<bool> {
    let agent = db
        .agent_snapshot(crate::nodes::run_agent(run))?
        .unwrap_or_default();
    allows_agent(db, run, node, &agent, Admission::Execution)
}

/// Only the fenced migration read path can substitute the saved backend for the
/// current profile. All principal, node-owner and parent revocations still apply.
pub(crate) fn allows_migration_source(db: &Db<'_>, run: &Value, node: &Value) -> Result<bool> {
    if run["backendMigration"]["sourceReadReady"] != true {
        return Ok(false);
    }
    let mut agent = db
        .agent_snapshot(crate::nodes::run_agent(run))?
        .unwrap_or_default();
    agent["access"]["backend"] = "native".into();
    agent["access"]["nodes"] = json!([node["id"]]);
    let mut source = run.clone();
    apply(&mut source, &run["backendMigration"]["grant"]);
    allows_agent(db, &source, node, &agent, Admission::MigrationSource)
}

pub(crate) fn merge_plugin_grants(grant: &mut Value, previous: &Value) {
    if !previous["nativePluginGrants"].is_null() && !previous["nativePluginGrants"].is_array() {
        grant["nativeProvenanceUnknown"] = true.into();
    }
    let mut plugins = previous["nativePluginGrants"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for plugin in grant["nativePluginGrants"].as_array().into_iter().flatten() {
        if !plugins.contains(plugin) {
            plugins.push(plugin.clone());
        }
    }
    grant["nativePluginGrants"] = plugins.into();
}

/// A plugin delegates its saved native grant, never an external person's claimed
/// Cairn identity. Lease renewal follows that live connection and its owner.
fn plugin_grants_allow(db: &Db<'_>, scope: &Value, node: &Value) -> Result<bool> {
    let grants = &scope["nativePluginGrants"];
    if grants.is_null() {
        return Ok(true);
    }
    let Some(grants) = grants.as_array() else {
        return Ok(false);
    };
    let authority = db.kv("native-authority")?.unwrap_or_default();
    for grant in grants {
        let Some(value) = db.plugin_instance(text(grant, "connectionId"))? else {
            return Ok(false);
        };
        let connection = value;
        let owner = connection
            .native_authority
            .as_ref()
            .map_or("", |grant| grant.plugin_owner_account_id.as_str());
        if crate::plugins::connections::authorize(db, &connection).is_err()
            || !crate::plugins::connections::external_id(text(grant, "authorId"))
            || connection.revision != grant["revision"]
            || !connection.allows(text(grant, "authorId"))
            || connection.agent_id != scope["snapshot"]["agent"]["id"]
            || !current_account(db, owner)?
            || authority["accounts"][owner]["accessId"]
                != serde_json::to_value(&connection.owner_access_id)?
            || connection
                .native_authority
                .as_ref()
                .is_none_or(|grant| grant.native_provenance_unknown)
            || connection.native_authority.as_ref().is_none_or(|grant| {
                !grant
                    .native_authority_nodes
                    .iter()
                    .any(|id| id == text(node, "id"))
            })
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn authority_allows(db: &Db<'_>, run: &Value, node: &Value) -> Result<bool> {
    let agent = db
        .agent_snapshot(crate::nodes::run_agent(run))?
        .unwrap_or_default();
    allows_agent(db, run, node, &agent, Admission::Authority)
}

#[derive(Clone, Copy)]
enum Admission {
    Execution,
    Authority,
    MigrationSource,
}

fn allows_agent(
    db: &Db<'_>,
    run: &Value,
    node: &Value,
    agent: &Value,
    admission: Admission,
) -> Result<bool> {
    if !supports(agent, "native")
        || !supports(&run["snapshot"]["agent"], "native")
        || !node_native(node)
        || node["nativeEnabled"] != true
        || node["revoked"] == true
        || !matches!(admission, Admission::Authority) && !crate::nodes::native_mode::applied(node)
        // A fenced migration reads existing storage; it never launches a workload.
        // Switching backend revokes the agent's administrator execution review.
        || !matches!(admission, Admission::MigrationSource)
            && !crate::nodes::native_mode::reviewed(node, agent)
        || !crate::service::allowed(&policy(agent)["nodes"], text(node, "id"))
    {
        return Ok(false);
    }
    if run["trigger"] == "schedule" {
        let Some(author) = run["snapshot"]["task"]["authorId"].as_str() else {
            return Ok(false);
        };
        if !explicit(&node["nativeAccounts"], author) || !current_account(db, author)? {
            return Ok(false);
        }
    }
    let mut scope = run.clone();
    let mut seen = std::collections::HashSet::new();
    if let Some(id) = run["id"].as_str() {
        // A prospective destination may not yet have this grant in storage.
        // Do not validate a back edge by loading its old parentless version.
        seen.insert(id.to_owned());
    }
    for _ in 0..MAX_AUTHORITY_DEPTH {
        if scope["nativeProvenanceUnknown"] == true
            || !explicit(&scope["nativeAuthorityNodes"], text(node, "id"))
            || principals(&scope).as_array().is_none_or(Vec::is_empty)
            || !plugin_grants_allow(db, &scope, node)?
        {
            return Ok(false);
        }
        for person in principals(&scope).as_array().into_iter().flatten() {
            let Some(person) = person.as_str() else {
                return Ok(false);
            };
            if !explicit(&node["nativeAccounts"], person) || !current_account(db, person)? {
                return Ok(false);
            }
        }
        let Some(parent) = scope["nativeParentRunId"].as_str() else {
            return Ok(true);
        };
        if !seen.insert(parent.to_owned()) {
            return Ok(false);
        }
        let Some(parent) = db.run(parent)? else {
            return Ok(false);
        };
        let current = db
            .agent_snapshot(crate::nodes::run_agent(&parent))?
            .unwrap_or_default();
        // Historical and management-created edges retain the installation requirement.
        // Only delegate_task records the narrower capability on its own parent edge.
        let permission = if scope["nativeParentAccess"] == "delegation" {
            "delegation"
        } else {
            "installation"
        };
        if !parent["cancelRequestedAt"].is_null()
            || policy(&current)[permission] != true
            || policy(&parent["snapshot"]["agent"])[permission] != true
            || !crate::installation_mcp::agent_allowed(&current, &parent["snapshot"]["agent"])
            || permission == "delegation" && !crate::nodes::native_mode::reviewed(node, &current)
        {
            return Ok(false);
        }
        scope = parent;
    }
    Ok(false)
}

pub(crate) fn authorize_origin(db: &Db<'_>, agent: &Value, source: &Origin) -> Result<Value> {
    let grant = provenance(db, source)?;
    if !native(agent) {
        return Ok(grant);
    }

    // Only saved tasks are stored origins. An edit never replaces unknown provenance,
    // so a fresh authenticated commitment is the only way back to native execution.
    if matches!(source, Origin::Stored(_)) && grant["nativeProvenanceUnknown"] != false {
        return Err(Error::forbidden(
            "This mission has no recorded native authority, so it cannot run on a native node. A trusted person must create it again, for example by duplicating it.",
        ));
    }

    let mut run = json!({ "snapshot": { "agent": agent } });
    apply(&mut run, &grant);
    for node in db.nodes_boundary()? {
        if authority_allows(db, &run, &node)? {
            return Ok(grant);
        }
    }
    Err(Error::forbidden(
        "No native node is explicitly authorized for this agent and its originating callers.",
    ))
}

/// A prospective grant must be checked against its destination identity before
/// committing it. Traversing only the old stored graph misses an edge back to
/// a currently parentless destination. The writer transaction serializes these
/// checks with the grant and directing effect, including opposing requests.
pub(crate) fn require_acyclic_parent(db: &Db<'_>, destination: &str, grant: &Value) -> Result<()> {
    if destination.is_empty() {
        // A conversation without a run receives a fresh run identity later.
        return Ok(());
    }
    let mut seen = std::collections::HashSet::from([destination.to_owned()]);
    let mut parent = grant["nativeParentRunId"].as_str().map(str::to_owned);
    for _ in 0..MAX_AUTHORITY_DEPTH {
        let Some(id) = parent else {
            return Ok(());
        };
        if !seen.insert(id.clone()) {
            return Err(Error::forbidden(
                "This directing action would create a cyclic native authority lineage.",
            ));
        }
        // A missing parent cannot introduce a cycle. Native authority still
        // rejects it through the separate live admission checks.
        parent = db.native_parent_run(&id)?;
    }
    Err(Error::forbidden(
        "Native authority lineage is too deep to extend safely.",
    ))
}

pub(crate) fn chat_grant(db: &Db<'_>, chat: &Value, source: &Origin) -> Result<Value> {
    let agent = db
        .agent_snapshot(text(chat, "agentId"))?
        .unwrap_or_default();
    let run = db.run(text(chat, "runId"))?.unwrap_or_default();
    if run["backendMigration"].is_object() {
        return Err(Error::conflict(
            "Wait for the backend migration to finish before directing this conversation.",
        ));
    }
    if !supports(&agent, text(&policy(&run["snapshot"]["agent"]), "backend"))
        && policy(&agent)["backend"] != "auto"
        && run["id"].is_string()
    {
        return Err(Error::conflict(
            "Execution backend changed. Migrate this conversation from its execution-node details before continuing.",
        ));
    }
    // An automatic profile follows the concrete environment of this conversation.
    // New conversations retain provenance even when only a VM can admit it.
    let mut effective = agent.clone();
    if policy(&agent)["backend"] == "auto" && run["id"].is_string() {
        effective["access"]["backend"] = policy(&run["snapshot"]["agent"])["backend"].clone();
    }
    // Directing one's own conversation retains its existing ancestry. Treating
    // this as delegation would make the run its own parent and invalidate leases.
    let directing_self = matches!(source, Origin::Run(id) if run["id"] == *id);
    let retained;
    let source = if directing_self {
        retained = Origin::Stored(run.clone());
        &retained
    } else {
        source
    };
    let incoming = authorize_origin(db, &effective, source)?;
    if chat["id"].is_string()
        && run["id"].is_string()
        && !chat["nativeProvenanceUnknown"].is_boolean()
    {
        return Err(Error::conflict(
            "Saved conversation authority is incomplete.",
        ));
    }
    let mut unknown =
        chat["nativeProvenanceUnknown"] == true || incoming["nativeProvenanceUnknown"] == true;
    let mut people = chat["nativePrincipals"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for person in incoming["nativePrincipals"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if !people.contains(person) {
            people.push(person.clone());
        }
    }
    let parent = chat["nativeParentRunId"]
        .as_str()
        .or(incoming["nativeParentRunId"].as_str())
        .map(str::to_owned);
    if chat["nativeParentRunId"].is_string()
        && incoming["nativeParentRunId"].is_string()
        && chat["nativeParentRunId"] != incoming["nativeParentRunId"]
    {
        if native(&effective) {
            return Err(Error::forbidden(
                "A native conversation cannot combine unrelated delegation grants.",
            ));
        }
        // One parent pointer cannot retain both revocation paths. The VM may
        // continue ordinary work, but neither existing nor new native descendants
        // may inherit a partial lineage.
        unknown = true;
    }
    let mut nodes = Vec::new();
    for node in db.nodes_boundary()? {
        if !node_native(&node) || node["nativeEnabled"] != true || node["revoked"] == true {
            continue;
        }
        if !people.iter().all(|person| {
            person
                .as_str()
                .is_some_and(|person| explicit(&node["nativeAccounts"], person))
        }) {
            continue;
        }
        if (parent.is_some() || directing_self)
            && !explicit(&incoming["nativeAuthorityNodes"], text(&node, "id"))
        {
            continue;
        }
        if chat["nativeParentRunId"].is_string()
            && !explicit(&chat["nativeAuthorityNodes"], text(&node, "id"))
        {
            continue;
        }
        nodes.push(node["id"].clone());
    }
    let mut grant = json!({
        "nativePrincipals": people,
        "nativeAuthorityNodes": nodes,
        "nativeParentRunId": parent,
        "nativeParentAccess": if chat["nativeParentRunId"].is_string() {
            chat["nativeParentAccess"].clone()
        } else {
            incoming["nativeParentAccess"].clone()
        },
        "nativeProvenanceUnknown": unknown,
    });
    grant["nativePluginGrants"] = incoming["nativePluginGrants"].clone();
    merge_plugin_grants(&mut grant, chat);
    require_acyclic_parent(db, text(&run, "id"), &grant)?;
    if native(&effective) {
        let mut combined = json!({ "snapshot": { "agent": effective } });
        apply(&mut combined, &grant);
        if !db.nodes_boundary()?.iter().any(|node| {
            run["nodeId"]
                .as_str()
                .is_none_or(|current| node["id"] == current)
                && authority_allows(db, &combined, node).unwrap_or(false)
        }) {
            return Err(Error::forbidden(
                "The conversation's callers have no common authorized native node.",
            ));
        }
    }
    Ok(grant)
}

pub(crate) fn authorize_chat(db: &Db<'_>, chat: &mut Value, source: &Origin) -> Result<()> {
    let grant = chat_grant(db, chat, source)?;
    apply(chat, &grant);
    if db.run(text(chat, "runId"))?.is_some() {
        db.update_run_boundary(text(chat, "runId"), &grant)?;
    }
    Ok(())
}

/// Commit a directing control's provenance in the same transaction as its effect.
pub(crate) fn authorize_chat_in(db: &Db<'_>, id: &str, source: &Origin) -> Result<()> {
    let mut chat = crate::error::required(db.conversation_boundary(id)?, "Chat not found")?;
    authorize_chat(db, &mut chat, source)?;
    db.save_conversation_boundary(&chat)?;
    Ok(())
}

pub(crate) fn chat_authorization(
    id: &str,
) -> impl Fn(&Db<'_>) -> Result<()> + Clone + Send + 'static {
    let (id, source) = (id.to_owned(), origin());
    move |db| authorize_chat_in(db, &id, &source)
}

/// Task runs have the same retained caller and delegation bounds as chat runs.
/// This read-only grant can also constrain destination selection before a move.
pub(crate) fn run_grant(db: &Db<'_>, run: &Value, source: &Origin) -> Result<Value> {
    let chat = crate::chats::chat_for_run(db, text(run, "id"))?.unwrap_or_else(|| {
        let mut record = run.clone();
        record["agentId"] = crate::nodes::run_agent(run).into();
        record["runId"] = run["id"].clone();
        record
    });
    chat_grant(db, &chat, source)
}

pub(crate) fn authorize_run_in(db: &Db<'_>, id: &str, source: &Origin) -> Result<()> {
    let run = crate::error::required(db.run(id)?, "Run not found")?;
    let grant = run_grant(db, &run, source)?;
    if let Some(mut chat) = crate::chats::chat_for_run(db, id)? {
        apply(&mut chat, &grant);
        db.save_conversation_boundary(&chat)?;
        if let Some(current) = chat["runId"].as_str()
            && current != id
            && db.run(current)?.is_some()
        {
            db.update_run_boundary(current, &grant)?;
        }
    }
    db.update_run_boundary(id, &grant)?;
    Ok(())
}

pub(crate) fn run_authorization(
    id: &str,
) -> impl Fn(&Db<'_>) -> Result<()> + Clone + Send + 'static {
    let (id, source) = (id.to_owned(), origin());
    move |db| authorize_run_in(db, &id, &source)
}

fn authority_age(value: &Value) -> Option<i64> {
    value["observedAt"]
        .as_i64()
        .map(|at| crate::config::now() - at)
        .filter(|age| *age >= 0)
}

pub async fn refresh(s: &Service) -> Result<()> {
    let _refresh = s.native_authority_refresh.lock().await;
    refresh_locked(s).await
}

async fn refresh_locked(s: &Service) -> Result<()> {
    let recent = s
        .store
        .kv("native-authority")
        .await?
        .as_ref()
        .and_then(authority_age)
        .is_some_and(|age| age < 15_000);
    if recent {
        return Ok(());
    }
    let started = tokio::time::Instant::now();
    let accounts = crate::relay::native_authority(s).await?;
    s.store
        .set(
            "native-authority",
            json!({ "observedAt": crate::config::now(), "accounts": accounts }),
            None,
        )
        .await?;
    tracing::debug!(target: "cairn_performance", elapsed_ms = started.elapsed().as_millis() as u64, "Native authority refreshed");
    Ok(())
}

/// A still-valid proof can renew existing work while its refresh runs in the
/// background. The lease writer caps grants at that proof's original deadline.
pub(crate) async fn refresh_for_heartbeat(s: &Service) -> Result<()> {
    let age = s
        .store
        .kv("native-authority")
        .await?
        .as_ref()
        .and_then(authority_age);
    if !age.is_some_and(|age| age < 60_000) {
        return refresh(s).await;
    }
    if age.is_some_and(|age| age < 15_000) {
        return Ok(());
    }
    if let Ok(guard) = s.native_authority_refresh.clone().try_lock_owned() {
        let s = s.clone();
        tokio::spawn(async move {
            let _guard = guard;
            if let Err(error) = refresh_locked(&s).await {
                tracing::warn!(
                    status = error.status,
                    "Native authority refresh failed; existing grants retain their original authority deadline"
                );
            }
        });
    }
    Ok(())
}

pub(crate) fn lease_remaining_ms(db: &Db<'_>) -> Result<u64> {
    let authority = db.kv("native-authority")?.unwrap_or_default();
    Ok(authority_age(&authority).map_or(0, |age| 60_000_i64.saturating_sub(age).max(0) as u64))
}

pub async fn refresh_agent(s: &Service, agent: &str) -> Result<()> {
    refresh_for(s, &s.agent_snapshot(agent).await?).await
}

/// Native authority must never become a dependency of VM admission. An automatic
/// profile may fall back to a VM when fresh host authority cannot be obtained.
/// Invalidate stale evidence so that fallback cannot accidentally admit native work.
pub(crate) async fn refresh_for(s: &Service, agent: &Value) -> Result<()> {
    if native(agent) {
        return refresh(s).await;
    }
    if policy(agent)["backend"] != "auto" {
        return Ok(());
    }
    let has_native = s.store.nodes_boundary().await?.iter().any(|node| {
        node_native(node)
            && node["revoked"] != true
            && node["nativeEnabled"] == true
            && crate::nodes::agent_allows(agent, text(node, "id"))
    });
    if !has_native {
        return Ok(());
    }
    if let Err(error) = refresh(s).await {
        s.store
            .set(
                "native-authority",
                json!({ "observedAt": 0, "accounts": {} }),
                None,
            )
            .await?;
        tracing::warn!(
            status = error.status,
            "Native authority unavailable; automatic placement is limited to VM nodes"
        );
    }
    Ok(())
}

fn for_environment(mut agent: Value, run: &Value) -> Value {
    if policy(&agent)["backend"] == "auto"
        && let Some(backend @ ("native" | "firecracker")) =
            run["snapshot"]["agent"]["access"]["backend"].as_str()
    {
        agent["access"]["backend"] = backend.into();
    }
    agent
}

async fn chat_agent(s: &Service, chat: &str) -> Result<Value> {
    let chat = chat.to_owned();
    s.store
        .read(move |db| {
            let chat = crate::error::required(db.conversation_boundary(&chat)?, "Chat not found")?;
            let agent = crate::error::required(
                db.agent_snapshot(text(&chat, "agentId"))?,
                "Agent not found",
            )?;
            let run = db.run(text(&chat, "runId"))?.unwrap_or_default();
            Ok(for_environment(agent, &run))
        })
        .await
}

pub async fn refresh_chat(s: &Service, chat: &str) -> Result<()> {
    refresh_for(s, &chat_agent(s, chat).await?).await
}

/// Cover resume, retries, question responses and other mutation routes, including
/// routes which reuse a previously authorized conversation rather than queueing one.
/// This admission preflight is read-only. Accepted effects record provenance in
/// their own transaction so invalid or unsupported requests never acquire it.
pub async fn authorize_request(s: &Service, input: &crate::http::Input) -> Result<()> {
    if matches!(input.method.as_str(), "GET" | "HEAD" | "DELETE") {
        return Ok(());
    }
    let parts = input
        .path
        .trim_start_matches("/api/")
        .split('/')
        .collect::<Vec<_>>();
    if input.method == "POST"
        && matches!(parts.as_slice(), ["runs" | "chats", _, "timeline-content"])
    {
        // This endpoint reads an immutable source snapshot; use the same admission as history GET.
        return Ok(());
    }
    let reduces_execution = input.method == "POST"
        && match parts.as_slice() {
            ["runs", _, "cancel" | "cleanup"] | ["chats", _, "stop"] => true,
            ["chats", _, "pause"] => input.body["paused"] == true,
            _ => false,
        };
    if reduces_execution {
        return Ok(());
    }
    let agent = match parts.as_slice() {
        ["chats", id, ..] => chat_agent(s, id).await?,
        ["runs", id, ..] | ["nodes", "placement", id, ..] => {
            let run = s.store.run(id).await?;
            if !native(&run["snapshot"]["agent"]) {
                return Ok(());
            }
            for_environment(s.agent_snapshot(crate::nodes::run_agent(&run)).await?, &run)
        }
        _ => return Ok(()),
    };
    if !supports(&agent, "native") {
        return Ok(());
    }
    refresh_for(s, &agent).await?;
    let caller = input
        .identity
        .as_ref()
        .filter(|identity| identity.mcp_scopes.is_none())
        .map(|identity| identity.account_id.clone());
    let (target, chat_target) = match parts.as_slice() {
        ["chats", id, ..] => ((*id).to_owned(), true),
        ["runs", id, ..] | ["nodes", "placement", id, ..] => ((*id).to_owned(), false),
        _ => return Ok(()),
    };
    s.store
        .read(move |db| {
            let origin = caller.map_or(Origin::Unknown, Origin::Human);
            authorize_origin(db, &agent, &origin)?;
            if chat_target {
                let chat =
                    crate::error::required(db.conversation_boundary(&target)?, "Chat not found")?;
                chat_grant(db, &chat, &origin)?;
            } else {
                let run = crate::error::required(db.run(&target)?, "Run not found")?;
                run_grant(db, &run, &origin)?;
            }
            Ok(())
        })
        .await
}
