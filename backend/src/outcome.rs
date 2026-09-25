//! Agent-reported delivery is distinct from successful process execution.
use crate::{
    config::now,
    error::{Error, Result},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};

pub fn tool() -> Value {
    json!({
        "name": "report_outcome",
        "description": "Before your final response, report whether the user's current request was completed, \
            blocked, or needs user input. Include concrete validation/delivery evidence and a precise \
            reason. Process exit success alone is not task completion. Replace your report if \
            circumstances change.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "enum": ["completed", "blocked", "needs_input"]
                },
                "reason": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 4000
                },
                "evidence": {
                    "type": "array",
                    "maxItems": 20,
                    "items": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 1000
                    }
                }
            },
            "required": ["status", "reason", "evidence"],
            "additionalProperties": false
        }
    })
}

pub async fn report(s: &Service, bearer: &str, input: &Value) -> Result<Value> {
    let schema = tool()["inputSchema"].clone();
    if !jsonschema::is_valid(&schema, input) || text(input, "reason").trim().is_empty() {
        return Err(Error::bad(
            "Provide a valid outcome, a concrete reason and evidence.",
        ));
    }
    let mut outcome = crate::run_output::payload(input, &[]);
    outcome["reportedAt"] = now().into();
    let bearer = bearer.to_owned();
    s.store
        .write(move |db| {
            // Authorize and write in the same transaction; a report from an expired
            // attempt cannot overwrite the next user message's outcome.
            let run = crate::project_workspaces::authorize_in(db, &bearer)?;
            outcome["messageId"] = run["chatExecution"]["messageId"].clone();
            db.patch_run(
                text(&run, "id"),
                &json!({
                    "outcome": outcome
                }),
            )?;
            Ok(outcome)
        })
        .await
}
