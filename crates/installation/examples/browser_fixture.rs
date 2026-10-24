//! Test-only stdin/stdout bridge to the native fixture APIs. Never shipped in the image.
use cairn_installation::{
    config::Config,
    error::{Error, Result},
    service::Service,
    store::Store,
    validation::text,
    vault::Vault,
};
use serde_json::{Value, json};
use std::io::Read;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
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
            let service = Service::new(config).await?;
            let existing = args.get(1).and_then(Value::as_str);
            match operation {
                "agent" => service.agent(arg(0), existing).await?,
                "project" => service.project(arg(0), existing).await?,
                "task" => service.task(arg(0), existing).await?,
                "enqueue" => service.enqueue(string(0), "manual", None).await?,
                "skill" => service.skills.save(string(0), string(1), None).await?,
                _ => service.chat_detail(string(0)).await?,
            }
        }
        "vault" => {
            let store = Store::open(&config.data_dir)?;
            Vault::new(store, &config.data_dir)?
                .set(string(0), &arg(1))
                .await?;
            Value::Null
        }
        _ => {
            let store = Store::open(&config.data_dir)?;
            let operation = operation.to_owned();
            store
                .transaction(move |db| {
                    let string =
                        |index: usize| args.get(index).and_then(Value::as_str).unwrap_or("");
                    let arg = |index: usize| args.get(index).cloned().unwrap_or(Value::Null);
                    Ok(match operation.as_str() {
                        "get" => db.get(string(0), string(1))?.unwrap_or(Value::Null),
                        "list" => db.list(string(0))?.into(),
                        "put" => db.put(string(0), &arg(1))?,
                        "run" => db.run(string(0))?.unwrap_or(Value::Null),
                        "runs" => db.runs(None, None, 10000, 0, true)?.into(),
                        "addRun" => db.add_run(&arg(0), None)?,
                        "updateRun" => db.patch_run(string(0), &arg(1))?,
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
                    })
                })
                .await?
        }
    };
    println!("{}", json!({ "result": result }));
    Ok(())
}
