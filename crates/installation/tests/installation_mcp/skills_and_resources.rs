use super::*;

#[tokio::test]
async fn skill_lifecycle_matches_http_public_and_installation_without_granting_access() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use cairn_installation::skills::zip;

    for internal in [false, true] {
        let root = TempDir::new().unwrap();
        let s = service(&root).await;
        let manager = s.agent_fixture(json!({"name": "Resource manager", "access": {"installation": true, "projects": [], "skills": []}}), None).await.unwrap();
        let (_, configuration) = active_run(&s, &manager).await;
        let invoke = |name: &'static str, input: Value| {
            let s = s.clone();
            let configuration = configuration.clone();
            async move {
                if internal {
                    call(&s, &configuration, name, input).await
                } else {
                    public_call(&s, &["read", "manage"], name, input).await
                }
            }
        };
        let manifest = b"---\nname: review\ndescription: Review changes\n---\nReview.";
        let archive = zip::write(&[zip::File {
            path: "review/SKILL.md",
            bytes: manifest,
            executable: false,
        }])
        .unwrap();
        let staged = invoke(
            "stage_skill_upload",
            json!({"content": STANDARD.encode(&archive)}),
        )
        .await;
        let upload = result(&staged)["id"].clone();
        let imported = invoke(
            "import_skill",
            json!({"source": "archive", "scope": "global", "upload": upload}),
        )
        .await;
        let bundle = result(&imported)["imported"].clone();
        assert_eq!(bundle["name"], "review");
        let http =
            common::owner_api_route(&s, "GET", "/api/skills/global/review/bundle", Value::Null)
                .await
                .unwrap();
        assert_eq!(http, bundle);
        let chunk = invoke(
            "export_skill_archive",
            json!({"scope": "global", "name": "review", "offset": 0}),
        )
        .await;
        let chunk = result(&chunk);
        assert_eq!(STANDARD.decode(text(chunk, "content")).unwrap(), archive);
        let stale = invoke("edit_skill_files", json!({"scope": "global", "name": "review", "base": "stale", "changes": [{"op":"delete", "path":"SKILL.md"}]})).await;
        assert_eq!(stale["result"]["isError"], true);
        let missing = invoke(
            "get_skill_source_status",
            json!({"scope": "global", "name": "review"}),
        )
        .await;
        assert_eq!(missing["result"]["isError"], true);
        let forgotten = invoke(
            "forget_skill_source",
            json!({"scope": "global", "name": "review"}),
        )
        .await;
        assert_eq!(result(&forgotten)["detached"], true);
        let deleted = invoke("delete_skill", json!({"scope":"global", "name":"review"})).await;
        assert_eq!(result(&deleted)["deleted"], true);
        let current = s.store.agent(text(&manager, "id")).await.unwrap().unwrap();
        assert_eq!(
            current.settings.access,
            serde_json::from_value::<cairn_contracts::Agent>(manager)
                .unwrap()
                .settings
                .access
        );
        let denied = public_call(
            &s,
            &["read"],
            "stage_skill_upload",
            json!({"content":STANDARD.encode(&archive)}),
        )
        .await;
        assert_eq!(denied["result"]["isError"], true);
        let malformed = invoke("stage_skill_upload", json!({"content": "not-base64"})).await;
        assert_eq!(malformed["result"]["isError"], true);
    }
}

#[tokio::test]
async fn resource_sources_require_the_calling_agents_existing_instance_grant() {
    use common::github::{self, Github};
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let github = Github::start(root.path()).await;
    github.attach(&s);
    let main = enabled_main(&s).await;
    let instance = github::instance(&s, "Private account", "synthetic-resource-secret", &[]).await;
    let (_, configuration) = active_run(&s, &main).await;
    for (name, input) in [
        (
            "list_source_projects",
            json!({"pluginId":"github", "source":"repositories", "instance":instance}),
        ),
        (
            "import_project",
            json!({"pluginId":"github", "source":"repositories", "instance":instance, "id":"fixture/repo"}),
        ),
        (
            "import_skill",
            json!({"source":"plugin", "scope":"global", "pluginId":"github", "skillSource":"repository", "instance":instance, "location":{"repository":"fixture/skills"}}),
        ),
    ] {
        let response = call(&s, &configuration, name, input).await;
        assert_eq!(response["result"]["isError"], true, "{name}: {response}");
        assert!(!response.to_string().contains("synthetic-resource-secret"));
    }
    assert!(
        github.api_calls().is_empty(),
        "Unapproved sources must not call the provider."
    );
    assert!(s.store.projects().await.unwrap().is_empty());
}

#[tokio::test]
async fn skill_source_without_instance_uses_an_instance_granted_for_that_source() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use common::github::{self, Github};

    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let github = Github::start(root.path()).await;
    github.attach(&s);
    let main = enabled_main(&s).await;
    let unused = github::instance(
        &s,
        "A enabled but ungranted account",
        "synthetic-unused-secret",
        &[],
    )
    .await;
    let granted = github::instance(
        &s,
        "Z source account",
        "synthetic-source-secret",
        &[MAIN_AGENT_ID],
    )
    .await;
    assert_ne!(unused, granted);

    let grant_path = format!("/api/agents/{MAIN_AGENT_ID}/plugin-grants");
    let current =
        common::owner_api_route(&s, "GET", &grant_path, json!({ "agentId": MAIN_AGENT_ID }))
            .await
            .unwrap();
    common::owner_api_route(
        &s,
        "PUT",
        &grant_path,
        json!({
            "agentId": MAIN_AGENT_ID,
            "revision": current["revision"],
            "grants": [{
                "instanceId": granted,
                "access": {
                    "type": "selected",
                    "contributions": [{
                        "kind": "skillSource",
                        "name": "repository",
                        "resources": [],
                    }],
                },
            }],
            "runCredentialInstances": [],
        }),
    )
    .await
    .unwrap();

    let version = "1".repeat(40);
    let tree = "2".repeat(40);
    let blob = "a".repeat(40);
    let manifest = b"---\nname: review\ndescription: Review source\n---\nReview.";
    github.respond(
        "repos/fixture/skills/commits/main",
        &json!({
            "sha": version,
            "commit": {
                "tree": { "sha": tree },
                "message": "Release",
                "committer": { "date": "2026-10-01" },
            },
        }),
    );
    github.respond(
        &format!("repos/fixture/skills/commits/{version}"),
        &json!({
            "sha": version,
            "commit": {
                "tree": { "sha": tree },
            },
        }),
    );
    github.respond(
        &format!("repos/fixture/skills/git/trees/{tree}?recursive=1"),
        &json!({
            "tree": [{
                "path": "SKILL.md",
                "type": "blob",
                "mode": "100644",
                "sha": blob,
                "size": manifest.len(),
            }],
            "truncated": false,
        }),
    );
    github.respond(
        &format!("repos/fixture/skills/git/blobs/{blob}"),
        &json!({
            "content": STANDARD.encode(manifest),
            "encoding": "base64",
        }),
    );

    let (_, configuration) = active_run(&s, &main).await;
    let imported = call(
        &s,
        &configuration,
        "import_skill",
        json!({
            "source": "plugin",
            "scope": "global",
            "pluginId": "github",
            "skillSource": "repository",
            "location": { "repository": "fixture/skills", "ref": "main" },
        }),
    )
    .await;

    assert_ne!(imported["result"]["isError"], true, "{imported}");
    assert_eq!(result(&imported)["imported"]["name"], "review");
    assert!(!imported.to_string().contains("synthetic-source-secret"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_tools_share_status_updates_and_fence_instance_revocation_during_download() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use common::github::{self, Github};
    for internal in [false, true] {
        let root = TempDir::new().unwrap();
        let s = service(&root).await;
        let github = Github::start(root.path()).await;
        github.attach(&s);
        enabled_main(&s).await;
        let instance = github::instance(
            &s,
            "Source account",
            "synthetic-source-credential",
            &[MAIN_AGENT_ID],
        )
        .await;
        let main = s.agent_snapshot(MAIN_AGENT_ID).await.unwrap();
        let (_, configuration) = active_run(&s, &main).await;
        let invoke = |name: &'static str, input: Value| {
            let s = s.clone();
            let configuration = configuration.clone();
            Box::pin(async move {
                if internal {
                    call(&s, &configuration, name, input).await
                } else {
                    public_call(&s, &["read", "manage"], name, input).await
                }
            })
        };
        let first = "1".repeat(40);
        let second = "2".repeat(40);
        let manifest = "---\nname: review\ndescription: Review source\n---\nReview.";
        for (version, tree, blob, content) in [
            (&first, "3".repeat(40), "a".repeat(40), manifest.to_owned()),
            (
                &second,
                "4".repeat(40),
                "b".repeat(40),
                format!("{manifest}\nUpdated."),
            ),
        ] {
            github.respond(&format!("repos/fixture/skills/commits/{version}"), &json!({"sha":version,"commit":{"tree":{"sha":tree},"message":"Release","committer":{"date":"2026-10-01"}}}));
            github.respond(&format!("repos/fixture/skills/git/trees/{tree}?recursive=1"), &json!({"tree":[{"path":"SKILL.md","type":"blob","mode":"100644","sha":blob,"size":content.len()}],"truncated":false}));
            github.respond(
                &format!("repos/fixture/skills/git/blobs/{blob}"),
                &json!({"content":STANDARD.encode(content),"encoding":"base64"}),
            );
        }
        github.respond("repos/fixture/skills/commits/main", &json!({"sha":first,"commit":{"tree":{"sha":"3".repeat(40)},"message":"Release","committer":{"date":"2026-10-01"}}}));
        let imported = invoke("import_skill", json!({"source":"plugin","scope":"global","pluginId":"github","skillSource":"repository","instance":instance,"location":{"repository":"fixture/skills","ref":"main"}})).await;
        let base = result(&imported)["imported"]["revision"].clone();
        assert!(!imported.to_string().contains("synthetic-source-credential"));
        github.respond("repos/fixture/skills/commits/main", &json!({"sha":second,"commit":{"tree":{"sha":"4".repeat(40)},"message":"Release","committer":{"date":"2026-10-01"}}}));
        let status = invoke(
            "get_skill_source_status",
            json!({"scope":"global","name":"review"}),
        )
        .await;
        assert_eq!(result(&status)["current"], false);
        assert_eq!(result(&status)["latest"]["version"], second);
        assert_eq!(result(&status)["changes"][0]["status"], "modified");
        let started = root.path().join("source-started");
        let released = root.path().join("source-released");
        github.state(&json!({"gate":{"path":format!("repos/fixture/skills/git/blobs/{}", "b".repeat(40)),"started":started,"released":released}}));
        let pending = invoke(
            "update_skill_source",
            json!({"scope":"global","name":"review","base":base,"version":second}),
        );
        let pending = tokio::spawn(pending);
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            while !started.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let id = instance.clone();
        s.store
            .transaction(move |db| {
                let mut connection = db.plugin_instance(&id)?.unwrap();
                connection.enabled = false;
                connection.revision = uuid::Uuid::new_v4().to_string();
                db.save_plugin_instance(&connection)
            })
            .await
            .unwrap();
        std::fs::write(&released, "release").unwrap();
        let response = pending.await.unwrap();
        assert_eq!(response["result"]["isError"], true, "{response}");
        let current =
            common::owner_api_route(&s, "GET", "/api/skills/global/review/bundle", Value::Null)
                .await
                .unwrap();
        assert_eq!(current["revision"], base);
        assert_eq!(current["source"]["version"], first);
        let id = instance.clone();
        s.store
            .transaction(move |db| {
                let mut connection = db.plugin_instance(&id)?.unwrap();
                connection.enabled = true;
                connection.revision = uuid::Uuid::new_v4().to_string();
                db.save_plugin_instance(&connection)
            })
            .await
            .unwrap();
        let updated = invoke(
            "update_skill_source",
            json!({"scope":"global","name":"review","base":base,"version":second}),
        )
        .await;
        assert_eq!(result(&updated)["source"]["version"], second);
        let detached = invoke(
            "forget_skill_source",
            json!({"scope":"global","name":"review"}),
        )
        .await;
        assert_eq!(result(&detached)["detached"], true);
        assert_eq!(
            s.store
                .agent(MAIN_AGENT_ID)
                .await
                .unwrap()
                .unwrap()
                .settings
                .plugins,
            serde_json::from_value::<cairn_contracts::Agent>(main)
                .unwrap()
                .settings
                .plugins
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_resource_imports_complete_through_both_tools_without_extending_code_deadlines() {
    async fn scenario(internal: bool) {
        use base64::{Engine, engine::general_purpose::STANDARD};
        use common::github::{self, Github};
        let root = TempDir::new().unwrap();
        let s = service(&root).await;
        let github = Github::start(root.path()).await;
        github.attach(&s);
        enabled_main(&s).await;
        let instance =
            github::instance(&s, "Slow source", "synthetic-slow-source", &[MAIN_AGENT_ID]).await;
        let main = s.agent_snapshot(MAIN_AGENT_ID).await.unwrap();
        let (_, configuration) = active_run(&s, &main).await;
        let public = common::relay_fixture::grant(&s, &["read", "manage"]).await;
        let token = if internal {
            configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
                .as_str()
                .unwrap()
        } else {
            &public
        };
        let endpoint = if internal {
            "/mcp-installation"
        } else {
            "/mcp"
        };
        let version = "1".repeat(40);
        let tree = "2".repeat(40);
        let commit = json!({
            "sha": version,
            "commit": {
                "tree": { "sha": tree },
                "message": "Release",
                "committer": { "date": "2026-10-01" },
            },
        });
        github.respond("repos/fixture/skills/commits/main", &commit);
        github.respond(&format!("repos/fixture/skills/commits/{version}"), &commit);
        let mut files = Vec::new();
        for index in 0..7 {
            let blob = format!("{:040x}", index + 100);
            let path = if index == 0 {
                "SKILL.md".to_owned()
            } else {
                format!("data/{index}.txt")
            };
            let content = if index == 0 {
                "---\nname: review\ndescription: A slow source\n---\nReview."
            } else {
                "reference"
            };
            files.push(
                json!({"path":path,"type":"blob","mode":"100644","sha":blob,"size":content.len()}),
            );
            github.respond(
                &format!("repos/fixture/skills/git/blobs/{blob}"),
                &json!({"content":STANDARD.encode(content),"encoding":"base64"}),
            );
        }
        github.respond(
            &format!("repos/fixture/skills/git/trees/{tree}?recursive=1"),
            &json!({"tree":files,"truncated":false}),
        );
        // Seven individually valid 17 s downloads exceed 30 s with six workers.
        github.state(&json!({"blobDelay":17000}));
        let app = common::relay_fixture::router(s.clone()).await.unwrap();
        let (_, started) = tokio::time::timeout(std::time::Duration::from_secs(10), node_management_call(&app, endpoint, token, "import_skill", json!({"source":"plugin","scope":"global","pluginId":"github","skillSource":"repository","instance":instance,"location":{"repository":"fixture/skills","ref":"main"}}))).await.unwrap();
        let id = result(&started)["operation"].as_str().unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(31)).await;
        let (_, pending) = node_management_call(
            &app,
            endpoint,
            token,
            "get_plugin_operation",
            json!({"operationId":id}),
        )
        .await;
        assert_eq!(result(&pending)["state"], "running", "{pending}");
        let imported = finish_resource_call(&s, endpoint, token, "import_skill", started).await;
        assert_eq!(
            result(&imported)["imported"]["files"]
                .as_array()
                .unwrap()
                .len(),
            7
        );
        assert!(!imported.to_string().contains("synthetic-slow-source"));
    }
    tokio::join!(scenario(false), scenario(true));
}

#[tokio::test]
async fn both_mcp_endpoints_bind_approval_to_the_latest_root_lifecycle() {
    for internal in [false, true] {
        let root = TempDir::new().unwrap();
        let s = service(&root).await;
        let main = enabled_main(&s).await;
        let (_, configuration) = active_run(&s, &main).await;
        let invoke = |name, args| {
            let s = s.clone();
            let configuration = configuration.clone();
            async move {
                if internal {
                    call(&s, &configuration, name, args).await
                } else {
                    public_call(&s, &["read", "manage", "run"], name, args).await
                }
            }
        };
        for proposal in ["original", "replacement"] {
            for phase in [
                EventKind::ItemStarted,
                EventKind::ItemUpdated,
                EventKind::ItemCompleted,
            ] {
                let created = invoke("create_chat", json!({})).await;
                let chat = result(&created).clone();
                let chat_id = text(&chat, "id").to_owned();
                let run = uuid::Uuid::new_v4().to_string();
                let (c, r) = (chat_id.clone(), run.clone());
                let original = s.store.transaction(move |db| {
                    db.add_run_fixture(&json!({
                        "id": r, "taskId": r, "createdAt": 0, "status": "cancelled",
                        "nativeProvenanceUnknown": true, "nativePrincipals": [], "nativeAuthorityNodes": [],
                        "snapshot": { "agent": { "provider": "codex" } }
                    }), None)?;
                    let mut chat = db.conversation_boundary(&c)?.unwrap();
                    chat["runId"] = r.clone().into();
                    db.save_conversation_boundary(&chat)?;
                    db.event(&r, &EventKind::ChatUser, "Plan request", Some(&json!({ "messageId": "request-a" })))?;
                    db.event(&r, &EventKind::ItemCompleted, "Reviewed plan", Some(&json!({
                        "item": {
                            "id": "original", "type": "proposed_plan",
                            "text": "Reviewed plan", "request_id": "request-a",
                        }
                    })))?;
                    let original: i64 = db.0.query_row("SELECT MAX(id) FROM events WHERE run_id=?", [&r], |row| row.get(0))?;
                    db.event(&r, &EventKind::ChatUser, "Clarification", Some(&json!({ "messageId": "request-b" })))?;
                    let replaying = matches!(phase, EventKind::ItemCompleted);
                    db.event(&r, &phase, "", Some(&json!({
                        "item": { "id": proposal, "type": "proposed_plan", "text": if replaying { "Reviewed plan" } else { "" }, "request_id": "request-a" }
                    })))?;
                    if replaying {
                        Ok(db.0.query_row("SELECT MAX(id) FROM events WHERE run_id=?", [&r], |row| row.get::<_, i64>(0))?)
                    } else {
                        Ok(original)
                    }
                }).await.unwrap();
                let current = s.chat_detail(&chat_id).await.unwrap();
                let refused = invoke("send_chat_message", json!({
                    "chatId": chat_id,
                    "message": {
                        "id": uuid::Uuid::new_v4(), "text": "Implement the plan you proposed.",
                        "planApproval": { "sourceId": format!("{run}:{original}"), "revision": current["revision"] }
                    }
                })).await;
                assert_eq!(
                    refused["result"]["isError"], true,
                    "internal={internal}: {refused}"
                );
                assert!(
                    s.chat_detail(&chat_id).await.unwrap()["messages"]
                        .as_array()
                        .unwrap()
                        .is_empty()
                );
                let r = run.clone();
                let completed = s
                    .store
                    .transaction(move |db| {
                        db.event(&r, &EventKind::ChatUser, "Clarification", Some(&json!({ "messageId": "request-b" })))?;
                        db.event(&r, &EventKind::ItemCompleted, "Final plan", Some(&json!({
                        "item": { "id": proposal, "type": "proposed_plan", "text": "Final plan", "request_id": "request-b" }
                    })))?;
                        Ok(db.0.query_row(
                            "SELECT MAX(id) FROM events WHERE run_id=?",
                            [r],
                            |row| row.get::<_, i64>(0),
                        )?)
                    })
                    .await
                    .unwrap();
                let current = s.chat_detail(&chat_id).await.unwrap();
                let request = json!({
                    "chatId": chat_id,
                    "message": {
                        "id": uuid::Uuid::new_v4(), "text": "Implement the plan you proposed.",
                        "planApproval": { "sourceId": format!("{run}:{completed}"), "revision": current["revision"] }
                    }
                });
                let accepted = invoke("send_chat_message", request.clone()).await;
                assert_ne!(accepted["result"]["isError"], true, "{accepted}");
                let retry = invoke("send_chat_message", request).await;
                assert_eq!(result(&accepted)["id"], result(&retry)["id"]);
            }
        }
    }
}

#[tokio::test]
async fn resource_reads_match_http_public_mcp_and_installation_with_live_grants() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    s.plugins
        .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let installed = common::package_install(
        &s,
        &json!({
            "manifest": {
                "id":"resource-probe", "name":"Resources", "version":"1", "apiVersion":2,
                "capabilities":[], "agentAccess":{},
                "resources":[{"name":"repository","description":"A repository"}]
            },
            "source": r#"return { resources = function(input)
            local item = {resourceType = "repository", id = "repo", name = "Repository"}
            if input.op == "list" then return {items = cairn.array({item})} end
            return item
        end }"#
        }),
    )
    .await
    .unwrap();
    common::package_approve(&s, "resource-probe", "1", text(&installed, "digest"))
        .await
        .unwrap();
    let saved = common::save_plugin(
        &s,
        "resource-probe",
        &json!({"name":"Private","enabled":true}),
        None,
    )
    .await
    .unwrap();
    let instance = text(&saved, "id");
    let main = enabled_main(&s).await;
    let (_, ungranted) = active_run(&s, &main).await;
    let list_args = json!({"pluginId":"resource-probe","instanceId":instance,"resourceType":"repository","page":1,"query":""});
    let resolve_args = json!({"pluginId":"resource-probe","instanceId":instance,"resourceType":"repository","id":"repo"});
    for (name, args) in [
        ("list_plugin_resources", list_args.clone()),
        ("resolve_plugin_resource", resolve_args.clone()),
    ] {
        let refused = call(&s, &ungranted, name, args.clone()).await;
        assert_eq!(refused["result"]["isError"], true, "{refused}");
        let refused = public_call(&s, &["manage"], name, args).await;
        assert_eq!(refused["result"]["isError"], true, "{refused}");
    }
    let main = s
        .agent_fixture(json!({"plugins":[instance]}), Some(MAIN_AGENT_ID))
        .await
        .unwrap();
    let (_, configuration) = active_run(&s, &main).await;
    let app = cairn_installation::http::router(s.clone()).await.unwrap();
    let mut completed = Vec::new();
    let mut public_operations = Vec::new();
    for (name, args, path) in [
        (
            "list_plugin_resources",
            list_args,
            format!(
                "/api/plugins/resource-probe/connections/{instance}/resources?resourceType=repository&page=1&query="
            ),
        ),
        (
            "resolve_plugin_resource",
            resolve_args,
            format!(
                "/api/plugins/resource-probe/connections/{instance}/resources/ancestors?resourceType=repository&id=repo"
            ),
        ),
    ] {
        let http = common::send(
            &app,
            common::request("GET", &path)
                .extension(cairn_installation::auth::InstallationIdentity::trusted(
                    cairn_installation::auth::InstallationRole::Owner,
                    "fixture-owner",
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(http.status(), StatusCode::OK);
        let started = common::read_json(http).await;
        let expected = poll_resource_http(&app, text(&started, "operation")).await;
        let public_token = common::relay_fixture::grant(&s, &["read"]).await;
        let public_app = common::relay_fixture::router(s.clone()).await.unwrap();
        let (_, started) =
            node_management_call(&public_app, "/mcp", &public_token, name, args.clone()).await;
        public_operations.push((
            public_token.clone(),
            text(result(&started), "operation").to_owned(),
        ));
        let public = finish_plugin_resource_call(&s, "/mcp", &public_token, started).await;
        let started = call(&s, &configuration, name, args.clone()).await;
        completed.push(text(result(&started), "operation").to_owned());
        let token = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
        let internal = finish_plugin_resource_call(&s, "/mcp-installation", token, started).await;
        assert_eq!(result(&public), &expected);
        assert_eq!(result(&internal), &expected);
        let member = common::send(
            &app,
            common::request("GET", &path)
                .extension(cairn_installation::auth::InstallationIdentity::trusted(
                    cairn_installation::auth::InstallationRole::Member,
                    "member",
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(member.status(), StatusCode::FORBIDDEN);
    }
    // Manage and read are separate public scopes, including when a caller uses
    // the older management poller to retrieve a retained resource result.
    for (_, id) in &public_operations {
        let refused = public_call(
            &s,
            &["manage"],
            "get_plugin_operation",
            json!({"operationId":id}),
        )
        .await;
        assert_eq!(refused["result"]["isError"], true, "{refused}");
    }
    let unverified_app = cairn_installation::http::router(s.clone()).await.unwrap();
    for (name, args) in [
        (
            "list_plugin_resources",
            json!({"pluginId":"resource-probe","instanceId":instance,"resourceType":"repository"}),
        ),
        (
            "get_plugin_resource_operation",
            json!({"operationId":public_operations[0].1}),
        ),
        (
            "get_plugin_operation",
            json!({"operationId":public_operations[0].1}),
        ),
    ] {
        let mut identity = cairn_installation::auth::InstallationIdentity::trusted(
            cairn_installation::auth::InstallationRole::Owner,
            "fixture-owner",
        );
        identity.mcp_scopes = Some(vec!["read".into(), "manage".into()]);
        let response = common::send(
            &unverified_app,
            common::request("POST", "/api/mcp")
                .extension(identity)
                .body(Body::from(
                    json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                "params": common::mcp_arguments(name, &args)})
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let response = common::read_json(response).await;
        assert_eq!(response["result"]["isError"], true, "{response}");
    }

    // A read token cannot use the new polling API to read management preparations.
    let management = s
        .plugins
        .operations
        .start(
            s.clone(),
            cairn_installation::plugins::operations::Audience::Public,
            async {
                Err(cairn_installation::error::Error::bad(
                    "Synthetic management failure.",
                ))
            },
        )
        .unwrap();
    let refused = public_call(
        &s,
        &["read"],
        "get_plugin_resource_operation",
        json!({"operationId": management["operation"]}),
    )
    .await;
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    let allowed = public_call(
        &s,
        &["manage"],
        "get_plugin_operation",
        json!({"operationId": management["operation"]}),
    )
    .await;
    assert_ne!(allowed["result"]["isError"], true, "{allowed}");

    // Revoking the original public read also fences already completed results,
    // including attempts to bypass read polling through the existing manage tool.
    for (token, id) in public_operations {
        s.store
            .delete(&format!(
                "test-relay:{}",
                cairn_installation::auth::digest(&token)
            ))
            .await
            .unwrap();
        for (scopes, tool) in [
            (&["read"][..], "get_plugin_resource_operation"),
            (&["manage"][..], "get_plugin_operation"),
        ] {
            let refused = public_call(&s, scopes, tool, json!({"operationId":id})).await;
            assert_eq!(refused["result"]["isError"], true, "{refused}");
        }
    }
    s.agent_fixture(json!({"plugins":[]}), Some(MAIN_AGENT_ID))
        .await
        .unwrap();
    for id in completed {
        for tool in [
            "get_plugin_resource_operation",
            "get_plugin_operation",
            "cancel_plugin_operation",
        ] {
            let refused = call(&s, &configuration, tool, json!({"operationId":id})).await;
            assert_eq!(refused["result"]["isError"], true, "{refused}");
        }
    }
    let refused = call(
        &s,
        &configuration,
        "list_plugin_resources",
        json!({"pluginId":"resource-probe","instanceId":instance,"resourceType":"repository"}),
    )
    .await;
    assert_eq!(refused["result"]["isError"], true, "{refused}");
}

#[tokio::test]
async fn http_resource_handlers_retain_the_originating_owner_authority() {
    use axum::{Json, Router, http::HeaderMap, routing::get};
    use cairn_protocol::{TaskAuthorGrant, TaskAuthorPolicy};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::sync::Notify;

    for resolve in [false, true] {
        for replace_access in [false, true] {
            let root = TempDir::new().unwrap();
            let s = service(&root).await;
            s.plugins
                .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
            let installed = common::package_install(&s, &json!({
                "manifest": {
                    "id":"resource-owner",
                    "name":"Owner resources",
                    "version":"1",
                    "apiVersion":2,
                    "capabilities":["http"],
                    "hosts":["api.example.test"],
                    "agentAccess":{},
                    "resources":[{"name":"repository","description":"Repository"}]
                },
                "source": r#"return {resources = function(input)
                    pcall(function() cairn.http.request({url = "https://api.example.test/hold"}) end)
                    pcall(function() cairn.http.request({url = "https://api.example.test/second"}) end)
                    local item = {resourceType = "repository", id = "repo", name = "Repository"}
                    if input.op == "list" then return {items = cairn.array({item})} end
                    return item
                end}"#,
            })).await.unwrap();
            common::package_approve(&s, "resource-owner", "1", text(&installed, "digest"))
                .await
                .unwrap();
            let saved = common::save_plugin(
                &s,
                "resource-owner",
                &json!({"name":"Private","enabled":true}),
                None,
            )
            .await
            .unwrap();
            let entered = Arc::new(Notify::new());
            let release = Arc::new(Notify::new());
            let replaced = Arc::new(AtomicBool::new(false));
            let second_calls = Arc::new(AtomicUsize::new(0));
            let gateway = Router::new()
                .route(
                    "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
                    get({
                        let replaced = replaced.clone();
                        move |headers: HeaderMap| {
                            let replaced = replaced.clone();
                            async move {
                                if headers.get("authorization").and_then(|v| v.to_str().ok())
                                    != Some("Bearer fixture-only")
                                {
                                    return Err(StatusCode::UNAUTHORIZED);
                                }
                                Ok(Json(TaskAuthorPolicy {
                                    owner: TaskAuthorGrant {
                                        account_id: "fixture-owner".into(),
                                        access_id: if replaced.load(Ordering::SeqCst) {
                                            "replacement-access"
                                        } else {
                                            "fixture-owner"
                                        }
                                        .into(),
                                    },
                                    members: Vec::new(),
                                }))
                            }
                        }
                    }),
                )
                .route(
                    "/hold",
                    get({
                        let entered = entered.clone();
                        let release = release.clone();
                        move || {
                            let entered = entered.clone();
                            let release = release.clone();
                            async move {
                                entered.notify_one();
                                release.notified().await;
                                Json(json!({}))
                            }
                        }
                    }),
                )
                .route(
                    "/second",
                    get({
                        let second_calls = second_calls.clone();
                        move || {
                            second_calls.fetch_add(1, Ordering::SeqCst);
                            async { Json(json!({})) }
                        }
                    }),
                );
            let (origin, server) = common::serve_locally(gateway).await;
            s.plugins.route_http_to(Some(origin.clone()));
            let identity_path = s.config.data_dir.join("installation-relay/identity.json");
            let mut identity: Value =
                serde_json::from_slice(&std::fs::read(&identity_path).unwrap()).unwrap();
            identity["origin"] = origin.into();
            std::fs::write(&identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
            let app = cairn_installation::http::router(s.clone()).await.unwrap();
            let instance = text(&saved, "id");
            let suffix = if resolve {
                "resources/ancestors?resourceType=repository&id=repo"
            } else {
                "resources?resourceType=repository"
            };
            let response = common::send(
                &app,
                common::request(
                    "GET",
                    &format!("/api/plugins/resource-owner/connections/{instance}/{suffix}"),
                )
                .extension(cairn_installation::auth::InstallationIdentity::trusted(
                    cairn_installation::auth::InstallationRole::Owner,
                    "fixture-owner",
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let started = common::read_json(response).await;
            tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
                .await
                .unwrap();
            if replace_access {
                replaced.store(true, Ordering::SeqCst);
            } else {
                identity["token"] = "revoked-fixture-identity".into();
                std::fs::write(&identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
            }
            release.notify_one();
            let state = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    // Inspect the worker's terminal state directly so a new HTTP
                    // request cannot mask missing authority in the handler itself.
                    let state = s
                        .plugins
                        .operations
                        .get(text(&started, "operation"))
                        .unwrap();
                    if matches!(text(&state, "state"), "completed" | "failed" | "cancelled") {
                        break state;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                second_calls.load(Ordering::SeqCst),
                0,
                "resolve={resolve} replace_access={replace_access}: {state}"
            );
            assert_eq!(state["state"], "failed", "{state}");
            server.abort();
            s.shutdown.cancel();
        }
    }
}

#[tokio::test]
async fn resource_handlers_stop_service_effects_when_the_initiating_authority_is_revoked() {
    use axum::{Json, Router, routing::get};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Notify;

    for internal in [false, true] {
        let root = TempDir::new().unwrap();
        let s = service(&root).await;
        s.plugins
            .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
        let main = enabled_main(&s).await;
        let (run, configuration) = active_run(&s, &main).await;
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let second_calls = Arc::new(AtomicUsize::new(0));
        let upstream = Router::new()
            .route(
                "/hold",
                get({
                    let started = started.clone();
                    let release = release.clone();
                    move || {
                        let started = started.clone();
                        let release = release.clone();
                        async move {
                            started.notify_one();
                            release.notified().await;
                            Json(json!({}))
                        }
                    }
                }),
            )
            .route(
                "/second",
                get({
                    let calls = second_calls.clone();
                    move || {
                        calls.fetch_add(1, Ordering::SeqCst);
                        async { Json(json!({})) }
                    }
                }),
            );
        let (origin, server) = common::serve_locally(upstream).await;
        s.plugins.route_http_to(Some(origin));
        let installed = common::package_install(&s, &json!({
            "manifest": {
                "id": "resource-fence", "name": "Resource fence", "version": "1", "apiVersion": 2,
                "capabilities": ["http"], "hosts": ["api.example.test"], "agentAccess": {},
                "resources": [{ "name": "repository", "description": "Repository" }],
            },
            "source": r#"return {resources = function(input)
                pcall(function() cairn.http.request({url = "https://api.example.test/hold"}) end)
                pcall(function() cairn.http.request({url = "https://api.example.test/second"}) end)
                return {items = cairn.array({})}
            end}"#,
        })).await.unwrap();
        common::package_approve(&s, "resource-fence", "1", text(&installed, "digest"))
            .await
            .unwrap();
        let saved = common::save_plugin(
            &s,
            "resource-fence",
            &json!({
                "name": "Private", "enabled": true, "agentIds": [MAIN_AGENT_ID],
            }),
            None,
        )
        .await
        .unwrap();
        // The run's prior plugin snapshot need not grant this account: installation
        // resource reads check the agent's current instance grant on every effect.
        let token = common::relay_fixture::grant(&s, &["read"]).await;
        let app = common::relay_fixture::router(s.clone()).await.unwrap();
        let args = json!({
            "pluginId": "resource-fence", "instanceId": saved["id"], "resourceType": "repository",
        });
        let invoke = async {
            if internal {
                let started = call(&s, &configuration, "list_plugin_resources", args).await;
                finish_plugin_resource_call(
                    &s,
                    "/mcp-installation",
                    text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN"),
                    started,
                )
                .await
            } else {
                let response = common::send(
                    &app,
                    common::request("POST", "/mcp")
                        .header("authorization", format!("Bearer {token}"))
                        .body(Body::from(
                            json!({
                                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                                "params": common::mcp_arguments("list_plugin_resources", &args),
                            })
                            .to_string(),
                        ))
                        .unwrap(),
                )
                .await;
                assert_eq!(response.status(), StatusCode::OK);
                let started = common::read_json(response).await;
                finish_plugin_resource_call(&s, "/mcp", &token, started).await
            }
        };
        let revoke = async {
            started.notified().await;
            if internal {
                s.mcps.revoke_run(&s, text(&run, "id")).await.unwrap();
            } else {
                s.store
                    .delete(&format!(
                        "test-relay:{}",
                        cairn_installation::auth::digest(&token)
                    ))
                    .await
                    .unwrap();
            }
            release.notify_one();
        };
        let (response, ()) = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            tokio::join!(Box::pin(invoke), Box::pin(revoke))
        })
        .await
        .unwrap();
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert_eq!(
            second_calls.load(Ordering::SeqCst),
            0,
            "internal={internal}"
        );
        server.abort();
    }
}

#[tokio::test]
async fn resource_handlers_do_not_send_after_grant_revocation_during_oauth_refresh() {
    use axum::{
        Json, Router,
        routing::{get, post},
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Notify;

    for operation in [
        "list_plugin_resources",
        "resolve_plugin_resource",
        "import_project",
        "run_credentials_fetch",
    ] {
        let root = TempDir::new().unwrap();
        let s = service(&root).await;
        s.plugins
            .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
        let main = enabled_main(&s).await;
        let (run, configuration) = active_run(&s, &main).await;
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let service_calls = Arc::new(AtomicUsize::new(0));
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let upstream = Router::new()
            .route(
                "/token",
                post({
                    let started = started.clone();
                    let release = release.clone();
                    let refresh_calls = refresh_calls.clone();
                    move || {
                        let started = started.clone();
                        let release = release.clone();
                        let refresh_calls = refresh_calls.clone();
                        async move {
                            refresh_calls.fetch_add(1, Ordering::SeqCst);
                            started.notify_one();
                            release.notified().await;
                            Json(json!({
                                "access_token": "fixture-refreshed",
                                "token_type": "Bearer",
                                "expires_in": 3600,
                            }))
                        }
                    }
                }),
            )
            .route(
                "/resources",
                get({
                    let service_calls = service_calls.clone();
                    move || {
                        service_calls.fetch_add(1, Ordering::SeqCst);
                        async { Json(json!({})) }
                    }
                }),
            );
        let (origin, server) = common::serve_locally(upstream).await;
        s.plugins.route_http_to(Some(origin));
        let installed = common::package_install(
            &s,
            &json!({
                "manifest": {
                    "id": "resource-refresh",
                    "name": "Resource refresh",
                    "version": "1",
                    "apiVersion": 2,
                    "capabilities": ["http"],
                    "hosts": ["api.example.test"],
                    "agentAccess": {},
                    "settings": [{
                        "key": "clientId",
                        "title": "Client ID",
                        "type": "string",
                        "required": true,
                    }],
                    "credential": {
                        "label": "Authorization",
                        "header": "authorization",
                        "prefix": "Bearer ",
                        "oauth": {
                            "authorizationUrl": "https://api.example.test/authorize",
                            "tokenUrl": "https://api.example.test/token",
                            "clientIdSetting": "clientId",
                        },
                    },
                    "instructions": "Fixture instructions",
                    "projectSources": [{ "name": "repositories", "title": "Repositories" }],
                    "runCredentials": {
                        "git": [{
                            "url": "https://api.example.test",
                            "username": "fixture",
                            "password": "{{credential}}",
                        }],
                    },
                    "resources": [{ "name": "repository", "description": "Repository" }],
                },
                "source": r#"return {
                    projects = function(input) return {id=input.id, name="Repository", origin="https://api.example.test/repo.git", defaultBranch="main"} end,
                    resources = function(input)
                cairn.http.request({url = "https://api.example.test/resources", auth = true})
                if input.op == "list" then return {items = cairn.array({})} end
                return {resourceType = "repository", id = input.id, name = "Repository"}
            end}"#,
            }),
        )
        .await
        .unwrap();
        common::package_approve(&s, "resource-refresh", "1", text(&installed, "digest"))
            .await
            .unwrap();
        let instance = uuid::Uuid::new_v4().to_string();
        let saved = s
            .store
            .put_instance_fixture(json!({
                "id": instance,
                "pluginId": "resource-refresh",
                "enabled": true,
                "settings": { "clientId": "fixture-client" },
            }))
            .await
            .unwrap();
        if operation != "run_credentials_fetch" {
            s.agent_fixture(json!({ "plugins": [instance] }), Some(MAIN_AGENT_ID))
                .await
                .unwrap();
        }
        assert_eq!(
            s.store
                .plugin_instance(&instance)
                .await
                .unwrap()
                .unwrap()
                .grants(MAIN_AGENT_ID),
            operation != "run_credentials_fetch",
        );
        if operation == "import_project" {
            let current = common::owner_api_route(
                &s,
                "GET",
                &format!("/api/agents/{MAIN_AGENT_ID}/plugin-grants"),
                Value::Null,
            )
            .await
            .unwrap();
            common::owner_api_route(
                &s,
                "PUT",
                &format!("/api/agents/{MAIN_AGENT_ID}/plugin-grants"),
                json!({
                    "agentId": MAIN_AGENT_ID,
                    "revision": current["revision"],
                    "grants": [{
                        "instanceId": instance,
                        "access": {
                            "type": "selected",
                            "contributions": [
                                {
                                    "kind": "projectSource",
                                    "name": "repositories",
                                    "resources": [],
                                },
                                {
                                    "kind": "instructions",
                                    "name": "",
                                    "resources": [],
                                },
                            ],
                        },
                    }],
                    "runCredentialInstances": [],
                }),
            )
            .await
            .unwrap();
        }
        let key = cairn_installation::plugins::connections::credential_key(&instance);
        let credential = json!({
            "token": "fixture-expired",
            "oauth": {
                "expiresAt": 1,
                "refreshToken": "fixture-refresh",
                "tokenUrl": "https://api.example.test/token",
                "clientId": "fixture-client",
                "scope": "",
            },
        });
        s.vault.set(&key, &credential).await.unwrap();
        let mut args = json!({
            "pluginId": "resource-refresh",
            "instanceId": instance,
            "resourceType": "repository",
        });
        if operation == "resolve_plugin_resource" {
            args["id"] = json!("repo");
        }
        if operation == "import_project" {
            args = json!({
                "pluginId": "resource-refresh",
                "source": "repositories",
                "instance": instance,
                "id": "repo",
            });
        }
        let invoke = async {
            if operation == "run_credentials_fetch" {
                let empty =
                    cairn_installation::plugins::run_credentials::for_execution(&s, &run, &[])
                        .await
                        .unwrap();
                assert!(empty.git.is_empty());
                assert_eq!(refresh_calls.load(Ordering::SeqCst), 0);
                let result = cairn_installation::plugins::run_credentials::for_execution(
                    &s,
                    &run,
                    &[json!({
                        "id": uuid::Uuid::new_v4().to_string(),
                        "origin": "https://api.example.test/repo.git",
                    })],
                )
                .await;
                return json!({ "result": { "isError": result.is_err() } });
            }
            let started = call(&s, &configuration, operation, args).await;
            let response = if operation == "import_project" {
                started
            } else {
                finish_plugin_resource_call(
                    &s,
                    "/mcp-installation",
                    text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN"),
                    started,
                )
                .await
            };
            assert!(
                refresh_calls.load(Ordering::SeqCst) > 0,
                "Refresh was not reached: {response}"
            );
            response
        };
        let revoke = async {
            started.notified().await;
            if operation == "run_credentials_fetch" {
                s.worker.cancel(&s, text(&run, "id")).await.unwrap();
            } else if operation == "import_project" {
                let mut current = common::owner_api_route(
                    &s,
                    "GET",
                    &format!("/api/agents/{MAIN_AGENT_ID}/plugin-grants"),
                    Value::Null,
                )
                .await
                .unwrap();
                current["grants"][0]["access"]["contributions"] =
                    json!([{"kind":"instructions","name":"","resources":[]}]);
                common::owner_api_route(
                    &s,
                    "PUT",
                    &format!("/api/agents/{MAIN_AGENT_ID}/plugin-grants"),
                    current,
                )
                .await
                .unwrap();
            } else {
                s.agent_fixture(json!({ "plugins": [] }), Some(MAIN_AGENT_ID))
                    .await
                    .unwrap();
            }
            let current = s.store.plugin_instance(&instance).await.unwrap().unwrap();
            assert_eq!(current.revision, saved["revision"].as_str().unwrap());
            // Plugin grants are independent of the run's installation access.
            if operation != "run_credentials_fetch" {
                let still_authorized = call(&s, &configuration, "list_plugins", json!({})).await;
                assert_ne!(
                    still_authorized["result"]["isError"], true,
                    "{still_authorized}"
                );
            }
            release.notify_one();
        };
        let (response, ()) = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            tokio::join!(Box::pin(invoke), Box::pin(revoke))
        })
        .await
        .unwrap();
        assert_eq!(
            response["result"]["isError"], true,
            "{operation}: {response}"
        );
        assert_eq!(service_calls.load(Ordering::SeqCst), 0, "{operation}");
        assert!(s.vault.get(&key).await.unwrap().as_ref() == Some(&credential));
        server.abort();
    }
}

#[tokio::test]
async fn resource_reads_return_pollable_operations_before_the_service_replies() {
    use axum::{Json, Router, routing::get};
    use tokio::sync::Notify;

    for operation in ["list_plugin_resources", "resolve_plugin_resource"] {
        let root = TempDir::new().unwrap();
        let s = service(&root).await;
        s.plugins
            .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
        let main = enabled_main(&s).await;
        let (_, configuration) = active_run(&s, &main).await;
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let upstream = Router::new().route(
            "/resources",
            get({
                let entered = entered.clone();
                let release = release.clone();
                move || {
                    let entered = entered.clone();
                    let release = release.clone();
                    async move {
                        entered.notify_one();
                        release.notified().await;
                        Json(json!({}))
                    }
                }
            }),
        );
        let (origin, server) = common::serve_locally(upstream).await;
        s.plugins.route_http_to(Some(origin));
        let installed = common::package_install(
            &s,
            &json!({
                "manifest": {
                    "id":"resource-delay", "name":"Resource delay", "version":"1", "apiVersion":2,
                    "capabilities":["http"], "hosts":["api.example.test"], "agentAccess":{},
                    "resources":[{"name":"repository","description":"Repository"}],
                },
                "source": r#"return {resources = function(input)
                cairn.http.request({url = "https://api.example.test/resources"})
                local item = {resourceType = "repository", id = "repo", name = "Repository"}
                if input.op == "list" then return {items = cairn.array({item})} end
                return item
            end}"#,
            }),
        )
        .await
        .unwrap();
        common::package_approve(&s, "resource-delay", "1", text(&installed, "digest"))
            .await
            .unwrap();
        let saved = common::save_plugin(
            &s,
            "resource-delay",
            &json!({"name":"Private","enabled":true}),
            None,
        )
        .await
        .unwrap();
        s.agent_fixture(json!({"plugins":[saved["id"]]}), Some(MAIN_AGENT_ID))
            .await
            .unwrap();
        let mut args = json!({"pluginId":"resource-delay","instanceId":saved["id"],"resourceType":"repository"});
        if operation == "resolve_plugin_resource" {
            args["id"] = json!("repo");
        }
        let mut pending = tokio::spawn({
            let s = s.clone();
            async move { call(&s, &configuration, operation, args).await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified())
            .await
            .unwrap();
        let response = tokio::time::timeout(std::time::Duration::from_secs(2), &mut pending).await;
        let returned_early = response.is_ok();
        release.notify_one();
        let response = match response {
            Ok(response) => response.unwrap(),
            Err(_) => pending.await.unwrap(),
        };
        server.abort();
        s.shutdown.cancel();
        assert!(
            returned_early,
            "{operation} waited for the service instead of returning an operation"
        );
        assert!(result(&response)["operation"].is_string(), "{response}");
    }
}

#[tokio::test]
async fn resource_ancestry_completes_through_read_only_mcp_beyond_the_relay_deadline() {
    use axum::{Json, Router, routing::get};
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    s.plugins
        .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let upstream = Router::new().route(
        "/lookup",
        get(|| async {
            tokio::time::sleep(std::time::Duration::from_secs(11)).await;
            Json(json!({}))
        }),
    );
    let (origin, server) = common::serve_locally(upstream).await;
    s.plugins.route_http_to(Some(origin));
    let installed = common::package_install(
        &s,
        &json!({
            "manifest": {
                "id":"resource-tree", "name":"Resource tree", "version":"1", "apiVersion":2,
                "capabilities":["http"], "hosts":["api.example.test"], "agentAccess":{},
                "resources":[
                    {"name":"organization","description":"Organization"},
                    {"name":"team","description":"Team","parentType":"organization"},
                    {"name":"repository","description":"Repository","parentType":"team"},
                ],
            },
            "source": r#"return {resources = function(input)
            cairn.http.request({url = "https://api.example.test/lookup"})
            local item = {resourceType = input.resourceType, id = input.id, name = "Resource"}
            if input.resourceType == "repository" then
                item.parent = {resourceType = "team", id = "team"}
            elseif input.resourceType == "team" then
                item.parent = {resourceType = "organization", id = "org"}
            end
            return item
        end}"#,
        }),
    )
    .await
    .unwrap();
    common::package_approve(&s, "resource-tree", "1", text(&installed, "digest"))
        .await
        .unwrap();
    let saved = common::save_plugin(
        &s,
        "resource-tree",
        &json!({"name":"Private","enabled":true}),
        None,
    )
    .await
    .unwrap();
    let token = common::relay_fixture::grant(&s, &["read"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let beginning = std::time::Instant::now();
    let (_, started) = tokio::time::timeout(std::time::Duration::from_secs(10),
        node_management_call(&app, "/mcp", &token, "resolve_plugin_resource",
            json!({"pluginId":"resource-tree","instanceId":saved["id"],"resourceType":"repository","id":"repo"})))
        .await.unwrap();
    let response = finish_plugin_resource_call(&s, "/mcp", &token, started).await;
    let resolved = result(&response);
    assert_eq!(resolved["resource"]["id"], "repo");
    assert_eq!(resolved["ancestors"][0]["id"], "team");
    assert_eq!(resolved["ancestors"][1]["id"], "org");
    assert!(beginning.elapsed() > std::time::Duration::from_secs(30));
    server.abort();
    s.shutdown.cancel();
}

#[tokio::test]
async fn resource_poll_rechecks_its_requester_after_the_originating_authority() {
    use axum::{Json, Router, routing::post};
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Notify;

    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    s.plugins
        .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let installed = common::package_install(&s, &json!({
        "manifest": {"id":"resource-poll","name":"Resource poll","version":"1","apiVersion":2,
            "capabilities":[],"agentAccess":{},"resources":[{"name":"repository","description":"Repository"}]},
        "source": "return {resources = function() return {items = cairn.array({})} end}",
    })).await.unwrap();
    common::package_approve(&s, "resource-poll", "1", text(&installed, "digest"))
        .await
        .unwrap();
    let saved = common::save_plugin(
        &s,
        "resource-poll",
        &json!({"name":"Private","enabled":true}),
        None,
    )
    .await
    .unwrap();
    let origin_token = common::relay_fixture::grant(&s, &["read"]).await;
    let origin_digest = cairn_installation::auth::digest(&origin_token);
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let (_, started) = node_management_call(
        &app,
        "/mcp",
        &origin_token,
        "list_plugin_resources",
        json!({"pluginId":"resource-poll","instanceId":saved["id"],"resourceType":"repository"}),
    )
    .await;
    let id = text(result(&started), "operation").to_owned();
    let completed = finish_plugin_resource_call(&s, "/mcp", &origin_token, started).await;
    assert_eq!(result(&completed)["items"], json!([]));

    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let armed = Arc::new(AtomicBool::new(false));
    let gateway = Router::new().route(
        "/api/relay/00000000-0000-4000-8000-000000000055/plugin-mcp-authorization",
        post({
            let s = s.clone();
            let entered = entered.clone();
            let release = release.clone();
            let armed = armed.clone();
            move |Json(input): Json<Value>| {
                let s = s.clone();
                let entered = entered.clone();
                let release = release.clone();
                let armed = armed.clone();
                let origin_digest = origin_digest.clone();
                async move {
                    if input["credentialDigest"] == origin_digest
                        && armed.swap(false, Ordering::SeqCst)
                    {
                        entered.notify_one();
                        release.notified().await;
                    }
                    let current = s
                        .store
                        .kv(&format!("test-relay:{}", text(&input, "credentialDigest")))
                        .await
                        .unwrap();
                    Json(current.is_some_and(|current| {
                        current["scopes"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|scope| scope == &input["scope"])
                    }))
                }
            }
        }),
    );
    let (origin, server) = common::serve_locally(gateway).await;
    let identity_path = s.config.data_dir.join("installation-relay/identity.json");
    let mut identity: Value =
        serde_json::from_slice(&tokio::fs::read(&identity_path).await.unwrap()).unwrap();
    identity["origin"] = json!(origin);
    tokio::fs::write(&identity_path, serde_json::to_vec(&identity).unwrap())
        .await
        .unwrap();

    for (tool, revoke_all) in [
        ("get_plugin_resource_operation", true),
        ("get_plugin_operation", true),
        ("get_plugin_operation", false),
    ] {
        let token = common::relay_fixture::grant(&s, &["read", "manage"]).await;
        armed.store(true, Ordering::SeqCst);
        let poll = tokio::spawn({
            let app = app.clone();
            let token = token.clone();
            let id = id.clone();
            async move {
                node_management_call(&app, "/mcp", &token, tool, json!({"operationId":id})).await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified())
            .await
            .unwrap();
        let key = format!("test-relay:{}", cairn_installation::auth::digest(&token));
        if revoke_all {
            s.store.delete(&key).await.unwrap();
        } else {
            // The old manage poller must also lose resource access when only
            // read is removed while its originating authority is checked.
            s.store
                .set(&key, json!({"scopes":["manage"]}), None)
                .await
                .unwrap();
        }
        release.notify_one();
        let (status, response) = tokio::time::timeout(std::time::Duration::from_secs(10), poll)
            .await
            .unwrap()
            .unwrap();
        assert!(
            status == StatusCode::UNAUTHORIZED || response["result"]["isError"] == true,
            "{tool}: {response}"
        );
    }
    server.abort();
    s.shutdown.cancel();
}

#[tokio::test]
async fn resource_poll_fences_package_activation_during_remote_authorization() {
    use axum::{Json, Router, routing::post};
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Notify;

    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    s.plugins
        .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    for (version, label) in [("1", "Old"), ("2", "New")] {
        let source = r#"return {resources = function()
            return {items = cairn.array({{resourceType = "repository", id = "repo", name = "LABEL"}})}
        end}"#.replace("LABEL", label);
        let installed = common::package_install(&s, &json!({
            "manifest":{"id":"resource-package","name":"Resource package","version":version,"apiVersion":2,
                "capabilities":[],"agentAccess":{},"resources":[{"name":"repository","description":"Repository"}]},
            "source":source,
        })).await.unwrap();
        common::package_approve(&s, "resource-package", version, text(&installed, "digest"))
            .await
            .unwrap();
    }
    cairn_installation::plugins::registry::activate(&s, "resource-package", "1")
        .await
        .unwrap();
    let saved = common::save_plugin(
        &s,
        "resource-package",
        &json!({"name":"Private","enabled":true}),
        None,
    )
    .await
    .unwrap();
    let origin_token = common::relay_fixture::grant(&s, &["read", "manage"]).await;
    let origin_digest = cairn_installation::auth::digest(&origin_token);
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let (_, started) = node_management_call(
        &app,
        "/mcp",
        &origin_token,
        "list_plugin_resources",
        json!({"pluginId":"resource-package","instanceId":saved["id"],"resourceType":"repository"}),
    )
    .await;
    let id = text(result(&started), "operation").to_owned();
    let completed = finish_plugin_resource_call(&s, "/mcp", &origin_token, started).await;
    assert_eq!(result(&completed)["items"][0]["name"], "Old");
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let armed = Arc::new(AtomicBool::new(false));
    let gateway = Router::new().route(
        "/api/relay/00000000-0000-4000-8000-000000000055/plugin-mcp-authorization",
        post({
            let s = s.clone();
            let entered = entered.clone();
            let release = release.clone();
            let armed = armed.clone();
            move |Json(input): Json<Value>| {
                let s = s.clone();
                let entered = entered.clone();
                let release = release.clone();
                let armed = armed.clone();
                let origin_digest = origin_digest.clone();
                async move {
                    if input["credentialDigest"] == origin_digest
                        && armed.swap(false, Ordering::SeqCst)
                    {
                        entered.notify_one();
                        release.notified().await;
                    }
                    let current = s
                        .store
                        .kv(&format!("test-relay:{}", text(&input, "credentialDigest")))
                        .await
                        .unwrap();
                    Json(current.is_some_and(|current| {
                        current["scopes"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|scope| scope == &input["scope"])
                    }))
                }
            }
        }),
    );
    let (origin, server) = common::serve_locally(gateway).await;
    let identity_path = s.config.data_dir.join("installation-relay/identity.json");
    let mut identity: Value =
        serde_json::from_slice(&tokio::fs::read(&identity_path).await.unwrap()).unwrap();
    identity["origin"] = json!(origin);
    tokio::fs::write(&identity_path, serde_json::to_vec(&identity).unwrap())
        .await
        .unwrap();

    for tool in ["get_plugin_resource_operation", "get_plugin_operation"] {
        cairn_installation::plugins::registry::activate(&s, "resource-package", "1")
            .await
            .unwrap();
        armed.store(true, Ordering::SeqCst);
        let poll = tokio::spawn({
            let app = app.clone();
            let token = origin_token.clone();
            let id = id.clone();
            async move {
                node_management_call(&app, "/mcp", &token, tool, json!({"operationId":id})).await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified())
            .await
            .unwrap();
        cairn_installation::plugins::registry::activate(&s, "resource-package", "2")
            .await
            .unwrap();
        let current = s
            .store
            .plugin_instance(text(&saved, "id"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current.revision, text(&saved, "revision"));
        release.notify_one();
        let (_, response) = tokio::time::timeout(std::time::Duration::from_secs(10), poll)
            .await
            .unwrap()
            .unwrap();
        if response["result"]["isError"] != true {
            server.abort();
            s.shutdown.cancel();
            panic!("{tool} returned the old package result: {response}");
        }
    }
    server.abort();
    s.shutdown.cancel();
}

#[tokio::test]
async fn placement_capabilities_share_their_commands_scopes_and_live_installation_grant() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (run, configuration) = active_run(&s, &main).await;
    let agent_token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let owner_token = common::relay_fixture::grant(&s, &["read", "manage", "run"]).await;
    let read_token = common::relay_fixture::grant(&s, &["read"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    s.store
        .put_node_fixture(
            json!({"id":cairn_installation::nodes::LOCAL_NODE_ID,"name":"Placement fixture"}),
        )
        .await
        .unwrap();
    for (endpoint, token) in [
        ("/mcp", owner_token.as_str()),
        ("/mcp-installation", agent_token),
    ] {
        let (_, refused) = node_management_call(
            &app,
            endpoint,
            token,
            "move_to_node",
            json!({ "runId": run["id"], "nodeId": cairn_installation::nodes::LOCAL_NODE_ID }),
        )
        .await;
        assert_eq!(refused["result"]["isError"], true, "{refused}");
        assert!(
            refused
                .to_string()
                .contains(cairn_installation::environment_expiration::WARNING),
            "{refused}"
        );
        assert!(
            refused.to_string().contains("discardWorkingFiles: true"),
            "{refused}"
        );
        let (_, view) = node_management_call(
            &app,
            endpoint,
            token,
            "get_conversation_placement",
            json!({"runId":run["id"]}),
        )
        .await;
        assert!(
            result(&view)["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|node| node["id"] == cairn_installation::nodes::LOCAL_NODE_ID)
        );
        let (_,saved)=node_management_call(&app,endpoint,token,"configure_conversation_placement",json!({"runId":run["id"],"selection":{"pinnedNodeId":null,"preferredNodeId":cairn_installation::nodes::LOCAL_NODE_ID}})).await;
        assert_eq!(
            result(&saved)["preferredNodeId"],
            cairn_installation::nodes::LOCAL_NODE_ID
        );
        let (_, missing) = node_management_call(
            &app,
            endpoint,
            token,
            "release_node_reservation",
            json!({"reservationId":uuid::Uuid::new_v4()}),
        )
        .await;
        assert_eq!(missing["result"]["isError"], true);
    }
    for (name, args) in [
        (
            "configure_conversation_placement",
            json!({"runId":run["id"],"selection":{"pinnedNodeId":null,"preferredNodeId":null}}),
        ),
        (
            "move_to_node",
            json!({"runId":run["id"],"nodeId":cairn_installation::nodes::LOCAL_NODE_ID}),
        ),
        (
            "release_node_reservation",
            json!({"reservationId":uuid::Uuid::new_v4()}),
        ),
    ] {
        let (_, denied) = node_management_call(&app, "/mcp", &read_token, name, args).await;
        assert_eq!(denied["result"]["isError"], true, "{denied}");
    }
    s.agent_fixture(
        json!({"access":{"installation":false}}),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let (status, denied) = node_management_call(
        &app,
        "/mcp-installation",
        agent_token,
        "configure_conversation_placement",
        json!({"runId":run["id"],"selection":{"pinnedNodeId":null,"preferredNodeId":null}}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{denied}");
    assert!(denied["error"].is_string());
    assert_eq!(
        s.store.run(run["id"].as_str().unwrap()).await.unwrap()["preferredNodeId"],
        cairn_installation::nodes::LOCAL_NODE_ID
    );
}

#[tokio::test]
async fn mission_deliverables_work_without_a_conversation_on_both_management_tools() {
    for internal in [false, true] {
        let root = TempDir::new().unwrap();
        let s = service(&root).await;
        let main = enabled_main(&s).await;
        let (caller, configuration) = active_run(&s, &main).await;
        let (target, _) = active_run(&s, &main).await;
        let run = text(&target, "id");
        assert!(
            s.store
                .read({
                    let run = run.to_owned();
                    move |db| db.conversation_for_run(&run)
                })
                .await
                .unwrap()
                .is_none()
        );
        let id = uuid::Uuid::new_v4().to_string();
        let bytes = (0..220_000).map(|i| (i % 251) as u8).collect::<Vec<_>>();
        let digest = {
            use sha2::Digest;
            hex::encode(sha2::Sha256::digest(&bytes))
        };
        let item = common::deliverable(
            run,
            &id,
            &json!({
                "size": bytes.len(),
                "digest": digest,
            }),
        );
        let second_id = uuid::Uuid::new_v4().to_string();
        let second = common::deliverable(
            run,
            &second_id,
            &json!({
                "version": 2,
                "size": bytes.len(),
                "digest": digest,
            }),
        );
        s.store
            .transaction(move |db| {
                db.insert_artifact(&item)?;
                db.insert_artifact(&second)
            })
            .await
            .unwrap();
        tokio::fs::create_dir_all(s.config.data_dir.join("artifacts"))
            .await
            .unwrap();
        tokio::fs::write(s.config.data_dir.join("artifacts").join(&id), &bytes)
            .await
            .unwrap();
        let invoke = async |name: &str, args: Value| {
            if internal {
                call(&s, &configuration, name, args).await
            } else {
                public_call(&s, &["read", "manage"], name, args).await
            }
        };
        let first = invoke(
            "list_run_artifacts",
            json!({
                "runId": run,
                "limit": 1,
            }),
        )
        .await;
        assert_eq!(result(&first)["artifacts"].as_array().unwrap().len(), 1);
        assert_eq!(result(&first)["hasMore"], true);
        let second = invoke(
            "list_run_artifacts",
            json!({
                "runId": run,
                "limit": 1,
                "offset": result(&first)["nextOffset"],
            }),
        )
        .await;
        assert_eq!(result(&second)["hasMore"], false);
        assert_ne!(
            result(&first)["artifacts"][0]["id"],
            result(&second)["artifacts"][0]["id"]
        );
        for invalid in [
            json!({
                "runId": run,
                "limit": 0,
            }),
            json!({
                "runId": run,
                "offset": 9007199254740992_u64,
            }),
        ] {
            let denied = invoke("list_run_artifacts", invalid).await;
            assert_eq!(denied["result"]["isError"], true);
        }
        let wrong = invoke(
            "read_run_artifact",
            json!({
                "runId": caller["id"],
                "artifactId": id,
            }),
        )
        .await;
        assert_eq!(wrong["result"]["isError"], true);
        let mut received = Vec::new();
        let mut offset = 0;
        loop {
            let response = invoke(
                "read_run_artifact",
                json!({
                    "runId": run,
                    "artifactId": id,
                    "offset": offset,
                }),
            )
            .await;
            let chunk = result(&response);
            assert_eq!(chunk["file"]["digest"], digest);
            let data = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                chunk["data"].as_str().unwrap(),
            )
            .unwrap();
            assert!(data.len() <= 192 * 1024);
            received.extend(data);
            let Some(next) = chunk["nextOffset"].as_u64() else {
                break;
            };
            offset = next;
        }
        assert_eq!(received, bytes);
        let denied = public_call(
            &s,
            &["read"],
            "set_run_artifact_visibility",
            json!({
                "runId": run,
                "artifactId": id,
                "visibility": "public",
            }),
        )
        .await;
        assert_eq!(denied["result"]["isError"], true);
        let shared = invoke(
            "set_run_artifact_visibility",
            json!({
                "runId": run,
                "artifactId": id,
                "visibility": "public",
            }),
        )
        .await;
        let token = result(&shared)["publicToken"].as_str().unwrap().to_owned();
        assert!(
            result(&shared)["publicUrl"]
                .as_str()
                .unwrap()
                .contains(&token)
        );
        let private = invoke(
            "set_run_artifact_visibility",
            json!({
                "runId": run,
                "artifactId": id,
                "visibility": "private",
            }),
        )
        .await;
        assert!(result(&private)["publicUrl"].is_null());
        assert!(
            s.store
                .read(move |db| db.public_artifact(&token))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            s.store
                .read({
                    let run = run.to_owned();
                    move |db| db.conversation_for_run(&run)
                })
                .await
                .unwrap()
                .is_none()
        );
        // Revoking the execution's grant removes access even to another mission's files.
        if internal {
            s.mcps.revoke_run(&s, text(&caller, "id")).await.unwrap();
            let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
                .as_str()
                .unwrap();
            let (status, _) = request(
                &s,
                token,
                "tools/call",
                common::mcp_arguments(
                    "list_run_artifacts",
                    &json!({
                        "runId": run,
                    }),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }
    }
}

#[tokio::test]
async fn chat_plan_settings_have_http_and_both_mcp_parity_with_live_authorization() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let agent_token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let manage_token = common::relay_fixture::grant(&s, &["read", "manage"]).await;
    let read_token = common::relay_fixture::grant(&s, &["read"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let owner = common::RelayContext::new(&common::relay_fixture::context(&s).await);
    let member =
        common::RelayContext::new(&common::relay_fixture::member_context(&s, "plan-member").await);
    for method in ["GET", "PUT"] {
        let denied = member
            .authorize(common::request(method, "/api/settings/chat-plan"))
            .body(Body::from(
                json!({ "enabled": true, "revision": "default" }).to_string(),
            ))
            .unwrap();
        assert_eq!(
            common::send(&app, denied).await.status(),
            StatusCode::FORBIDDEN
        );
    }
    let read = owner
        .authorize(common::request("GET", "/api/settings/chat-plan"))
        .body(Body::empty())
        .unwrap();
    let response = common::send(&app, read).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut current = common::read_json(response).await;
    assert_eq!(current, json!({ "enabled": false, "revision": "default" }));
    for (endpoint, token, enabled) in [
        ("/mcp", manage_token.as_str(), true),
        ("/mcp-installation", agent_token, false),
    ] {
        let (_, view) =
            node_management_call(&app, endpoint, token, "get_chat_plan_settings", json!({})).await;
        assert_eq!(result(&view), &current);
        let command = json!({ "enabled": enabled, "revision": current["revision"] });
        let (_, saved) = node_management_call(
            &app,
            endpoint,
            token,
            "save_chat_plan_settings",
            command.clone(),
        )
        .await;
        current = result(&saved).clone();
        assert_eq!(current["enabled"], enabled);
        let (_, stale) =
            node_management_call(&app, endpoint, token, "save_chat_plan_settings", command).await;
        assert_eq!(stale["result"]["isError"], true);
    }
    let write = owner
        .authorize(common::request("PUT", "/api/settings/chat-plan"))
        .body(Body::from(
            json!({ "enabled": true, "revision": current["revision"] }).to_string(),
        ))
        .unwrap();
    let response = common::send(&app, write).await;
    assert_eq!(response.status(), StatusCode::OK);
    current = common::read_json(response).await;
    let (_, denied) = node_management_call(
        &app,
        "/mcp",
        &read_token,
        "save_chat_plan_settings",
        json!({ "enabled": false, "revision": current["revision"] }),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true);
    let (_, unchanged) = node_management_call(
        &app,
        "/mcp",
        &manage_token,
        "get_chat_plan_settings",
        json!({}),
    )
    .await;
    assert_eq!(result(&unchanged), &current);
    s.agent_fixture(
        json!({ "access": { "installation": false } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let (status, _) = node_management_call(
        &app,
        "/mcp-installation",
        agent_token,
        "get_chat_plan_settings",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn installation_tools_manage_expiring_conversation_links() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let chat = s.chat_create(json!({})).await.unwrap();
    let input = json!({ "chatId": chat["id"] });
    let created = call(&s, &configuration, "set_chat_share", input.clone()).await;
    assert!(created["result"]["isError"] != true, "{created}");
    let share = result(&created);
    assert!(
        share["publicUrl"]
            .as_str()
            .unwrap()
            .contains("/conversations/")
    );
    let read = call(&s, &configuration, "get_chat_share", input.clone()).await;
    assert_eq!(result(&read)["token"], share["token"]);
    let revoked = call(&s, &configuration, "revoke_chat_share", input.clone()).await;
    assert!(result(&revoked)["publicUrl"].is_null());
    let read = call(&s, &configuration, "get_chat_share", input).await;
    assert!(result(&read)["publicUrl"].is_null());
}
