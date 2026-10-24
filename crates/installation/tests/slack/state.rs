//! Native storage-lifetime fixtures, distinct from the real worker/provider journeys.
use super::*;

struct Native {
    _root: TempDir,
    service: Arc<Service>,
    context: capabilities::Context,
    delivery: Value,
    chat: Value,
}

impl Native {
    async fn new() -> Self {
        let root = TempDir::new().unwrap();
        let service = Service::new(common::config(root.path())).await.unwrap();
        common::relay_fixture::claimed(&service).await.unwrap();
        service
            .plugins
            .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
        let instance = connections::save(
            &service,
            "slack",
            &json!({
                "name": "Delivery storage", "enabled": true, "userIds": ["UALICE"],
                "settings": {"appId": "ATEST", "sharingAcknowledged": true},
                "secrets": {"signingSecret": SIGNING_SECRET},
            }),
            None,
        )
        .await
        .unwrap();
        let connection = connections::get(&service, "slack", text(&instance, "id"))
            .await
            .unwrap();
        let chat = service.chat_create(json!({})).await.unwrap();
        let delivery = json!({
            "id": cairn_installation::config::id(), "chatId": chat["id"],
            "connectionId": connection.id, "revision": connection.revision,
            "routing": null, "userId": "UALICE",
        });
        service
            .store
            .put(work::DELIVERIES, delivery.clone())
            .await
            .unwrap();
        let context = capabilities::Context {
            connection,
            management: None,
            manifest: registry::builtin("slack").unwrap().manifest,
            event: json!({"name": "question", "id": delivery["id"], "authorId": "UALICE"}),
            link: None,
            draft: None,
            resource_grant: None,
        };
        Self {
            _root: root,
            service,
            context,
            delivery,
            chat,
        }
    }

    async fn set(&self, key: &str, scoped: bool) -> cairn_installation::error::Result<Value> {
        self.set_value(
            key,
            scoped,
            json!({
                "status": "sent", "fields": ["synthetic nonsecret question"], "answers": {},
            }),
        )
        .await
    }

    async fn set_value(
        &self,
        key: &str,
        scoped: bool,
        value: Value,
    ) -> cairn_installation::error::Result<Value> {
        let mut input = json!({ "key": key, "value": value });
        if scoped {
            input["delivery"] = self.delivery["id"].clone();
        }
        capabilities::checked(&self.service, &self.context, "storage.set", input).await
    }
}

#[tokio::test]
async fn native_delivery_state_cannot_be_attached_to_another_author_or_execution() {
    let mut native = Native::new().await;
    native.set("issued", true).await.unwrap();
    for (field, value) in [
        ("name", json!("webhook")),
        ("authorId", json!("UBOB")),
        ("id", json!(cairn_installation::config::id())),
    ] {
        let old = native.context.event[field].clone();
        native.context.event[field] = value;
        assert_eq!(
            native.set("foreign", true).await.unwrap_err().status,
            403,
            "{field}"
        );
        native.context.event[field] = old;
    }
    assert_eq!(
        native
            .service
            .store
            .list("plugin-storage")
            .await
            .unwrap()
            .len(),
        1
    );
    native.context.event["name"] = json!("interaction");
    native.context.event["id"] = json!(cairn_installation::config::id());
    native.set("issued", true).await.unwrap();
    assert_eq!(
        native
            .set("not-previously-issued", true)
            .await
            .unwrap_err()
            .status,
        403
    );
}

#[tokio::test]
async fn ending_a_delivery_retires_its_state_without_erasing_instance_values() {
    let mut native = Native::new().await;
    native.set("ordinary", false).await.unwrap();
    native.set("issued", true).await.unwrap();
    work::finish(&native.service, &native.delivery, "failed")
        .await
        .unwrap();
    let rows = native.service.store.list("plugin-storage").await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["key"], "ordinary");
    native.context.event["name"] = json!("interaction");
    assert_eq!(
        native.set("issued", true).await.unwrap_err().status,
        403,
        "A queued callback recreated a retired key"
    );
    assert_eq!(
        native
            .service
            .store
            .list("plugin-storage")
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn purging_a_conversation_retires_its_pending_plugin_state_atomically() {
    let native = Native::new().await;
    native.set("ordinary", false).await.unwrap();
    native.set("issued", true).await.unwrap();
    native
        .service
        .chat_trash(text(&native.chat, "id"), true)
        .await
        .unwrap();
    // The API marks deletion; the ordinary cleanup pass performs the purge.
    // This fixture has no manager running that pass in the background.
    native.service.cleanup_conversations().await.unwrap();
    assert!(
        native
            .service
            .store
            .get("chats", text(&native.chat, "id"))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        native
            .service
            .store
            .get(work::DELIVERIES, text(&native.delivery, "id"))
            .await
            .unwrap()
            .is_none()
    );
    let rows = native.service.store.list("plugin-storage").await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["key"], "ordinary");
}

#[tokio::test]
#[ignore = "local native question-state storage comparison; run alone"]
async fn measure_question_state_storage() {
    measure_state_storage(false).await;
}

#[tokio::test]
#[ignore = "local native progress-state storage comparison; run alone"]
async fn measure_progress_state_storage() {
    measure_state_storage(true).await;
}

#[tokio::test]
#[ignore = "local native workspace app-binding record comparison; run alone"]
async fn measure_workspace_app_binding_storage() {
    use cairn_installation::store::Store;

    let root = TempDir::new().unwrap();
    let service = Service::new(common::config(root.path())).await.unwrap();
    common::relay_fixture::claimed(&service).await.unwrap();
    service
        .plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let slack = Arc::new(Slack::default());
    let provider = Router::new()
        .route("/api/{method}", post(api).get(api))
        .with_state(slack);
    let (origin, server) = common::serve_locally(provider).await;
    service.plugins.route_http_to(Some(origin));
    let app = cairn_installation::http::router(service.clone())
        .await
        .unwrap();
    let (_, resource) = configure(&app, &service).await;
    let id = text(&resource, "id").to_owned();
    let saved = service
        .store
        .get("plugin-resources", &id)
        .await
        .unwrap()
        .unwrap();
    service.store.transaction(|db| {
        for index in 0..63 {
            db.put("plugin-resources", &json!({"id": format!("unrelated-{index}"), "settings": {"label": "x".repeat(1800)}}))?;
        }
        Ok(())
    }).await.unwrap();
    let mut cases = Vec::new();
    for bound in [false, true] {
        let mut row = saved.clone();
        if !bound {
            row["settings"].as_object_mut().unwrap().remove("appId");
        }
        service
            .store
            .put("plugin-resources", row.clone())
            .await
            .unwrap();
        let reopened = Store::open(&service.config.data_dir).unwrap();
        let key = id.clone();
        let reads = reopened.read(move |db| {
            let mut samples = Vec::new();
            for sample in 0..6 {
                let cpu = checkpoints::cpu_ms();
                let at = Instant::now();
                let actual = db.get("plugin-resources", &key)?.unwrap();
                samples.push(json!({"sample": sample, "wallMs": at.elapsed().as_secs_f64()*1000.0,
                    "processCpuMs": checkpoints::cpu_ms()-cpu, "returnedJsonBytes": serde_json::to_vec(&actual)?.len()}));
            }
            Ok(samples)
        }).await.unwrap();
        let mut writes = Vec::new();
        for sample in 0..6 {
            let cpu = checkpoints::cpu_ms();
            let at = Instant::now();
            service
                .store
                .put("plugin-resources", row.clone())
                .await
                .unwrap();
            writes.push(
                json!({"sample": sample, "wallMs": at.elapsed().as_secs_f64()*1000.0,
                "processCpuMs": checkpoints::cpu_ms()-cpu}),
            );
        }
        cases.push(json!({"appBound": bound, "unrelatedRows": 63,
            "rowJsonBytes": serde_json::to_vec(&row).unwrap().len(), "reads": reads, "writes": writes}));
    }
    eprintln!(
        "WORKSPACE_APP_BINDING_STORAGE {}",
        json!({"cases": cases,
        "workload": "same verified synthetic resource; previous row without appId versus candidate with provider-verified appId; six reads on each reopened reader and six raw store writes",
        "cache": "first read connection-cold, five warm; OS caches retained; setup/schema and initial row writes excluded",
        "limits": "row JSON excludes vault/database/WAL/index pages; reads exclude actor queue, writes include actor queue; no plugin RPC/approval/auth/provider or production network latency measurement",
        "host": {"parallelism": std::thread::available_parallelism().ok().map(std::num::NonZero::get),
            "ioPressure": std::fs::read_to_string("/proc/pressure/io").ok(),
            "memoryPressure": std::fs::read_to_string("/proc/pressure/memory").ok()}})
    );
    service.shutdown.cancel();
    server.abort();
}

async fn measure_state_storage(progress: bool) {
    use cairn_installation::store::Store;
    let mut cases = Vec::new();
    for scoped in [false, true] {
        let mut native = Native::new().await;
        let value = if progress {
            native.context.event["name"] = json!("progress");
            json!({
                "key": format!("progress:{}", text(&native.delivery, "id")),
                "status": "sent",
                "delivery": native.delivery["id"],
                "conversation": native.chat["id"],
                "userId": "UALICE",
                "resource": "11111111-1111-4111-8111-111111111111",
                "team": "TTEST",
                "channel": "CTEST",
                "thread": "1700000000.000001",
                "readyAt": 1792753600,
                "message": "1700000001.000001",
                "label": "Working — Writing the answer",
            })
        } else {
            json!({
                "status": "sent",
                "fields": ["synthetic nonsecret question"],
                "answers": {},
            })
        };
        let connection = native.context.connection.id.clone();
        // Same near-capacity instance in each case. The one observed value
        // differs only by native ownership, not payload or unrelated occupancy.
        native
            .service
            .store
            .transaction(move |db| {
                for index in 0..999 {
                    db.put(
                        "plugin-storage",
                        &json!({
                            "id": format!("unrelated-{index}"), "connectionId": connection,
                            "key": format!("unrelated-{index}"), "value": "synthetic".repeat(200),
                            "updatedAt": 0,
                        }),
                    )?;
                }
                Ok(())
            })
            .await
            .unwrap();
        native
            .set_value("measured", scoped, value.clone())
            .await
            .unwrap();
        let rows = native.service.store.list("plugin-storage").await.unwrap();
        let row = rows.iter().find(|row| row["key"] == "measured").unwrap();
        let encoded = serde_json::to_vec(row).unwrap().len();
        let id = text(row, "id").to_owned();
        let reopened = Store::open(&native.service.config.data_dir).unwrap();
        let reads = reopened.read(move |db| {
            let mut samples = Vec::new();
            for sample in 0..6 {
                let cpu = checkpoints::cpu_ms();
                let at = Instant::now();
                let row = db.get("plugin-storage", &id)?.unwrap();
                samples.push(json!({"sample": sample, "wallMs": at.elapsed().as_secs_f64()*1000.0,
                    "processCpuMs": checkpoints::cpu_ms()-cpu, "returnedJsonBytes": serde_json::to_vec(&row)?.len()}));
            }
            Ok(samples)
        }).await.unwrap();
        let mut writes = Vec::new();
        let mut retirements = Vec::new();
        for sample in 0..6 {
            native
                .service
                .store
                .put(work::DELIVERIES, native.delivery.clone())
                .await
                .unwrap();
            let cpu = checkpoints::cpu_ms();
            let at = Instant::now();
            native
                .set_value("measured", scoped, value.clone())
                .await
                .unwrap();
            writes.push(
                json!({"sample":sample,"wallMs":at.elapsed().as_secs_f64()*1000.0,
                "processCpuMs":checkpoints::cpu_ms()-cpu}),
            );
            let cpu = checkpoints::cpu_ms();
            let at = Instant::now();
            work::finish(&native.service, &native.delivery, "failed")
                .await
                .unwrap();
            retirements.push(
                json!({"sample":sample,"wallMs":at.elapsed().as_secs_f64()*1000.0,
                "processCpuMs":checkpoints::cpu_ms()-cpu}),
            );
        }
        let retained = native.service.store.list("plugin-storage").await.unwrap();
        cases.push(json!({"deliveryOwned":scoped,"unrelatedRows":999,"observedRowJsonBytes":encoded,
            "remainingRows":retained.len(),"reads":reads,"writes":writes,"retirements":retirements}));
    }
    eprintln!(
        "{} {}",
        if progress {
            "PROGRESS_STATE_STORAGE"
        } else {
            "QUESTION_STATE_STORAGE"
        },
        json!({"cases":cases,
        "valueKind": if progress {"synthetic confirmed progress"} else {"synthetic question"},
        "workload":"same synthetic value in 1000-key instance; 6 reads on one reopened reader, 6 actual native writes/retirements; reference ordinary state versus delivery-owned candidate",
        "cache":"connection-cold first read, five warm reads; OS cache retained; prepared schema/auth and fixture setup excluded",
        "limits":"serialized JSON bytes exclude database/WAL/index pages; reads bypass capability authorization, writes include host checks and actor scheduling; no live Slack/model/VM/network latency claim",
        "host":{"parallelism":std::thread::available_parallelism().ok().map(std::num::NonZero::get),
            "cpuMax":std::fs::read_to_string("/sys/fs/cgroup/cpu.max").ok(),"memoryMax":std::fs::read_to_string("/sys/fs/cgroup/memory.max").ok()}})
    );
}
