use cairn_contracts::{beacon::*, *};
use schemars::{JsonSchema, generate::SchemaSettings};
use std::{fs, path::Path};
use ts_rs::TS;

fn export<T: JsonSchema + TS + 'static>(
    root: &Path,
    generator: &mut schemars::SchemaGenerator,
    contract: Contract,
) {
    let schema = cairn_contracts::schema_for::<T>(contract);
    let config = ts_rs::Config::default().with_out_dir(root.join("typescript"));
    fs::write(
        root.join(format!("{}.json", T::ident(&config))),
        serde_json::to_string_pretty(&schema).unwrap() + "\n",
    )
    .unwrap();
    T::export_all(&config).unwrap();
    generator.subschema_for::<T>();
}

fn main() {
    let root = std::env::args().nth(1).expect("output directory");
    let root = Path::new(&root);
    fs::create_dir_all(root).unwrap();
    let mut generator = SchemaSettings::draft07().for_serialize().into_generator();
    export::<ResumeRunInput>(root, &mut generator, Contract::Deserialize);
    export::<ResumeRunOutput>(root, &mut generator, Contract::Serialize);
    export::<QualificationFixture>(root, &mut generator, Contract::Serialize);
    export::<CreateProjectInput>(root, &mut generator, Contract::Deserialize);
    export::<ReplaceProjectInput>(root, &mut generator, Contract::Deserialize);
    export::<ProjectInput>(root, &mut generator, Contract::Deserialize);
    export::<ListProjectsInput>(root, &mut generator, Contract::Deserialize);
    export::<ListProjectsOutput>(root, &mut generator, Contract::Serialize);
    export::<Project>(root, &mut generator, Contract::Serialize);
    export::<DeleteProjectOutput>(root, &mut generator, Contract::Serialize);
    export::<CreateAgentInput>(root, &mut generator, Contract::Deserialize);
    export::<ReplaceAgentInput>(root, &mut generator, Contract::Deserialize);
    export::<SetAgentAvatarInput>(root, &mut generator, Contract::Deserialize);
    export::<AgentInput>(root, &mut generator, Contract::Deserialize);
    export::<ListAgentsInput>(root, &mut generator, Contract::Deserialize);
    export::<ListAgentsOutput>(root, &mut generator, Contract::Serialize);
    export::<Agent>(root, &mut generator, Contract::Serialize);
    export::<DeleteAgentOutput>(root, &mut generator, Contract::Serialize);
    export::<AvatarConfiguration>(root, &mut generator, Contract::Serialize);
    export::<AgentPreview>(root, &mut generator, Contract::Serialize);
    export::<MergeAgentsInput>(root, &mut generator, Contract::Deserialize);
    export::<MergeAgentsOutput>(root, &mut generator, Contract::Serialize);
    let clients = serde_json::json!([
        {
            "name": "setAgentAvatar",
            "input": "SetAgentAvatarInput",
            "output": "Agent",
            "method": "POST",
            "path": "/agents/{agentId}/avatar/upload"
        },
        {
            "name": "listAgents",
            "input": "ListAgentsInput",
            "output": "ListAgentsOutput",
            "method": "GET",
            "path": "/agents"
        },
        {
            "name": "getAgent",
            "input": "AgentInput",
            "output": "Agent",
            "method": "GET",
            "path": "/agents/{agentId}"
        },
        {
            "name": "createAgent",
            "input": "CreateAgentInput",
            "output": "Agent",
            "method": "POST",
            "path": "/agents"
        },
        {
            "name": "replaceAgent",
            "input": "ReplaceAgentInput",
            "output": "Agent",
            "method": "PUT",
            "path": "/agents/{agentId}"
        },
        {
            "name": "deleteAgent",
            "input": "AgentInput",
            "output": "DeleteAgentOutput",
            "method": "DELETE",
            "path": "/agents/{agentId}"
        },
        {
            "name": "previewAgent",
            "input": "ReplaceAgentInput",
            "output": "AgentPreview",
            "method": "POST",
            "path": "/agents/{agentId}/preview"
        },
        {
            "name": "mergeAgents",
            "input": "MergeAgentsInput",
            "output": "MergeAgentsOutput",
            "method": "POST",
            "path": "/agents/{sourceId}/merge"
        },
        {
            "name": "getAgentAvatarConfiguration",
            "input": "ListAgentsInput",
            "output": "AvatarConfiguration",
            "method": "GET",
            "path": "/agent-avatars"
        },
        {
            "name": "generateAgentAvatar",
            "input": "AgentInput",
            "output": "Agent",
            "method": "POST",
            "path": "/agents/{agentId}/avatar/generate"
        },
    ]);
    fs::write(
        root.join("agent-clients.json"),
        serde_json::to_string_pretty(&clients).unwrap() + "\n",
    )
    .unwrap();

    let agents: serde_json::Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/agent-contracts.json")).unwrap();
    let creates: Vec<CreateAgentInput> = agents["creates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| serde_json::from_value(value.clone()).unwrap())
        .collect();
    let views: Vec<Agent> = creates
        .iter()
        .enumerate()
        .map(|(index, input)| Agent {
            id: AgentId(uuid::Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap()),
            settings: input.settings.clone(),
            revision: "profile-revision".into(),
            created_at: 9007199254740991,
            avatar: if index == 0 {
                None
            } else {
                Some(AgentAvatar {
                    status: if index == 1 { "generating" } else { "failed" }.into(),
                    revision: "avatar-revision".into(),
                    url: None,
                    error: if index == 2 {
                        Some("Portrait generation was interrupted. You can try again.".into())
                    } else {
                        None
                    },
                })
            },
        })
        .collect();
    fs::write(
        root.join("agent-wire.json"),
        serde_json::to_string_pretty(&serde_json::json!({ "creates": creates, "views": views }))
            .unwrap()
            + "\n",
    )
    .unwrap();
    let projects: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/project-contracts.json"
    ))
    .unwrap();
    let creates: Vec<CreateProjectInput> = projects["creates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| serde_json::from_value(value.clone()).unwrap())
        .collect();
    let views: Vec<Project> = creates
        .iter()
        .enumerate()
        .map(|(index, input)| Project {
            id: ProjectId(uuid::Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap()),
            settings: input.settings.clone(),
            origin: "forge.example:team/project.git".into(),
            source: if index == 0 {
                None
            } else {
                Some(ProjectSource {
                    plugin_id: "forge".into(),
                    source: "repositories".into(),
                    instance: "00000000-0000-4000-8000-000000000003".into(),
                    id: "team/project".into(),
                })
            },
            revision: "00000000-0000-4000-8000-000000000004".into(),
            created_at: 9007199254740991,
        })
        .collect();
    fs::write(
        root.join("project-wire.json"),
        serde_json::to_string_pretty(&serde_json::json!({ "creates": creates, "views": views }))
            .unwrap()
            + "\n",
    )
    .unwrap();
    let clients = serde_json::json!([
        { "name": "listProjects", "input": "ListProjectsInput", "output": "ListProjectsOutput", "method": "GET", "path": "/projects" },
        { "name": "getProject", "input": "ProjectInput", "output": "Project", "method": "GET", "path": "/projects/{projectId}" },
        { "name": "createProject", "input": "CreateProjectInput", "output": "Project", "method": "POST", "path": "/projects" },
        { "name": "replaceProject", "input": "ReplaceProjectInput", "output": "Project", "method": "PUT", "path": "/projects/{projectId}" },
        { "name": "deleteProject", "input": "ProjectInput", "output": "DeleteProjectOutput", "method": "DELETE", "path": "/projects/{projectId}" }
    ]);
    fs::write(
        root.join("project-clients.json"),
        serde_json::to_string_pretty(&clients).unwrap() + "\n",
    )
    .unwrap();
    export::<ListAccountMethodsOutput>(root, &mut generator, Contract::Serialize);
    fs::write(
        root.join("list-account-methods.json"),
        serde_json::to_string_pretty(&serde_json::json!({ "path": LIST_ACCOUNT_METHODS_PATH }))
            .unwrap()
            + "\n",
    )
    .unwrap();
    // Canonical wire values come from the Rust serializer, including omission/null semantics.
    let corpus: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/resume-contracts.json"
    ))
    .unwrap();
    let wire: Vec<_> = corpus["valid"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| {
            let model: QualificationFixture = serde_json::from_value(value.clone()).unwrap();
            serde_json::to_value(model).unwrap()
        })
        .collect();
    fs::write(
        root.join("qualification-wire.json"),
        serde_json::to_string_pretty(&wire).unwrap() + "\n",
    )
    .unwrap();
    let descriptor = serde_json::json!({
        "id": RESUME_RUN_ID,
        "scope": RESUME_RUN_SCOPE,
        "path": RESUME_RUN_PATH,
        "description": RESUME_RUN_DESCRIPTION,
    });
    fs::write(
        root.join("resume-run.json"),
        serde_json::to_string_pretty(&descriptor).unwrap() + "\n",
    )
    .unwrap();
    let schema = serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "definitions": generator.take_definitions(true),
    });
    fs::write(
        root.join("models.json"),
        serde_json::to_string_pretty(&schema).unwrap() + "\n",
    )
    .unwrap();
}
