//! Installation plugin contracts, independent of storage and agent engines.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const API_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 1_048_576;
pub const MAX_SOURCE_BYTES: usize = 262_144;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    /// One sentence shown in the owner's plugin catalogue.
    #[serde(default)]
    pub description: String,
    pub api_version: u32,
    pub capabilities: Vec<String>,
}

impl Manifest {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.api_version != API_VERSION {
            return Err("Plugin API version is incompatible.");
        }
        if self.id.is_empty()
            || self.id.len() > 80
            || !self
                .id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            || self.name.is_empty()
            || self.name.len() > 100
            || self.version.is_empty()
            || self.description.chars().count() > 200
        {
            return Err("Invalid plugin manifest.");
        }
        Ok(())
    }

    pub fn permits(&self, method: &str) -> bool {
        self.capabilities
            .iter()
            .any(|capability| capability == method)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Frame {
    Invoke {
        api_version: u32,
        source: String,
        input: Value,
    },
    Call {
        method: String,
        args: Value,
    },
    Reply {
        value: Value,
    },
    Failure,
    Finished {
        value: Value,
    },
}
