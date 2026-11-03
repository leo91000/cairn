use super::*;
use crate::store::Store;

fn target(agent_id: Option<AgentId>) -> CodingAgentConfigurationInput {
    CodingAgentConfigurationInput {
        provider: AgentProvider::Codex,
        agent_id,
    }
}

fn change(
    db: &Db<'_>,
    target: CodingAgentConfigurationInput,
    document: &str,
    changes: Vec<CodingAgentSettingChange>,
) -> Result<CodingAgentConfiguration> {
    let current = view(db, target)?;
    edit(
        db,
        &EditCodingAgentConfigurationInput {
            provider: target.provider,
            agent_id: target.agent_id,
            revision: current.revision,
            document: document.into(),
            changes,
            inherit_paths: vec![],
            reset: false,
        },
        true,
    )
}

fn agent(db: &Db<'_>, provider: &str) -> Result<AgentId> {
    let settings = serde_json::from_value(json!({
        "name": "Native configuration", "provider": provider, "reasoning": "",
        "access": { "backend": "auto", "nodes": [], "projects": [], "skills": [], "mcps": [], "mcpTools": {} },
        "exclusiveNodes": [], "plugins": [],
    }))?;
    let agent = crate::agents::new_agent(settings, None);
    db.save_agent(&agent)?;
    Ok(agent.id)
}

#[test]
fn native_edits_preserve_comments_unknown_options_and_empty_objects() {
    for (provider, source, path, replacement) in [
        (
            Provider::Codex,
            "# keep this comment\nmodel_context_window = 200000 # model capacity\n[experimental_custom]\n",
            "/model_context_window",
            json!(1000000),
        ),
        (
            Provider::Opencode,
            "{\n  // keep this comment\n  \"snapshot\": true,\n  \"custom\": {},\n}\n",
            "/snapshot",
            json!(false),
        ),
    ] {
        let mut desired = parse(provider, source).unwrap();
        set(&mut desired, path, Some(replacement)).unwrap();
        let edited = document::rewrite(provider, source, &desired).unwrap();
        assert!(edited.contains("keep this comment"), "{edited}");
        if provider == Provider::Codex {
            assert!(edited.contains("# model capacity"));
        }
        assert_eq!(parse(provider, &edited).unwrap(), desired);
        assert_eq!(
            document::rewrite(provider, source, &parse(provider, source).unwrap()).unwrap(),
            source
        );
    }
}

#[test]
fn managed_settings_are_refused_before_launch_and_errors_do_not_echo_values() {
    for (provider, value) in [
        (Provider::Codex, json!({ "model_provider": "untrusted" })),
        (
            Provider::Codex,
            json!({ "profiles": { "other": { "approval_policy": "never" } } }),
        ),
        (Provider::Codex, json!({ "features": { "plugins": true } })),
        (
            Provider::Codex,
            json!({ "features": { "default_mode_request_user_input": true } }),
        ),
        (Provider::Codex, json!({ "features": { "hooks": true } })),
        (
            Provider::Codex,
            json!({ "features": { "codex_hooks": true } }),
        ),
        (
            Provider::Claude,
            json!({ "env": { "ANTHROPIC_BASE_URL": "https://untrusted.invalid" } }),
        ),
        (
            Provider::Claude,
            json!({ "env": { "ANTHROPIC_AUTH_TOKEN": "private-test-value" } }),
        ),
        (
            Provider::Claude,
            json!({ "permissions": { "allow": ["*"] } }),
        ),
        (
            Provider::Opencode,
            json!({ "compaction": { "auto": true } }),
        ),
        (Provider::Opencode, json!({ "plugin": ["untrusted"] })),
        (Provider::Opencode, json!({ "autoupdate": true })),
    ] {
        let error = validate(provider, &value).unwrap_err().to_string();
        assert!(!error.contains("private-test-value"));
        assert!(!error.contains("https://untrusted.invalid"));
    }
    assert!(
        validate(
            Provider::Codex,
            &json!({ "model_context_window": 1000000, "model_auto_compact_token_limit": 900000, "features": { "default_mode_request_user_input": false, "hooks": false, "codex_hooks": false } })
        )
        .is_ok()
    );
    assert!(
        validate(
            Provider::Claude,
            &json!({ "env": { "CLAUDE_CODE_MAX_OUTPUT_TOKENS": "64000", "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE": "80", "MAX_THINKING_TOKENS": "32000" } })
        )
        .is_ok()
    );
}

#[tokio::test]
async fn restoring_deleted_objects_clears_descendant_removals_and_keeps_inheritance_live() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store.transaction(|db| {
        let defaults = CodingAgentConfigurationInput { provider: AgentProvider::Claude, agent_id: None };
        for restore_whole_object in [true, false] {
            change(db, defaults, r#"{"env":{"CLAUDE_AUTOCOMPACT_PCT_OVERRIDE":"80","CLAUDE_CODE_MAX_OUTPUT_TOKENS":"32000"}}"#, vec![])?;
            let id = agent(db, "claude")?;
            let target = CodingAgentConfigurationInput { provider: AgentProvider::Claude, agent_id: Some(id) };
            change(db, target, r#"{"env":{"CLAUDE_CODE_MAX_OUTPUT_TOKENS":"32000"}}"#, vec![])?;
            change(db, target, "{}", vec![])?;
            let current = view(db, target)?;
            let input = EditCodingAgentConfigurationInput {
                provider: target.provider,
                agent_id: target.agent_id,
                revision: current.revision,
                document: current.document,
                changes: vec![CodingAgentSettingChange {
                    path: if restore_whole_object { "/env" } else { "/env/CLAUDE_CODE_MAX_OUTPUT_TOKENS" }.into(),
                    value: "null".into(),
                }],
                inherit_paths: vec![],
                reset: false,
            };
            let preview = edit(db, &input, false)?;
            let saved = edit(db, &input, true)?;
            assert_eq!(parse(Provider::Claude, &saved.document)?, parse(Provider::Claude, &preview.document)?);
            let restored = parse(Provider::Claude, &saved.document)?;
            assert_eq!(restored["env"]["CLAUDE_CODE_MAX_OUTPUT_TOKENS"], "32000");
            assert_eq!(restored["env"].get("CLAUDE_AUTOCOMPACT_PCT_OVERRIDE").is_some(), restore_whole_object);
            change(db, defaults, r#"{"env":{"CLAUDE_AUTOCOMPACT_PCT_OVERRIDE":"70","CLAUDE_CODE_MAX_OUTPUT_TOKENS":"64000"}}"#, vec![])?;
            let inherited = parse(Provider::Claude, &view(db, target)?.document)?;
            assert_eq!(inherited["env"]["CLAUDE_CODE_MAX_OUTPUT_TOKENS"], "64000");
            assert_eq!(inherited["env"].get("CLAUDE_AUTOCOMPACT_PCT_OVERRIDE").is_some(), restore_whole_object);
            if restore_whole_object {
                assert_eq!(inherited["env"]["CLAUDE_AUTOCOMPACT_PCT_OVERRIDE"], "70");
            }
        }
        Ok(())
    }).await.unwrap();
}

#[tokio::test]
async fn preview_and_save_agree_when_replacing_nested_native_values() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            for provider in [Provider::Codex, Provider::Claude] {
                let defaults = CodingAgentConfigurationInput {
                    provider: provider.into(),
                    agent_id: None,
                };
                let baseline = json!({ "custom": { "a": 1, "b": { "c": 2 }, "d": {} } });
                change(db, defaults, &render(provider, &baseline)?, vec![])?;
                let id = agent(db, provider.as_str())?;
                let target = CodingAgentConfigurationInput {
                    provider: provider.into(),
                    agent_id: Some(id),
                };
                for desired in [
                    json!({ "custom": { "a": 1, "b": {}, "d": {} } }),
                    json!({}),
                    json!({ "custom": { "a": 1 } }),
                    json!({ "custom": { "b": { "c": 3 } } }),
                    json!({ "custom": {} }),
                    json!({ "custom": { "b": { "c": 2 } } }),
                    json!({ "custom": { "b": 5 } }),
                    json!({ "custom": { "b": { "c": 2 } } }),
                ] {
                    let current = view(db, target)?;
                    let input = EditCodingAgentConfigurationInput {
                        provider: target.provider,
                        agent_id: target.agent_id,
                        revision: current.revision,
                        document: render(provider, &desired)?,
                        changes: vec![],
                        inherit_paths: vec![],
                        reset: false,
                    };
                    assert_eq!(
                        parse(provider, &edit(db, &input, false)?.document)?,
                        desired
                    );
                    assert_eq!(parse(provider, &edit(db, &input, true)?.document)?, desired);
                }
                let empty = json!({ "custom": {} });
                change(db, defaults, &render(provider, &empty)?, vec![])?;
                assert_eq!(parse(provider, &view(db, target)?.document)?, empty);
                change(db, target, &render(provider, &empty)?, vec![])?;
                change(db, defaults, &render(provider, &baseline)?, vec![])?;
                assert_eq!(
                    parse(provider, &view(db, target)?.document)?,
                    json!({ "custom": { "b": { "c": 2 } } })
                );
            }
            Ok(())
        })
        .await
        .unwrap();
}

#[test]
fn native_configuration_cannot_launch_callbacks_or_replace_managed_claude_environment() {
    let mut cases = vec![
        (Provider::Codex, json!({ "notify": ["blocked-callback"] })),
        (
            Provider::Codex,
            json!({
                "hooks": {
                    "SessionStart": [{
                        "matcher": "startup|resume",
                        "hooks": [{
                            "type": "command",
                            "command": "printf hook-proof",
                            "timeout": 600,
                            "async": false
                        }]
                    }],
                    "state": {
                        "/<session-flags>/config.toml:session_start:0:0": {
                            "trusted_hash": "sha256:b4e875f9a217a9060e320cedf9f75751478f4e4a69c7e23f6309cff4796cc1b1"
                        }
                    }
                }
            }),
        ),
        (
            Provider::Claude,
            json!({
                "hooks": {
                    "SessionStart": [{
                        "hooks": [{
                            "type": "command",
                            "command": "blocked-callback"
                        }]
                    }]
                }
            }),
        ),
    ];
    for key in [
        "statusLine",
        "fileSuggestion",
        "awsAuthRefresh",
        "awsCredentialExport",
        "otelHeadersHelper",
        "appendPlugins",
    ] {
        cases.push((Provider::Claude, json!({ key: "blocked-callback" })));
    }
    for key in [
        "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS",
        "CLAUDE_CODE_EFFORT_LEVEL",
        "ANTHROPIC_CUSTOM_HEADERS",
        "NODE_OPTIONS",
        "LD_PRELOAD",
        "HTTPS_PROXY",
        "NODE_TLS_REJECT_UNAUTHORIZED",
    ] {
        cases.push((
            Provider::Claude,
            json!({ "env": { key: "private-test-value" } }),
        ));
    }
    for (provider, value) in cases {
        let document = render(provider, &value).unwrap();
        let mut plan = json!({ "provider": provider });
        let error = apply(
            &mut plan,
            &json!({ "provider": provider, "document": document }),
        )
        .unwrap_err()
        .to_string();
        assert!(!error.contains("private-test-value"));
        assert!(plan.get("nativeSettings").is_none());
    }
}

#[test]
fn opencode_interpolation_is_refused_in_property_names_and_nested_values_before_loading() {
    for value in [
        json!({ "{file:/workspace/option-name}": { "rogue": {} } }),
        json!({ "{env:RUNTIME_CONFIG_KEY}": {} }),
        json!({ "instructions": ["{file:/workspace/private-preference}"] }),
        json!({ "experimental": { "preference": "prefix {env:RUNTIME_CONFIG_VALUE} suffix" } }),
    ] {
        let mut plan = json!({ "provider": "opencode" });
        let document = render(Provider::Opencode, &value).unwrap();
        assert!(
            apply(
                &mut plan,
                &json!({ "provider": "opencode", "document": document })
            )
            .is_err()
        );
        assert!(plan.get("nativeSettings").is_none());
    }
    assert!(
        validate(
            Provider::Opencode,
            &json!({ "snapshot": false, "compaction": { "reserved": 10000 } })
        )
        .is_ok()
    );
}

#[test]
fn native_codex_arguments_are_real_toml_and_keep_generated_auth_separate() {
    let native = json!({ "model_context_window": 1000000, "features": { "multi_agent": true }, "model_verbosity": "high" });
    let args = codex_arguments(&json!({ "nativeSettings": native })).unwrap();
    let mut recovered = json!({});
    for pair in args.chunks_exact(2) {
        assert_eq!(pair[0], "-c");
        let parsed: toml::Value = toml::from_str(&pair[1]).unwrap();
        merge(&mut recovered, &serde_json::to_value(parsed).unwrap());
    }
    assert_eq!(recovered, native);
    assert!(
        !args
            .iter()
            .any(|arg| arg.contains("mcp_servers") || arg.contains("auth"))
    );
}

#[tokio::test]
async fn an_explicit_inheritance_reset_survives_preview_when_the_values_are_equal() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            change(db, target(None), "model = 'model-a'\n", vec![])?;
            let id = agent(db, "codex")?;
            let mut explicit = db.agent(&id.to_string())?.unwrap();
            explicit.settings.model = "model-a".into();
            db.save_agent(&explicit)?;
            let current = view(db, target(Some(id)))?;
            assert!(current.customized_paths.contains(&"/model".into()));
            let mut command = EditCodingAgentConfigurationInput {
                provider: AgentProvider::Codex,
                agent_id: Some(id),
                revision: current.revision,
                document: current.document,
                changes: vec![CodingAgentSettingChange {
                    path: "/model".into(),
                    value: "null".into(),
                }],
                inherit_paths: vec![],
                reset: false,
            };
            let preview = edit(db, &command, false)?;
            assert!(!preview.customized_paths.contains(&"/model".into()));
            assert_eq!(
                db.agent(&id.to_string())?.unwrap().settings.model,
                "model-a"
            );
            command.document = preview.document;
            command.changes.clear();
            command.inherit_paths = vec!["/model".into()];
            edit(db, &command, true)?;
            assert_eq!(db.agent(&id.to_string())?.unwrap().settings.model, "");
            change(db, target(None), "model = 'model-b'\n", vec![])?;
            assert_eq!(
                parse(Provider::Codex, &view(db, target(Some(id)))?.document)?["model"],
                "model-b"
            );
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn another_engine_keeps_its_model_until_its_profile_becomes_current() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            let id = agent(db, "codex")?;
            let target = CodingAgentConfigurationInput {
                provider: AgentProvider::Claude,
                agent_id: Some(id),
            };
            let current = view(db, target)?;
            let command = EditCodingAgentConfigurationInput {
                provider: target.provider,
                agent_id: target.agent_id,
                revision: current.revision,
                document: "{\"model\":\"opus\",\"language\":\"French\"}".into(),
                changes: vec![],
                inherit_paths: vec![],
                reset: false,
            };
            edit(db, &command, true)?;
            assert_eq!(
                parse(Provider::Claude, &view(db, target)?.document)?["model"],
                "opus"
            );
            let mut profile = db.agent(&id.to_string())?.unwrap();
            assert_eq!(profile.settings.provider, AgentProvider::Codex);
            profile.settings.provider = AgentProvider::Claude;
            db.save_agent(&profile)?;
            let current = view(db, target)?;
            let command = EditCodingAgentConfigurationInput {
                revision: current.revision,
                document: current.document,
                changes: vec![CodingAgentSettingChange {
                    path: "/language".into(),
                    value: "\"English\"".into(),
                }],
                ..command
            };
            edit(db, &command, true)?;
            assert_eq!(db.agent(&id.to_string())?.unwrap().settings.model, "opus");
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn unrelated_edits_do_not_pin_inherited_models_and_reset_keeps_inheritance_live() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            change(
                db,
                target(None),
                "model = 'model-a'\nmodel_context_window = 200000\n",
                vec![],
            )?;
            let id = agent(db, "codex")?;
            let initial = view(db, target(Some(id)))?;
            let updated = change(
                db,
                target(Some(id)),
                &initial.document,
                vec![CodingAgentSettingChange {
                    path: "/model_context_window".into(),
                    value: "1000000".into(),
                }],
            )?;
            assert_eq!(db.agent(&id.to_string())?.unwrap().settings.model, "");
            assert!(!updated.customized_paths.contains(&"/model".into()));
            change(
                db,
                target(None),
                "model = 'model-b'\nmodel_context_window = 300000\n",
                vec![],
            )?;
            let inherited = view(db, target(Some(id)))?;
            let value = parse(Provider::Codex, &inherited.document)?;
            assert_eq!(value["model"], "model-b");
            assert_eq!(value["model_context_window"], 1000000);
            let reset = change(
                db,
                target(Some(id)),
                &inherited.document,
                vec![CodingAgentSettingChange {
                    path: "/model_context_window".into(),
                    value: "null".into(),
                }],
            )?;
            assert!(
                !reset
                    .customized_paths
                    .contains(&"/model_context_window".into())
            );
            change(
                db,
                target(None),
                "model = 'model-c'\nmodel_context_window = 400000\n",
                vec![],
            )?;
            let inherited = parse(Provider::Codex, &view(db, target(Some(id)))?.document)?;
            assert_eq!(inherited["model"], "model-c");
            assert_eq!(inherited["model_context_window"], 400000);
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn mapped_values_have_one_owner_and_native_standard_speed_roundtrips() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            let id = agent(db, "codex")?;
            let initial = view(db, target(Some(id)))?;
            let changed = change(
                db,
                target(Some(id)),
                &initial.document,
                vec![
                    CodingAgentSettingChange {
                        path: "/model".into(),
                        value: "\"custom-model\"".into(),
                    },
                    CodingAgentSettingChange {
                        path: "/service_tier".into(),
                        value: "\"default\"".into(),
                    },
                ],
            )?;
            let saved = db.agent(&id.to_string())?.unwrap();
            assert_eq!(saved.settings.model, "custom-model");
            assert_eq!(saved.settings.speed, "standard");
            assert!(changed.customized_paths.contains(&"/model".into()));
            assert_eq!(
                parse(Provider::Codex, &changed.document)?["service_tier"],
                "default"
            );
            assert!(
                record(db, Provider::Codex, Some(id))?
                    .value
                    .get("model")
                    .is_none()
            );
            let reset = change(
                db,
                target(Some(id)),
                &changed.document,
                vec![CodingAgentSettingChange {
                    path: "/model".into(),
                    value: "null".into(),
                }],
            )?;
            assert_eq!(db.agent(&id.to_string())?.unwrap().settings.model, "");
            assert!(!reset.customized_paths.contains(&"/model".into()));
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn default_changes_invalidate_drafts_and_frozen_turns_keep_their_configuration() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store.transaction(|db| {
        let id = agent(db, "codex")?;
        change(db, target(None), "model_context_window = 200000\n", vec![])?;
        let draft = view(db, target(Some(id)))?;
        let frozen = snapshot(db, Provider::Codex, &id.to_string())?;
        change(db, target(None), "model_context_window = 1000000\n", vec![])?;
        let stale = EditCodingAgentConfigurationInput { provider: AgentProvider::Codex, agent_id: Some(id), revision: draft.revision, document: draft.document, changes: vec![], inherit_paths: vec![], reset: false };
        assert!(edit(db, &stale, true).is_err());
        let mut plan = json!({ "provider": "codex", "codexConfig": { "mcp_servers": { "cairn": { "url": "private-run-url" } } } });
        apply(&mut plan, &serde_json::to_value(frozen)?)?;
        assert_eq!(plan["codexConfig"]["model_context_window"], 200000);
        assert_eq!(plan["codexConfig"]["mcp_servers"]["cairn"]["url"], "private-run-url");
        let next = snapshot(db, Provider::Codex, &id.to_string())?;
        let mut next_plan = json!({ "provider": "codex", "codexConfig": { "mcp_servers": {} } });
        apply(&mut next_plan, &serde_json::to_value(next)?)?;
        assert_eq!(next_plan["codexConfig"]["model_context_window"], 1000000);
        assert_eq!(plan["codexConfig"]["model_context_window"], 200000);
        Ok(())
    }).await.unwrap();
}

#[tokio::test]
async fn native_defaults_apply_to_implicit_messages_and_engine_switches_use_the_new_format() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            let id = agent(db, "codex")?;
            change(
                db,
                target(None),
                "model = 'model-a'\ndeveloper_instructions = 'Installation instructions'\n",
                vec![],
            )?;
            let profile = serde_json::to_value(db.agent(&id.to_string())?.unwrap())?;
            let mut first = json!({ "snapshot": { "agent": profile } });
            freeze_message(
                db,
                &mut first,
                None,
                &json!({ "model": "", "reasoning": "", "speed": "" }),
            )?;
            assert_eq!(first["snapshot"]["agent"]["model"], "model-a");
            first["snapshot"]["agent"]["reasoning"] = "ultra".into();
            assert_eq!(
                first["snapshot"]["agent"]["instructions"],
                "Installation instructions"
            );
            change(
                db,
                target(None),
                "model = 'model-b'\ndeveloper_instructions = 'Updated instructions'\n",
                vec![],
            )?;
            let mut next = json!({ "snapshot": { "agent": profile } });
            freeze_message(
                db,
                &mut next,
                Some(&first),
                &json!({ "model": "", "reasoning": "", "speed": "" }),
            )?;
            assert_eq!(next["snapshot"]["agent"]["model"], "model-b");
            assert_eq!(next["snapshot"]["agent"]["reasoning"], "");
            assert_eq!(
                next["snapshot"]["agent"]["instructions"],
                "Updated instructions"
            );
            let mut explicit = json!({ "snapshot": { "agent": profile } });
            explicit["snapshot"]["agent"]["model"] = "message-model".into();
            freeze_message(
                db,
                &mut explicit,
                Some(&first),
                &json!({ "model": "message-model" }),
            )?;
            assert_eq!(explicit["snapshot"]["agent"]["model"], "message-model");
            let mut switched = json!({ "snapshot": { "agent": profile } });
            switched["snapshot"]["agent"]["provider"] = "claude".into();
            freeze_message(db, &mut switched, Some(&first), &json!({}))?;
            assert_eq!(
                switched["snapshot"]["nativeConfiguration"]["provider"],
                "claude"
            );
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn raw_deletions_and_unknown_empty_values_survive_saves_and_parent_updates() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            let id = agent(db, "codex")?;
            change(db, target(None), "web_search = 'live'\n[custom]\n", vec![])?;
            let initial = view(db, target(Some(id)))?;
            let mut value = parse(Provider::Codex, &initial.document)?;
            set(&mut value, "/web_search", None)?;
            let saved = change(
                db,
                target(Some(id)),
                &render(Provider::Codex, &value)?,
                vec![],
            )?;
            assert_eq!(
                parse(Provider::Codex, &saved.document)?["custom"],
                json!({})
            );
            change(
                db,
                target(None),
                "web_search = 'cached'\n[custom]\n",
                vec![],
            )?;
            let next = parse(Provider::Codex, &view(db, target(Some(id)))?.document)?;
            assert!(next.get("web_search").is_none());
            assert_eq!(next["custom"], json!({}));
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn removed_mapped_defaults_stay_removed_and_can_resume_inheriting() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            let id = agent(db, "codex")?;
            change(db, target(None), "model = 'inherited-model'\n", vec![])?;
            let removed = change(db, target(Some(id)), "", vec![])?;
            assert!(removed.customized_paths.contains(&"/model".into()));
            assert!(
                parse(Provider::Codex, &removed.document)?
                    .get("model")
                    .is_none()
            );
            change(db, target(None), "model = 'changed-model'\n", vec![])?;
            let current = view(db, target(Some(id)))?;
            assert!(
                parse(Provider::Codex, &current.document)?
                    .get("model")
                    .is_none()
            );
            let restored = change(
                db,
                target(Some(id)),
                &current.document,
                vec![CodingAgentSettingChange {
                    path: "/model".into(),
                    value: "null".into(),
                }],
            )?;
            assert_eq!(
                parse(Provider::Codex, &restored.document)?["model"],
                "changed-model"
            );
            assert!(!restored.customized_paths.contains(&"/model".into()));
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn next_message_preferences_match_launch_and_opencode_can_inherit_a_qualified_model() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            let id = agent(db, "codex")?;
            change(
                db,
                target(None),
                "model = 'model-a'\nmodel_reasoning_effort = 'high'\n",
                vec![],
            )?;
            let profile = serde_json::to_value(db.agent(&id.to_string())?.unwrap())?;
            assert_eq!(profile["model"], "");
            assert_eq!(profile["codingAgentDefaults"]["codex"]["model"], "model-a");
            let mut previous = json!({ "snapshot": { "agent": profile } });
            freeze_message(db, &mut previous, None, &json!({}))?;
            // An explicit earlier choice remains until the configured default changes.
            previous["snapshot"]["agent"]["model"] = "earlier-choice".into();
            assert_eq!(
                next_selection(db, &id.to_string(), Some(&previous))?
                    .unwrap()
                    .model,
                "earlier-choice"
            );
            change(
                db,
                target(None),
                "model = 'model-b'\nmodel_reasoning_effort = 'medium'\n",
                vec![],
            )?;
            let preferences = next_selection(db, &id.to_string(), Some(&previous))?.unwrap();
            assert_eq!(preferences.model, "model-b");
            assert_eq!(preferences.reasoning, "medium");
            let mut next = json!({ "snapshot": { "agent": profile } });
            crate::chats::select_agent(
                &mut next,
                Some(&previous),
                &json!({ "provider": "codex" }),
            )?;
            freeze_message(db, &mut next, Some(&previous), &json!({}))?;
            assert_eq!(next["snapshot"]["agent"]["model"], preferences.model);
            let open = CodingAgentConfigurationInput {
                provider: AgentProvider::Opencode,
                agent_id: None,
            };
            change(db, open, "{\"model\":\"provider/model-a\"}", vec![])?;
            let open_id = agent(db, "opencode")?;
            let open = CodingAgentConfigurationInput {
                agent_id: Some(open_id),
                ..open
            };
            let current = view(db, open)?;
            change(
                db,
                open,
                &current.document,
                vec![CodingAgentSettingChange {
                    path: "/model".into(),
                    value: "\"provider/custom\"".into(),
                }],
            )?;
            let current = view(db, open)?;
            change(
                db,
                open,
                &current.document,
                vec![CodingAgentSettingChange {
                    path: "/model".into(),
                    value: "null".into(),
                }],
            )?;
            let inherited = db.agent(&open_id.to_string())?.unwrap();
            assert_eq!(inherited.settings.model, "");
            validate_profile(db, &inherited)?;
            assert_eq!(
                next_selection(db, &open_id.to_string(), None)?
                    .unwrap()
                    .model,
                "provider/model-a"
            );
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn version_twenty_four_upgrade_preserves_profiles_and_reopens_with_native_documents() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    let id = store.transaction(|db| agent(db, "codex")).await.unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(root.path().join("manager.db")).unwrap();
    connection
        .execute_batch("DROP TABLE coding_agent_configurations; PRAGMA user_version=24;")
        .unwrap();
    drop(connection);
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(move |db| {
            assert!(db.agent(&id.to_string())?.is_some());
            change(
                db,
                target(Some(id)),
                "model_context_window = 1000000\n",
                vec![],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    drop(store);
    let store = Store::open(root.path()).unwrap();
    store.read(move |db| {
        assert_eq!(parse(Provider::Codex, &view(db, target(Some(id)))?.document)?["model_context_window"], 1000000);
        Ok(())
    }).await.unwrap();
}

/// Local SQLite costs only; cold means a reopened connection with the OS cache warm.
#[tokio::test]
#[ignore = "local configuration storage measurement; run alone with --ignored --nocapture"]
async fn native_configuration_storage_measurement() {
    use std::time::Instant;
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    let id = store.transaction(|db| agent(db, "codex")).await.unwrap();
    let document = format!(
        "{}model = 'model-a'\nmodel_context_window = 1000000\nmodel_auto_compact_token_limit = 900000\n",
        "# Native preference documentation.\n".repeat(40)
    );
    store
        .transaction(move |db| {
            change(db, target(None), &document, vec![])?;
            let current = view(db, target(Some(id)))?;
            change(
                db,
                target(Some(id)),
                &current.document,
                vec![CodingAgentSettingChange {
                    path: "/model_context_window".into(),
                    value: "800000".into(),
                }],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let cpu_start = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let started = Instant::now();
    let reference = store.read(move |db| {
        let started = Instant::now();
        for _ in 0..1000 {
            let document: String = db.0.query_row("SELECT document FROM coding_agent_configurations WHERE scope='' AND provider='codex'", [], |row| row.get(0))?;
            std::hint::black_box(document);
        }
        Ok(started.elapsed().as_micros())
    }).await.unwrap();
    let warm = store
        .read(move |db| {
            let started = Instant::now();
            for _ in 0..1000 {
                std::hint::black_box(view(db, target(Some(id)))?);
            }
            Ok(started.elapsed().as_micros())
        })
        .await
        .unwrap();
    let writes = Instant::now();
    for index in 0..100 {
        store
            .transaction(move |db| {
                let current = view(db, target(Some(id)))?;
                change(
                    db,
                    target(Some(id)),
                    &current.document,
                    vec![CodingAgentSettingChange {
                        path: "/model_context_window".into(),
                        value: if index % 2 == 0 { "800000" } else { "1000000" }.into(),
                    }],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }
    let write_us = writes.elapsed().as_micros() / 100;
    let bytes = store.read(|db| Ok(db.0.query_row("SELECT sum(length(CAST(document AS BLOB))+length(CAST(presentation AS BLOB))+length(CAST(removed AS BLOB))+length(CAST(revision AS BLOB))) FROM coding_agent_configurations", [], |row| row.get::<_, i64>(0))?)).await.unwrap();
    drop(store);
    let cold = Instant::now();
    for _ in 0..25 {
        let store = Store::open(root.path()).unwrap();
        store
            .read(move |db| {
                std::hint::black_box(view(db, target(Some(id)))?);
                Ok(())
            })
            .await
            .unwrap();
    }
    println!(
        "CONFIGURATION_STORAGE reference_raw_read_us={} warm_effective_read_us={} reopen_and_read_us={} write_us={} encoded_document_bytes={} wall_ms={} cpu_stat_before={:?} cpu_stat_after={:?}",
        reference / 1000,
        warm / 1000,
        cold.elapsed().as_micros() / 25,
        write_us,
        bytes,
        started.elapsed().as_millis(),
        cpu_start,
        std::fs::read_to_string("/proc/self/stat").unwrap_or_default()
    );
}

#[tokio::test]
async fn transferred_conversations_keep_their_selection_until_target_preferences_change() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store.transaction(|db| {
        let source = agent(db, "codex")?;
        let mut profile = db.agent(&source.to_string())?.unwrap();
        profile.settings.model = "source-model".into();
        profile.settings.reasoning = "high".into();
        db.save_agent(&profile)?;
        change(db, target(Some(source)), "model = 'source-model'\nmodel_reasoning_effort = 'high'\nmodel_context_window = 1000000\n", vec![])?;
        let mut previous = json!({ "snapshot": { "agent": profile } });
        freeze_message(db, &mut previous, None, &json!({}))?;
        let destination = agent(db, "codex")?;
        let mut profile = db.agent(&destination.to_string())?.unwrap();
        profile.settings.model = "target-model".into();
        profile.settings.reasoning = "low".into();
        db.save_agent(&profile)?;
        previous["snapshot"]["agent"]["id"] = destination.to_string().into();
        let mut next = json!({ "snapshot": { "agent": profile } });
        crate::chats::select_agent(&mut next, Some(&previous), &json!({}))?;
        freeze_message(db, &mut next, Some(&previous), &json!({}))?;
        assert_eq!(next["snapshot"]["agent"]["model"], "source-model");
        assert_eq!(next["snapshot"]["agent"]["reasoning"], "high");
        assert_eq!(next["snapshot"]["nativeConfiguration"]["agentId"], destination.to_string());
        let current = view(db, target(Some(destination)))?;
        change(db, target(Some(destination)), &current.document, vec![CodingAgentSettingChange { path: "/model".into(), value: "\"updated-target-model\"".into() }])?;
        let previous = next.clone();
        crate::chats::select_agent(&mut next, Some(&previous), &json!({}))?;
        freeze_message(db, &mut next, Some(&previous), &json!({}))?;
        assert_eq!(next["snapshot"]["agent"]["model"], "updated-target-model");
        assert_eq!(next["snapshot"]["agent"]["reasoning"], "low");
        assert!(next_selection(db, &destination.to_string(), Some(&json!({ "snapshot": null })))?.is_none());
        Ok(())
    }).await.unwrap();
}
