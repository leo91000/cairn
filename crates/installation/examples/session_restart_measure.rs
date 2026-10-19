//! Synthetic local metadata measurement for docs/SESSION-RESTART.md.
//! The reference handler is frozen at main 98da98bd, before F2.
use cairn_installation::{
    config::Config,
    conversation_lifecycle::require_active,
    error::{Error, Result, required},
    run_status::RunStatus,
    service::Service,
    store::{Db, Store},
    validation::text,
};
use serde_json::{Value, json};
use std::time::Instant;

fn reference_request(db: &Db<'_>, id: &str) -> Result<Value> {
    let mut chat = required(db.get("chats", id)?, "Chat not found")?;
    require_active(&chat)?;
    let run = required(db.run(text(&chat, "runId"))?, "Run not found")?;
    let restored_session_failed = !chat["restoredAt"].is_null()
        && matches!(
            RunStatus::of(&run),
            Some(RunStatus::Failed | RunStatus::Interrupted)
        );
    if !restored_session_failed {
        return Err(Error::conflict(
            "A fresh session is available after a restored session fails.",
        ));
    }
    let key = format!("run-checkpoint:{}", text(&run, "id"));
    let mut checkpoint = required(db.kv(&key)?, "Workspace checkpoint not found")?;
    checkpoint["freshSession"] = true.into();
    checkpoint["launched"] = false.into();
    if let Some(checkpoint) = checkpoint.as_object_mut() {
        checkpoint.remove("controllerRecoveries");
    }
    db.set(&key, &checkpoint, None)?;
    db.patch_run(
        text(&run, "id"),
        &json!({
            "sessionId": null,
            "resumeAvailable": false
        }),
    )?;
    chat["sessionRestartRequested"] = true.into();
    chat["wakeup"] = Value::Null;
    chat["paused"] = true.into();
    db.put("chats", &chat)
}

fn cpu_ticks() -> u64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let fields: Vec<_> = stat
        .rsplit_once(") ")
        .unwrap()
        .1
        .split_whitespace()
        .collect();
    fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap()
}

fn stats(mut samples: Vec<u128>) -> Value {
    samples.sort_unstable();
    json!({
        "medianUs": samples[samples.len() / 2] as f64 / 1000.,
        "p95Us": samples[samples.len() * 95 / 100] as f64 / 1000.,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    for reference in [true, false] {
        let root = tempfile::tempdir()?;
        let service = Service::new(Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().to_owned()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "unused".into(),
            claude_bin: "unused".into(),
            opencode_bin: "unused".into(),
            gh_bin: "unused".into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await?;
        let chat = json!({
            "id": "chat",
            "runId": "run",
            "restoredAt": 10,
            "paused": false,
        });
        let run = json!({
            "id": "run",
            "taskId": "chat",
            "createdAt": 0,
            "status": "failed",
            "sessionId": "old-native",
            "restoredAt": 10,
        });
        let checkpoint = json!({
            "launched": true,
            "completed": true,
            "controllerRecoveries": 2,
            "prepared": { "workspace": "preserved", "isolated": false },
            "fixturePadding": "x".repeat(16384),
        });
        let seed_chat = chat.clone();
        let seed_run = run.clone();
        let seed_cp = checkpoint.clone();
        service.store.transaction(move |db| {
            db.put("chats", &seed_chat)?;
            db.0.execute("UPDATE records SET data=json_set(data,'$.slug','synthetic-chat') WHERE id='chat'", [])?;
            db.add_run(&seed_run, None)?;
            db.set("run-checkpoint:run", &seed_cp, None)
        }).await?;
        let mut writes = Vec::new();
        let mut cpu = 0;
        for _ in 0..1000 {
            let chat = chat.clone();
            let run = run.clone();
            let checkpoint = checkpoint.clone();
            service
                .store
                .transaction(move |db| {
                    db.put("chats", &chat)?;
                    db.patch_run("run", &run)?;
                    db.set("run-checkpoint:run", &checkpoint, None)
                })
                .await?;
            let ticks = cpu_ticks();
            let start = Instant::now();
            if reference {
                service
                    .store
                    .transaction(|db| reference_request(db, "chat"))
                    .await?;
            } else {
                service.chat_new_session("chat", true).await?;
            }
            writes.push(start.elapsed().as_nanos());
            cpu += cpu_ticks() - ticks;
        }
        let sizes = service
            .store
            .read(|db| {
                Ok(json!({
                    "chatBytes": db.get("chats","chat")?.unwrap().to_string().len(),
                    "runBytes": db.run("run")?.unwrap().to_string().len(),
                    "checkpointBytes": db.kv("run-checkpoint:run")?.unwrap().to_string().len()
                }))
            })
            .await?;
        let mut warm = Vec::new();
        for _ in 0..1000 {
            let start = Instant::now();
            service
                .store
                .read(|db| {
                    Ok((
                        db.get("chats", "chat")?,
                        db.run("run")?,
                        db.kv("run-checkpoint:run")?,
                    ))
                })
                .await?;
            warm.push(start.elapsed().as_nanos());
        }
        let mut cold = Vec::new();
        for _ in 0..30 {
            let store = Store::open(&service.config.data_dir)?;
            let start = Instant::now();
            store
                .read(|db| {
                    Ok((
                        db.get("chats", "chat")?,
                        db.run("run")?,
                        db.kv("run-checkpoint:run")?,
                    ))
                })
                .await?;
            cold.push(start.elapsed().as_nanos());
        }
        println!(
            "{}",
            json!({
                "reference": reference,
                "sizes": sizes,
                "write": stats(writes),
                "warmRead": stats(warm),
                "coldConnectionRead": stats(cold),
                "handlerCpuTicks": cpu,
            })
        );
    }
    Ok(())
}
