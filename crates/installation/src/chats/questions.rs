//! Typed questionnaire lifecycle, backed by the owning question tables.
use super::{chat, chat_for_run, find_message, send};
use crate::{
    config::now,
    error::{Error, Result, required},
    notifications,
    service::Service,
    store::Db,
    validation::{text, uuid},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};

pub const NATIVE_DENIED: &str =
    "Native question tools are disabled. Use cairn_workspace.ask_questions to ask the user.";

use cairn_contracts::{
    AskQuestionsInput, AskQuestionsOutput, PendingQuestion, PendingQuestionsOutput,
    QuestionAnswer as FieldAnswer, QuestionAnswerInput as AnswerInput, WithdrawQuestionInput,
};
pub use cairn_contracts::{ChatQuestion as Question, QuestionField, QuestionStatus};

pub(super) fn questions(db: &Db<'_>, chat: &str) -> Result<Vec<Question>> {
    db.questions(chat)
}

pub(super) fn find_question(db: &Db<'_>, chat: &str, id: &str) -> Result<Option<Question>> {
    db.question(chat, id)
}

pub(super) fn save_question(db: &Db<'_>, question: &Question) -> Result<()> {
    db.save_question(question)
}

/// Agent-generated question identifiers are lowercase SHA-256 digests.
fn valid_question_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn validate_fields(fields: &[QuestionField]) -> Result<()> {
    cairn_contracts::validate_question_fields(fields).map_err(Error::bad)
}

fn validate_answer(field: &QuestionField, answer: &FieldAnswer) -> Result<()> {
    if answer.selected.is_empty() {
        if answer.comment.trim().is_empty() {
            return Err(Error::bad("Answer each question before sending."));
        }
        return Ok(());
    }
    let mut selected = HashSet::new();
    let known = answer
        .selected
        .iter()
        .all(|id| selected.insert(id) && field.options.iter().any(|option| option.id == *id));
    let maximum = if field.mode == "single" {
        1
    } else {
        field.max_choices.unwrap_or(field.options.len())
    };
    if !known
        || answer.selected.len() < field.min_choices.unwrap_or(1)
        || answer.selected.len() > maximum
    {
        return Err(Error::bad(
            "Select distinct available options within the question's bounds, or answer using text only.",
        ));
    }
    Ok(())
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

pub(super) fn release(db: &Db<'_>, run_id: &str, id: Option<&str>) -> Result<()> {
    release_matching(db, run_id, id, false)
}

fn release_matching(
    db: &Db<'_>,
    run_id: &str,
    id: Option<&str>,
    include_common: bool,
) -> Result<()> {
    let Some(chat) = chat_for_run(db, run_id)? else {
        return Ok(());
    };
    for mut question in questions(db, text(&chat, "id"))? {
        if question.run_id != run_id
            || !id.is_none_or(|id| question.id == id)
            || (!include_common && question.source == "cairn")
        {
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

pub fn ask_tool() -> Value {
    json!({
        "name": "ask_questions",
        "description": "Ask the user a questionnaire in this conversation. Returns immediately with questionId; answers arrive later as a user message with separate selected options and comment. Reuse requestId only when retrying the same questionnaire. Choose single or multiple selection per question, optional bounds and recommended options (never preselected). Free text is always available. Keep the complete questionnaire within 64 KiB of encoded JSON. Continue independent work or report needs_input and end the turn when a reply is required. Use this tool instead of native question tools.",
        "inputSchema": cairn_contracts::schema_for::<AskQuestionsInput>(cairn_contracts::Contract::Deserialize),
        "outputSchema": cairn_contracts::schema_for::<AskQuestionsOutput>(cairn_contracts::Contract::Serialize),
    })
}

pub async fn ask(s: &Service, bearer: &str, input: Value) -> Result<Value> {
    let input: AskQuestionsInput = serde_json::from_value(input)
        .map_err(|_| Error::bad("Provide a requestId and questions."))?;
    uuid(&input.request_id)?;
    let fields = input.questions;
    validate_fields(&fields)?;
    let bearer = bearer.to_owned();
    let result = s
        .store
        .transaction(move |db| {
            let run = crate::project_workspaces::authorize_in(db, &bearer)?;
            let run_id = text(&run, "id");
            let chat = required(chat_for_run(db, run_id)?, "This run has no conversation.")?;
            crate::conversation_lifecycle::require_active(&chat)?;
            let chat_id = text(&chat, "id");
            let id = crate::auth::hex_digest(&format!("{chat_id}:{}", input.request_id));
            if let Some(existing) = find_question(db, chat_id, &id)? {
                if serde_json::to_value(&existing.fields)? != serde_json::to_value(&fields)? {
                    return Err(Error::conflict(
                        "This requestId has already been used for different questions.",
                    ));
                }
                return Ok(serde_json::to_value(AskQuestionsOutput {
                    question_id: id,
                    status: existing.status,
                })?);
            }
            receive(
                db,
                Question {
                    id: id.clone(),
                    chat_id: chat_id.to_owned(),
                    run_id: run_id.to_owned(),
                    blocking: false,
                    source: "cairn".into(),
                    status: QuestionStatus::Pending,
                    fields,
                    created_at: Some(now()),
                    message_id: None,
                },
            )?;
            Ok(serde_json::to_value(AskQuestionsOutput {
                question_id: id,
                status: QuestionStatus::Pending,
            })?)
        })
        .await?;
    s.worker.notify();
    Ok(result)
}

/// Retire pre-Cairn questionnaires without rewriting historical user messages.
pub(crate) async fn retire_native(s: &Service) -> Result<()> {
    s.store.transaction(|db| {
        db.0.execute("UPDATE chat_questions SET status=CASE WHEN status='pending' THEN 'cancelled' ELSE status END,blocking=0 WHERE source!='cairn' AND (status='pending' OR blocking=1)", [])?;
        Ok(())
    }).await
}

pub fn list_tool() -> Value {
    json!({
        "name": "pending_questions",
        "outputSchema": cairn_contracts::schema_for::<PendingQuestionsOutput>(cairn_contracts::Contract::Serialize),
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
        "inputSchema": cairn_contracts::schema_for::<WithdrawQuestionInput>(cairn_contracts::Contract::Deserialize),
        "outputSchema": cairn_contracts::schema_for::<Question>(cairn_contracts::Contract::Serialize),
    })
}

pub async fn for_agent(s: &Service, bearer: &str, input: Option<&Value>) -> Result<Value> {
    let input = input
        .map(|value| {
            serde_json::from_value::<WithdrawQuestionInput>(value.clone())
                .map_err(|_| Error::bad("Provide a valid questionId from pending_questions."))
        })
        .transpose()?;
    if input
        .as_ref()
        .is_some_and(|i| !valid_question_id(&i.question_id))
    {
        return Err(Error::bad(
            "Provide a valid questionId from pending_questions.",
        ));
    }
    let bearer = bearer.to_owned();
    let result = s
        .store
        .transaction(move |db| {
            let run = crate::project_workspaces::authorize_in(db, &bearer)?;
            let run_id = text(&run, "id");
            let chat = required(chat_for_run(db, run_id)?, "This run has no conversation.")?;
            let chat_id = text(&chat, "id");

            if let Some(input) = input {
                let id = &input.question_id;
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
                .map(|question| PendingQuestion {
                    question_id: question.id,
                    fields: question.fields,
                    blocking: question.blocking,
                })
                .collect::<Vec<_>>();
            Ok(serde_json::to_value(PendingQuestionsOutput {
                questions: pending,
            })?)
        })
        .await?;
    s.worker.notify();
    Ok(result)
}

fn answer_text(fields: &[QuestionField], answers: &BTreeMap<String, FieldAnswer>) -> String {
    let parts = fields
        .iter()
        .map(|field| {
            let answer = &answers[&field.id];
            let choices = answer
                .selected
                .iter()
                .map(|id| {
                    let option = field
                        .options
                        .iter()
                        .find(|option| option.id == *id)
                        .expect("validated option");
                    json!({
                        "id": id,
                        "label": option.label,
                    })
                })
                .collect::<Vec<_>>();
            json!({
                "questionId": field.id,
                "question": field.title,
                "selected": choices,
                "comment": answer.comment,
            })
        })
        .collect::<Vec<_>>();
    format!(
        "My answers to your questions:\n\n{}",
        serde_json::to_string_pretty(&parts).expect("serializable answers")
    )
}

fn answer(
    db: &Db<'_>,
    chat_id: &str,
    id: &str,
    input: &AnswerInput,
    origin: &crate::native::policy::Origin,
) -> Result<Question> {
    chat(db, chat_id)?;
    let question = required(find_question(db, chat_id, id)?, "Question not found")?;
    let answers = serde_json::to_value(&input.answers)?;
    let message_id = input.id.clone();
    let previous = find_message(db, chat_id, &message_id)?;
    let already_sent = question.message_id.as_deref() == Some(message_id.as_str())
        && previous.is_some_and(|message| {
            serde_json::to_value(message.answers).is_ok_and(|stored| stored == answers)
        });
    if already_sent {
        return Ok(question);
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
    for field in &question.fields {
        validate_answer(field, &input.answers[&field.id])?;
    }
    let mut message = super::parse_message(json!({
        "id": input.id,
        "mode": "steer",
        "text": "Questionnaire response",
    }))?;
    // This generated text is bounded by the questionnaire and answer contracts,
    // independently of the 50,000-character ordinary chat-composer limit.
    message["text"] = answer_text(&question.fields, &input.answers).into();
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
        let Ok(fields) = serde_json::from_value::<Vec<QuestionField>>(input["fields"].clone())
        else {
            return Ok(());
        };
        validate_fields(&fields)?;
        let question = Question {
            id: id.to_owned(),
            chat_id: String::new(),
            run_id: run_id.to_owned(),
            blocking,
            source: "cairn".into(),
            status: QuestionStatus::Pending,
            fields,
            created_at: Some(now()),
            message_id: None,
        };
        self.store
            .transaction(move |db| receive(db, question))
            .await
    }

    pub async fn question_complete(&self, run_id: &str) -> Result<()> {
        let run_id = run_id.to_owned();
        self.store
            .transaction(move |db| release_matching(db, &run_id, None, true))
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
        self.question_answer_authorized(chat_id, id, input, |_| Ok(()))
            .await
    }

    pub(crate) async fn question_answer_authorized<F>(
        &self,
        chat_id: &str,
        id: &str,
        input: Value,
        authorize: F,
    ) -> Result<Value>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        let origin = crate::native::policy::origin();
        if origin.known() {
            crate::native::policy::refresh_chat(self, chat_id).await?;
        }
        let mut input: AnswerInput = serde_json::from_value(input)
            .map_err(|_| Error::bad("Provide an answer message id and answers."))?;
        input.validate().map_err(Error::bad)?;
        let (chat_id, id) = (chat_id.to_owned(), id.to_owned());
        let question = self
            .store
            .transaction(move |db| {
                authorize(db)?;
                answer(db, &chat_id, &id, &input, &origin)
            })
            .await?;
        self.worker.notify();
        Ok(serde_json::to_value(question)?)
    }
}
