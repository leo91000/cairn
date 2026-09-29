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

/// One row of the cached Codex catalog, `codex-models:{account}`.
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
}

fn valid_effort(effort: &str) -> bool {
    !effort.is_empty()
        && effort.len() <= 40
        && effort
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
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
                json!({ "limit": 100, "includeHidden": true, "cursor": cursor }),
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
    cached["checkedAt"]
        .as_i64()
        .is_none_or(|at| now() - at > TTL)
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
    if due(&cached) {
        record(s, &key, cached, discover(session).await).await?;
    }
    Ok(())
}
/// Combines one model listed by several accounts: only effort levels supported by
/// every account, hidden only when hidden everywhere, default when default anywhere.
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
        }
        let models = models.into_values().collect::<Vec<_>>();
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

// Preserve existing/custom model configurations when there is no current catalog.
pub async fn account_supports(s: &Service, account: &str, model: &str) -> Result<bool> {
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
}
