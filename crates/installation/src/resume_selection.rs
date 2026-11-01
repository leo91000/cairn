//! Validation for an explicit model choice while resuming a saved execution.
use crate::{
    error::{Error, Result},
    provider::Provider,
    service::Service,
    validation::text,
};
use cairn_contracts::ResumeExecutionSelection;
use serde_json::Value;

pub(crate) async fn validate(
    s: &Service,
    run: &Value,
    selection: &mut ResumeExecutionSelection,
    expected_turn_id: &str,
) -> Result<Provider> {
    if !matches!(text(run, "status"), "failed" | "interrupted" | "cancelled")
        || run["resumeAvailable"] != true
        || !run["workspaceCleanedAt"].is_null()
        || run["backendMigration"].is_object()
        || text(run, "trigger") != "chat"
        || run["chatExecution"]["messageId"].as_str() != Some(expected_turn_id)
    {
        return Err(Error::conflict(
            "This stopped chat turn is not available for a model-aware resume.",
        ));
    }
    let provider = Provider::of_run(run)?;
    if provider == Provider::Codex && selection.speed.is_empty() {
        selection.speed = text(&run["snapshot"]["agent"], "speed").to_owned();
    }
    validate_shape(selection)?;
    validate_provider_speed(provider, &selection.speed)?;

    let catalog = provider.adapter().model_catalog(s).await?;
    validate_catalog(selection, &catalog)?;
    validate_shape(selection)?;

    let mut agent = run["snapshot"]["agent"].clone();
    agent["model"] = selection.model.clone().into();
    agent["reasoning"] = selection.reasoning.clone().into();
    agent["speed"] = selection.speed.clone().into();
    provider.adapter().validate_agent(&agent)?;
    Ok(provider)
}

fn validate_shape(selection: &ResumeExecutionSelection) -> Result<()> {
    let valid_model = selection.model.len() <= 120;
    if !valid_model || !valid_effort(&selection.reasoning) || !valid_effort(&selection.speed) {
        return Err(Error::bad(
            "Choose a valid model, reasoning level and speed.",
        ));
    }
    Ok(())
}

fn valid_effort(value: &str) -> bool {
    value.is_empty()
        || (value.len() <= 40
            && value.bytes().enumerate().all(|(index, byte)| {
                byte.is_ascii_lowercase()
                    || (index > 0 && (byte.is_ascii_digit() || matches!(byte, b'_' | b'-')))
            }))
}

fn validate_provider_speed(provider: Provider, speed: &str) -> Result<()> {
    crate::models::validate_speed(speed)?;
    if !speed.is_empty() && provider != Provider::Codex {
        return Err(Error::bad(
            "Speed applies only to Codex. Remove it or choose Codex for this resume.",
        ));
    }
    Ok(())
}

fn model_row<'a>(catalog: &'a Value, model: &str) -> Option<(&'a Value, Option<&'a Value>)> {
    for row in catalog["models"].as_array()? {
        if row["model"] == model {
            return Some((row, None));
        }
        for route in row["routes"].as_array().into_iter().flatten() {
            if route["model"] == model {
                return Some((row, Some(route)));
            }
        }
    }
    None
}

fn validate_catalog(selection: &mut ResumeExecutionSelection, catalog: &Value) -> Result<()> {
    let selected_name = if selection.model.is_empty() {
        catalog["models"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|model| model["isDefault"] == true)
            .map_or("", |model| text(model, "model"))
            .to_owned()
    } else {
        selection.model.clone()
    };
    if selected_name.is_empty() {
        return Err(Error::bad(
            "Choose a model from this coding agent's current model catalog.",
        ));
    }
    let Some((model, route)) = model_row(catalog, &selected_name) else {
        return Err(Error::bad(
            "Choose a model from this coding agent's current model catalog.",
        ));
    };
    if selection.model.is_empty() {
        selection.model.clone_from(&selected_name);
    }

    let efforts = route
        .map(|route| &route["supportedReasoningEfforts"])
        .filter(|efforts| efforts.is_array())
        .unwrap_or(&model["supportedReasoningEfforts"]);
    if !selection.reasoning.is_empty()
        && !efforts.as_array().is_some_and(|efforts| {
            efforts
                .iter()
                .any(|effort| effort["reasoningEffort"] == selection.reasoning)
        })
    {
        return Err(Error::bad(
            "Choose a reasoning level offered for this model.",
        ));
    }

    if !selection.speed.is_empty()
        && selection.speed != crate::models::STANDARD_SPEED
        && let Some(tiers) = model["serviceTiers"].as_array()
        && !tiers.iter().any(|tier| tier["id"] == selection.speed)
    {
        return Err(Error::bad("Choose a speed offered for this model."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opencode_catalog_route_ids_keep_provider_validation_and_bounds() {
        let mut selection = ResumeExecutionSelection {
            model: "openrouter/model@revision+vision".into(),
            reasoning: "xhigh".into(),
            speed: String::new(),
        };
        validate_shape(&selection).unwrap();

        let agent = serde_json::json!({
            "provider": "opencode",
            "model": selection.model,
            "reasoning": selection.reasoning,
            "access": { "sandbox": "yolo" },
        });
        Provider::Opencode.adapter().validate_agent(&agent).unwrap();
        let catalog = serde_json::json!({
            "models": [{
                "model": "opencode/canonical-key",
                "routes": [{
                    "model": "openrouter/model@revision+vision",
                    "supportedReasoningEfforts": [{ "reasoningEffort": "xhigh" }],
                }],
            }],
        });
        validate_catalog(&mut selection, &catalog).unwrap();

        let mut oversized = selection.clone();
        oversized.model = "m".repeat(121);
        assert!(validate_shape(&oversized).is_err());

        let mut unlisted = selection.clone();
        unlisted.model = "openrouter/unlisted@revision+vision".into();
        assert!(validate_shape(&unlisted).is_ok());
        assert!(validate_catalog(&mut unlisted, &catalog).is_err());

        let mut invalid_effort = selection;
        invalid_effort.reasoning = "2xhigh".into();
        assert!(validate_shape(&invalid_effort).is_err());
    }

    #[test]
    fn empty_model_selection_resolves_the_catalog_default() {
        let mut selection = ResumeExecutionSelection {
            model: String::new(),
            reasoning: "high".into(),
            speed: String::new(),
        };
        let catalog = serde_json::json!({
            "models": [{
                "model": "gpt-6-sol",
                "isDefault": true,
                "supportedReasoningEfforts": [{ "reasoningEffort": "high" }],
            }],
        });

        validate_catalog(&mut selection, &catalog).unwrap();

        assert_eq!(selection.model, "gpt-6-sol");
    }

    #[test]
    fn empty_model_selection_requires_a_catalog_default() {
        let mut selection = ResumeExecutionSelection {
            model: String::new(),
            reasoning: String::new(),
            speed: String::new(),
        };
        let catalog = serde_json::json!({
            "models": [{
                "model": "opencode/sonnet",
                "isDefault": false,
                "supportedReasoningEfforts": [{ "reasoningEffort": "high" }],
            }],
        });

        assert!(validate_catalog(&mut selection, &catalog).is_err());
        assert!(selection.model.is_empty());
    }
}
