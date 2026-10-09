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
                if let Some(suffix) = self.suffix(&event["payload"]["item"]) {
                    if let Some(item) = event["payload"]["item"].as_object_mut() {
                        item.remove("text");
                        item.insert("delta".into(), suffix.into());
                    }
                    event["text"] = Value::String(String::new());
                }
                Ok(event)
            })
            .collect()
    }

    /// Remembers an agent message snapshot and returns what it appends to the
    /// previous snapshot of the same message, if it only appends.
    fn suffix(&mut self, item: &Value) -> Option<String> {
        if item["type"] != "agent_message" {
            return None;
        }
        let id = text(item, "id");
        let content = item["text"].as_str().filter(|_| !id.is_empty())?;
        let previous = self.0.insert(id.to_owned(), content.to_owned())?;
        content.strip_prefix(&previous).map(str::to_owned)
    }
}
