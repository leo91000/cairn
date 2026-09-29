//! Wire-only compression. Persisted events and the existing REST API retain their
//! complete, redacted snapshots; a new connection always establishes a baseline.
use crate::{error::Result, store::Event, validation::text};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct TextDeltas(HashMap<String, String>);

impl TextDeltas {
    pub fn encode(&mut self, events: Vec<Event>, reset: bool) -> Result<Vec<Value>> {
        if reset {
            self.0.clear();
        }
        events
            .into_iter()
            .map(|event| {
                let mut event = serde_json::to_value(event)?;
                if event["type"] == "turn.started" {
                    self.0.clear();
                }
                let item = &event["payload"]["item"];
                if item["type"] != "agent_message" {
                    return Ok(event);
                }
                let id = text(item, "id").to_owned();
                let Some(content) = item["text"].as_str().filter(|_| !id.is_empty()) else {
                    return Ok(event);
                };
                let content = content.to_owned();
                if let Some(previous) = self.0.insert(id, content.clone())
                    && let Some(suffix) = content.strip_prefix(&previous)
                {
                    event["payload"]["item"]
                        .as_object_mut()
                        .unwrap()
                        .remove("text");
                    event["payload"]["item"]["delta"] = suffix.into();
                    event["text"] = Value::String(String::new());
                }
                Ok(event)
            })
            .collect()
    }
}
