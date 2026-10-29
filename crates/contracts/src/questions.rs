//! Questionnaires shared by the apps, management capabilities and run workspace.
use crate::*;
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum QuestionStatus {
    Pending,
    Answering,
    Answered,
    Cancelled,
}

impl QuestionStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Answering => "answering",
            Self::Answered => "answered",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct QuestionOption {
    // Retired engine questions may have label-only options. Current commands
    // require nonempty unique IDs in validate_question_fields.
    #[serde(default)]
    #[schemars(length(max = 200))]
    pub id: String,
    #[schemars(length(min = 1, max = 2000))]
    pub label: String,
    #[serde(default)]
    #[schemars(length(max = 4000))]
    pub description: String,
    #[serde(default)]
    pub recommended: bool,
}

fn single() -> String {
    "single".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct QuestionField {
    #[schemars(length(min = 1, max = 200))]
    pub id: String,
    #[schemars(length(min = 1, max = 10000))]
    pub title: String,
    #[serde(default)]
    pub secret: bool,
    #[serde(default = "single")]
    #[schemars(pattern("^(single|multiple)$"))]
    #[ts(type = "\"single\" | \"multiple\"")]
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schemars(range(min = 1, max = 30))]
    pub min_choices: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schemars(range(min = 1, max = 30))]
    pub max_choices: Option<usize>,
    #[serde(default)]
    #[schemars(length(max = 30))]
    pub options: Vec<QuestionOption>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatQuestion {
    pub id: String,
    pub chat_id: String,
    pub run_id: String,
    pub blocking: bool,
    #[serde(default)]
    pub source: String,
    #[schemars(with = "String", pattern("^(pending|answering|answered|cancelled)$"))]
    pub status: QuestionStatus,
    pub fields: Vec<QuestionField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub created_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub message_id: Option<String>,
}

impl ChatQuestion {
    pub fn is_open(&self) -> bool {
        matches!(
            self.status,
            QuestionStatus::Pending | QuestionStatus::Answering
        )
    }

    pub fn is_private(&self) -> bool {
        self.fields.iter().any(|field| field.secret)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct QuestionAnswer {
    #[serde(default)]
    #[schemars(length(max = 30))]
    pub selected: Vec<String>,
    #[serde(default)]
    #[schemars(length(max = 10000))]
    pub comment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct QuestionAnswerInput {
    #[schemars(with = "uuid::Uuid")]
    pub id: String,
    pub answers: BTreeMap<String, QuestionAnswer>,
}

impl QuestionAnswerInput {
    pub fn validate(&mut self) -> Result<(), String> {
        uuid::Uuid::parse_str(&self.id).map_err(|_| "Invalid answer message identity.")?;
        for answer in self.answers.values_mut() {
            answer.comment = answer.comment.trim().to_owned();
            if answer.comment.chars().count() > 10000
                || answer.selected.len() > 30
                || answer
                    .selected
                    .iter()
                    .any(|id| id.is_empty() || id.chars().count() > 200)
            {
                return Err("Answer is too large.".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AskQuestionsInput {
    #[schemars(with = "uuid::Uuid")]
    pub request_id: String,
    #[schemars(length(min = 1, max = 20))]
    pub questions: Vec<QuestionField>,
}

pub fn validate_question_fields(fields: &[QuestionField]) -> Result<(), String> {
    let bounded = |value: &str, minimum: usize, maximum: usize| {
        let length = value.chars().count();
        (minimum..=maximum).contains(&length)
    };
    if !(1..=20).contains(&fields.len())
        || serde_json::to_vec(fields)
            .map_err(|_| "Invalid questionnaire.")?
            .len()
            > 64 * 1024
    {
        return Err("Keep 1 to 20 questions within 64 KiB of encoded JSON.".into());
    }
    let mut ids = HashSet::new();
    for field in fields {
        let mut options = HashSet::new();
        if !ids.insert(&field.id)
            || !bounded(&field.id, 1, 200)
            || !bounded(&field.title, 1, 10000)
            || !matches!(field.mode.as_str(), "single" | "multiple")
            || field.options.len() > 30
            || field.options.iter().any(|option| {
                !options.insert(&option.id)
                    || !bounded(&option.id, 1, 200)
                    || !bounded(&option.label, 1, 2000)
                    || !bounded(&option.description, 0, 4000)
            })
        {
            return Err("Invalid questions or duplicate question/option identifiers.".into());
        }
        let minimum = field.min_choices.unwrap_or(1);
        let maximum = field.max_choices.unwrap_or(if field.mode == "single" {
            1
        } else {
            field.options.len()
        });
        let bounded = field.min_choices.is_some() || field.max_choices.is_some();
        if field.min_choices.is_some_and(|n| !(1..=30).contains(&n))
            || field.max_choices.is_some_and(|n| !(1..=30).contains(&n))
            || bounded
                && (minimum > maximum
                    || maximum > field.options.len()
                    || field.mode == "single" && maximum > 1)
        {
            return Err("Selection bounds must fit the mode and the available options.".into());
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatQuestionInput {
    pub chat_id: ConversationId,
    #[schemars(length(min = 1, max = 200))]
    pub question_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnswerChatQuestionInput {
    pub chat_id: ConversationId,
    pub question_id: String,
    pub answer: QuestionAnswerInput,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ListChatQuestionsOutput {
    pub questions: Vec<ChatQuestion>,
    pub next_offset: Option<u32>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WithdrawQuestionInput {
    #[schemars(pattern("^[a-f0-9]{64}$"))]
    pub question_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct AskQuestionsOutput {
    pub question_id: String,
    #[schemars(with = "String", pattern("^(pending|answering|answered|cancelled)$"))]
    pub status: QuestionStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct PendingQuestion {
    pub question_id: String,
    pub fields: Vec<QuestionField>,
    pub blocking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct PendingQuestionsOutput {
    pub questions: Vec<PendingQuestion>,
}
