use crate::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(transparent)]
#[schemars(inline)]
pub struct ProjectId(pub Uuid);

impl std::fmt::Display for ProjectId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectSettings {
    #[schemars(length(min = 1, max = 100))]
    pub name: String,
    #[schemars(length(min = 1, max = 2000))]
    pub path: String,
    #[serde(default)]
    #[schemars(length(max = 500))]
    pub description: String,
    #[serde(default = "remote")]
    #[schemars(pattern("^(remote|local)$"))]
    pub source_mode: String,
    #[serde(default = "main_branch")]
    #[schemars(pattern("^[a-zA-Z0-9][a-zA-Z0-9_/.-]*$"))]
    pub base_branch: String,
}

fn remote() -> String {
    "remote".into()
}

fn main_branch() -> String {
    "main".into()
}

impl ProjectSettings {
    /// Server validation shared by every transport, including plugin imports.
    pub fn validate(&mut self) -> Result<(), &'static str> {
        self.name = self.name.trim().to_owned();
        if self.name.is_empty() || self.name.chars().count() > 100 {
            return Err("Enter a project name of 1 to 100 characters.");
        }
        if self.path.is_empty() || self.path.chars().count() > 2000 {
            return Err("Enter a project path of 1 to 2000 characters.");
        }
        if self.description.chars().count() > 500 {
            return Err("Enter a description of at most 500 characters.");
        }
        if !matches!(self.source_mode.as_str(), "remote" | "local") {
            return Err("Choose remote or local project sources.");
        }
        let mut branch = self.base_branch.chars();
        if !branch.next().is_some_and(|c| c.is_ascii_alphanumeric())
            || !branch.all(|c| c.is_ascii_alphanumeric() || "_/.-".contains(c))
        {
            return Err("Enter a valid base branch.");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateProjectInput {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "creation_id"
    )]
    #[schemars(with = "ProjectId")]
    #[ts(optional)]
    pub creation_id: Option<ProjectId>,
    pub settings: ProjectSettings,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplaceProjectInput {
    pub project_id: ProjectId,
    pub revision: String,
    pub settings: ProjectSettings,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectInput {
    pub project_id: ProjectId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectSource {
    pub plugin_id: String,
    pub source: String,
    pub instance: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: ProjectId,
    #[serde(flatten)]
    #[ts(flatten)]
    pub settings: ProjectSettings,
    pub origin: String,
    pub source: Option<ProjectSource>,
    pub revision: String,
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    #[serde(deserialize_with = "wire_integer")]
    #[ts(type = "number")]
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListProjectsInput {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListProjectsOutput {
    pub projects: Vec<Project>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteProjectOutput {
    pub deleted: bool,
}

fn creation_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ProjectId>, D::Error> {
    let id = ProjectId::deserialize(deserializer)?;
    if id.0.get_version() != Some(uuid::Version::Random) {
        return Err(serde::de::Error::custom("creationId must be a random UUID"));
    }
    Ok(Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_contracts_apply_defaults_and_reject_invalid_commands() {
        let corpus: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/project-contracts.json"
        ))
        .unwrap();
        for value in corpus["creates"].as_array().unwrap() {
            let mut input: CreateProjectInput = serde_json::from_value(value.clone()).unwrap();
            input.settings.validate().unwrap();
            let wire = serde_json::to_value(&input).unwrap();
            assert_eq!(
                serde_json::from_value::<CreateProjectInput>(wire).unwrap(),
                input
            );
        }
        for value in corpus["invalidCreates"].as_array().unwrap() {
            let invalid = match serde_json::from_value::<CreateProjectInput>(value.clone()) {
                Err(_) => true,
                Ok(mut input) => input.settings.validate().is_err(),
            };
            assert!(invalid, "{value}");
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportProjectInput {
    #[schemars(length(max = 100))]
    pub plugin_id: String,
    #[schemars(length(max = 100))]
    pub source: String,
    #[schemars(length(max = 100))]
    pub instance: String,
    #[schemars(length(max = 200))]
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub base_branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListSourceProjectsInput {
    #[schemars(length(max = 100))]
    pub plugin_id: String,
    #[schemars(length(max = 100))]
    pub source: String,
    #[schemars(length(max = 100))]
    pub instance: String,
    #[serde(default = "first_page")]
    #[schemars(range(min = 1_u32, max = 10000_u32))]
    pub page: u32,
    #[serde(default)]
    #[schemars(length(max = 200))]
    pub query: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginProject {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub project_name: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub default_branch: String,
    #[serde(default)]
    pub badges: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub url: Option<String>,
    #[serde(default)]
    pub imported: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginProjectPage {
    pub items: Vec<PluginProject>,
    #[schemars(range(min = 1_u32, max = 10000_u32))]
    pub next_page: Option<u32>,
}

fn first_page() -> u32 {
    1
}
