use common::ProjectFixture;

mod common;

use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    config::{MAIN_AGENT_ID, id},
    service::Service,
};
use serde_json::{Value, json};
use tempfile::TempDir;

#[tokio::test]
async fn creation_replays_survive_restart_without_overwriting_later_edits() {
    let root = TempDir::new().unwrap();
    let config = common::config(root.path());
    let service = Service::new(config.clone()).await.unwrap();
    for kind in ["agents", "projects", "tasks"] {
        let creation_id = id();
        let request = match kind {
            "agents" => json!({ "name": "Agent", "creationId": creation_id }),
            "projects" => {
                json!({ "name": "Project", "path": root.path(), "creationId": creation_id })
            }
            _ => json!({
                "name": "Scheduled mission",
                "prompt": "Do the work",
                "agentId": MAIN_AGENT_ID,
                "cron": "0 9 * * *",
                "creationId": creation_id,
            }),
        };
        let (first, concurrent) = tokio::join!(
            save(&service, kind, request.clone(), None),
            save(&service, kind, request.clone(), None),
        );
        assert_eq!(first, concurrent);
        assert_eq!(first["id"], creation_id);
        assert!(first.get("creationId").is_none());
        let mut edited = request.clone();
        edited["name"] = "Updated after creation".into();
        let mut edited = save(&service, kind, edited, Some(&creation_id)).await;
        if kind == "tasks" {
            // A scheduler may have advanced the commitment since the lost response.
            // Recovering the creation must not recompute its next occurrence.
            edited["nextRun"] = 1234.into();
            service.store.put("tasks", edited.clone()).await.unwrap();
        }
        let restarted = Service::new(config.clone()).await.unwrap();
        let replay = save(&restarted, kind, request, None).await;
        assert_eq!(
            replay, edited,
            "{kind} replay must preserve later edits and schedule"
        );
        assert_eq!(replay["createdAt"], first["createdAt"]);
        let records = if kind == "projects" {
            restarted
                .store
                .projects()
                .await
                .unwrap()
                .into_iter()
                .map(|project| serde_json::to_value(project).unwrap())
                .collect::<Vec<_>>()
        } else {
            restarted.store.list(kind).await.unwrap()
        };
        assert_eq!(
            records
                .iter()
                .filter(|record| record["id"] == creation_id)
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn creation_identity_is_validated_and_cannot_replace_an_update_identity() {
    let root = TempDir::new().unwrap();
    let service = Service::new(common::config(root.path())).await.unwrap();
    for invalid in [
        json!("not-an-id"),
        json!(1),
        Value::Null,
        json!("00000000-0000-0000-0000-000000000000"),
    ] {
        assert!(
            service
                .agent(json!({ "name": "Agent", "creationId": invalid }), None)
                .await
                .is_err()
        );
        assert!(
            service
                .project_fixture(
                    json!({ "name": "Project", "path": root.path(), "creationId": invalid }),
                    None
                )
                .await
                .is_err()
        );
        assert!(
            service
                .task(
                    json!({
                        "name": "Task",
                        "prompt": "Work",
                        "agentId": MAIN_AGENT_ID,
                        "creationId": invalid,
                    }),
                    None
                )
                .await
                .is_err()
        );
    }
    let original = service
        .agent(json!({ "name": "Original" }), None)
        .await
        .unwrap();
    let replacement_id = id();
    let updated = service
        .agent(
            json!({ "name": "Updated", "creationId": replacement_id }),
            original["id"].as_str(),
        )
        .await
        .unwrap();
    assert_eq!(updated["id"], original["id"]);
    assert!(
        service
            .store
            .get("agents", &replacement_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn another_member_cannot_recover_a_task_creation_as_their_own() {
    let root = TempDir::new().unwrap();
    let service = Service::new(common::config(root.path())).await.unwrap();
    let request = json!({
        "name": "Task",
        "prompt": "Work",
        "agentId": MAIN_AGENT_ID,
        "creationId": id(),
    });
    let mut task = service.task(request.clone(), None).await.unwrap();
    task["authorId"] = "original-author".into();
    service.store.put("tasks", task.clone()).await.unwrap();
    let identity = InstallationIdentity::trusted(InstallationRole::Member, "another-author");
    assert!(
        service
            .task_as(request.clone(), None, Some(&identity))
            .await
            .is_err()
    );
    assert_eq!(
        service
            .get("tasks", task["id"].as_str().unwrap())
            .await
            .unwrap(),
        task
    );
}

#[tokio::test]
async fn creation_identity_cannot_overwrite_another_resource_kind() {
    let root = TempDir::new().unwrap();
    let service = Service::new(common::config(root.path())).await.unwrap();
    let creation_id = id();
    let original = service
        .agent(json!({ "name": "Agent", "creationId": creation_id }), None)
        .await
        .unwrap();
    assert!(
        service
            .project_fixture(
                json!({ "name": "Project", "path": root.path(), "creationId": creation_id }),
                None
            )
            .await
            .is_err()
    );
    assert!(
        service
            .task(
                json!({
                    "name": "Task",
                    "prompt": "Work",
                    "agentId": MAIN_AGENT_ID,
                    "creationId": creation_id,
                }),
                None
            )
            .await
            .is_err()
    );
    assert_eq!(service.get("agents", &creation_id).await.unwrap(), original);
    assert!(service.store.projects().await.unwrap().is_empty());
    assert!(service.store.list("tasks").await.unwrap().is_empty());
}

async fn save(
    service: &std::sync::Arc<Service>,
    kind: &str,
    input: Value,
    existing: Option<&str>,
) -> Value {
    match kind {
        "agents" => service.agent(input, existing).await,
        "projects" => service.project_fixture(input, existing).await,
        _ => service.task(input, existing).await,
    }
    .unwrap()
}

/// Local SQLite reference: legacy POST identities versus replayable identities.
/// Connection-cold reads keep the host filesystem cache warm; this is not S3 or
/// production network latency. Run alone with --ignored --nocapture.
#[tokio::test]
#[ignore]
async fn measure_creation_retries() {
    use cairn_installation::store::Store;
    use std::time::Instant;

    for replayable in [false, true] {
        let root = TempDir::new().unwrap();
        let config = common::config(root.path());
        let service = Service::new(config.clone()).await.unwrap();
        let cpu_start = cpu_micros();
        let mut writes = Vec::new();
        let mut retries = Vec::new();
        let mut ids = Vec::new();
        for _ in 0..100 {
            let mut request = json!({
                "name": "Scheduled mission",
                "prompt": "Check the repository and report the result.",
                "agentId": MAIN_AGENT_ID,
                "cron": "0 9 * * *",
            });
            if replayable {
                request["creationId"] = id().into();
            }
            let start = Instant::now();
            let saved = service.task(request.clone(), None).await.unwrap();
            writes.push(start.elapsed().as_micros());
            ids.push(saved["id"].as_str().unwrap().to_owned());
            let start = Instant::now();
            service.task(request, None).await.unwrap();
            retries.push(start.elapsed().as_micros());
        }
        let mut cold = Vec::new();
        for id in ids.iter().take(20) {
            let store = Store::open(&config.data_dir).unwrap();
            let start = Instant::now();
            store.get("tasks", id).await.unwrap().unwrap();
            cold.push(start.elapsed().as_micros());
        }
        let mut warm = Vec::new();
        for id in ids.iter().cycle().take(200) {
            let start = Instant::now();
            service.store.get("tasks", id).await.unwrap().unwrap();
            warm.push(start.elapsed().as_micros());
        }
        let records = service.store.list("tasks").await.unwrap();
        let bytes: usize = records.iter().map(|record| record.to_string().len()).sum();
        println!(
            "{} records={} encoded_record_bytes={} cpu_us={} write_us={} retry_us={} connection_cold_read_us={} warm_read_us={}",
            if replayable {
                "stable_identity"
            } else {
                "legacy_identity"
            },
            records.len(),
            bytes,
            cpu_micros() - cpu_start,
            percentiles(writes),
            percentiles(retries),
            percentiles(cold),
            percentiles(warm)
        );
    }
}

fn percentiles(mut samples: Vec<u128>) -> String {
    samples.sort_unstable();
    format!(
        "p50:{},p95:{}",
        samples[samples.len() / 2],
        samples[samples.len() * 95 / 100]
    )
}

fn cpu_micros() -> i64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage initializes the provided rusage on success.
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
        0
    );
    let usage = unsafe { usage.assume_init() };
    usage.ru_utime.tv_sec * 1_000_000
        + usage.ru_utime.tv_usec
        + usage.ru_stime.tv_sec * 1_000_000
        + usage.ru_stime.tv_usec
}
