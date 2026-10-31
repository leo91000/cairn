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
    export::<ReadSkillInput>(root, &mut generator, Contract::Deserialize);
    export::<SkillInput>(root, &mut generator, Contract::Deserialize);
    export::<ListSkillsInput>(root, &mut generator, Contract::Deserialize);
    export::<Skill>(root, &mut generator, Contract::Deserialize);
    export::<SkillSource>(root, &mut generator, Contract::Deserialize);
    export::<SkillFile>(root, &mut generator, Contract::Deserialize);
    export::<SkillLimits>(root, &mut generator, Contract::Deserialize);
    export::<SkillBundle>(root, &mut generator, Contract::Deserialize);
    export::<SkillFileContent>(root, &mut generator, Contract::Deserialize);
    export::<SkillUpload>(root, &mut generator, Contract::Deserialize);
    export::<SkillImport>(root, &mut generator, Contract::Deserialize);
    export::<SkillArchiveChunk>(root, &mut generator, Contract::Deserialize);
    export::<SkillDiffLine>(root, &mut generator, Contract::Deserialize);
    export::<SkillSourceChange>(root, &mut generator, Contract::Deserialize);
    export::<SkillSourceVersion>(root, &mut generator, Contract::Deserialize);
    export::<SkillSourceStatus>(root, &mut generator, Contract::Deserialize);
    export::<SkillChange>(root, &mut generator, Contract::Deserialize);
    export::<EditSkillFilesInput>(root, &mut generator, Contract::Deserialize);
    export::<SaveSkillInput>(root, &mut generator, Contract::Deserialize);
    export::<ReadSkillFileInput>(root, &mut generator, Contract::Deserialize);
    export::<ExportSkillArchiveInput>(root, &mut generator, Contract::Deserialize);
    export::<StageSkillUploadInput>(root, &mut generator, Contract::Deserialize);
    export::<UpdateSkillSourceInput>(root, &mut generator, Contract::Deserialize);
    export::<DeleteSkillOutput>(root, &mut generator, Contract::Deserialize);
    export::<ForgetSkillSourceOutput>(root, &mut generator, Contract::Deserialize);
    export::<SkillMetadata>(root, &mut generator, Contract::Deserialize);
    export::<ImportSkillInput>(root, &mut generator, Contract::Deserialize);
    export::<ImportProjectInput>(root, &mut generator, Contract::Deserialize);
    export::<ListSourceProjectsInput>(root, &mut generator, Contract::Deserialize);
    export::<PluginProject>(root, &mut generator, Contract::Deserialize);
    export::<PluginProjectPage>(root, &mut generator, Contract::Deserialize);
    let skill_clients = serde_json::json!([
        {
            "name": "listSkills",
            "input": "ListSkillsInput",
            "output": "Skill",
            "method": "GET",
            "path": "/skills",
            "query": [
                "scope"
            ],
            "optionalQuery": [
                "scope"
            ],
            "array": true
        },
        {
            "name": "readSkillBundle",
            "input": "SkillInput",
            "output": "SkillBundle",
            "method": "GET",
            "path": "/skills/{scope}/{name}/bundle"
        },
        {
            "name": "readSkillFile",
            "input": "ReadSkillFileInput",
            "output": "SkillFileContent",
            "method": "GET",
            "path": "/skills/{scope}/{name}/file",
            "query": [
                "path"
            ]
        },
        {
            "name": "editSkillFiles",
            "input": "EditSkillFilesInput",
            "output": "SkillBundle",
            "method": "POST",
            "path": "/skills/{scope}/{name}/bundle"
        },
        {
            "name": "saveSkill",
            "input": "SaveSkillInput",
            "output": "SkillMetadata",
            "method": "PUT",
            "path": "/skills/{scope}/{name}"
        },
        {
            "name": "importSkill",
            "deferred": true,
            "input": "ImportSkillInput",
            "output": "SkillImport",
            "method": "POST",
            "path": "/skills/import"
        },
        {
            "name": "deleteSkill",
            "input": "SkillInput",
            "output": "DeleteSkillOutput",
            "method": "DELETE",
            "path": "/skills/{scope}/{name}"
        },
        {
            "name": "exportSkillArchive",
            "input": "ExportSkillArchiveInput",
            "output": "SkillArchiveChunk",
            "method": "GET",
            "path": "/skills/{scope}/{name}/archive",
            "query": [
                "offset",
                "revision"
            ],
            "optionalQuery": [
                "revision"
            ]
        },
        {
            "name": "getSkillSourceStatus",
            "deferred": true,
            "input": "SkillInput",
            "output": "SkillSourceStatus",
            "method": "GET",
            "path": "/skills/{scope}/{name}/source"
        },
        {
            "name": "updateSkillSource",
            "deferred": true,
            "input": "UpdateSkillSourceInput",
            "output": "SkillBundle",
            "method": "POST",
            "path": "/skills/{scope}/{name}/source/update"
        },
        {
            "name": "forgetSkillSource",
            "input": "SkillInput",
            "output": "ForgetSkillSourceOutput",
            "method": "DELETE",
            "path": "/skills/{scope}/{name}/source"
        },
        {
            "name": "stageSkillUpload",
            "input": "StageSkillUploadInput",
            "output": "SkillUpload",
            "method": "POST",
            "path": "/skills/stage"
        },
        {
            "name": "importProject",
            "deferred": true,
            "input": "ImportProjectInput",
            "output": "Project",
            "method": "POST",
            "path": "/projects/import"
        },
        {
            "name": "listSourceProjects",
            "deferred": true,
            "input": "ListSourceProjectsInput",
            "output": "PluginProjectPage",
            "method": "GET",
            "path": "/plugins/{pluginId}/projects",
            "query": [
                "source",
                "instance",
                "page",
                "query"
            ]
        }
    ]);
    fs::write(
        root.join("skill-clients.json"),
        serde_json::to_string_pretty(&skill_clients).unwrap() + "\n",
    )
    .unwrap();
    export::<CodingAgentAccount>(root, &mut generator, Contract::Deserialize);
    export::<CodingAgentAccounts>(root, &mut generator, Contract::Deserialize);
    export::<AccountUsage>(root, &mut generator, Contract::Deserialize);
    export::<AccountWindow>(root, &mut generator, Contract::Deserialize);
    export::<AccountResets>(root, &mut generator, Contract::Deserialize);
    export::<AccountSignIn>(root, &mut generator, Contract::Deserialize);
    export::<ListAccountsInput>(root, &mut generator, Contract::Deserialize);
    export::<AccountInput>(root, &mut generator, Contract::Deserialize);
    export::<AccountSettings>(root, &mut generator, Contract::Deserialize);
    export::<ReplaceAccountInput>(root, &mut generator, Contract::Deserialize);
    export::<OrderAccountsInput>(root, &mut generator, Contract::Deserialize);
    export::<ConnectAccountInput>(root, &mut generator, Contract::Deserialize);
    export::<ReplaceAccountKeyInput>(root, &mut generator, Contract::Deserialize);
    export::<AccountCodeInput>(root, &mut generator, Contract::Deserialize);
    export::<AccountMutationOutput>(root, &mut generator, Contract::Deserialize);
    export::<ApiKeyProvider>(root, &mut generator, Contract::Deserialize);
    export::<AccountProviderInput>(root, &mut generator, Contract::Deserialize);
    export::<ConnectAccountOutput>(root, &mut generator, Contract::Deserialize);
    export::<ModelCatalog>(root, &mut generator, Contract::Deserialize);
    export::<CodingAgentModel>(root, &mut generator, Contract::Deserialize);
    export::<ModelRoute>(root, &mut generator, Contract::Deserialize);
    export::<ServiceTier>(root, &mut generator, Contract::Deserialize);
    export::<ReasoningOption>(root, &mut generator, Contract::Deserialize);
    export::<ListModelsInput>(root, &mut generator, Contract::Deserialize);
    export::<AccountProvidersOutput>(root, &mut generator, Contract::Deserialize);
    export::<AccountSignInProgressOutput>(root, &mut generator, Contract::Deserialize);
    export::<AccountSignInProgress>(root, &mut generator, Contract::Deserialize);
    export::<OrderAccountsOutput>(root, &mut generator, Contract::Deserialize);
    export::<RefreshAccountsOutput>(root, &mut generator, Contract::Deserialize);
    let account_clients = serde_json::json!([
        {"name":"listModels","input":"ListModelsInput","output":"ModelCatalog","method":"GET","path":"/{provider}/models"},
        {"name":"listCodingAgentAccountProviders","input":"AccountProviderInput","output":"AccountProvidersOutput","method":"GET","path":"/accounts/providers/{provider}"},
        {"name":"getCodingAgentAccountSignIn","input":"ListAccountsInput","output":"AccountSignInProgressOutput","method":"GET","path":"/accounts/sign-in"},
        {"name":"listCodingAgentAccounts","input":"ListAccountsInput","output":"CodingAgentAccounts","method":"GET","path":"/accounts"},
        {"name":"refreshCodingAgentAccounts","input":"ListAccountsInput","output":"RefreshAccountsOutput","method":"POST","path":"/accounts/refresh"},
        {"name":"connectCodingAgentAccount","input":"ConnectAccountInput","output":"ConnectAccountOutput","method":"POST","path":"/accounts"},
        {"name":"reconnectCodingAgentAccount","input":"AccountInput","output":"ConnectAccountOutput","method":"POST","path":"/accounts/{accountId}/sign-in"},
        {"name":"replaceCodingAgentAccountKey","input":"ReplaceAccountKeyInput","output":"ConnectAccountOutput","method":"POST","path":"/accounts/{accountId}/api-key"},
        {"name":"replaceCodingAgentAccount","input":"ReplaceAccountInput","output":"CodingAgentAccount","method":"PUT","path":"/accounts/{accountId}"},
        {"name":"orderCodingAgentAccounts","input":"OrderAccountsInput","output":"OrderAccountsOutput","method":"POST","path":"/accounts/order"},
        {"name":"deleteCodingAgentAccount","input":"AccountInput","output":"AccountMutationOutput","method":"DELETE","path":"/accounts/{accountId}"},
        {"name":"submitCodingAgentAccountCode","input":"AccountCodeInput","output":"AccountMutationOutput","method":"POST","path":"/accounts/sign-in/code"},
        {"name":"cancelCodingAgentAccountSignIn","input":"ListAccountsInput","output":"AccountMutationOutput","method":"DELETE","path":"/accounts/sign-in"}
    ]);
    fs::write(
        root.join("account-clients.json"),
        serde_json::to_string_pretty(&account_clients).unwrap() + "\n",
    )
    .unwrap();

    export::<DelegationProject>(root, &mut generator, Contract::Deserialize);
    export::<DelegateTaskInput>(root, &mut generator, Contract::Deserialize);
    export::<TaskCancelInput>(root, &mut generator, Contract::Deserialize);
    export::<TaskStatusInput>(root, &mut generator, Contract::Deserialize);
    export::<DelegatedTask>(root, &mut generator, Contract::Deserialize);
    export::<TaskStatusOutput>(root, &mut generator, Contract::Deserialize);

    export::<WithdrawQuestionInput>(root, &mut generator, Contract::Deserialize);
    export::<AskQuestionsOutput>(root, &mut generator, Contract::Deserialize);
    export::<PendingQuestion>(root, &mut generator, Contract::Deserialize);
    export::<PendingQuestionsOutput>(root, &mut generator, Contract::Deserialize);

    export::<ChatQuestion>(root, &mut generator, Contract::Deserialize);
    export::<QuestionField>(root, &mut generator, Contract::Deserialize);
    export::<QuestionOption>(root, &mut generator, Contract::Deserialize);
    export::<QuestionAnswer>(root, &mut generator, Contract::Deserialize);
    export::<QuestionAnswerInput>(root, &mut generator, Contract::Deserialize);
    export::<AskQuestionsInput>(root, &mut generator, Contract::Deserialize);
    export::<ChatQuestionInput>(root, &mut generator, Contract::Deserialize);
    export::<AnswerChatQuestionInput>(root, &mut generator, Contract::Deserialize);
    export::<ListChatQuestionsOutput>(root, &mut generator, Contract::Deserialize);
    export::<WaitForOutput>(root, &mut generator, Contract::Deserialize);
    export::<ChatWakeup>(root, &mut generator, Contract::Deserialize);
    export::<WaitExternalCondition>(root, &mut generator, Contract::Deserialize);
    export::<WaitResult>(root, &mut generator, Contract::Deserialize);
    export::<WaitCondition>(root, &mut generator, Contract::Deserialize);
    export::<TimeCondition>(root, &mut generator, Contract::Deserialize);
    export::<WaitObservation>(root, &mut generator, Contract::Deserialize);
    export::<WaitObservationMember>(root, &mut generator, Contract::Deserialize);
    export::<WaitDisplay>(root, &mut generator, Contract::Deserialize);
    export::<WaitResultDisplay>(root, &mut generator, Contract::Deserialize);
    export::<WaitMemberProgress>(root, &mut generator, Contract::Deserialize);
    export::<PluginWaitData>(root, &mut generator, Contract::Deserialize);

    export::<ConversationRetention>(root, &mut generator, Contract::Deserialize);
    export::<GetConversationRetentionInput>(root, &mut generator, Contract::Deserialize);
    export::<ChatExecution>(root, &mut generator, Contract::Deserialize);
    export::<CodingAgentOptions>(root, &mut generator, Contract::Deserialize);
    export::<SwitchChatCodingAgentInput>(root, &mut generator, Contract::Deserialize);
    export::<EditChatMessageInput>(root, &mut generator, Contract::Deserialize);
    export::<ChatMessageReference>(root, &mut generator, Contract::Deserialize);
    export::<ListChatsInput>(root, &mut generator, Contract::Deserialize);
    export::<GetChatInput>(root, &mut generator, Contract::Deserialize);
    export::<ReadChatContentInput>(root, &mut generator, Contract::Deserialize);
    export::<ChatContentOutput>(root, &mut generator, Contract::Deserialize);
    export::<GetChatOutput>(root, &mut generator, Contract::Deserialize);
    export::<ListChatsOutput>(root, &mut generator, Contract::Deserialize);
    export::<DeleteChatMessageOutput>(root, &mut generator, Contract::Deserialize);
    export::<ConversationId>(root, &mut generator, Contract::Deserialize);
    export::<CreateChatInput>(root, &mut generator, Contract::Deserialize);
    export::<ChatInput>(root, &mut generator, Contract::Deserialize);
    export::<ChatMessageInput>(root, &mut generator, Contract::Deserialize);
    export::<SendChatMessageInput>(root, &mut generator, Contract::Deserialize);
    export::<RenameChatInput>(root, &mut generator, Contract::Deserialize);
    export::<SetChatPausedInput>(root, &mut generator, Contract::Deserialize);
    export::<ConfirmChatInput>(root, &mut generator, Contract::Deserialize);
    export::<UploadChatAttachmentInput>(root, &mut generator, Contract::Deserialize);
    export::<ChatAttachment>(root, &mut generator, Contract::Deserialize);
    export::<ChatMessage>(root, &mut generator, Contract::Deserialize);
    export::<Chat>(root, &mut generator, Contract::Deserialize);
    export::<ChatCompaction>(root, &mut generator, Contract::Deserialize);
    export::<ChatRequester>(root, &mut generator, Contract::Deserialize);
    export::<ConversationRequester>(root, &mut generator, Contract::Deserialize);
    export::<CodingAgentSwitch>(root, &mut generator, Contract::Deserialize);
    export::<ContextUsage>(root, &mut generator, Contract::Deserialize);
    export::<ExecutionNode>(root, &mut generator, Contract::Deserialize);
    export::<NodeConfiguration>(root, &mut generator, Contract::Deserialize);
    export::<NodeCapabilities>(root, &mut generator, Contract::Deserialize);
    export::<NodeResources>(root, &mut generator, Contract::Deserialize);
    export::<NativeModeRequest>(root, &mut generator, Contract::Deserialize);
    export::<NativeModeObserved>(root, &mut generator, Contract::Deserialize);
    export::<NativeModeProposal>(root, &mut generator, Contract::Deserialize);
    export::<NodeStoragePolicy>(root, &mut generator, Contract::Deserialize);
    export::<NodeDiskAccounting>(root, &mut generator, Contract::Deserialize);
    export::<NodeAgent>(root, &mut generator, Contract::Deserialize);
    export::<NodeUsage>(root, &mut generator, Contract::Deserialize);
    export::<StaleDisks>(root, &mut generator, Contract::Deserialize);
    export::<NativeCaller>(root, &mut generator, Contract::Deserialize);
    export::<NativeModePreview>(root, &mut generator, Contract::Deserialize);
    export::<NativeModeChange>(root, &mut generator, Contract::Deserialize);
    export::<ChangeNativeNodeModeInput>(root, &mut generator, Contract::Deserialize);
    export::<NativeModeChangeOutput>(root, &mut generator, Contract::Deserialize);
    export::<CreateNodeEnrollmentInput>(root, &mut generator, Contract::Deserialize);
    export::<UpdateNodeInput>(root, &mut generator, Contract::Deserialize);
    export::<UpdateNodeStorageInput>(root, &mut generator, Contract::Deserialize);
    export::<RevokeNodeInput>(root, &mut generator, Contract::Deserialize);
    export::<NodeRevocation>(root, &mut generator, Contract::Deserialize);
    export::<NodeEnrollment>(root, &mut generator, Contract::Deserialize);
    export::<ListNodesInput>(root, &mut generator, Contract::Deserialize);
    export::<NodeInput>(root, &mut generator, Contract::Deserialize);
    let chat_clients = serde_json::json!(
        [
        {"name":"answerChatQuestion","input":"AnswerChatQuestionInput","output":"ChatQuestion","method":"POST","path":"/chats/{chatId}/questions/{questionId}/answer","bodyField":"answer"},
        {"name":"cancelChatQuestion","input":"ChatQuestionInput","output":"ChatQuestion","method":"POST","path":"/chats/{chatId}/questions/{questionId}/cancel"},

            {
                "name": "getConversationRetention",
                "input": "GetConversationRetentionInput",
                "output": "ConversationRetention",
                "method": "GET",
                "path": "/settings/conversations"
            },
            {
                "name": "setConversationRetention",
                "input": "ConversationRetention",
                "output": "ConversationRetention",
                "method": "PUT",
                "path": "/settings/conversations"
            },
            {
                "name": "createChat",
                "input": "CreateChatInput",
                "output": "Chat",
                "method": "POST",
                "path": "/chats"
            },
            {
                "name": "getChat",
                "input": "ChatInput",
                "output": "Chat",
                "method": "GET",
                "path": "/chats/{chatId}"
            },
            {
                "name": "sendChatMessage",
                "bodyField": "message",
                "input": "SendChatMessageInput",
                "output": "ChatMessage",
                "method": "POST",
                "path": "/chats/{chatId}/messages"
            },
            {
                "name": "editChatMessage",
                "bodyField": "message",
                "input": "EditChatMessageInput",
                "output": "ChatMessage",
                "method": "PUT",
                "path": "/chats/{chatId}/messages/{messageId}"
            },
            {
                "name": "deleteChatMessage",
                "input": "ChatMessageReference",
                "output": "DeleteChatMessageOutput",
                "method": "DELETE",
                "path": "/chats/{chatId}/messages/{messageId}"
            },
            {
                "name": "setChatPaused",
                "input": "SetChatPausedInput",
                "output": "Chat",
                "method": "POST",
                "path": "/chats/{chatId}/pause"
            },
            {
                "name": "stopChat",
                "input": "ChatInput",
                "output": "Chat",
                "method": "POST",
                "path": "/chats/{chatId}/stop"
            },
            {
                "name": "compactChat",
                "input": "ChatInput",
                "output": "Chat",
                "method": "POST",
                "path": "/chats/{chatId}/compact"
            },
            {
                "name": "restartChatSession",
                "input": "ConfirmChatInput",
                "output": "Chat",
                "method": "POST",
                "path": "/chats/{chatId}/new-session"
            },
            {
                "name": "restoreChat",
                "input": "ChatInput",
                "output": "Chat",
                "method": "POST",
                "path": "/chats/{chatId}/restore"
            },
            {
                "name": "deleteChat",
                "input": "ConfirmChatInput",
                "output": "Chat",
                "method": "DELETE",
                "path": "/chats/{chatId}"
            },
            {
                "name": "convertRunToChat",
                "input": "RunInput",
                "output": "Chat",
                "method": "POST",
                "path": "/runs/{runId}/chat"
            }
        ]
    );
    fs::write(
        root.join("chat-clients.json"),
        serde_json::to_string_pretty(&chat_clients).unwrap() + "\n",
    )
    .unwrap();
    let node_clients = serde_json::json!([
        {"name":"createNodeEnrollment", "input":"CreateNodeEnrollmentInput", "output":"NodeEnrollment", "method":"POST", "path":"/nodes/enrollments"},
        {"name":"updateNodeStorage", "input":"UpdateNodeStorageInput", "output":"ExecutionNode", "method":"PUT", "path":"/nodes/{nodeId}/storage"},
        {"name":"revokeNode", "input":"RevokeNodeInput", "output":"NodeRevocation", "method":"POST", "path":"/nodes/{nodeId}/revoke"}
    ]);
    fs::write(
        root.join("node-clients.json"),
        serde_json::to_string_pretty(&node_clients).unwrap() + "\n",
    )
    .unwrap();
    let run_clients = serde_json::json!([
        {
            "name": "readRunHistory", "input": "ReadRunHistoryInput", "output": "HistoryPage",
            "method": "GET", "path": "/runs/{runId}/history", "query": ["before", "history"],
        },
        {
            "name": "runMission", "input": "RunMissionInput", "output": "Run",
            "method": "POST", "path": "/tasks/{taskId}/run",
        },
        {
            "name": "retryRun", "input": "RunInput", "output": "Run",
            "method": "POST", "path": "/runs/{runId}/retry",
        },
        {
            "name": "cancelRun", "input": "RunInput", "output": "CancelRunOutput",
            "method": "POST", "path": "/runs/{runId}/cancel",
        },
        {
            "name": "cleanupRun", "input": "RunInput", "output": "CleanupRunOutput",
            "method": "POST", "path": "/runs/{runId}/cleanup",
        },
    ]);
    fs::write(
        root.join("run-clients.json"),
        serde_json::to_string_pretty(&run_clients).unwrap() + "\n",
    )
    .unwrap();
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
    export::<RunEvent>(root, &mut generator, Contract::Deserialize);
    export::<RunHistoryWindowOutput>(root, &mut generator, Contract::Serialize);
    export::<ReadRunHistoryInput>(root, &mut generator, Contract::Deserialize);
    export::<HistoryPage>(root, &mut generator, Contract::Deserialize);
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
    export::<ListPluginPackagesInput>(root, &mut generator, Contract::Deserialize);
    export::<DeletePluginPackageOutput>(root, &mut generator, Contract::Serialize);
    export::<PluginManifest>(root, &mut generator, Contract::Serialize);
    export::<PluginSetup>(root, &mut generator, Contract::Serialize);
    export::<PackageVersion>(root, &mut generator, Contract::Serialize);
    export::<PluginEntry>(root, &mut generator, Contract::Serialize);
    export::<PluginCatalog>(root, &mut generator, Contract::Serialize);
    export::<ProgramRelease>(root, &mut generator, Contract::Serialize);
    export::<InstalledProgram>(root, &mut generator, Contract::Serialize);
    export::<ProgramState>(root, &mut generator, Contract::Serialize);
    export::<ListPluginResourcesInput>(root, &mut generator, Contract::Deserialize);
    export::<ListPluginResourcesOutput>(root, &mut generator, Contract::Serialize);
    export::<ResolvePluginResourceInput>(root, &mut generator, Contract::Deserialize);
    export::<ResolvePluginResourceOutput>(root, &mut generator, Contract::Serialize);
    export::<ListPluginProgramsInput>(root, &mut generator, Contract::Deserialize);
    export::<ListPluginProgramsOutput>(root, &mut generator, Contract::Serialize);
    export::<InstallPluginPackageInput>(root, &mut generator, Contract::Deserialize);
    export::<ApprovePluginPackageInput>(root, &mut generator, Contract::Deserialize);
    export::<ActivatePluginPackageInput>(root, &mut generator, Contract::Deserialize);
    export::<PluginPackageInput>(root, &mut generator, Contract::Deserialize);
    export::<PluginOperationInput>(root, &mut generator, Contract::Deserialize);
    export::<PluginProgramInput>(root, &mut generator, Contract::Deserialize);
    export::<PluginOperationResult>(root, &mut generator, Contract::Serialize);
    export::<PluginOperation>(root, &mut generator, Contract::Serialize);
    export::<PluginOperationStarted>(root, &mut generator, Contract::Serialize);
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
            manifest: Box::new(PluginManifest::parse("json",r#"{"id":"fixture","name":"Fixture","version":"1.0.0","apiVersion":2,"capabilities":[]}"#).unwrap()),
            setup: PluginSetup::default(),
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
        {"name": "listPluginPackages", "input": "ListPluginPackagesInput", "output": "PluginCatalog", "method": "GET", "path": "/plugins"},
        {"name": "installPluginPackage", "input": "InstallPluginPackageInput", "output": "PackageVersion", "method": "POST", "path": "/plugins/packages"},
        {"deferred": true, "name": "approvePluginPackage", "input": "ApprovePluginPackageInput", "output": "PackageVersion", "method": "POST", "path": "/plugins/packages/{pluginId}/approve"},
        {"deferred": true, "name": "activatePluginPackage", "input": "ActivatePluginPackageInput", "output": "PackageVersion", "method": "POST", "path": "/plugins/packages/{pluginId}/activate"},
        {"name": "getPluginResourceOperation", "input": "PluginOperationInput", "output": "PluginOperation", "method": "GET", "path": "/plugins/resources/operations/{operationId}"},
        {"name": "listPluginResources", "input": "ListPluginResourcesInput", "output": "ListPluginResourcesOutput", "method": "GET", "deferred": true, "path": "/plugins/{pluginId}/connections/{instanceId}/resources", "query": ["resourceType", "page", "query"]},
        {"name": "resolvePluginResource", "input": "ResolvePluginResourceInput", "output": "ResolvePluginResourceOutput", "method": "GET", "deferred": true, "path": "/plugins/{pluginId}/connections/{instanceId}/resources/ancestors", "query": ["resourceType", "id"]},
        {"name": "listPluginPrograms", "input": "ListPluginProgramsInput", "output": "ListPluginProgramsOutput", "method": "GET", "path": "/plugins/{pluginId}/programs"},
        {"name": "getPluginOperation", "input": "PluginOperationInput", "output": "PluginOperation", "method": "GET", "path": "/plugins/packages/operations/{operationId}"},
        { "name": "preparePluginProgram", "input": "PluginProgramInput", "output": "ProgramState", "method": "POST", "path": "/plugins/programs/prepare", "deferred": true },
        { "name": "deletePluginPackage", "input": "PluginPackageInput", "output": "DeletePluginPackageOutput", "method": "DELETE", "path": "/plugins/packages/{pluginId}" },
        { "name": "cancelPluginOperation", "input": "PluginOperationInput", "output": "PluginOperation", "method": "DELETE", "path": "/plugins/packages/operations/{operationId}" },
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
    export::<ReadTarget>(root, &mut generator, Contract::Deserialize);
    export::<ReadMark>(root, &mut generator, Contract::Deserialize);
    export::<MarkReadInput>(root, &mut generator, Contract::Deserialize);
    export::<MarkReadOutput>(root, &mut generator, Contract::Serialize);
    export::<OutcomeAsideAction>(root, &mut generator, Contract::Deserialize);
    export::<SetOutcomeAsideInput>(root, &mut generator, Contract::Deserialize);
    export::<OutcomeDismissal>(root, &mut generator, Contract::Serialize);
    export::<SetOutcomeAsideOutput>(root, &mut generator, Contract::Serialize);
    export::<GetInstallationSettingsInput>(root, &mut generator, Contract::Deserialize);
    export::<InstallationSettings>(root, &mut generator, Contract::Serialize);
    export::<GetInstallationOverviewInput>(root, &mut generator, Contract::Deserialize);
    export::<InstallationOverview>(root, &mut generator, Contract::Serialize);
    export::<ListAuditInput>(root, &mut generator, Contract::Deserialize);
    export::<AuditEntry>(root, &mut generator, Contract::Serialize);
    export::<ListAuditOutput>(root, &mut generator, Contract::Serialize);
    export::<GetStorageSettingsInput>(root, &mut generator, Contract::Deserialize);
    export::<StorageSettings>(root, &mut generator, Contract::Serialize);
    export::<SaveStorageSettingsInput>(root, &mut generator, Contract::Deserialize);
    export::<CheckStorageInput>(root, &mut generator, Contract::Deserialize);
    export::<StorageCheck>(root, &mut generator, Contract::Serialize);
    let settings_clients = serde_json::json!([
        { "name": "markRead", "input": "MarkReadInput", "output": "MarkReadOutput", "method": "POST", "path": "/reads" },
        { "name": "setOutcomeAside", "input": "SetOutcomeAsideInput", "output": "SetOutcomeAsideOutput", "method": "POST", "path": "/dismissals" },
        { "name": "getInstallationSettings", "input": "GetInstallationSettingsInput", "output": "InstallationSettings", "method": "GET", "path": "/settings" },
        { "name": "getInstallationOverview", "input": "GetInstallationOverviewInput", "output": "InstallationOverview", "method": "GET", "path": "/overview" },
        { "name": "listAudit", "input": "ListAuditInput", "output": "ListAuditOutput", "method": "GET", "path": "/audit" },
        { "name": "getStorageSettings", "input": "GetStorageSettingsInput", "output": "StorageSettings", "method": "GET", "path": "/settings/storage" },
        { "name": "saveStorageSettings", "input": "SaveStorageSettingsInput", "output": "StorageSettings", "method": "PUT", "path": "/settings/storage" },
        { "name": "checkStorage", "input": "CheckStorageInput", "output": "StorageCheck", "method": "POST", "path": "/settings/storage/check" }
    ]);
    fs::write(
        root.join("settings-clients.json"),
        serde_json::to_string_pretty(&settings_clients).unwrap() + "\n",
    )
    .unwrap();
    let mut definitions = generator.take_definitions(true);
    // Kotlin consumes run views as well as emitting commands. Honor Rust's
    // deserialization defaults (notably absent nullable timestamps in summaries).
    // The public output schemas above still describe the serialized wire shape.
    let mut account_decoder = SchemaSettings::draft07().for_deserialize().into_generator();
    account_decoder.subschema_for::<CodingAgentAccounts>();
    account_decoder.subschema_for::<ModelCatalog>();
    for (name, schema) in account_decoder.take_definitions(true) {
        if name != "AgentProvider" {
            definitions.insert(name, schema);
        }
    }
    let mut chat_decoder = SchemaSettings::draft07().for_deserialize().into_generator();
    chat_decoder.subschema_for::<Chat>();
    for (name, schema) in chat_decoder.take_definitions(true) {
        if matches!(
            name.as_str(),
            "Chat"
                | "ChatMessage"
                | "ChatAttachment"
                | "ChatCompaction"
                | "CodingAgentSwitch"
                | "ChatRequester"
                | "ConversationRequester"
                | "ContextUsage"
        ) {
            definitions.insert(name, schema);
        }
    }
    let mut node_decoder = SchemaSettings::draft07().for_deserialize().into_generator();
    node_decoder.subschema_for::<ExecutionNode>();
    for (name, schema) in node_decoder.take_definitions(true) {
        definitions.insert(name, schema);
    }
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
