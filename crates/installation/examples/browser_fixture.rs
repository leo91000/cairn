//! Test-only stdin/stdout bridge to the native fixture APIs. Never shipped in the image.
use cairn_contracts::EventKind;

#[path = "../tests/common/node_fixture.rs"]
mod node_fixture;

use node_fixture::DbNodeFixture;
use run_fixture::DbRunFixture;

#[path = "../tests/common/mission_fixture.rs"]
mod mission_fixture;

use mission_fixture::MissionFixture;

#[path = "../tests/common/agent_fixture.rs"]
mod agent_fixture;

use agent_fixture::AgentFixture;
use cairn_installation::{
    config::Config,
    error::{Error, Result},
    service::Service,
    store::{Db, Store},
    validation::text,
    vault::Vault,
};
use serde_json::{Value, json};
use std::io::Read;

#[path = "browser_fixture/store_migration.rs"]
mod store_migration;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let request: Value = serde_json::from_str(&input)?;
    let operation = text(&request, "operation");
    if operation == "upgradeStoreCopy" {
        let directory = request["args"][0]
            .as_str()
            .ok_or_else(|| Error::bad("Expected a private offline copy directory."))?;
        let result = store_migration::upgrade_copy(std::path::Path::new(directory))?;
        println!("{}", json!({ "result": result }));
        return Ok(());
    }
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_writer(std::io::stderr)
        .init();
    let config: Config = serde_json::from_value(request["config"].clone())?;
    let args = request["args"]
        .as_array()
        .ok_or_else(|| Error::bad("Expected args"))?
        .clone();
    let arg = |index: usize| args.get(index).cloned().unwrap_or(Value::Null);
    let string = |index: usize| args.get(index).and_then(Value::as_str).unwrap_or("");
    let result = match operation {
        "agent" | "project" | "task" | "enqueue" | "skill" | "detail" => {
            let service = open_service(config).await?;
            let existing = args.get(1).and_then(Value::as_str);
            match operation {
                "agent" => service.agent_fixture(arg(0), existing).await?,
                "project" => {
                    let settings = serde_json::from_value(arg(0))?;
                    let project = if let Some(id) = existing {
                        let current = service.get_project(id).await?;
                        service
                            .replace_project(
                                cairn_contracts::ReplaceProjectInput {
                                    project_id: current.id,
                                    revision: current.revision,
                                    settings,
                                },
                                |_| Ok(()),
                            )
                            .await?
                    } else {
                        service
                            .create_project(
                                cairn_contracts::CreateProjectInput {
                                    creation_id: None,
                                    settings,
                                },
                                None,
                                |_| Ok(()),
                            )
                            .await?
                    };
                    serde_json::to_value(project)?
                }
                "task" => service.task_fixture(arg(0), existing).await?,
                "enqueue" => service.enqueue(string(0), "manual", None).await?,
                "skill" => service.skills.save(string(0), string(1), None).await?,
                _ => service.chat_detail(string(0)).await?,
            }
        }
        "vault" => {
            let store = open_store(&config.data_dir).await?;
            Vault::new(store, &config.data_dir)?
                .set(string(0), &arg(1))
                .await?;
            Value::Null
        }
        _ => {
            // The server owns schema initialization. Opening the native Store here
            // would rerun migrations while its worker writes to the same database.
            let mut connection = rusqlite::Connection::open_with_flags(
                config.data_dir.join("manager.db"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
            )?;
            connection.busy_timeout(std::time::Duration::from_secs(5))?;
            connection.execute_batch("PRAGMA foreign_keys=ON;")?;
            // Reading a run needs a consistent snapshot, not the writer lock.
            // Writes still acquire their lock before reading mutable state.
            let behavior = if operation == "run" {
                rusqlite::TransactionBehavior::Deferred
            } else {
                rusqlite::TransactionBehavior::Immediate
            };
            let transaction = connection.transaction_with_behavior(behavior)?;
            let db = Db(&transaction);
            let string = |index: usize| args.get(index).and_then(Value::as_str).unwrap_or("");
            let arg = |index: usize| args.get(index).cloned().unwrap_or(Value::Null);
            let result = match operation {
                "accountFixture" => serde_json::to_value(db.coding_account(string(0))?)?,
                "saveAccountFixture" => {
                    db.save_coding_account(&serde_json::from_value(arg(0))?)?;
                    arg(0)
                }

                "get" if string(0) == "agents" => {
                    db.agent_snapshot(string(1))?.unwrap_or(Value::Null)
                }
                "get" if string(0) == "tasks" => serde_json::to_value(db.mission(string(1))?)?,
                "get" if string(0) == "nodes" => {
                    db.node_boundary(string(1))?.unwrap_or(Value::Null)
                }
                "get" if string(0) == "chats" => {
                    db.conversation_boundary(string(1))?.unwrap_or(Value::Null)
                }
                "get" => db.get(string(0), string(1))?.unwrap_or(Value::Null),
                "list" if string(0) == "agents" => db.agent_snapshots()?.into(),
                "list" if string(0) == "tasks" => serde_json::to_value(db.missions()?)?,
                "list" if string(0) == "nodes" => db.nodes_boundary()?.into(),
                "list" if string(0) == "chats" => db.conversations_boundary()?.into(),
                "list" => db.list(string(0))?.into(),
                "projects" => serde_json::to_value(db.projects()?)?,
                "put" if string(0) == "agents" => {
                    serde_json::to_value(db.save_agent(&serde_json::from_value(arg(1))?)?)?
                }
                "put" if string(0) == "tasks" => {
                    db.save_mission(&serde_json::from_value(arg(1))?)?;
                    arg(1)
                }
                "put" if string(0) == "nodes" => db.put_node_fixture(&arg(1))?,
                "put" if string(0) == "chats" => {
                    let input = arg(1);
                    let current = db.conversation(text(&input, "id"))?;
                    let mut value = current
                        .as_ref()
                        .map(serde_json::to_value)
                        .transpose()?
                        .unwrap_or_else(|| json!({}));
                    cairn_installation::store::merge(&mut value, &input);
                    let mut chat: cairn_installation::chats::Conversation =
                        serde_json::from_value(value)?;
                    if let Some(current) = current {
                        chat.revision = current.revision;
                    }
                    serde_json::to_value(db.save_conversation(&chat)?)?
                }
                "put" => db.put(string(0), &arg(1))?,
                "run" => db.run(string(0))?.unwrap_or(Value::Null),
                "runs" => db.runs(None, None, 10000, 0, true)?.into(),
                "addRun" => db.add_run_fixture(&arg(0), None)?,
                "updateRun" => run_fixture::change_run_fixture(&db, string(0), &arg(1))?,
                "events" => db
                    .events_fixture(
                        string(0),
                        arg(1).as_i64().unwrap_or(0),
                        arg(2).as_i64().unwrap_or(10000),
                    )?
                    .into(),
                "chatMessages" => db.messages(string(0))?.into(),
                "event" => {
                    db.event(
                        string(0),
                        &EventKind::from_adapter(string(1)),
                        string(2),
                        args.get(3).filter(|v| !v.is_null()),
                    )?;
                    Value::Null
                }
                "seedEvents" => {
                    let events = arg(1);
                    let events = events
                        .as_array()
                        .ok_or_else(|| Error::bad("Expected fixture events"))?;

                    for event in events {
                        db.event(
                            string(0),
                            &EventKind::from_adapter(text(event, "type")),
                            text(event, "text"),
                            event.get("payload").filter(|value| !value.is_null()),
                        )?;
                    }

                    Value::Null
                }
                "insertArtifact" => {
                    db.insert_artifact(&serde_json::from_value(arg(0))?)?;
                    arg(0)
                }
                "set" => {
                    db.set(string(0), &arg(1), arg(2).as_i64())?;
                    Value::Null
                }
                "delete" => {
                    db.delete(string(0))?;
                    Value::Null
                }
                "clearEvents" => {
                    db.0.execute("DELETE FROM events WHERE run_id=?", [string(0)])?;
                    Value::Null
                }
                _ => return Err(Error::bad("Unknown browser fixture operation")),
            };
            transaction.commit()?;
            result
        }
    };
    println!("{}", json!({ "result": result }));
    Ok(())
}

// Startup's schema transaction can lose its WAL read snapshot to the real worker.
// Retry only initialization, never an action that could already have committed.
async fn open_service(config: Config) -> Result<std::sync::Arc<Service>> {
    for attempt in 0..20 {
        match Service::new(config.clone()).await {
            Ok(service) => return Ok(service),
            Err(error) if error.status == 500 && attempt < 19 => {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!()
}

async fn open_store(directory: &std::path::Path) -> Result<Store> {
    for attempt in 0..20 {
        match Store::open(directory) {
            Ok(store) => return Ok(store),
            Err(error) if error.status == 500 && attempt < 19 => {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!()
}

#[path = "../tests/common/run_fixture.rs"]
mod run_fixture;
