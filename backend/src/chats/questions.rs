//! Questions an agent asks the user during a chat, stored as
//! `chat-question:{chatId}:{id}` key-value records.
use super::{chat, chat_for_run, find_message, send};
use crate::{
    config::now,
    error::{Error, Result, required},
    notifications,
    service::Service,
    store::Db,
    validation::{parse, parse_as, string_enum, text},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

string_enum! {
    pub enum QuestionStatus {
        Pending => "pending",
        Answering => "answering",
        Answered => "answered",
        Cancelled => "cancelled",
    }
}

/// One entry of the validated `questions` input schema.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QuestionField {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub options: Vec<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Absent fields are tolerated and kept absent: records predate this type.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    #[serde(default)]
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub chat_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub run_id: String,
    #[serde(default)]
    pub blocking: bool,
    pub status: QuestionStatus,
    #[serde(default)]
    pub fields: Vec<QuestionField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    /// The queued answer message, while the answer is being delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Question {
    /// Still waiting for, or delivering, the user's answer.
    pub fn is_open(&self) -> bool {
        matches!(
            self.status,
            QuestionStatus::Pending | QuestionStatus::Answering
        )
    }

    /// Answers to secret fields are never shown again nor forwarded to a new session.
    pub fn is_private(&self) -> bool {
        self.fields.iter().any(|field| field.secret)
    }
}

pub fn question_prefix(chat: &str) -> String {
    format!("chat-question:{chat}:")
}

pub(super) fn questions(db: &Db<'_>, chat: &str) -> Result<Vec<Question>> {
    let mut questions = db
        .keys_as::<Question>(&question_prefix(chat))?
        .into_iter()
        .map(|(_, question)| question)
        .collect::<Vec<_>>();
    questions.sort_by_key(|question| question.created_at);
    Ok(questions)
}

pub(super) fn find_question(db: &Db<'_>, chat: &str, id: &str) -> Result<Option<Question>> {
    Ok(questions(db, chat)?
        .into_iter()
        .find(|question| question.id == id))
}

pub(super) fn save_question(db: &Db<'_>, question: &Question) -> Result<()> {
    db.set_as(
        &format!("{}{}", question_prefix(&question.chat_id), question.id),
        question,
        None,
    )
}

/// Agent-generated question identifiers are lowercase SHA-256 digests.
fn valid_question_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

#[derive(Deserialize)]
struct AnswerInput {
    id: String,
    answers: BTreeMap<String, Vec<String>>,
}

fn receive(db: &Db<'_>, mut question: Question) -> Result<()> {
    let Some(chat) = chat_for_run(db, &question.run_id)? else {
        return Ok(());
    };
    if !crate::conversation_lifecycle::is_active(&chat) {
        return Ok(());
    }
    question.chat_id = text(&chat, "id").to_owned();
    if let Some(mut existing) = find_question(db, &question.chat_id, &question.id)? {
        existing.blocking = existing.status == QuestionStatus::Pending && question.blocking;
        return save_question(db, &existing);
    }
    save_question(db, &question)?;
    notifications::enqueue(db, &question)
}

fn release(db: &Db<'_>, run_id: &str, id: Option<&str>) -> Result<()> {
    let Some(chat) = chat_for_run(db, run_id)? else {
        return Ok(());
    };
    for mut question in questions(db, text(&chat, "id"))? {
        if question.blocking && id.is_none_or(|id| question.id == id) {
            question.blocking = false;
            save_question(db, &question)?;
        }
    }
    Ok(())
}

fn answer_text(fields: &[QuestionField], answers: &BTreeMap<String, Vec<String>>) -> String {
    let parts = fields
        .iter()
        .map(|field| format!("{}\n{}", field.title, answers[&field.id].join("\n")))
        .collect::<Vec<_>>();
    format!("My answers to your questions:\n\n{}", parts.join("\n\n"))
}

fn answer(db: &Db<'_>, chat_id: &str, id: &str, input: &AnswerInput) -> Result<Question> {
    chat(db, chat_id)?;
    let question = required(find_question(db, chat_id, id)?, "Question not found")?;
    let answers = serde_json::to_value(&input.answers)?;
    let previous = find_message(db, chat_id, &input.id)?;
    let already_sent = question.message_id.as_deref() == Some(input.id.as_str())
        && previous.is_some_and(|message| message["answers"] == answers);
    if already_sent {
        return Ok(question);
    }
    if question.status != QuestionStatus::Pending {
        return Err(Error::conflict("This question has already been answered."));
    }
    let complete = input.answers.len() == question.fields.len()
        && question
            .fields
            .iter()
            .all(|field| input.answers.contains_key(&field.id));
    if !complete {
        return Err(Error::bad("Answer each question before sending."));
    }
    let message = parse(
        "message",
        json!({
            "id": input.id,
            "mode": "steer",
            "text": answer_text(&question.fields, &input.answers),
        }),
    )?;
    send(db, chat_id, message, Some((question, answers)))?;
    required(find_question(db, chat_id, id)?, "Question not found")
}

impl Service {
    pub async fn question_receive(&self, run_id: &str, input: Value) -> Result<()> {
        let id = text(&input, "id");
        let Some(blocking) = input["blocking"].as_bool() else {
            return Ok(());
        };
        if !valid_question_id(id) {
            return Ok(());
        }
        let Ok(fields) = parse_as::<Vec<QuestionField>>("questions", input["fields"].clone())
        else {
            return Ok(());
        };
        let question = Question {
            id: id.to_owned(),
            chat_id: String::new(),
            run_id: run_id.to_owned(),
            blocking,
            status: QuestionStatus::Pending,
            fields,
            created_at: Some(now()),
            message_id: None,
            extra: Map::new(),
        };
        self.store
            .transaction(move |db| receive(db, question))
            .await
    }

    pub async fn question_release(&self, run_id: &str, id: Option<&str>) -> Result<()> {
        let (run_id, id) = (run_id.to_owned(), id.map(str::to_owned));
        self.store
            .transaction(move |db| release(db, &run_id, id.as_deref()))
            .await
    }

    pub async fn question_answer(&self, chat_id: &str, id: &str, input: Value) -> Result<Value> {
        let input = parse_as::<AnswerInput>("answer", input)?;
        let (chat_id, id) = (chat_id.to_owned(), id.to_owned());
        let question = self
            .store
            .transaction(move |db| answer(db, &chat_id, &id, &input))
            .await?;
        self.worker.notify();
        Ok(serde_json::to_value(question)?)
    }
}
