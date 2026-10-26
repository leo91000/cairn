use crate::{
    config::now,
    error::{Error, Result},
    provider::Provider,
    rpc::Session,
    service::Service,
    validation::text,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    time::Duration,
};
use tokio::sync::Mutex;

const TTL: i64 = 300_000;
/// Usage polls refresh a catalog this long before it expires, so routing and the speed check
/// keep a fresh one between polls (one a minute).
const REFRESH_AHEAD: i64 = 120_000;
/// How long a catalog's speeds decide routing and the speed check. Plans change rarely, and usage
/// polls skip a busy account, so this outlasts the picker's freshness.
const SPEEDS_TTL: i64 = 30 * 60_000;

#[derive(Default)]
pub struct Models {
    refresh: Mutex<()>,
}

pub async fn discover(session: &mut Session) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(20), discover_pages(session))
        .await
        .unwrap_or_else(|_| Err(Error::gateway_timeout("Codex model discovery timed out.")))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReasoningEffort {
    reasoning_effort: Value,
    description: String,
}

/// A speed Codex offers for a model, such as `priority`, which it names Fast. Its
/// description is Codex's own wording of the speed and usage trade-off.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ServiceTier {
    id: String,
    name: String,
    description: String,
}

/// One row of the cached Codex catalog, `codex-models:{account}`. Rows cached before
/// speeds were discovered have no `serviceTiers`: their speeds are unknown, not absent.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexModel {
    model: String,
    display_name: String,
    description: String,
    hidden: bool,
    is_default: bool,
    default_reasoning_effort: String,
    supported_reasoning_efforts: Vec<ReasoningEffort>,
    service_tiers: Vec<ServiceTier>,
    default_service_tier: Option<String>,
}

/// The speed a run or message asks for: empty inherits, `standard` asks for Codex's
/// standard speed, anything else names a catalog service tier.
pub const STANDARD_SPEED: &str = "standard";

/// What Codex calls its standard speed in `serviceTier`.
pub const CODEX_STANDARD_TIER: &str = "default";

fn valid_effort(effort: &str) -> bool {
    !effort.is_empty()
        && effort.len() <= 40
        && effort
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Codex's tier IDs share the shape of effort names. `default` is its standard speed,
/// which Cairn offers as its own choice rather than as a tier.
fn valid_tier(tier: &str) -> bool {
    valid_effort(tier) && tier != CODEX_STANDARD_TIER && tier != STANDARD_SPEED
}

fn parse_tiers(row: &Value) -> Vec<ServiceTier> {
    let mut tiers: Vec<ServiceTier> = Vec::new();
    for tier in row["serviceTiers"].as_array().into_iter().flatten() {
        let id = text(tier, "id");
        if !valid_tier(id) || tiers.iter().any(|known| known.id == id) {
            continue;
        }
        let name = match text(tier, "name").trim() {
            "" => id,
            name => name,
        };
        let description = text(tier, "description").trim();
        tiers.push(ServiceTier {
            id: id.to_owned(),
            name: name.chars().take(60).collect(),
            description: description.chars().take(300).collect(),
        });
    }
    tiers
}

fn parse_model(row: &Value) -> Result<CodexModel> {
    let model = text(row, "model");
    let Some(efforts) = row["supportedReasoningEfforts"].as_array() else {
        return Err(Error::bad_gateway("Codex returned an invalid model."));
    };
    if model.is_empty() || model.len() > 120 {
        return Err(Error::bad_gateway("Codex returned an invalid model."));
    }
    let display_name = match text(row, "displayName") {
        "" => model,
        name => name,
    };
    Ok(CodexModel {
        model: model.to_owned(),
        display_name: display_name.to_owned(),
        description: text(row, "description").to_owned(),
        hidden: row["hidden"] == true,
        is_default: row["isDefault"] == true,
        default_reasoning_effort: text(row, "defaultReasoningEffort").to_owned(),
        supported_reasoning_efforts: efforts
            .iter()
            .filter(|e| valid_effort(text(e, "reasoningEffort")))
            .map(|e| ReasoningEffort {
                reasoning_effort: e["reasoningEffort"].clone(),
                description: text(e, "description").to_owned(),
            })
            .collect(),
        service_tiers: parse_tiers(row),
        default_service_tier: Some(text(row, "defaultServiceTier"))
            .filter(|tier| valid_tier(tier))
            .map(str::to_owned),
    })
}

async fn discover_pages(session: &mut Session) -> Result<Value> {
    let mut models = BTreeMap::new();
    let mut cursor = Value::Null;
    let mut seen = HashSet::new();
    for _ in 0..20 {
        let page = session
            .request(
                "model/list",
                json!({
                    "limit": 100,
                    "includeHidden": true,
                    "cursor": cursor
                }),
            )
            .await?;
        let rows = page["data"]
            .as_array()
            .ok_or_else(|| Error::bad_gateway("Codex returned an invalid model list."))?;
        for row in rows {
            let model = parse_model(row)?;
            models.insert(model.model.clone(), model);
        }
        cursor = page["nextCursor"].clone();
        if cursor.is_null() {
            return Ok(serde_json::to_value(
                models.into_values().collect::<Vec<_>>(),
            )?);
        }
        if !cursor.is_string() || !seen.insert(cursor.to_string()) {
            break;
        }
    }
    Err(Error::bad_gateway(
        "Codex model pagination did not complete.",
    ))
}

fn due(cached: &Value) -> bool {
    due_within(cached, TTL)
}

fn due_within(cached: &Value, age: i64) -> bool {
    cached["checkedAt"]
        .as_i64()
        .is_none_or(|at| now() - at > age)
        && cached["attemptedAt"]
            .as_i64()
            .is_none_or(|at| now() - at > 30_000)
}

async fn record(s: &Service, key: &str, mut cached: Value, result: Result<Value>) -> Result<Value> {
    if cached.is_null() {
        cached = json!({});
    }
    cached["attemptedAt"] = now().into();
    if let Ok(data) = result {
        cached["models"] = data;
        cached["checkedAt"] = now().into();
    }
    s.store.set(key, cached.clone(), None).await?;
    Ok(cached)
}

pub async fn refresh_from_session(s: &Service, source: &str, session: &mut Session) -> Result<()> {
    let key = format!("codex-models:{source}");
    let cached = s.store.kv(&key).await?.unwrap_or(Value::Null);
    if due_within(&cached, TTL - REFRESH_AHEAD) {
        record(s, &key, cached, discover(session).await).await?;
    }
    Ok(())
}

/// Combines one model listed by several accounts: only effort levels supported by
/// every account, hidden only when hidden everywhere, default when default anywhere.
/// Speeds are offered when any account offers them, since runs prefer an account that
/// does; `limited` marks a speed that some of these accounts lack or do not confirm.
fn intersect(existing: &mut Value, model: &Value) {
    let supported = model["supportedReasoningEfforts"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    if let Some(efforts) = existing["supportedReasoningEfforts"].as_array_mut() {
        efforts.retain(|e| {
            supported
                .iter()
                .any(|other| other["reasoningEffort"] == e["reasoningEffort"])
        });
    }
    existing["hidden"] = (existing["hidden"] == true && model["hidden"] == true).into();
    existing["isDefault"] = (existing["isDefault"] == true || model["isDefault"] == true).into();
    merge_tiers(existing, model);
}

fn merge_tiers(existing: &mut Value, model: &Value) {
    // A catalog cached before speeds were discovered says nothing about them.
    let Some(offered) = model["serviceTiers"].as_array() else {
        return;
    };
    let Some(tiers) = existing["serviceTiers"].as_array_mut() else {
        existing["serviceTiers"] = model["serviceTiers"].clone();
        existing["defaultServiceTier"] = model["defaultServiceTier"].clone();
        return;
    };

    for tier in tiers.iter_mut() {
        if !offered.iter().any(|other| other["id"] == tier["id"]) {
            tier["limited"] = true.into();
        }
    }
    for tier in offered {
        if !tiers.iter().any(|known| known["id"] == tier["id"]) {
            let mut tier = tier.clone();
            tier["limited"] = true.into();
            tiers.push(tier);
        }
    }

    // Accounts that disagree on the default leave it to each run's account.
    if existing["defaultServiceTier"] != model["defaultServiceTier"] {
        existing["defaultServiceTier"] = Value::Null;
    }
}

/// Marks `limited` a speed that some account able to run the model does not confirm: one whose
/// catalog is unknown, outdated or silent about the model's speeds. A run there asks for it
/// without knowing whether Codex applies it.
fn mark_unconfirmed(model: &mut Value, catalogs: &[Value]) {
    let name = text(model, "model").to_owned();
    // As routing: an account whose fresh catalog lacks the model runs it only when no other
    // fresh catalog lists the model.
    let listed_fresh = catalogs
        .iter()
        .any(|catalog| fresh(catalog) && catalog_row(catalog, &name).is_some());
    let runs_elsewhere =
        |catalog: &Value| listed_fresh && fresh(catalog) && catalog_row(catalog, &name).is_none();
    let Some(tiers) = model["serviceTiers"].as_array_mut() else {
        return;
    };
    for tier in tiers {
        let id = text(tier, "id").to_owned();
        let unconfirmed = catalogs.iter().any(|catalog| {
            !runs_elsewhere(catalog) && offers_tier(catalog, &name, &id) != Some(true)
        });
        if unconfirmed {
            tier["limited"] = true.into();
        }
    }
}

/// Whether a catalog is recent enough to route models, as the picker's own freshness.
fn fresh(catalog: &Value) -> bool {
    catalog["checkedAt"]
        .as_i64()
        .and_then(|at| now().checked_sub(at))
        .is_some_and(|age| (0..=TTL).contains(&age))
}

async fn discover_local(s: &Service) -> Result<Value> {
    let mut session = Session::codex(&s.config, &s.config.home.join(".codex"), &[], None).await?;
    let result = discover(&mut session).await;
    session.close().await;
    result
}

impl Models {
    pub async fn list(&self, s: &Service) -> Result<Value> {
        // Deduplicate simultaneous editor/chat requests, with a short retry backoff.
        let _guard = self.refresh.lock().await;
        s.accounts.initialize(s).await?;
        let accounts = s
            .accounts
            .records(s, Provider::Codex)
            .await?
            .into_iter()
            .filter(|a| a["enabled"] == true)
            .collect::<Vec<_>>();
        let sources = if accounts.is_empty() && !Provider::Codex.driver().managed(s).await? {
            vec![String::new()]
        } else {
            accounts.iter().map(|a| text(a, "id").to_owned()).collect()
        };
        let mut models = BTreeMap::<String, Value>::new();
        let mut catalogs = Vec::new();
        let mut stale = false;
        let mut checked_at: Option<i64> = None;
        for source in &sources {
            let key = format!("codex-models:{source}");
            let mut cached = s.store.kv(&key).await?.unwrap_or(Value::Null);
            if due(&cached) {
                let result = if source.is_empty() {
                    discover_local(s).await
                } else {
                    crate::accounts::codex::discover_models(s, source).await
                };
                cached = record(s, &key, cached, result).await?;
            }
            let checked = cached["checkedAt"].as_i64();
            stale |= checked.is_none_or(|at| now() - at > TTL);
            if let Some(at) = checked {
                checked_at = Some(checked_at.map_or(at, |old| old.min(at)));
            }
            for model in cached["models"].as_array().into_iter().flatten() {
                let name = text(model, "model").to_owned();
                match models.get_mut(&name) {
                    Some(existing) => intersect(existing, model),
                    None => {
                        models.insert(name, model.clone());
                    }
                }
            }
            // Only accounts a run can land on decide whether a speed is confirmed.
            let signed_in = source.is_empty()
                || accounts
                    .iter()
                    .any(|a| text(a, "id") == source && a["state"] == "ready");
            if signed_in {
                catalogs.push(cached);
            }
        }
        let mut models = models.into_values().collect::<Vec<_>>();
        // Only accounts a run can land on decide which speeds are limited.
        for model in &mut models {
            for tier in model["serviceTiers"].as_array_mut().into_iter().flatten() {
                if let Some(tier) = tier.as_object_mut() {
                    tier.remove("limited");
                }
            }
            mark_unconfirmed(model, &catalogs);
        }
        let error = if sources.is_empty() {
            "Connect a Codex account to load available models."
        } else if models.is_empty() {
            "Unable to load Codex models. Check the connection and retry."
        } else if stale {
            "Model list could not be refreshed. Showing the last available options."
        } else {
            ""
        };
        Ok(json!({
            "models": models,
            "checkedAt": checked_at,
            "stale": stale,
            "error": error,
        }))
    }
}

/// Whether `account` can run `model` at `speed`. Unknown capabilities never block a model; a
/// speed only orders accounts, behind those whose catalog confirms it.
pub async fn account_supports(
    s: &Service,
    account: &str,
    model: &str,
    speed: &str,
) -> Result<bool> {
    Ok(account_offers_model(s, account, model).await?
        && account_offers_speed(s, account, model, speed).await?)
}

fn speeds_known(cached: &Value) -> bool {
    cached["checkedAt"]
        .as_i64()
        .and_then(|at| now().checked_sub(at))
        .is_some_and(|age| (0..=SPEEDS_TTL).contains(&age))
}

/// The catalog row a run of `model` uses; an empty model is the catalog default.
fn catalog_row<'a>(cached: &'a Value, model: &str) -> Option<&'a Value> {
    cached["models"].as_array()?.iter().find(|row| {
        if model.is_empty() {
            row["isDefault"] == true
        } else {
            row["model"] == model
        }
    })
}

/// Whether a fresh catalog offers `tier` for `model`, or `None` when it cannot tell. An empty
/// model is the catalog default, which only orders accounts: the turn checks the model it runs.
fn offers_tier(cached: &Value, model: &str, tier: &str) -> Option<bool> {
    if !speeds_known(cached) {
        return None;
    }
    tier_listed(cached, model, tier)
}

/// Whether a catalog lists `tier` for `model`, or `None` when it does not list the model or its
/// speeds.
fn tier_listed(catalog: &Value, model: &str, tier: &str) -> Option<bool> {
    let tiers = catalog_row(catalog, model)?["serviceTiers"].as_array()?;
    Some(tiers.iter().any(|offered| offered["id"] == tier))
}

/// What the Codex process running a turn says about the `tier` it is asked for.
#[derive(Debug, PartialEq, Eq)]
pub enum TierCheck {
    /// Its catalog lists the tier for the model.
    Offered,
    /// Its catalog lists the model without the tier: Codex would silently run Standard.
    Lacking,
    /// Its catalog could not be read, or does not list the model exactly. Codex applies a listed
    /// model's tiers to some other names, such as dated versions, and drops them for the rest.
    Unconfirmed,
}

/// Checks `tier` against the model list of the Codex process about to run the turn, the
/// catalog Codex itself applies, rather than Cairn's cached copy of an account's.
pub async fn check_tier(session: &mut Session, model: &str, tier: &str) -> TierCheck {
    let Ok(models) = discover(session).await else {
        return TierCheck::Unconfirmed;
    };
    match tier_listed(&json!({ "models": models }), model, tier) {
        Some(true) => TierCheck::Offered,
        Some(false) => TierCheck::Lacking,
        None => TierCheck::Unconfirmed,
    }
}

async fn account_offers_speed(
    s: &Service,
    account: &str,
    model: &str,
    speed: &str,
) -> Result<bool> {
    if speed.is_empty() || speed == STANDARD_SPEED {
        return Ok(true);
    }
    // An account whose catalog confirms the speed comes first. One that lacks it, or cannot
    // tell, comes after any enabled account whose catalog confirms it.
    let own = s.store.kv(&format!("codex-models:{account}")).await?;
    if own.is_some_and(|cached| offers_tier(&cached, model, speed) == Some(true)) {
        return Ok(true);
    }
    for source in s.accounts.records(s, Provider::Codex).await? {
        if source["enabled"] != true || text(&source, "id") == account {
            continue;
        }
        let key = format!("codex-models:{}", text(&source, "id"));
        if let Some(catalog) = s.store.kv(&key).await?
            && offers_tier(&catalog, model, speed) == Some(true)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The speeds `account`'s recent cached catalog lists, by model, or `None` when it has none. The
/// turn needs them as well as its Codex process's list to confirm a tier: Codex answers with its
/// built-in list when it cannot fetch the account's.
pub async fn account_tiers(s: &Service, account: &str) -> Result<Option<Value>> {
    let Some(cached) = s.store.kv(&format!("codex-models:{account}")).await? else {
        return Ok(None);
    };
    if !speeds_known(&cached) {
        return Ok(None);
    }

    let mut tiers = serde_json::Map::new();
    for row in cached["models"].as_array().into_iter().flatten() {
        if let Some(listed) = row["serviceTiers"].as_array() {
            let ids = listed.iter().map(|tier| tier["id"].clone()).collect();
            tiers.insert(text(row, "model").to_owned(), Value::Array(ids));
        }
    }
    Ok(Some(Value::Object(tiers)))
}

/// Codex's own name for Standard is not a speed choice: `standard` asks for it.
pub fn validate_speed(speed: &str) -> Result<()> {
    if speed == CODEX_STANDARD_TIER {
        return Err(Error::bad(
            "Choose the standard speed as `standard`, or leave the speed empty to inherit it.",
        ));
    }
    Ok(())
}

/// The `serviceTier` a run asks Codex for. A faster tier is then checked by the turn itself,
/// against the Codex process that runs it ([`check_tier`]), which alone decides and reports it.
pub fn service_tier(speed: &str) -> &str {
    match speed {
        // Codex's app server applies no model default and keeps a thread's earlier tier:
        // a turn without a chosen speed asks for Standard, as it runs.
        "" | STANDARD_SPEED => CODEX_STANDARD_TIER,
        tier => tier,
    }
}

// Preserve existing/custom model configurations when there is no current catalog.
async fn account_offers_model(s: &Service, account: &str, model: &str) -> Result<bool> {
    if model.is_empty() {
        return Ok(true);
    }
    let Some(cached) = s.store.kv(&format!("codex-models:{account}")).await? else {
        return Ok(true);
    };
    if cached["checkedAt"]
        .as_i64()
        .is_none_or(|at| now() - at > TTL)
    {
        return Ok(true);
    }
    if cached["models"]
        .as_array()
        .is_none_or(|models| models.iter().any(|m| m["model"] == model))
    {
        return Ok(true);
    }
    // Preserve provider aliases absent from every catalog. Restrict routing only
    // for a model actually discovered on another enabled account.
    for source in s.accounts.records(s, Provider::Codex).await? {
        if source["enabled"] != true {
            continue;
        }
        if let Some(catalog) = s
            .store
            .kv(&format!("codex-models:{}", text(&source, "id")))
            .await?
            && catalog["checkedAt"]
                .as_i64()
                .is_some_and(|at| now() - at <= TTL)
            && catalog["models"]
                .as_array()
                .is_some_and(|models| models.iter().any(|m| m["model"] == model))
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Use only this account's fresh capabilities; cache misses retain guest discovery.
/// Bind model and effort together so a provider default change cannot mix capabilities.
pub async fn cached_defaults(
    s: &Service,
    account: &str,
    model: &str,
) -> Result<Option<(String, String)>> {
    let cached = s.store.kv(&format!("codex-models:{account}")).await?;
    Ok(cached
        .as_ref()
        .and_then(|cached| defaults(cached, model))
        .map(|(model, effort)| (model.to_owned(), effort.to_owned())))
}

fn defaults<'a>(cached: &'a Value, model: &str) -> Option<(&'a str, &'a str)> {
    let age = now().checked_sub(cached["checkedAt"].as_i64()?)?;
    if !(0..=TTL).contains(&age) {
        return None;
    }
    let selected = cached["models"].as_array()?.iter().find(|row| {
        if model.is_empty() {
            row["isDefault"] == true
        } else {
            row["model"] == model
        }
    })?;
    let effort = text(selected, "defaultReasoningEffort");
    if text(selected, "model").is_empty()
        || effort.is_empty()
        || !selected["supportedReasoningEfforts"]
            .as_array()?
            .iter()
            .any(|e| e["reasoningEffort"] == effort)
    {
        return None;
    }
    Some((text(selected, "model"), effort))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_efforts_require_fresh_matching_capabilities() {
        let mut cached = json!({
            "checkedAt": now(),
            "models": [
                {
                    "model": "default-model",
                    "isDefault": true,
                    "defaultReasoningEffort": "high",
                    "supportedReasoningEfforts": [{
                        "reasoningEffort": "high"
                    }]
                },
                {
                    "model": "fast-model",
                    "defaultReasoningEffort": "low",
                    "supportedReasoningEfforts": [{
                        "reasoningEffort": "low"
                    }]
                }
            ]
        });
        assert_eq!(defaults(&cached, ""), Some(("default-model", "high")));
        assert_eq!(defaults(&cached, "fast-model"), Some(("fast-model", "low")));
        assert_eq!(defaults(&cached, "custom-alias"), None);
        cached["models"][0]["defaultReasoningEffort"] = "unsupported".into();
        assert_eq!(defaults(&cached, ""), None);
        cached["checkedAt"] = (now() - TTL - 1).into();
        assert_eq!(defaults(&cached, "fast-model"), None);
        cached["checkedAt"] = (now() + 60_000).into();
        assert_eq!(defaults(&cached, "fast-model"), None);
        assert_eq!(defaults(&Value::Null, ""), None);
    }

    #[test]
    fn catalog_rows_keep_codex_speeds_and_reject_invalid_or_standard_ids() {
        let row = json!({
            "model": "gpt-6-astra",
            "supportedReasoningEfforts": [],
            "defaultServiceTier": "priority",
            "serviceTiers": [
                { "id": "priority", "name": "Fast", "description": "2x speed, increased usage" },
                { "id": "priority", "name": "Duplicate", "description": "" },
                { "id": "ultrafast", "name": "", "description": "" },
                { "id": "default", "name": "Standard", "description": "" },
                { "id": "Not Valid", "name": "Broken", "description": "" },
            ],
        });
        let parsed = serde_json::to_value(parse_model(&row).unwrap()).unwrap();
        assert_eq!(
            parsed["serviceTiers"],
            json!([
                { "id": "priority", "name": "Fast", "description": "2x speed, increased usage" },
                { "id": "ultrafast", "name": "ultrafast", "description": "" },
            ])
        );
        assert_eq!(parsed["defaultServiceTier"], "priority");

        // Engines without speeds, or older ones, list none.
        let older = json!({ "model": "gpt-5.5", "supportedReasoningEfforts": [] });
        let parsed = serde_json::to_value(parse_model(&older).unwrap()).unwrap();
        assert_eq!(parsed["serviceTiers"], json!([]));
        assert_eq!(parsed["defaultServiceTier"], Value::Null);
    }

    #[test]
    fn merged_catalogs_offer_any_account_speed_and_mark_the_limited_ones() {
        let tier = |id: &str| json!({ "id": id, "name": id, "description": "" });
        let mut merged = json!({
            "model": "gpt-6-astra",
            "serviceTiers": [tier("priority")],
            "defaultServiceTier": "priority",
        });
        intersect(
            &mut merged,
            &json!({
                "model": "gpt-6-astra",
                "serviceTiers": [tier("priority"), tier("ultrafast")],
                "defaultServiceTier": null,
            }),
        );
        assert_eq!(merged["serviceTiers"][0]["limited"], Value::Null);
        assert_eq!(merged["serviceTiers"][1]["id"], "ultrafast");
        assert_eq!(merged["serviceTiers"][1]["limited"], true);
        assert_eq!(merged["defaultServiceTier"], Value::Null);

        // A row cached before speeds were listed says nothing about them.
        intersect(&mut merged, &json!({ "model": "gpt-6-astra" }));
        assert_eq!(merged["serviceTiers"][0]["limited"], Value::Null);
        intersect(
            &mut merged,
            &json!({ "model": "gpt-6-astra", "serviceTiers": [] }),
        );
        assert_eq!(merged["serviceTiers"][0]["limited"], true);

        let mut unknown = json!({ "model": "gpt-6-astra" });
        intersect(
            &mut unknown,
            &json!({ "serviceTiers": [tier("priority")], "defaultServiceTier": null }),
        );
        assert_eq!(unknown["serviceTiers"], json!([tier("priority")]));
    }

    #[test]
    fn limited_speeds_follow_the_accounts_routing_can_use() {
        let fast = json!({ "model": "fast", "serviceTiers": [{ "id": "priority" }] });
        let confirming = |age: i64| json!({ "checkedAt": now() - age, "models": [fast.clone()] });
        let without_model =
            json!({ "checkedAt": now(), "models": [{ "model": "other", "serviceTiers": [] }] });
        let limited = |catalogs: &[Value]| {
            let mut model = fast.clone();
            mark_unconfirmed(&mut model, catalogs);
            model["serviceTiers"][0]["limited"] == true
        };

        // A fresh catalog listing the model keeps the run off an account that lacks it.
        assert!(!limited(&[confirming(0), without_model.clone()]));
        // Once that catalog is past routing's freshness, the other account can take the run, so
        // the speed it cannot confirm is limited.
        assert!(limited(&[confirming(TTL + 60_000), without_model]));
    }

    #[test]
    fn speeds_map_to_tiers_and_only_known_catalogs_route_them() {
        let cached = json!({
            "checkedAt": now(),
            "models": [
                {
                    "model": "gpt-6-astra",
                    "isDefault": true,
                    "serviceTiers": [{ "id": "priority" }],
                },
                { "model": "gpt-6-luna", "serviceTiers": [] },
                { "model": "gpt-5.5" },
            ],
        });
        // Without a chosen speed, including older messages and agents, a turn runs at Standard
        // even after one at another speed, and whatever the model's catalog default.
        assert_eq!(service_tier(""), "default");
        assert_eq!(service_tier("standard"), "default");
        assert_eq!(service_tier("priority"), "priority");

        // Routing prefers accounts whose recent catalog confirms a tier.
        assert_eq!(offers_tier(&cached, "gpt-6-astra", "priority"), Some(true));
        assert_eq!(
            offers_tier(&cached, "gpt-6-astra", "ultrafast"),
            Some(false)
        );
        assert_eq!(offers_tier(&cached, "gpt-6-luna", "priority"), Some(false));
        // A catalog past the picker's freshness still knows its speeds between usage polls.
        let mut lapsed = cached.clone();
        lapsed["checkedAt"] = (now() - TTL - 60_000).into();
        assert_eq!(offers_tier(&lapsed, "gpt-6-luna", "priority"), Some(false));

        // Unknown capabilities: a model or speeds the catalog does not list, no model (Codex
        // runs the one its configuration sets), or an outdated catalog.
        assert_eq!(offers_tier(&cached, "gpt-5.5", "priority"), None);
        assert_eq!(offers_tier(&cached, "custom-alias", "priority"), None);
        // An empty model is the catalog default, for ordering accounts only.
        assert_eq!(offers_tier(&cached, "", "ultrafast"), Some(false));
        let mut stale = cached.clone();
        stale["checkedAt"] = (now() - SPEEDS_TTL - 1).into();
        assert_eq!(offers_tier(&stale, "gpt-6-luna", "priority"), None);
    }
}
