//! Side questions: explanations the running agent answers from its current context,
//! beside its reply, without tools and outside the conversation's history.
use crate::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum SideQuestionStatus {
    Pending,
    Answered,
    Failed,
    Cancelled,
}

/// Why the conversation's reply cannot take a side question now, for clients that word
/// it themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
pub enum SideQuestionUnavailable {
    /// No reply is running.
    BetweenReplies,
    Codex,
    Opencode,
    /// The reply's coding agent could not be read.
    UnknownAgent,
    /// The reply is stopping or switching coding agent.
    Stopping,
    Compacting,
    /// The Claude Code release is older than the one qualified for side questions.
    ClaudeRelease,
    /// The reply has not offered side questions yet: starting, or a runtime not updated.
    NotOffered,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatSideQuestion {
    pub id: String,
    pub chat_id: String,
    pub run_id: String,
    pub text: String,
    /// The passage of a reply the question is about.
    #[serde(default)]
    pub quote: String,
    /// The agent message the question refers to.
    #[serde(default)]
    pub item_id: String,
    #[serde(default)]
    #[ts(optional = nullable)]
    #[schemars(with = "Option<String>", pattern("^(codex|claude|opencode)$"))]
    pub provider: Option<AgentProvider>,
    #[schemars(with = "String", pattern("^(pending|answered|failed|cancelled)$"))]
    pub status: SideQuestionStatus,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub answer: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub error: Option<String>,
    #[ts(type = "number")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub created_at: i64,
    #[serde(default)]
    #[ts(optional = nullable, type = "number | null")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub settled_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SideQuestionInput {
    /// Client-chosen identity: retrying with the same content returns the same question.
    #[schemars(with = "uuid::Uuid")]
    pub id: String,
    #[schemars(length(min = 1, max = 4000))]
    pub text: String,
    #[serde(default)]
    #[schemars(length(max = 4000))]
    pub quote: String,
    #[serde(default)]
    #[schemars(length(max = 200))]
    pub item_id: String,
}

impl SideQuestionInput {
    /// Trims the question and passage, then checks their bounds.
    pub fn validate(&mut self) -> Result<(), String> {
        uuid::Uuid::parse_str(&self.id).map_err(|_| "Invalid side question identity.")?;
        self.text = self.text.trim().to_owned();
        self.quote = self.quote.trim().to_owned();
        if self.text.is_empty() {
            return Err("Write a question.".into());
        }
        if self.text.chars().count() > 4000
            || self.quote.chars().count() > 4000
            || self.item_id.chars().count() > 200
        {
            return Err("This side question is too long.".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AskChatSideQuestionInput {
    pub chat_id: ConversationId,
    pub question: SideQuestionInput,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatSideQuestionInput {
    pub chat_id: ConversationId,
    #[schemars(with = "uuid::Uuid")]
    pub side_question_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ListChatSideQuestionsOutput {
    pub side_questions: Vec<ChatSideQuestion>,
    pub next_offset: Option<u32>,
    pub has_more: bool,
    /// The running reply's coding agent can answer a side question now.
    pub can_ask_side_question: bool,
    /// Why a side question cannot be asked now, when it cannot.
    pub side_question_reason: Option<String>,
    /// The case `side_question_reason` explains.
    #[schemars(
        with = "Option<String>",
        pattern(
            "^(between-replies|codex|opencode|unknown-agent|stopping|compacting|claude-release|not-offered)$"
        )
    )]
    pub side_question_unavailable: Option<SideQuestionUnavailable>,
}
