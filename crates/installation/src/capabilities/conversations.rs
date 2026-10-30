//! Conversation capabilities shared by HTTP and both management tools.
use super::{ResumeContext, decode};
use crate::{
    error::{Error, Result},
    service::Service,
};
use cairn_contracts::*;
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) struct ChatOperation {
    pub name: &'static str,
    scope: &'static str,
    description: &'static str,
}

pub(crate) const CHAT_OPERATIONS: &[ChatOperation] = &[
    ChatOperation {
        name: "upload_chat_attachment",
        scope: "manage",
        description: "Upload a file to a conversation as base64 data (up to 10 MiB decoded, subject to transport request limits). Supply a fresh UUID attachmentId; identical retries are safe. Include its ID in message.attachmentIds to send it.",
    },
    ChatOperation {
        name: "list_chat_questions",
        scope: "read",
        description: "List conversation questionnaires with pagination, including fields and delivery status; saved answers are never returned.",
    },
    ChatOperation {
        name: "answer_chat_question",
        scope: "run",
        description: "Answer a pending questionnaire. Supply selected option IDs and a separate comment for every field, with a stable answer message UUID for retries.",
    },
    ChatOperation {
        name: "cancel_chat_question",
        scope: "run",
        description: "Cancel a pending questionnaire without supplying an answer or granting permission. Management can cancel a question from any turn; workspace withdraw_question remains limited to its own run.",
    },
    ChatOperation {
        name: "list_chat_side_questions",
        scope: "read",
        description: "List side questions asked beside a conversation's running reply, oldest first, with their answers, and whether one can be asked now.",
    },
    ChatOperation {
        name: "ask_chat_side_question",
        scope: "run",
        description: "Ask the running reply's coding agent a question about the conversation, optionally about a quoted passage of one of its replies. It answers from its current context with every tool denied, beside the turn: the question never steers the reply, joins the queue or enters the conversation history. Only Claude Code replies support this; Codex and OpenCode report it unavailable. Supply a fresh UUID id; retrying with the same content returns the same question. Read the answer with list_chat_side_questions.",
    },
    ChatOperation {
        name: "cancel_chat_side_question",
        scope: "run",
        description: "Withdraw a pending side question. The agent stops answering it and nothing is reported.",
    },
    ChatOperation {
        name: "get_conversation_retention",
        scope: "read",
        description: "Read the installation inactivity policy. Default 7 days; 0 disables automatic environment expiration. Conversations, uploaded attachments, published artifact versions and public links are retained.",
    },
    ChatOperation {
        name: "set_conversation_retention",
        scope: "manage",
        description: "Set the installation inactivity delay, in whole days from 0 through 36500. Zero disables automatic environment expiration. Expiration removes working environments and unpushed changes; history, uploaded attachments and published artifacts with their public links are retained. Pending work is protected. Explicit deletion still removes the entire conversation.",
    },
    ChatOperation {
        name: "get_chat_coding_agent_options",
        scope: "read",
        description: "Read coding agents currently available to recover this conversation's quota-blocked turn.",
    },
    ChatOperation {
        name: "switch_chat_coding_agent",
        scope: "run",
        description: "Recover the same quota-blocked turn on an available coding agent, retaining its workspace, history and current authorization.",
    },
    ChatOperation {
        name: "list_chats",
        scope: "read",
        description: "List conversations with pagination, optional agent/project filters and active/trash view. Returns metadata including accountWaitReason and accountRequired for quota waits, without messages or marking chats read. unread and preview describe the Cairn account that authorized this connection; executions receive none. Includes environmentState, environmentExpiredAt and environmentError; after expiration, a new message starts a fresh environment using preserved history.",
    },
    ChatOperation {
        name: "get_chat",
        scope: "read",
        description: "Read conversation metadata and a bounded page of user/queued messages in chronological order. Use chat.runId with get_run for assistant replies and activity. Continue with nextOffset while hasMore. Use read_chat_content for records marked truncated. Private answers are redacted. Includes environmentState, environmentExpiredAt and environmentError; after expiration, a new message starts a fresh environment using preserved history.",
    },
    ChatOperation {
        name: "create_chat",
        scope: "manage",
        description: "Create an empty conversation for an agent and optional project. Send a message separately to start execution.",
    },
    ChatOperation {
        name: "convert_run_to_chat",
        scope: "manage",
        description: "Move a successfully completed mission run into Chats, preserving its session, workspace and history. The original mission schedule remains unchanged. No execution starts until a message is sent. Retrying returns the same chat.",
    },
    ChatOperation {
        name: "send_chat_message",
        scope: "run",
        description: "Send a message to a conversation. Supply a fresh UUID message.id; reuse it with identical content for safe retries. Queue waits for the current turn; steer joins it. Starts agent work with its configured permissions.",
    },
    ChatOperation {
        name: "edit_chat_message",
        scope: "run",
        description: "Replace a queued message before delivery. Delivered or sending messages cannot be edited.",
    },
    ChatOperation {
        name: "delete_chat_message",
        scope: "run",
        description: "Remove a queued ordinary message before delivery. Submitted questionnaire answers cannot be removed; send a new message to correct them.",
    },
    ChatOperation {
        name: "rename_chat",
        scope: "manage",
        description: "Set a conversation title. Automatic title evaluation may update it after later replies.",
    },
    ChatOperation {
        name: "set_chat_paused",
        scope: "run",
        description: "Pause or resume the follow-up queue. Pausing lets the current turn finish; resuming permits queued agent work. Use stop_chat to interrupt the current turn.",
    },
    ChatOperation {
        name: "stop_chat",
        scope: "run",
        description: "Stop the current turn or pending automatic retry, cancel a scheduled wakeup and pause follow-ups. Queued messages remain available for later resumption.",
    },
    ChatOperation {
        name: "compact_chat",
        scope: "run",
        description: "Compact the coding agent's native context for an idle conversation whose last turn succeeded. Runs as its own execution, not a user message, and never rewrites the visible history. Codex and Claude Code only; get_chat reports chat.compaction availability and chat.contextUsage.",
    },
    ChatOperation {
        name: "delete_chat",
        scope: "manage",
        description: "Delete a conversation using the existing cleanup lifecycle. Busy chats require confirm: true to stop execution and cancel pending messages/questions. Cleanup may permanently remove history, files and workspace.",
    },
    ChatOperation {
        name: "resume_chat",
        scope: "run",
        description: "Resume a failed or stopped agent execution using its saved session, subject to existing recovery rules. Use set_chat_paused(false) to resume queued follow-ups.",
    },
    ChatOperation {
        name: "restart_chat_session",
        scope: "run",
        description: "Request a fresh agent session using preserved history and workspace after an unrecoverable session failure. Requires confirm: true and leaves the queue paused.",
    },
    ChatOperation {
        name: "restore_chat",
        scope: "manage",
        description: "Restore a deleted conversation only while the existing retention rules still permit recovery. Does not replay cancelled work.",
    },
    ChatOperation {
        name: "read_chat_content",
        scope: "read",
        description: "Read complete sanitized chat metadata, user messages and question fields as bounded JSON text chunks, including records marked truncated by get_chat or list_chat_questions. Assistant replies are available through get_run. Start at offset 0; concatenate data and pass nextOffset plus sha256 until nextOffset is null. Restart if content changes. Saved private answers are redacted.",
    },
];

pub(crate) fn chat_operation(name: &str) -> Option<&'static ChatOperation> {
    CHAT_OPERATIONS.iter().find(|op| op.name == name)
}

impl ChatOperation {
    pub(crate) fn descriptor(&self) -> Value {
        let (input, output) = match self.name {
            "upload_chat_attachment" => (
                schema_for::<UploadChatAttachmentInput>(Contract::Deserialize),
                schema_for::<ChatAttachment>(Contract::Serialize),
            ),
            "list_chat_questions" => (
                schema_for::<GetChatInput>(Contract::Deserialize),
                schema_for::<ListChatQuestionsOutput>(Contract::Serialize),
            ),
            "answer_chat_question" => (
                schema_for::<AnswerChatQuestionInput>(Contract::Deserialize),
                schema_for::<ChatQuestion>(Contract::Serialize),
            ),
            "cancel_chat_question" => (
                schema_for::<ChatQuestionInput>(Contract::Deserialize),
                schema_for::<ChatQuestion>(Contract::Serialize),
            ),
            "list_chat_side_questions" => (
                schema_for::<GetChatInput>(Contract::Deserialize),
                schema_for::<ListChatSideQuestionsOutput>(Contract::Serialize),
            ),
            "ask_chat_side_question" => (
                schema_for::<AskChatSideQuestionInput>(Contract::Deserialize),
                schema_for::<ChatSideQuestion>(Contract::Serialize),
            ),
            "cancel_chat_side_question" => (
                schema_for::<ChatSideQuestionInput>(Contract::Deserialize),
                schema_for::<ChatSideQuestion>(Contract::Serialize),
            ),

            "get_conversation_retention" => (
                schema_for::<GetConversationRetentionInput>(Contract::Deserialize),
                schema_for::<ConversationRetention>(Contract::Serialize),
            ),
            "set_conversation_retention" => (
                schema_for::<ConversationRetention>(Contract::Deserialize),
                schema_for::<ConversationRetention>(Contract::Serialize),
            ),
            "get_chat_coding_agent_options" => (
                schema_for::<ChatInput>(Contract::Deserialize),
                schema_for::<CodingAgentOptions>(Contract::Serialize),
            ),
            "switch_chat_coding_agent" => (
                schema_for::<SwitchChatCodingAgentInput>(Contract::Deserialize),
                schema_for::<Chat>(Contract::Serialize),
            ),
            "list_chats" => (
                schema_for::<ListChatsInput>(Contract::Deserialize),
                schema_for::<ListChatsOutput>(Contract::Serialize),
            ),
            "get_chat" => (
                schema_for::<GetChatInput>(Contract::Deserialize),
                schema_for::<GetChatOutput>(Contract::Serialize),
            ),
            "create_chat" => (
                schema_for::<CreateChatInput>(Contract::Deserialize),
                schema_for::<Chat>(Contract::Serialize),
            ),
            "convert_run_to_chat" => (
                schema_for::<RunInput>(Contract::Deserialize),
                schema_for::<Chat>(Contract::Serialize),
            ),
            "send_chat_message" => (
                schema_for::<SendChatMessageInput>(Contract::Deserialize),
                schema_for::<ChatMessage>(Contract::Serialize),
            ),
            "edit_chat_message" => (
                schema_for::<EditChatMessageInput>(Contract::Deserialize),
                schema_for::<ChatMessage>(Contract::Serialize),
            ),
            "delete_chat_message" => (
                schema_for::<ChatMessageReference>(Contract::Deserialize),
                schema_for::<DeleteChatMessageOutput>(Contract::Serialize),
            ),
            "rename_chat" => (
                schema_for::<RenameChatInput>(Contract::Deserialize),
                schema_for::<Chat>(Contract::Serialize),
            ),
            "set_chat_paused" => (
                schema_for::<SetChatPausedInput>(Contract::Deserialize),
                schema_for::<Chat>(Contract::Serialize),
            ),
            "stop_chat" => (
                schema_for::<ChatInput>(Contract::Deserialize),
                schema_for::<Chat>(Contract::Serialize),
            ),
            "compact_chat" => (
                schema_for::<ChatInput>(Contract::Deserialize),
                schema_for::<Chat>(Contract::Serialize),
            ),
            "delete_chat" => (
                schema_for::<ConfirmChatInput>(Contract::Deserialize),
                schema_for::<Chat>(Contract::Serialize),
            ),
            "resume_chat" => (
                schema_for::<ChatInput>(Contract::Deserialize),
                schema_for::<Run>(Contract::Serialize),
            ),
            "restart_chat_session" => (
                schema_for::<ConfirmChatInput>(Contract::Deserialize),
                schema_for::<Chat>(Contract::Serialize),
            ),
            "restore_chat" => (
                schema_for::<ChatInput>(Contract::Deserialize),
                schema_for::<Chat>(Contract::Serialize),
            ),
            "read_chat_content" => (
                schema_for::<ReadChatContentInput>(Contract::Deserialize),
                schema_for::<ChatContentOutput>(Contract::Serialize),
            ),
            _ => unreachable!(),
        };
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": input,
            "outputSchema": output,
            "_meta": { "securitySchemes": [{ "type": "oauth2", "scopes": [self.scope] }] },
            "annotations": { "readOnlyHint": self.scope == "read", "destructiveHint": self.scope != "read", "openWorldHint": self.scope != "read" }
        })
    }

    pub(crate) async fn invoke(
        &self,
        s: &Arc<Service>,
        input: Value,
        context: ResumeContext,
    ) -> Result<Value> {
        let reader = super::personal::listing_reader(&context);
        let (bearer, origin) = match context {
            ResumeContext::Human(account) => (None, crate::native::policy::Origin::Human(account)),
            ResumeContext::Public(scopes, _) => {
                if !scopes.iter().any(|scope| scope == self.scope) {
                    return Err(Error::forbidden(format!(
                        "The {} scope is required.",
                        self.scope
                    )));
                }
                (
                    None,
                    crate::native::policy::caller().map_or(
                        crate::native::policy::Origin::Unknown,
                        crate::native::policy::Origin::Human,
                    ),
                )
            }
            ResumeContext::Installation(bearer) => {
                let run = crate::installation_mcp::authorize(s, &bearer).await?;
                (
                    Some(bearer),
                    crate::native::policy::Origin::Run(
                        crate::validation::text(&run, "id").to_owned(),
                    ),
                )
            }
        };
        if self.name == "upload_chat_attachment" {
            use base64::Engine;
            let input: UploadChatAttachmentInput = decode(input)?;
            if input.name.is_empty()
                || input.name.chars().count() > 180
                || input.data.len() > 13981016
            {
                return Err(Error::bad("Invalid attachment name or encoded file size."));
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&input.data)
                .map_err(|_| Error::bad("Expected base64 file data."))?;
            let attachment = s
                .store_attachment_authorized(
                    &input.chat_id,
                    &input.attachment_id,
                    &input.name,
                    &bytes,
                    crate::installation_mcp::management_authorization(bearer.as_deref()),
                )
                .await?;
            return Ok(serde_json::to_value(attachment)?);
        }
        let args = match self.name {
            "list_chat_questions" => {
                let input: GetChatInput = decode(input)?;
                if !(1..=100).contains(&input.limit) {
                    return Err(Error::bad("Choose a limit from 1 to 100."));
                }
                serde_json::to_value(input)?
            }
            "answer_chat_question" => {
                let mut input: AnswerChatQuestionInput = decode(input)?;
                input.answer.validate().map_err(Error::bad)?;
                if input.question_id.is_empty() || input.question_id.chars().count() > 200 {
                    return Err(Error::bad("Invalid question identity."));
                }
                serde_json::to_value(input)?
            }
            "cancel_chat_question" => {
                let input: ChatQuestionInput = decode(input)?;
                if input.question_id.is_empty() || input.question_id.chars().count() > 200 {
                    return Err(Error::bad("Invalid question identity."));
                }
                serde_json::to_value(input)?
            }
            "list_chat_side_questions" => {
                let input: GetChatInput = decode(input)?;
                if !(1..=100).contains(&input.limit) {
                    return Err(Error::bad("Choose a limit from 1 to 100."));
                }
                serde_json::to_value(input)?
            }
            "ask_chat_side_question" => {
                let mut input: AskChatSideQuestionInput = decode(input)?;
                input.question.validate().map_err(Error::bad)?;
                serde_json::to_value(input)?
            }
            "cancel_chat_side_question" => {
                let input: ChatSideQuestionInput = decode(input)?;
                if uuid::Uuid::parse_str(&input.side_question_id).is_err() {
                    return Err(Error::bad("Invalid side question identity."));
                }
                serde_json::to_value(input)?
            }

            "get_conversation_retention" => {
                let _: GetConversationRetentionInput = decode(input)?;
                json!({})
            }
            "set_conversation_retention" => {
                let policy: ConversationRetention = decode(input)?;
                if policy.inactivity_days > 36500 {
                    return Err(Error::bad("Choose a whole number of days from 0 to 36500."));
                }
                serde_json::to_value(policy)?
            }
            "get_chat_coding_agent_options" => {
                let input: ChatInput = decode(input)?;
                serde_json::to_value(input)?
            }
            "switch_chat_coding_agent" => {
                let input: SwitchChatCodingAgentInput = decode(input)?;
                if input.turn_id.is_empty() || input.turn_id.chars().count() > 200 {
                    return Err(Error::bad("Invalid turn identifier."));
                }
                serde_json::to_value(input)?
            }
            "list_chats" => {
                let input: ListChatsInput = decode(input)?;
                if !(1..=100).contains(&input.limit)
                    || !matches!(input.view.as_str(), "active" | "trash")
                {
                    return Err(Error::bad(
                        "Choose active or trash and a limit from 1 to 100.",
                    ));
                }
                serde_json::to_value(input)?
            }
            "get_chat" => {
                let input: GetChatInput = decode(input)?;
                if !(1..=100).contains(&input.limit) {
                    return Err(Error::bad("Choose a limit from 1 to 100."));
                }
                serde_json::to_value(input)?
            }
            "create_chat" => {
                let input: CreateChatInput = decode(input)?;
                serde_json::to_value(input)?
            }
            "convert_run_to_chat" => {
                let input: RunInput = decode(input)?;
                serde_json::to_value(input)?
            }
            "send_chat_message" => {
                let input: SendChatMessageInput = decode(input)?;
                serde_json::to_value(input)?
            }
            "edit_chat_message" => {
                let input: EditChatMessageInput = decode(input)?;
                serde_json::to_value(input)?
            }
            "delete_chat_message" => {
                let input: ChatMessageReference = decode(input)?;
                serde_json::to_value(input)?
            }
            "rename_chat" => {
                let input: RenameChatInput = decode(input)?;
                serde_json::to_value(input)?
            }
            "set_chat_paused" => {
                let input: SetChatPausedInput = decode(input)?;
                serde_json::to_value(input)?
            }
            "stop_chat" => {
                let input: ChatInput = decode(input)?;
                serde_json::to_value(input)?
            }
            "compact_chat" => {
                let input: ChatInput = decode(input)?;
                serde_json::to_value(input)?
            }
            "delete_chat" => {
                let input: ConfirmChatInput = decode(input)?;
                serde_json::to_value(input)?
            }
            "resume_chat" => {
                let input: ChatInput = decode(input)?;
                serde_json::to_value(input)?
            }
            "restart_chat_session" => {
                let input: ConfirmChatInput = decode(input)?;
                serde_json::to_value(input)?
            }
            "restore_chat" => {
                let input: ChatInput = decode(input)?;
                serde_json::to_value(input)?
            }
            "read_chat_content" => {
                let input: ReadChatContentInput = decode(input)?;
                if input.offset < 0
                    || input.sha256.as_ref().is_some_and(|v| {
                        v.len() != 64
                            || !v
                                .bytes()
                                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
                    })
                {
                    return Err(Error::bad("Invalid content cursor."));
                }
                serde_json::to_value(input)?
            }
            _ => unreachable!(),
        };
        if self.name == "get_conversation_retention" {
            return s
                .conversation_retention_authorized(
                    crate::installation_mcp::management_authorization(bearer.as_deref()),
                )
                .await;
        }
        if self.name == "set_conversation_retention" {
            return s
                .save_conversation_retention_authorized(
                    args,
                    crate::installation_mcp::management_authorization(bearer.as_deref()),
                )
                .await;
        }
        let result = crate::native::policy::DELEGATION
            .scope(origin, async {
                if self.name == "convert_run_to_chat" {
                    let authorize =
                        crate::installation_mcp::management_authorization(bearer.as_deref());
                    return s
                        .chat_from_run_authorized(
                            crate::validation::text(&args, "runId"),
                            authorize,
                        )
                        .await;
                }
                crate::mcp_server::chat_tools::invoke(s, self.name, args, bearer.as_deref(), reader)
                    .await
            })
            .await?;
        if matches!(
            self.name,
            "create_chat"
                | "convert_run_to_chat"
                | "rename_chat"
                | "set_chat_paused"
                | "stop_chat"
                | "compact_chat"
                | "delete_chat"
                | "restart_chat_session"
                | "restore_chat"
        ) {
            return s.store.read(move |db| crate::chats::view(db, result)).await;
        }
        Ok(result)
    }
}
