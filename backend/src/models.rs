use crate::{
    config::now,
    error::{Error, Result},
    provider::Provider,
    rpc::Session,
    service::Service,
    validation::text,
};
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
            let model = text(row, "model");
            if model.is_empty() || model.len() > 120 || !row["supportedReasoningEfforts"].is_array()
            {
                return Err(Error::bad_gateway("Codex returned an invalid model."));
            }
            let efforts = row["supportedReasoningEfforts"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| {
                    let effort = text(e, "reasoningEffort");
                    !effort.is_empty()
                        && effort.len() <= 40
                        && effort.bytes().all(|b| {
                            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-'
                        })
                })
                .map(|e| {
                    json!({
                        "reasoningEffort": e["reasoningEffort"],
                        "description": text(e, "description")
                    })
                })
                .collect::<Vec<_>>();
            models.insert(
                model.to_owned(),
                json!({
                    "model": model,
                    "displayName": if text(row, "displayName").is_empty() {
                        model
                    } else {
                        text(row, "displayName")
                    },
                    "description": text(row, "description"),
                    "hidden": row["hidden"] == true,
                    "isDefault": row["isDefault"] == true,
                    "defaultReasoningEffort": text(row, "defaultReasoningEffort"),
                    "supportedReasoningEfforts": efforts
                }),
            );
        }
        cursor = page["nextCursor"].clone();
        if cursor.is_null() {
            return Ok(models.into_values().collect::<Vec<_>>().into());
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
                    async {
                        let mut session =
                            Session::codex(&s.config, &s.config.home.join(".codex"), &[], None)
                                .await?;
                        let result = discover(&mut session).await;
                        session.close().await;
                        result
                    }
                    .await
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
                if let Some(existing) = models.get_mut(&name) {
                    // Only offer effort levels supported by every account exposing this model.
                    existing["supportedReasoningEfforts"]
                        .as_array_mut()
                        .unwrap()
                        .retain(|e| {
                            model["supportedReasoningEfforts"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .any(|other| other["reasoningEffort"] == e["reasoningEffort"])
                        });
                    existing["hidden"] =
                        (existing["hidden"] == true && model["hidden"] == true).into();
                    existing["isDefault"] =
                        (existing["isDefault"] == true || model["isDefault"] == true).into();
                } else {
                    models.insert(name, model.clone());
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
            "error": error
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
