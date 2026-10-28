use crate::{error::Result, service::Service, validation::text};
use serde_json::Value;
use std::collections::HashMap;

impl Service {
    /// A bounded projection of existing messages: no counters to migrate or retry.
    /// Usage follows the `$name` token, including across scopes with the same name.
    pub(crate) async fn attach_skill_usage(&self, skills: &mut [Value]) -> Result<()> {
        if skills.is_empty() {
            return Ok(());
        }
        let names: Vec<String> = skills
            .iter()
            .filter(|skill| skill["valid"] == true)
            .map(|skill| text(skill, "name").to_owned())
            .collect();
        let counts = self
            .store
            .read(move |db| {
                // rowid bounds the work without scanning or sorting the whole history.
                // Queued/cancelled messages never count as an invocation.
                let mut statement = db.0.prepare_cached(
                    "SELECT json_extract(data,'$.text') FROM
                     (SELECT data FROM chat_messages ORDER BY rowid DESC LIMIT 1000)
                     WHERE json_extract(data,'$.status')='delivered'",
                )?;
                let messages = statement.query_map([], |row| row.get::<_, String>(0))?;
                let names: Vec<&str> = names.iter().map(String::as_str).collect();
                let mut counts = HashMap::<String, u64>::new();
                for message in messages {
                    for name in super::mentions(&message?, &names) {
                        *counts.entry(name).or_default() += 1;
                    }
                }
                Ok(counts)
            })
            .await?;
        for skill in skills {
            skill["usageCount"] = counts.get(text(skill, "name")).copied().unwrap_or(0).into();
        }
        Ok(())
    }
}
