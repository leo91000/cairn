//! Test-only stdin/stdout bridge to the native fixture APIs. Never shipped in the image.
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

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_writer(std::io::stderr)
        .init();
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let request: Value = serde_json::from_str(&input)?;
    let config: Config = serde_json::from_value(request["config"].clone())?;
    let operation = text(&request, "operation");
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
            let transaction =
                connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let db = Db(&transaction);
            let string = |index: usize| args.get(index).and_then(Value::as_str).unwrap_or("");
            let arg = |index: usize| args.get(index).cloned().unwrap_or(Value::Null);
            let result = match operation {
                "get" if string(0) == "agents" => {
                    db.agent_snapshot(string(1))?.unwrap_or(Value::Null)
                }
                "get" if string(0) == "tasks" => serde_json::to_value(db.mission(string(1))?)?,
                "get" => db.get(string(0), string(1))?.unwrap_or(Value::Null),
                "list" if string(0) == "agents" => db.agent_snapshots()?.into(),
                "list" if string(0) == "tasks" => serde_json::to_value(db.missions()?)?,
                "list" => db.list(string(0))?.into(),
                "projects" => serde_json::to_value(db.projects()?)?,
                "put" if string(0) == "agents" => {
                    serde_json::to_value(db.save_agent(&serde_json::from_value(arg(1))?)?)?
                }
                "put" if string(0) == "tasks" => {
                    db.save_mission(&serde_json::from_value(arg(1))?)?;
                    arg(1)
                }
                "put" => db.put(string(0), &arg(1))?,
                "run" => db.run(string(0))?.unwrap_or(Value::Null),
                "runs" => db.runs(None, None, 10000, 0, true)?.into(),
                "addRun" => db.add_run(&arg(0), None)?,
                "updateRun" => run_fixture::change_run_fixture(db, string(0), &arg(1))?,
                "events" => db
                    .events(
                        string(0),
                        arg(1).as_i64().unwrap_or(0),
                        arg(2).as_i64().unwrap_or(10000),
                    )?
                    .into(),
                "chatMessages" => db.messages(string(0))?.into(),
                "event" => {
                    db.event(
                        string(0),
                        string(1),
                        string(2),
                        args.get(3).filter(|v| !v.is_null()),
                    )?;
                    Value::Null
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
