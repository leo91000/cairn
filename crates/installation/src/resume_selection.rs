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
    selection: &ResumeExecutionSelection,
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
    validate_shape(selection)?;
    crate::models::validate_speed(&selection.speed)?;

    let mut agent = run["snapshot"]["agent"].clone();
    agent["model"] = selection.model.clone().into();
    agent["reasoning"] = selection.reasoning.clone().into();
    agent["speed"] = selection.speed.clone().into();
    provider.adapter().validate_agent(&agent)?;

    let catalog = provider.adapter().model_catalog(s).await?;
    validate_catalog(provider, selection, &catalog)?;
    Ok(provider)
}

fn validate_shape(selection: &ResumeExecutionSelection) -> Result<()> {
    let valid_model = selection.model.len() <= 120
        && selection.model.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'.' | b'/' | b':' | b'[' | b']' | b'-')
        });
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

fn validate_catalog(
    provider: Provider,
    selection: &ResumeExecutionSelection,
    catalog: &Value,
) -> Result<()> {
    let selected_name = if selection.model.is_empty() {
        catalog["models"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|model| model["isDefault"] == true)
            .map(|model| text(model, "model"))
            .unwrap_or("")
    } else {
        selection.model.as_str()
    };
    let Some((model, route)) = model_row(catalog, selected_name) else {
        return Err(Error::bad(
            "Choose a model from this coding agent's current model catalog.",
        ));
    };

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

    if !selection.speed.is_empty() && provider != Provider::Codex {
        return Err(Error::bad(
            "Speed applies only to Codex. Remove it or choose Codex for this resume.",
        ));
    }
    if !selection.speed.is_empty() && selection.speed != crate::models::STANDARD_SPEED {
        if let Some(tiers) = model["serviceTiers"].as_array()
            && !tiers.iter().any(|tier| tier["id"] == selection.speed)
        {
            return Err(Error::bad("Choose a speed offered for this model."));
        }
    }
    Ok(())
}
