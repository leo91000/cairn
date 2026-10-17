//! The plan an agent keeps while it works, shown as one `todo_list` item whichever agent wrote it.
//! Claude Code writes it with `TodoWrite`, OpenCode with `todowrite`, and the Codex app-server
//! reports it with each turn's plan updates. Every update carries the whole list.
use crate::validation::text;
use serde_json::{Value, json};

/// A step of the plan: pending, in_progress, completed or cancelled. It keeps `completed` for
/// clients that only know whether a step is done.
fn step(label: &Value, status: &str, active_text: &Value) -> Value {
    let status = match status {
        "completed" => "completed",
        "in_progress" | "inProgress" => "in_progress",
        "cancelled" => "cancelled",
        _ => "pending",
    };
    let mut step = json!({
        "text": label,
        "status": status,
        "completed": status == "completed",
    });
    if active_text
        .as_str()
        .is_some_and(|value| !value.trim().is_empty())
    {
        step["active_text"] = active_text.clone();
    }
    step
}

/// The list written by Claude Code's `TodoWrite` or OpenCode's `todowrite`. Claude Code also
/// names what the agent does during a step, as its `activeForm`.
pub fn todos(id: &str, input: &Value) -> Value {
    let items: Vec<_> = input["todos"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|todo| step(&todo["content"], text(todo, "status"), &todo["activeForm"]))
        .collect();
    json!({ "id": id, "type": "todo_list", "items": items })
}

/// A Codex `turn/plan/updated` notification, with the explanation the agent gave for its plan.
pub fn codex(id: &str, params: &Value) -> Value {
    let items: Vec<_> = params["plan"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|entry| step(&entry["step"], text(entry, "status"), &Value::Null))
        .collect();
    let mut item = json!({ "id": id, "type": "todo_list", "items": items });
    if !text(params, "explanation").trim().is_empty() {
        item["explanation"] = params["explanation"].clone();
    }
    item
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todo_tools_keep_each_step_state_and_what_the_agent_does_during_it() {
        let input = json!({
            "todos": [
                {
                    "content": "Explore the middleware",
                    "status": "completed",
                    "activeForm": "Exploring the middleware",
                },
                {
                    "content": "Add the migration",
                    "status": "in_progress",
                    "activeForm": "Adding the migration",
                },
                {
                    "content": "Update the docs",
                    "status": "cancelled",
                    "priority": "low",
                    "id": "3",
                },
                {
                    "content": "Run the checks",
                    "status": "pending",
                    "activeForm": " ",
                },
            ],
        });

        assert_eq!(
            todos("toolu_1", &input),
            json!({
                "id": "toolu_1",
                "type": "todo_list",
                "items": [
                    {
                        "text": "Explore the middleware",
                        "status": "completed",
                        "completed": true,
                        "active_text": "Exploring the middleware",
                    },
                    {
                        "text": "Add the migration",
                        "status": "in_progress",
                        "completed": false,
                        "active_text": "Adding the migration",
                    },
                    {
                        "text": "Update the docs",
                        "status": "cancelled",
                        "completed": false,
                    },
                    {
                        "text": "Run the checks",
                        "status": "pending",
                        "completed": false,
                    },
                ],
            })
        );
        // A call announced before its arguments has no steps yet.
        assert_eq!(todos("part", &json!({}))["items"], json!([]));
    }

    #[test]
    fn codex_plans_keep_their_explanation_and_name_steps_in_progress_alike() {
        let params = json!({
            "threadId": "thread",
            "turnId": "turn",
            "explanation": "Data model first.",
            "plan": [
                { "step": "Add the migration", "status": "completed" },
                { "step": "Refresh tokens", "status": "inProgress" },
                { "step": "Run the checks", "status": "pending" },
            ],
        });

        let item = codex("plan:turn", &params);

        assert_eq!(item["explanation"], "Data model first.");
        let states: Vec<_> = item["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|step| (text(step, "text"), text(step, "status")))
            .collect();
        assert_eq!(
            states,
            vec![
                ("Add the migration", "completed"),
                ("Refresh tokens", "in_progress"),
                ("Run the checks", "pending"),
            ]
        );
        assert!(
            codex("plan:turn", &json!({ "plan": [], "explanation": null }))
                .get("explanation")
                .is_none()
        );
    }
}
