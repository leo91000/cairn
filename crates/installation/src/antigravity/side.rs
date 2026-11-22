//! Side questions for Antigravity. Its `/btw` exists only in the interactive TUI, so a
//! second `agy` resumes a private copy of the saved conversation with every tool
//! refused, then the copy is deleted. The running turn and its history are untouched.
use super::{
    hook::Mode,
    turn::{Group, args, configure, emit, policy, read_line, spawn},
};
use crate::{
    config::Config,
    error::{Error, Result},
    skills::private_dir,
    validation::text,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{io::AsyncWriteExt, io::BufReader, sync::mpsc, task::JoinHandle};

const TIMEOUT: Duration = Duration::from_secs(600);

/// Native state never copied into a side question's private home.
const SKIPPED: &[&str] = &["conversations", "presence", "log", "crashes", "updater"];

pub(super) struct Sides {
    config: Config,
    home: PathBuf,
    plan: Value,
    credentials: Value,
    events: mpsc::Sender<Value>,
    running: HashMap<String, JoinHandle<()>>,
    results: (mpsc::Sender<Answer>, mpsc::Receiver<Answer>),
}

/// A side question's identifier and its answer.
type Answer = (String, Result<String>);

impl Sides {
    pub fn new(
        config: &Config,
        home: &Path,
        plan: &Value,
        credentials: &Value,
        events: mpsc::Sender<Value>,
    ) -> Self {
        Self {
            config: config.clone(),
            home: home.to_owned(),
            plan: plan.clone(),
            credentials: credentials.clone(),
            events,
            running: HashMap::new(),
            results: mpsc::channel(16),
        }
    }

    pub fn ask(&mut self, side: &Value, conversation: &str) {
        let id = text(side, "id").to_owned();
        if id.is_empty() || self.running.contains_key(&id) {
            return;
        }
        let question = text(side, "text").to_owned();
        let task = Task {
            config: self.config.clone(),
            home: self.home.clone(),
            plan: self.plan.clone(),
            credentials: self.credentials.clone(),
            conversation: conversation.to_owned(),
            id: id.clone(),
        };
        let results = self.results.0.clone();
        let handle = tokio::spawn(async move {
            let answer = tokio::time::timeout(TIMEOUT, task.answer(&question))
                .await
                .unwrap_or_else(|_| Err(Error::gateway_timeout("The side question timed out.")));
            task.clean().await;
            let _ = results.send((task.id, answer)).await;
        });
        self.running.insert(id, handle);
    }

    pub fn withdraw(&mut self, id: &str) {
        if let Some(handle) = self.running.remove(id) {
            handle.abort();
            let directory = side_home(&self.home, id);
            tokio::spawn(async move {
                let _ = tokio::fs::remove_dir_all(directory).await;
            });
        }
    }

    /// Publishes the side answers that finished.
    pub async fn settle(&mut self) -> Result<()> {
        while let Ok((id, answer)) = self.results.1.try_recv() {
            if self.running.remove(&id).is_none() {
                continue;
            }
            self.publish(&id, answer).await?;
        }
        Ok(())
    }

    async fn publish(&self, id: &str, answer: Result<String>) -> Result<()> {
        match answer {
            Ok(answer) if crate::chats::side_questions::answer_fits(&answer) => {
                emit(
                    &self.events,
                    json!({ "type": "chat.side.answered", "sideQuestionId": id, "text": answer }),
                )
                .await
            }
            Ok(_) => {
                self.failed(id, "The side answer exceeded the size limit.")
                    .await
            }
            Err(error) => self.failed(id, &error.message).await,
        }
    }

    async fn failed(&self, id: &str, error: &str) -> Result<()> {
        let error = if crate::chats::side_questions::error_fits(error) {
            error
        } else {
            "The side question error exceeded the size limit."
        };
        emit(
            &self.events,
            json!({ "type": "chat.side.failed", "sideQuestionId": id, "error": error }),
        )
        .await
    }

    /// The reply ended: questions still being answered can be asked again later.
    pub async fn expire(&mut self) -> Result<()> {
        self.settle().await?;
        emit(&self.events, json!({ "type": "chat.side.closed" })).await?;
        for id in self.running.keys().cloned().collect::<Vec<_>>() {
            self.withdraw(&id);
            self.failed(&id, crate::chats::side_questions::EXPIRED)
                .await?;
        }
        Ok(())
    }

    pub fn stop(&mut self) {
        for id in self.running.keys().cloned().collect::<Vec<_>>() {
            self.withdraw(&id);
        }
    }
}

impl Drop for Sides {
    /// A cancelled turn drops its side questions: their `agy` processes and the
    /// private copies holding credentials go with it.
    fn drop(&mut self) {
        for (_, handle) in self.running.drain() {
            handle.abort();
        }
        let _ = std::fs::remove_dir_all(self.home.join(SIDE_QUESTIONS));
    }
}

pub(crate) const SIDE_QUESTIONS: &str = "side-questions";

fn side_home(home: &Path, id: &str) -> PathBuf {
    home.join(SIDE_QUESTIONS).join(
        id.chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect::<String>(),
    )
}

struct Task {
    config: Config,
    home: PathBuf,
    plan: Value,
    credentials: Value,
    conversation: String,
    id: String,
}

/// Copies the native state except conversations and live process markers.
fn copy_state(from: &Path, to: &Path, root: bool) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if root && SKIPPED.iter().any(|skipped| name == *skipped) {
            continue;
        }
        let kind = entry.file_type()?;
        let target = to.join(&name);
        if kind.is_dir() {
            copy_state(&entry.path(), &target, false)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// A consistent copy of the conversation database, even while `agy` writes to it.
fn copy_conversation(from: &Path, to: &Path) -> Result<()> {
    let source = rusqlite::Connection::open_with_flags(
        from,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| Error::conflict("The conversation is not saved yet. Ask again shortly."))?;
    source
        .execute("VACUUM INTO ?1", [to.to_string_lossy()])
        .map_err(|_| Error::conflict("The conversation is busy. Ask again shortly."))?;
    Ok(())
}

impl Task {
    async fn answer(&self, question: &str) -> Result<String> {
        if crate::validation::uuid(&self.conversation).is_err() {
            return Err(Error::conflict(
                "The agent is still starting. Ask again in a moment.",
            ));
        }
        let home = side_home(&self.home, &self.id);
        let _ = tokio::fs::remove_dir_all(&home).await;
        private_dir(&home).await?;
        let (state, copy, conversation) = (
            self.home.join(".gemini/antigravity-cli"),
            home.join(".gemini/antigravity-cli"),
            self.conversation.clone(),
        );
        tokio::task::spawn_blocking(move || -> Result<()> {
            copy_state(&state, &copy, true)?;
            std::fs::create_dir_all(copy.join("conversations"))?;
            let database = format!("conversations/{conversation}.db");
            copy_conversation(&state.join(&database), &copy.join(&database))
        })
        .await
        .map_err(Error::internal)??;
        super::prepare_native_home(&self.config, &home, Some(&self.credentials)).await?;
        configure(&home, &self.plan, &policy(&self.plan, Mode::Side)).await?;
        let mut launch = args(&self.plan, &self.credentials, Mode::Side)?;
        launch.extend([
            "--conversation".into(),
            self.conversation.clone(),
            "-p=".into(),
        ]);
        let cwd = PathBuf::from(text(&self.plan, "cwd"));
        let mut child = spawn(&self.config, &home, &cwd, &launch, &self.credentials).await?;
        let mut group = Group::of(&child);
        let mut stdin = child.stdin.take().unwrap();
        // An undrained stderr pipe would block `agy` once it fills.
        let mut stderr = child.stderr.take().unwrap();
        let drain = tokio::spawn(async move {
            let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
        });
        let message = json!({
            "event": "user",
            "message": {
                "content": format!(
                    "Side question about this conversation. Answer briefly from the conversation so far. Tools are unavailable, and neither the question nor the answer is added to the conversation.\n\n{question}"
                ),
            },
        });
        let mut line = serde_json::to_vec(&message)?;
        line.push(b'\n');
        stdin.write_all(&line).await?;
        drop(stdin);
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let outcome = async {
            while let Some(line) = read_line(&mut stdout).await? {
                let Ok(event) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if event["event"] == "result" {
                    let result = &event["result"];
                    let answer = text(result, "response").trim().to_owned();
                    return if result["status"] == "SUCCESS" && !answer.is_empty() {
                        Ok(answer)
                    } else {
                        Err(Error::bad_gateway(
                            "Antigravity could not answer this side question.",
                        ))
                    };
                }
            }
            Err(Error::bad_gateway(
                "Antigravity could not answer this side question.",
            ))
        }
        .await;
        group.stop();
        let _ = child.wait().await;
        drain.abort();
        outcome
    }

    async fn clean(&self) {
        let _ = tokio::fs::remove_dir_all(side_home(&self.home, &self.id)).await;
    }
}
