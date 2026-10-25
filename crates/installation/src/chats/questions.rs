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

/// A persisted question and its delivery state.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub id: String,
    pub chat_id: String,
    pub run_id: String,
    /// The user message whose execution issued this question, not its answer.
    pub turn_id: String,
    pub blocking: bool,
    pub status: QuestionStatus,
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
    question.turn_id = db
        .run(&question.run_id)?
        .map(|run| text(&run["chatExecution"], "messageId").to_owned())
        .unwrap_or_default();
    if let Some(mut existing) = find_question(db, &question.chat_id, &question.id)?
        && existing.run_id == question.run_id
        && existing.turn_id == question.turn_id
    {
        existing.blocking = existing.status == QuestionStatus::Pending && question.blocking;
        return save_question(db, &existing);
    }
    save_question(db, &question)?;
    notifications::enqueue(db, &question)
}

pub(super) fn release(db: &Db<'_>, run_id: &str, id: Option<&str>) -> Result<()> {
    let Some(chat) = chat_for_run(db, run_id)? else {
        return Ok(());
    };
    for mut question in questions(db, text(&chat, "id"))? {
        if question.run_id != run_id || !id.is_none_or(|id| question.id == id) {
            continue;
        }
        let pending = question.status == QuestionStatus::Pending;
        if pending || question.blocking {
            if pending {
                question.status = QuestionStatus::Cancelled;
            }
            question.blocking = false;
            save_question(db, &question)?;
        }
    }
    Ok(())
}

fn cancel(db: &Db<'_>, chat_id: &str, id: &str) -> Result<Question> {
    let chat = chat(db, chat_id)?;
    crate::conversation_lifecycle::require_active(&chat)?;
    let mut question = required(find_question(db, chat_id, id)?, "Question not found")?;
    if question.status == QuestionStatus::Cancelled {
        return Ok(question);
    }
    if question.status != QuestionStatus::Pending {
        return Err(Error::conflict("This question already has an answer."));
    }
    question.status = QuestionStatus::Cancelled;
    question.blocking = false;
    save_question(db, &question)?;
    Ok(question)
}

pub fn list_tool() -> Value {
    json!({
        "name": "pending_questions",
        "description": "List unanswered questions in this conversation, with their questionId and fields. Use withdraw_question when a question has become unnecessary or the user has answered it in ordinary chat. Never infer authorization from a withdrawn question.",
        "inputSchema": {
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        },
    })
}

pub fn cancel_tool() -> Value {
    json!({
        "name": "withdraw_question",
        "description": "Withdraw one pending question that is no longer needed. Get its questionId from pending_questions. This removes the answer prompt without supplying an answer or granting permission. Questions whose answers are being delivered cannot be withdrawn.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "questionId": { "type": "string", "pattern": "^[a-f0-9]{64}$" },
            },
            "required": ["questionId"],
            "additionalProperties": false,
        },
    })
}

pub async fn for_agent(s: &Service, bearer: &str, input: Option<&Value>) -> Result<Value> {
    if let Some(input) = input
        && !jsonschema::is_valid(&cancel_tool()["inputSchema"], input)
    {
        return Err(Error::bad(
            "Provide a valid questionId from pending_questions.",
        ));
    }
    let bearer = bearer.to_owned();
    let input = input.cloned();
    let result = s
        .store
        .transaction(move |db| {
            let run = crate::project_workspaces::authorize_in(db, &bearer)?;
            let run_id = text(&run, "id");
            let chat = required(chat_for_run(db, run_id)?, "This run has no conversation.")?;
            let chat_id = text(&chat, "id");

            if let Some(input) = input {
                let id = text(&input, "questionId");
                let question = required(find_question(db, chat_id, id)?, "Question not found")?;
                if question.run_id != run_id {
                    return Err(Error::forbidden("This question belongs to another run."));
                }
                return Ok(serde_json::to_value(cancel(db, chat_id, id)?)?);
            }

            let pending = questions(db, chat_id)?
                .into_iter()
                .filter(|question| {
                    question.run_id == run_id && question.status == QuestionStatus::Pending
                })
                .map(|question| {
                    json!({
                        "questionId": question.id,
                        "fields": question.fields,
                        "blocking": question.blocking,
                    })
                })
                .collect::<Vec<_>>();
            Ok(json!({ "questions": pending }))
        })
        .await?;
    s.worker.notify();
    Ok(result)
}

fn answer_text(fields: &[QuestionField], answers: &BTreeMap<String, Vec<String>>) -> String {
    let parts = fields
        .iter()
        .map(|field| format!("{}\n{}", field.title, answers[&field.id].join("\n")))
        .collect::<Vec<_>>();
    format!("My answers to your questions:\n\n{}", parts.join("\n\n"))
}

fn answer(
    db: &Db<'_>,
    chat_id: &str,
    id: &str,
    input: &AnswerInput,
    origin: &crate::native::policy::Origin,
    principal: Option<&crate::plugins::requesters::Principal>,
    owner_control: bool,
) -> Result<Question> {
    use crate::plugins::requesters::{self, Capture};

    chat(db, chat_id)?;
    let question = required(find_question(db, chat_id, id)?, "Question not found")?;
    let answers = serde_json::to_value(&input.answers)?;
    let previous = find_message(db, chat_id, &input.id)?;
    let already_sent = question.message_id.as_deref() == Some(input.id.as_str())
        && previous
            .as_ref()
            .is_some_and(|message| message["answers"] == answers);
    if already_sent {
        // A receipt is read-only. In particular, an owner's old retry must not
        // suspend a later turn or move the answer's original provenance.
        if !owner_control && requesters::recorded(db, chat_id, &input.id)?.as_ref() != principal {
            return Err(Error::conflict(
                "This message belongs to another requester.",
            ));
        }
        return Ok(question);
    }
    if previous.is_some() {
        return Err(Error::conflict(
            "This message identifier has already been used.",
        ));
    }
    if question.status != QuestionStatus::Pending {
        return Err(Error::conflict(
            "This question is no longer awaiting an answer.",
        ));
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
    let kind = if owner_control {
        Capture::OwnerAnswer
    } else {
        Capture::New
    };
    requesters::capture_principal(db, chat_id, &message, principal, kind)?;
    send(db, chat_id, message, Some((question, answers)), origin)?;
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
            turn_id: String::new(),
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

    pub async fn question_cancel(&self, chat_id: &str, id: &str) -> Result<Value> {
        self.question_cancel_authorized(chat_id, id, |_| Ok(()))
            .await
    }

    pub(crate) async fn question_cancel_authorized<F>(
        &self,
        chat_id: &str,
        id: &str,
        authorize: F,
    ) -> Result<Value>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        let (chat_id, id) = (chat_id.to_owned(), id.to_owned());
        let question = self
            .store
            .transaction(move |db| {
                authorize(db)?;
                cancel(db, &chat_id, &id)
            })
            .await?;
        self.worker.notify();
        Ok(serde_json::to_value(question)?)
    }

    pub async fn question_answer(&self, chat_id: &str, id: &str, input: Value) -> Result<Value> {
        self.question_answer_as(chat_id, id, input, None).await
    }

    pub async fn question_answer_as(
        &self,
        chat_id: &str,
        id: &str,
        input: Value,
        identity: Option<&crate::auth::InstallationIdentity>,
    ) -> Result<Value> {
        use crate::{auth::InstallationRole, plugins::requesters};

        let actor = match identity {
            Some(identity) => Some(requesters::message_actor(self, &identity.account_id).await?),
            None => None,
        };
        // The role is supplied by the authenticated relay/direct lease. Owner
        // control suspends personal credentials; it never supplies their proof.
        let owner_control = identity.is_some_and(|i| i.role == InstallationRole::Owner);

        self.question_answer_authorized(
            chat_id,
            id,
            input,
            actor.map(requesters::Principal::Account),
            owner_control,
            |_, _| Ok(()),
        )
        .await
    }

    pub(crate) async fn question_answer_authorized<F>(
        &self,
        chat_id: &str,
        id: &str,
        input: Value,
        principal: Option<crate::plugins::requesters::Principal>,
        owner_control: bool,
        authorize: F,
    ) -> Result<Value>
    where
        F: FnOnce(&Db<'_>, &Question) -> Result<()> + Send + 'static,
    {
        let origin = crate::native::policy::origin();
        if origin.known() {
            crate::native::policy::refresh_chat(self, chat_id).await?;
        }
        let input = parse_as::<AnswerInput>("answer", input)?;

        let (chat_id, id) = (chat_id.to_owned(), id.to_owned());
        let question = self
            .store
            .transaction(move |db| {
                let question = required(find_question(db, &chat_id, &id)?, "Question not found")?;
                authorize(db, &question)?;
                answer(
                    db,
                    &chat_id,
                    &id,
                    &input,
                    &origin,
                    principal.as_ref(),
                    owner_control,
                )
            })
            .await?;
        self.worker.notify();
        Ok(serde_json::to_value(question)?)
    }
}
