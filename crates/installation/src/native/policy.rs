//! Explicit host grants and bounded delegation from authenticated human requests.
use crate::{
    error::{Error, Result},
    service::{Service, policy},
    store::Db,
    validation::text,
};
use serde_json::{Value, json};

tokio::task_local! { pub static CALLER: Option<String>; }

tokio::task_local! { pub(crate) static PARENT: String; }

#[derive(Clone, Default)]
pub(crate) enum Origin {
    Human(String),
    Run(String),
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
/// historical/plugin provenance never turns into a native execution grant.
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
                .list("nodes")?
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
        Origin::Run(id) => {
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
        "nativeProvenanceUnknown",
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
    let agent = db.get("agents", text(task, "agentId"))?.unwrap_or_default();
    let mut grant = provenance(db, source)?;
    let mut people = principals(&grant).as_array().cloned().unwrap_or_default();
    if let Some(previous) = previous {
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
        .get("agents", crate::nodes::run_agent(run))?
        .unwrap_or_default();
    if !native(&agent)
        || !native(&run["snapshot"]["agent"])
        || !node_native(node)
        || node["nativeEnabled"] != true
        || node["revoked"] == true
        || !explicit(&policy(&agent)["nodes"], text(node, "id"))
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
    for _ in 0..16 {
        if scope["nativeProvenanceUnknown"] == true
            || !explicit(&scope["nativeAuthorityNodes"], text(node, "id"))
            || principals(&scope).as_array().is_none_or(Vec::is_empty)
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
            .get("agents", crate::nodes::run_agent(&parent))?
            .unwrap_or_default();
        if !parent["cancelRequestedAt"].is_null()
            || policy(&current)["installation"] != true
            || !crate::installation_mcp::agent_allowed(&current, &parent["snapshot"]["agent"])
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
    let mut run = json!({ "snapshot": { "agent": agent } });
    apply(&mut run, &grant);
    for node in db.list("nodes")? {
        if allows(db, &run, &node)? {
            return Ok(grant);
        }
    }
    Err(Error::forbidden(
        "No native node is explicitly authorized for this agent and its originating callers.",
    ))
}

pub fn authorize_caller(db: &Db<'_>, agent: &Value, caller: Option<&str>) -> Result<()> {
    let source = caller.map_or(Origin::Unknown, |caller| Origin::Human(caller.into()));
    authorize_origin(db, agent, &source).map(|_| ())
}

pub(crate) fn chat_grant(db: &Db<'_>, chat: &Value, source: &Origin) -> Result<Value> {
    let agent = db.get("agents", text(chat, "agentId"))?.unwrap_or_default();
    let run = db.run(text(chat, "runId"))?.unwrap_or_default();
    if run["backendMigration"].is_object() {
        return Err(Error::conflict(
            "Wait for the backend migration to finish before directing this conversation.",
        ));
    }
    if native(&run["snapshot"]["agent"]) != native(&agent) && run["id"].is_string() {
        return Err(Error::conflict(
            "Execution backend changed. Start a new conversation.",
        ));
    }
    let incoming = authorize_origin(db, &agent, source)?;
    let legacy = chat["id"].is_string()
        && run["id"].is_string()
        && !chat["nativeProvenanceUnknown"].is_boolean();
    let mut unknown = legacy
        || chat["nativeProvenanceUnknown"] == true
        || incoming["nativeProvenanceUnknown"] == true;
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
        if native(&agent) {
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
    for node in db.list("nodes")? {
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
        if parent.is_some() && !explicit(&incoming["nativeAuthorityNodes"], text(&node, "id")) {
            continue;
        }
        if chat["nativeParentRunId"].is_string()
            && !explicit(&chat["nativeAuthorityNodes"], text(&node, "id"))
        {
            continue;
        }
        nodes.push(node["id"].clone());
    }
    let grant = json!({
        "nativePrincipals": people,
        "nativeAuthorityNodes": nodes,
        "nativeParentRunId": parent,
        "nativeProvenanceUnknown": unknown,
    });
    if native(&agent) {
        let mut combined = json!({ "snapshot": { "agent": agent } });
        apply(&mut combined, &grant);
        if !db.list("nodes")?.iter().any(|node| {
            run["nodeId"]
                .as_str()
                .is_none_or(|current| node["id"] == current)
                && allows(db, &combined, node).unwrap_or(false)
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
        db.patch_run(text(chat, "runId"), &grant)?;
    }
    Ok(())
}

/// Commit a directing control's provenance in the same transaction as its effect.
pub(crate) fn authorize_chat_in(db: &Db<'_>, id: &str, source: &Origin) -> Result<()> {
    let mut chat = crate::error::required(db.get("chats", id)?, "Chat not found")?;
    authorize_chat(db, &mut chat, source)?;
    db.put("chats", &chat)?;
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
        db.put("chats", &chat)?;
        if let Some(current) = chat["runId"].as_str()
            && current != id
            && db.run(current)?.is_some()
        {
            db.patch_run(current, &grant)?;
        }
    }
    db.patch_run(id, &grant)?;
    Ok(())
}

pub(crate) fn run_authorization(
    id: &str,
) -> impl Fn(&Db<'_>) -> Result<()> + Clone + Send + 'static {
    let (id, source) = (id.to_owned(), origin());
    move |db| authorize_run_in(db, &id, &source)
}

pub async fn refresh(s: &Service) -> Result<()> {
    let recent = s.store.kv("native-authority").await?.is_some_and(|value| {
        value["observedAt"].as_i64().is_some_and(|at| {
            let age = crate::config::now() - at;
            (0..15_000).contains(&age)
        })
    });
    if !recent {
        let accounts = crate::relay::native_authority(s).await?;
        s.store
            .set(
                "native-authority",
                json!({ "observedAt": crate::config::now(), "accounts": accounts }),
                None,
            )
            .await?;
    }
    Ok(())
}

pub async fn refresh_agent(s: &Service, agent: &str) -> Result<()> {
    if native(&s.get("agents", agent).await?) {
        refresh(s).await?;
    }
    Ok(())
}

pub async fn refresh_chat(s: &Service, chat: &str) -> Result<()> {
    let record = s.get("chats", chat).await?;
    refresh_agent(s, text(&record, "agentId")).await
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
        ["chats", id, ..] => {
            let chat = s.get("chats", id).await?;
            s.get("agents", text(&chat, "agentId")).await?
        }
        ["runs", id, ..] | ["nodes", "placement", id, ..] => {
            let run = s.store.run(id).await?;
            if !native(&run["snapshot"]["agent"]) {
                return Ok(());
            }
            s.get("agents", crate::nodes::run_agent(&run)).await?
        }
        _ => return Ok(()),
    };
    if !native(&agent) {
        return Ok(());
    }
    refresh(s).await?;
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
                let chat = crate::error::required(db.get("chats", &target)?, "Chat not found")?;
                chat_grant(db, &chat, &origin)?;
            } else {
                let run = crate::error::required(db.run(&target)?, "Run not found")?;
                run_grant(db, &run, &origin)?;
            }
            Ok(())
        })
        .await
}
