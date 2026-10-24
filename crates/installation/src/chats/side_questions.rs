//! Side questions: explanations the running agent answers from its current context,
//! beside its reply. They never join the queue, steer the turn or enter the native
//! session's history. Stored as `chat-side-question:{chatId}:{id}` key-value records.
//!
//! The adapter of a running reply announces `chat.side.available` once its native
//! session can answer one: Claude Code's `side_question` control request, or an
//! ephemeral, tool-less fork of the Codex thread. Older adapters never announce it,
//! so they never receive side questions in their inbox and cannot mistake one for
//! steering.
use super::{chat, chat_for_run};
use crate::{
    config::now,
    conversation_lifecycle::require_active,
    error::{Error, Result, required},
    provider::Provider,
    run_status::RunStatus,
    service::Service,
    store::Db,
    validation::{parse_as, string_enum, text},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

string_enum! {
    pub enum SideQuestionStatus {
        Pending => "pending",
        Answered => "answered",
        Failed => "failed",
        Cancelled => "cancelled",
    }
}

/// Unanswered side questions a conversation can hold at once.
const MAX_PENDING: usize = 5;
/// Side questions kept per conversation; the oldest settled ones are forgotten first.
const MAX_KEPT: usize = 100;
/// Answers longer than this are cut, as no explanation needs more.
const MAX_ANSWER: usize = 100_000;
const MAX_ERROR: usize = 1_000;

pub(crate) const EXPIRED: &str = "The reply finished before this side question was answered. Ask it again during a reply, or ask in the conversation.";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SideQuestion {
    pub id: String,
    pub chat_id: String,
    pub run_id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub quote: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub item_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub status: SideQuestionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<i64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AskInput {
    id: String,
    text: String,
    quote: String,
    item_id: String,
}

pub fn side_question_prefix(chat: &str) -> String {
    format!("chat-side-question:{chat}:")
}

pub(crate) fn side_questions(db: &Db<'_>, chat: &str) -> Result<Vec<SideQuestion>> {
    let mut questions = db
        .keys_as::<SideQuestion>(&side_question_prefix(chat))?
        .into_iter()
        .map(|(_, question)| question)
        .collect::<Vec<_>>();
    questions.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(questions)
}

fn find(db: &Db<'_>, chat: &str, id: &str) -> Result<Option<SideQuestion>> {
    db.kv(&format!("{}{id}", side_question_prefix(chat)))?
        .map(serde_json::from_value)
        .transpose()
        .map_err(Into::into)
}

fn save(db: &Db<'_>, question: &SideQuestion) -> Result<()> {
    db.set_as(
        &format!("{}{}", side_question_prefix(&question.chat_id), question.id),
        question,
        None,
    )
}

fn run_is_active(run: &Value) -> bool {
    RunStatus::of(run).is_some_and(RunStatus::is_active)
}

/// Why the chat's current reply cannot take a side question, if it cannot.
pub(crate) fn unavailable(run: Option<&Value>) -> Option<&'static str> {
    let Some(run) = run.filter(|run| run_is_active(run)) else {
        return Some(
            "Side questions are answered while the agent works on a reply. Send a message instead.",
        );
    };
    if Provider::of_run(run).is_ok_and(|provider| provider == Provider::Opencode) {
        return Some(
            "OpenCode cannot answer side questions in Cairn yet. Ask in the conversation instead.",
        );
    }
    if run["sideQuestions"] != true {
        return Some("The agent is still starting its reply. Try again in a moment.");
    }
    None
}

/// The question as the agent receives it, with the passage it is about.
fn wire_text(question: &SideQuestion) -> String {
    if question.quote.is_empty() {
        return question.text.clone();
    }
    let quoted = question
        .quote
        .lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "About this passage of your reply:\n{quoted}\n\n{}",
        question.text
    )
}

fn same_question(existing: &SideQuestion, input: &AskInput) -> bool {
    existing.text == input.text
        && existing.quote == input.quote
        && existing.item_id == input.item_id
}

/// Forgets the oldest settled side questions beyond what a conversation keeps.
fn prune(db: &Db<'_>, chat_id: &str) -> Result<()> {
    let questions = side_questions(db, chat_id)?;
    let excess = questions.len().saturating_sub(MAX_KEPT);
    let settled = questions
        .iter()
        .filter(|question| question.status != SideQuestionStatus::Pending)
        .take(excess);
    for question in settled {
        db.delete(&format!("{}{}", side_question_prefix(chat_id), question.id))?;
    }
    Ok(())
}

fn ask(
    db: &Db<'_>,
    chat_id: &str,
    input: AskInput,
    origin: &crate::native::policy::Origin,
) -> Result<SideQuestion> {
    let mut chat = chat(db, chat_id)?;
    // The agent answers with the conversation's native grant, as for a message.
    crate::native::policy::authorize_chat(db, &mut chat, origin)?;
    require_active(&chat)?;
    db.put("chats", &chat)?;

    // Retried submissions are idempotent; a reused identifier with other content is not.
    if let Some(existing) = find(db, chat_id, &input.id)? {
        if !same_question(&existing, &input) {
            return Err(Error::conflict(
                "This side question identifier has already been used.",
            ));
        }
        return Ok(existing);
    }

    let run = db.run(text(&chat, "runId"))?;
    if let Some(reason) = unavailable(run.as_ref()) {
        return Err(Error::conflict(reason));
    }
    let run = run.unwrap();
    let pending = side_questions(db, chat_id)?
        .iter()
        .filter(|question| question.status == SideQuestionStatus::Pending)
        .count();
    if pending >= MAX_PENDING {
        return Err(Error::conflict(
            "Wait for an answer before asking more side questions.",
        ));
    }

    let question = SideQuestion {
        id: input.id,
        chat_id: chat_id.to_owned(),
        run_id: text(&run, "id").to_owned(),
        text: input.text,
        quote: input.quote,
        item_id: input.item_id,
        provider: Provider::of_run(&run)
            .ok()
            .map(|provider| provider.as_str().to_owned()),
        status: SideQuestionStatus::Pending,
        answer: None,
        error: None,
        created_at: now(),
        settled_at: None,
        extra: Map::new(),
    };
    save(db, &question)?;
    prune(db, chat_id)?;
    Ok(question)
}

fn cancel(db: &Db<'_>, chat_id: &str, id: &str) -> Result<SideQuestion> {
    let chat = chat(db, chat_id)?;
    require_active(&chat)?;
    let mut question = required(find(db, chat_id, id)?, "Side question not found")?;
    match question.status {
        SideQuestionStatus::Cancelled => Ok(question),
        SideQuestionStatus::Pending => {
            question.status = SideQuestionStatus::Cancelled;
            question.settled_at = Some(now());
            save(db, &question)?;
            Ok(question)
        }
        _ => Err(Error::conflict("This side question already has an answer.")),
    }
}

/// Inbox entries for the running reply: side questions to answer and withdrawals of
/// questions it may still be answering. Adapters answer each identifier once.
pub(crate) fn deliveries(db: &Db<'_>, chat_id: &str, run: &Value) -> Result<Vec<Value>> {
    if run["sideQuestions"] != true {
        return Ok(Vec::new());
    }
    let run_id = text(run, "id");
    let mut entries = Vec::new();
    for question in side_questions(db, chat_id)? {
        if question.run_id != run_id {
            continue;
        }
        match question.status {
            SideQuestionStatus::Pending => entries.push(json!({
                "id": format!("side-question:{}", question.id),
                "sideQuestion": {
                    "id": question.id,
                    "text": wire_text(&question),
                },
            })),
            SideQuestionStatus::Cancelled => entries.push(json!({
                "id": format!("withdraw-side-question:{}", question.id),
                "sideQuestionId": question.id,
                "sideQuestionCancelled": true,
            })),
            _ => {}
        }
    }
    Ok(entries)
}

/// Records the agent's answer, or why it could not answer. Settled questions keep
/// their first outcome: a withdrawal or a repeated answer after restart changes nothing.
fn settle(
    db: &Db<'_>,
    run_id: &str,
    id: &str,
    outcome: std::result::Result<&str, &str>,
) -> Result<()> {
    let Some(chat) = chat_for_run(db, run_id)? else {
        return Ok(());
    };
    let Some(mut question) = find(db, text(&chat, "id"), id)? else {
        return Ok(());
    };
    if question.status != SideQuestionStatus::Pending || question.run_id != run_id {
        return Ok(());
    }
    match outcome {
        Ok(answer) => {
            question.status = SideQuestionStatus::Answered;
            question.answer = Some(answer.chars().take(MAX_ANSWER).collect());
        }
        Err(error) => {
            question.status = SideQuestionStatus::Failed;
            question.error = Some(error.chars().take(MAX_ERROR).collect());
        }
    }
    question.settled_at = Some(now());
    save(db, &question)
}

/// Conversations with a side question still waiting for an answer.
pub(crate) fn pending_chats(db: &Db<'_>) -> Result<std::collections::HashSet<String>> {
    Ok(db
        .keys_as::<SideQuestion>("chat-side-question:")?
        .into_iter()
        .filter(|(_, question)| question.status == SideQuestionStatus::Pending)
        .map(|(_, question)| question.chat_id)
        .collect())
}

/// Side questions the chat's reply can no longer answer, once it is not running.
pub(crate) fn expire(db: &Db<'_>, chat_id: &str) -> Result<()> {
    let chat = chat(db, chat_id)?;
    let run = db.run(text(&chat, "runId"))?;
    let running = run.as_ref().is_some_and(run_is_active);
    for mut question in side_questions(db, chat_id)? {
        let current = run
            .as_ref()
            .is_some_and(|run| run["id"] == question.run_id.as_str());
        if question.status != SideQuestionStatus::Pending || (running && current) {
            continue;
        }
        question.status = SideQuestionStatus::Failed;
        question.error = Some(EXPIRED.into());
        question.settled_at = Some(now());
        save(db, &question)?;
    }
    Ok(())
}

/// What the live view and detail show about side questions.
pub(crate) fn view(db: &Db<'_>, chat: &mut Value, run: Option<&Value>) -> Result<()> {
    let questions = side_questions(db, text(chat, "id"))?;
    chat["pendingSideQuestions"] = questions
        .iter()
        .filter(|question| question.status == SideQuestionStatus::Pending)
        .count()
        .into();
    chat["canAskSideQuestion"] = unavailable(run).is_none().into();
    Ok(())
}

pub(crate) fn detail(db: &Db<'_>, chat: &str) -> Result<Value> {
    Ok(serde_json::to_value(side_questions(db, chat)?)?)
}

impl Service {
    pub(crate) async fn side_question_ask_authorized<F>(
        &self,
        chat_id: &str,
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
        let input = parse_as::<AskInput>("sideQuestion", input)?;
        let chat_id = chat_id.to_owned();
        let question = self
            .store
            .transaction(move |db| {
                authorize(db)?;
                ask(db, &chat_id, input, &origin)
            })
            .await?;
        self.worker.notify();
        Ok(serde_json::to_value(question)?)
    }

    pub(crate) async fn side_question_cancel_authorized<F>(
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

    /// The running reply's adapter can now answer side questions.
    pub async fn side_questions_available(&self, run_id: &str) -> Result<()> {
        self.store
            .patch_run(run_id, json!({ "sideQuestions": true }))
            .await?;
        Ok(())
    }

    pub async fn side_question_settle(&self, run_id: &str, event: &Value) -> Result<()> {
        let id = text(event, "sideQuestionId");
        if crate::validation::uuid(id).is_err() {
            return Ok(());
        }
        let (run_id, id) = (run_id.to_owned(), id.to_owned());
        let answered = event["type"] == "chat.side.answered";
        let body = if answered {
            text(event, "text").to_owned()
        } else {
            text(event, "error").to_owned()
        };
        self.store
            .transaction(move |db| {
                let outcome = if answered && !body.trim().is_empty() {
                    Ok(body.as_str())
                } else if answered {
                    Err("The agent returned an empty answer.")
                } else if body.is_empty() {
                    Err("The agent could not answer this side question.")
                } else {
                    Err(body.as_str())
                };
                settle(db, &run_id, &id, outcome)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(quote: &str) -> SideQuestion {
        SideQuestion {
            id: "a".into(),
            chat_id: "c".into(),
            run_id: "r".into(),
            text: "Why this order?".into(),
            quote: quote.into(),
            item_id: String::new(),
            provider: None,
            status: SideQuestionStatus::Pending,
            answer: None,
            error: None,
            created_at: 0,
            settled_at: None,
            extra: Map::new(),
        }
    }

    #[test]
    fn quotes_each_line_of_the_passage_before_the_question() {
        assert_eq!(wire_text(&question("")), "Why this order?");
        assert_eq!(
            wire_text(&question("first\nsecond")),
            "About this passage of your reply:\n> first\n> second\n\nWhy this order?"
        );
    }

    #[test]
    fn only_a_running_announced_codex_or_claude_reply_takes_side_questions() {
        let running = |provider: &str, announced: bool| {
            json!({
                "status": "running",
                "sideQuestions": announced,
                "snapshot": { "agent": { "provider": provider } },
            })
        };
        assert!(unavailable(None).is_some());
        assert!(unavailable(Some(&json!({ "status": "succeeded" }))).is_some());
        assert!(unavailable(Some(&running("codex", false))).is_some());
        assert!(unavailable(Some(&running("opencode", true))).is_some());
        assert!(unavailable(Some(&running("codex", true))).is_none());
        assert!(unavailable(Some(&running("claude", true))).is_none());
    }
}
