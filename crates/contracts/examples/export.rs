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
    fs::write(root.join("run-clients.json"), serde_json::to_string_pretty(&serde_json::json!([{"name": "runMission", "input": "RunMissionInput", "output": "Run", "method": "POST", "path": "/tasks/{taskId}/run"}, {"name": "retryRun", "input": "RunInput", "output": "Run", "method": "POST", "path": "/runs/{runId}/retry"}, {"name": "cancelRun", "input": "RunInput", "output": "CancelRunOutput", "method": "POST", "path": "/runs/{runId}/cancel"}, {"name": "cleanupRun", "input": "RunInput", "output": "CleanupRunOutput", "method": "POST", "path": "/runs/{runId}/cleanup"}])).unwrap() + "\n").unwrap();
    let corpus: serde_json::Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/run-contracts.json")).unwrap();
    let states: Vec<RunState> = serde_json::from_value(corpus["states"].clone()).unwrap();
    let retries: Vec<Retry> = serde_json::from_value(corpus["retries"].clone()).unwrap();
    let lists: Vec<ListRunsInput> = serde_json::from_value(corpus["lists"].clone()).unwrap();
    fs::write(
        root.join("run-wire.json"),
        serde_json::to_string_pretty(
            &serde_json::json!({"states": states, "retries": retries, "lists": lists}),
        )
        .unwrap()
            + "\n",
    )
    .unwrap();
    export::<ListRunsInput>(root, &mut generator, Contract::Deserialize);
    export::<GetRunInput>(root, &mut generator, Contract::Deserialize);
    export::<ReadRunContentInput>(root, &mut generator, Contract::Deserialize);
    export::<RunContentOutput>(root, &mut generator, Contract::Serialize);
    export::<RunHistoryOutput>(root, &mut generator, Contract::Serialize);
    export::<Run>(root, &mut generator, Contract::Serialize);
    export::<RunState>(root, &mut generator, Contract::Serialize);
    export::<Retry>(root, &mut generator, Contract::Serialize);
    export::<RunInput>(root, &mut generator, Contract::Deserialize);
    export::<RunMissionInput>(root, &mut generator, Contract::Deserialize);
    export::<CancelRunOutput>(root, &mut generator, Contract::Serialize);
    export::<CleanupRunOutput>(root, &mut generator, Contract::Serialize);
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
    export::<CreateMissionInput>(root, &mut generator, Contract::Deserialize);
    export::<ReplaceMissionInput>(root, &mut generator, Contract::Deserialize);
    export::<MissionStateInput>(root, &mut generator, Contract::Deserialize);
    export::<MissionInput>(root, &mut generator, Contract::Deserialize);
    export::<ListMissionsInput>(root, &mut generator, Contract::Deserialize);
    export::<ListMissionsOutput>(root, &mut generator, Contract::Serialize);
    export::<DeleteMissionOutput>(root, &mut generator, Contract::Serialize);
    export::<RunTaskSnapshot>(root, &mut generator, Contract::Serialize);
    export::<Mission>(root, &mut generator, Contract::Serialize);
    export::<SchedulePreviewInput>(root, &mut generator, Contract::Deserialize);
    export::<SchedulePreviewOutput>(root, &mut generator, Contract::Serialize);
    let clients = serde_json::json!([
        {
            "name": "listMissions",
            "input": "ListMissionsInput",
            "output": "ListMissionsOutput",
            "method": "GET",
            "path": "/tasks"
        },
        {
            "name": "getMission",
            "input": "MissionInput",
            "output": "Mission",
            "method": "GET",
            "path": "/tasks/{taskId}"
        },
        {
            "name": "createMission",
            "input": "CreateMissionInput",
            "output": "Mission",
            "method": "POST",
            "path": "/tasks"
        },
        {
            "name": "replaceMission",
            "input": "ReplaceMissionInput",
            "output": "Mission",
            "method": "PUT",
            "path": "/tasks/{taskId}"
        },
        {
            "name": "setMissionState",
            "input": "MissionStateInput",
            "output": "Mission",
            "method": "POST",
            "path": "/tasks/{taskId}/state"
        },
        {
            "name": "deleteMission",
            "input": "MissionInput",
            "output": "DeleteMissionOutput",
            "method": "DELETE",
            "path": "/tasks/{taskId}"
        },
        {
            "name": "previewSchedule",
            "input": "SchedulePreviewInput",
            "output": "SchedulePreviewOutput",
            "method": "POST",
            "path": "/schedule/preview"
        }
    ]);
    fs::write(
        root.join("mission-clients.json"),
        serde_json::to_string_pretty(&clients).unwrap() + "\n",
    )
    .unwrap();
    let missions: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/mission-contracts.json"
    ))
    .unwrap();
    let creates: Vec<CreateMissionInput> = missions["creates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect();
    let views: Vec<Mission> = creates
        .iter()
        .map(|input| Mission {
            id: MissionId(uuid::Uuid::parse_str("00000000-0000-4000-8000-000000000002").unwrap()),
            settings: input.settings.clone(),
            revision: "revision".into(),
            created_at: 9007199254740991,
            next_run: None,
            author_id: Some("owner".into()),
            author_access_id: Some("membership".into()),
            author_removed: false,
            schedule_wait_reason: None,
            schedule_failure: Some(ScheduleFailure {
                message: "Unavailable".into(),
                scheduled_for: 1,
                at: 9007199254740991,
            }),
            native_principals: Vec::new(),
            native_authority_nodes: Vec::new(),
            native_parent_run_id: None,
            native_parent_access: None,
            native_provenance_unknown: true,
            native_plugin_grants: input.settings.archived.then(|| {
                vec![MissionPluginGrant {
                    connection_id: "00000000-0000-4000-8000-000000000003".into(),
                    revision: "connection-revision".into(),
                    author_id: "external-person".into(),
                }]
            }),
        })
        .collect();
    fs::write(
        root.join("mission-wire.json"),
        serde_json::to_string_pretty(&serde_json::json!({"creates":creates,"views":views}))
            .unwrap()
            + "\n",
    )
    .unwrap();
    export::<McpInput>(root, &mut generator, Contract::Deserialize);
    export::<McpIdInput>(root, &mut generator, Contract::Deserialize);
    export::<UpdateMcpInput>(root, &mut generator, Contract::Deserialize);
    export::<ListMcpsInput>(root, &mut generator, Contract::Deserialize);
    export::<BeginMcpInput>(root, &mut generator, Contract::Deserialize);
    export::<McpView>(root, &mut generator, Contract::Serialize);
    export::<ListMcpsOutput>(root, &mut generator, Contract::Serialize);
    export::<McpMutationOutput>(root, &mut generator, Contract::Serialize);
    export::<BeginMcpOutput>(root, &mut generator, Contract::Serialize);
    export::<McpConnectionStatus>(root, &mut generator, Contract::Serialize);
    let corpus: serde_json::Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/mcp-contracts.json")).unwrap();
    let creates: Vec<McpInput> = corpus["creates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect();
    let replacements: Vec<UpdateMcpInput> = creates
        .iter()
        .map(|input| {
            serde_json::from_value(serde_json::json!({
                "id": "00000000-0000-4000-8000-000000000001",
                "revision": 1,
                "connection": input,
            }))
            .unwrap()
        })
        .collect();
    fs::write(
        root.join("mcp-wire.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "creates": creates,
            "replacements": replacements,
        }))
        .unwrap()
            + "\n",
    )
    .unwrap();
    let clients = serde_json::json!([
        {
            "name": "listMcps",
            "input": "ListMcpsInput",
            "output": "ListMcpsOutput",
            "method": "GET",
            "path": "/mcps"
        },
        {
            "name": "createMcp",
            "input": "McpInput",
            "output": "McpView",
            "method": "POST",
            "path": "/mcps"
        },
        {
            "name": "updateMcp",
            "input": "UpdateMcpInput",
            "output": "McpView",
            "method": "PUT",
            "path": "/mcps/{id}"
        },
        {
            "name": "deleteMcp",
            "input": "McpIdInput",
            "output": "McpMutationOutput",
            "method": "DELETE",
            "path": "/mcps/{id}"
        },
        {
            "name": "testMcp",
            "input": "McpIdInput",
            "output": "McpView",
            "method": "POST",
            "path": "/mcps/{id}/test"
        },
        {
            "name": "disconnectMcp",
            "input": "McpIdInput",
            "output": "McpMutationOutput",
            "method": "POST",
            "path": "/mcps/{id}/disconnect"
        },
        {
            "name": "beginMcpConnection",
            "input": "BeginMcpInput",
            "output": "BeginMcpOutput",
            "method": "POST",
            "path": "/mcps/{id}/connect"
        },
        {
            "name": "mcpConnectionStatus",
            "input": "McpIdInput",
            "output": "McpConnectionStatus",
            "method": "POST",
            "path": "/mcps/{id}/callback"
        },
        {
            "name": "cancelMcpConnection",
            "input": "McpIdInput",
            "output": "McpMutationOutput",
            "method": "POST",
            "path": "/mcps/{id}/cancel"
        }
    ]);
    fs::write(
        root.join("mcp-clients.json"),
        serde_json::to_string_pretty(&clients).unwrap() + "\n",
    )
    .unwrap();
    export::<PluginConnection>(root, &mut generator, Contract::Serialize);
    export::<PluginConfiguration>(root, &mut generator, Contract::Deserialize);
    export::<PluginFormInput>(root, &mut generator, Contract::Deserialize);
    export::<PluginActionInput>(root, &mut generator, Contract::Deserialize);
    export::<PluginEnabledInput>(root, &mut generator, Contract::Deserialize);
    export::<PluginInstanceInput>(root, &mut generator, Contract::Deserialize);
    export::<ManagePluginInput>(root, &mut generator, Contract::Deserialize);
    export::<ListPluginInstancesOutput>(root, &mut generator, Contract::Serialize);
    export::<ManagePluginOutput>(root, &mut generator, Contract::Serialize);
    export::<PluginFormResult>(root, &mut generator, Contract::Serialize);
    let plugin_corpus: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/plugin-contracts.json"
    ))
    .unwrap();
    let plugin_inputs: Vec<ManagePluginInput> = plugin_corpus["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| serde_json::from_value(value.clone()).unwrap())
        .collect();
    let plugin_instance = PluginConnection {
        id: "00000000-0000-4000-8000-000000000013".into(),
        plugin_id: "fixture".into(),
        name: "Test instance".into(),
        max_messages_per_hour: 20,
        max_per_person_per_hour: 5,
        enabled: true,
        revision: "revision".into(),
        settings: serde_json::from_value(
            serde_json::json!({ "nested": { "ids": [1,null,false] } }),
        )
        .unwrap(),
        ..Default::default()
    };
    let plugin_outputs = vec![
        ManagePluginOutput::Setup {
            manifest: serde_json::json!({ "id": "fixture" }),
            setup: serde_json::json!({ "steps": [] }),
            callback_url: None,
            management_url: None,
        },
        ManagePluginOutput::Saved {
            instance: plugin_instance.clone(),
        },
        ManagePluginOutput::Enabled {
            instance: plugin_instance.clone(),
        },
        ManagePluginOutput::People {
            instance: plugin_instance,
        },
        ManagePluginOutput::Form {
            form: PluginFormResult {
                values: serde_json::from_value(serde_json::json!({ "nested": [1,null,true] }))
                    .unwrap(),
                ..Default::default()
            },
        },
        ManagePluginOutput::Tested {
            ok: true,
            message: "Ready".into(),
        },
        ManagePluginOutput::View {
            view: serde_json::json!([{ "type": "text", "text": "Ready" }]),
        },
        ManagePluginOutput::Action {
            result: serde_json::json!({ "notice": "Updated", "view": [] }),
        },
        ManagePluginOutput::Signin {
            flow: PluginSigninFlow {
                state: "pending".into(),
                ..Default::default()
            },
        },
        ManagePluginOutput::SigninCancelled { cancelled: true },
        ManagePluginOutput::Disconnected {
            disconnected: true,
            revoked: None,
            message: "Disconnected".into(),
        },
        ManagePluginOutput::Deleted { deleted: true },
    ];
    fs::write(
        root.join("plugin-wire.json"),
        serde_json::to_string_pretty(
            &serde_json::json!({ "inputs": plugin_inputs, "outputs": plugin_outputs }),
        )
        .unwrap()
            + "\n",
    )
    .unwrap();
    fs::write(root.join("plugin-clients.json"), serde_json::to_string_pretty(&serde_json::json!([
        { "name": "managePlugin", "input": "ManagePluginInput", "output": "ManagePluginOutput", "method": "POST", "path": "/plugins/manage", "deferred": true }
    ])).unwrap() + "\n").unwrap();
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
    let mut definitions = generator.take_definitions(true);
    // Kotlin consumes run views as well as emitting commands. Honor Rust's
    // deserialization defaults (notably absent nullable timestamps in summaries).
    // The public output schemas above still describe the serialized wire shape.
    let mut run_decoder = SchemaSettings::draft07().for_deserialize().into_generator();
    run_decoder.subschema_for::<Run>();
    for (name, schema) in run_decoder.take_definitions(true) {
        if matches!(
            name.as_str(),
            "Run"
                | "RunState"
                | "RunSnapshot"
                | "RunSkill"
                | "RunWorkspace"
                | "RunRequester"
                | "RunTaskSnapshot"
                | "Retry"
                | "Cause"
        ) {
            definitions.insert(name, schema);
        }
    }
    let schema = serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "definitions": definitions,
    });
    fs::write(
        root.join("models.json"),
        serde_json::to_string_pretty(&schema).unwrap() + "\n",
    )
    .unwrap();
}
