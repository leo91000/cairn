use super::*;
use crate::common::run_fixture::DbRunFixture;

#[tokio::test]
async fn legacy_statuses_cannot_satisfy_multiple_application_bindings() {
    for second_app in [json!(456), Value::Null] {
        let f = Fixture::new().await;
        let status = json!({ "__typename": "StatusContext", "context": "CI", "state": "SUCCESS", "isRequired": true });
        let response = json!({
            "pr": pr(&[status], &["CI"]),
            "rules": [{ "type": "required_status_checks", "parameters": {
                "required_status_checks": [{ "context": "CI", "integration_id": second_app }],
            } }],
        });
        state(&f, &response);
        let result = wait_for::schedule(
            &f.s,
            &f.token,
            &json!({
                "conditions": [condition(&f)], "reason": "Ambiguous source", "timeoutSeconds": 420,
            }),
        )
        .await;
        assert_eq!(result.unwrap_err().status, 409);
        assert!(f.s.chat_detail(&f.chat).await.unwrap()["wakeup"].is_null());
        pending(&f);
        f.schedule(condition(&f)).await;
        f.finish().await;
        state(&f, &response);
        conditions::poll_once(&f.s).await.unwrap();
        assert_eq!(
            f.s.chat_detail(&f.chat).await.unwrap()["wakeup"]["observation"]["state"],
            "unavailable"
        );
    }
}

#[tokio::test]
async fn failures_bound_check_details_and_missing_names_with_full_counts() {
    let f = Fixture::new().await;
    let names: Vec<String> = std::iter::once("CI".to_owned())
        .chain((1..500).map(|i| format!("Required {i}")))
        .collect();
    let required: Vec<_> = names.iter().map(String::as_str).collect();
    state(
        &f,
        &json!({ "pr": pr(&[check("CI", "COMPLETED", Some("FAILURE"), true)], &required) }),
    );
    let result = f.schedule(condition(&f)).await;
    let observed = &result["observation"];
    assert_eq!(result["deferred"], false);
    assert_eq!(observed["conclusion"], "failure");
    assert_eq!(observed["requiredCount"], 500);
    assert_eq!(observed["missingCheckCount"], 499);
    assert_eq!(observed["pendingCount"], 499);
    assert_eq!(observed["checksTruncated"], true);
    assert_eq!(
        observed["checks"].as_array().unwrap().len()
            + observed["missingChecks"].as_array().unwrap().len(),
        100
    );
    assert_eq!(observed["checks"][0]["name"], "CI");
}

fn check(name: &str, status: &str, conclusion: Option<&str>, required: bool) -> Value {
    json!({
        "__typename": "CheckRun", "name": name, "status": status,
        "conclusion": conclusion, "isRequired": required,
        "checkSuite": { "commit": { "oid": SHA }, "app": { "databaseId": 123 } },
    })
}

fn pr(nodes: &[Value], required: &[&str]) -> Value {
    json!({
        "number": 7, "headRefOid": SHA, "state": "OPEN", "baseRefName": "main",
        "baseRef": { "branchProtectionRule": {
            "requiresStatusChecks": true,
            "requiredStatusChecks": required.iter().map(|name| json!({ "context": name, "app": { "databaseId": 123 } })).collect::<Vec<_>>(),
        } },
        "commits": { "nodes": [{ "commit": {
            "oid": SHA,
            "statusCheckRollup": { "contexts": {
                "totalCount": nodes.len(), "pageInfo": { "hasNextPage": false, "endCursor": null }, "nodes": nodes,
            } },
        } }] },
    })
}

fn condition(f: &Fixture) -> Value {
    json!({ "type": "github_pr_checks", "projectId": f.project, "prNumber": 7, "headSha": SHA })
}

fn state(f: &Fixture, value: &Value) {
    f.github.state(value);
}

pub(super) fn pending(f: &Fixture) {
    state(
        f,
        &json!({ "pr": pr(&[check("CI", "IN_PROGRESS", None, true)], &["CI"]) }),
    );
}

#[tokio::test]
async fn required_pr_checks_resume_once_without_waiting_for_optional_checks() {
    let f = Fixture::new().await;
    pending(&f);
    let wait = f.schedule(condition(&f)).await;
    assert_eq!(wait["deferred"], true);
    let calls = f.calls().len();
    assert_eq!(f.schedule(condition(&f)).await, wait);
    assert_eq!(f.calls().len(), calls);
    f.finish().await;
    let restarted = f.restart().await;
    restarted.chat_tick(&HashSet::new()).await.unwrap();
    assert_eq!(
        restarted.chat_detail(&f.chat).await.unwrap()["wakeup"],
        wait["wakeup"]
    );
    state(
        &f,
        &json!({ "pr": pr(&[
        check("CI", "COMPLETED", Some("SUCCESS"), true),
        check("Optional", "IN_PROGRESS", None, false),
    ], &["CI"]) }),
    );
    conditions::poll_once(&restarted).await.unwrap();
    let ready = restarted.chat_detail(&f.chat).await.unwrap();
    assert_eq!(ready["wakeup"]["observation"]["conclusion"], "success");
    assert_eq!(ready["wakeup"]["observation"]["requiredCount"], 1);
    restarted.chat_tick(&HashSet::new()).await.unwrap();
    let resumed = restarted.chat_detail(&f.chat).await.unwrap();
    assert!(resumed["wakeup"].is_null());
    assert_eq!(resumed["run"]["sessionId"], "saved-session");
    assert_eq!(resumed["messages"].as_array().unwrap().len(), 1);
    assert!(
        resumed["run"]["chatExecution"]["text"]
            .as_str()
            .unwrap()
            .contains("success")
    );
    let run = f.run.clone();
    let events = restarted
        .store
        .read(move |db| db.events_fixture(&run, 0, 1000))
        .await
        .unwrap();
    assert_eq!(
        events.iter().filter(|e| e["type"] == "chat.wakeup").count(),
        1
    );
}

#[tokio::test]
async fn missing_required_checks_and_wrong_apps_never_look_like_success() {
    for mode in ["classic", "ruleset", "wrong_app", "disabled_classic"] {
        let f = Fixture::new().await;
        let mut source = pr(&[], &["CI"]);
        let mut rules = json!([]);
        if mode == "ruleset" {
            source["baseRef"]["branchProtectionRule"] = Value::Null;
            rules = json!([{ "type": "required_status_checks", "parameters": {
                "required_status_checks": [{ "context": "CI", "integration_id": 123 }],
            } }]);
        } else if mode == "wrong_app" {
            let mut node = check("CI", "COMPLETED", Some("SUCCESS"), false);
            node["checkSuite"]["app"]["databaseId"] = 456.into();
            source = pr(&[node], &["CI"]);
        } else if mode == "disabled_classic" {
            source["baseRef"]["branchProtectionRule"]["requiresStatusChecks"] = false.into();
        }
        state(&f, &json!({ "pr": source, "rules": rules }));
        let result = f.schedule(condition(&f)).await;
        if mode == "disabled_classic" {
            assert_eq!(result["deferred"], false);
            assert_eq!(result["observation"]["conclusion"], "no_required_checks");
        } else {
            assert_eq!(result["deferred"], true, "{mode}: {result}");
            f.finish().await;
            conditions::poll_once(&f.s).await.unwrap();
            assert!(f.tick().await["wakeup"]["observation"].is_null());
        }
    }
}

#[tokio::test]
async fn required_failures_and_cancellations_wake_even_when_other_checks_are_missing() {
    for (conclusion, expected) in [
        ("FAILURE", "failure"),
        ("CANCELLED", "cancelled"),
        ("TIMED_OUT", "failure"),
        ("NEUTRAL", "success"),
        ("SKIPPED", "success"),
    ] {
        let f = Fixture::new().await;
        let required = if expected == "success" {
            vec!["CI"]
        } else {
            vec!["CI", "Not started"]
        };
        state(
            &f,
            &json!({ "pr": pr(&[check("CI", "COMPLETED", Some(conclusion), true)], &required) }),
        );
        let result = f.schedule(condition(&f)).await;
        assert_eq!(result["deferred"], false);
        assert_eq!(result["observation"]["conclusion"], expected);
        assert!(f.s.chat_detail(&f.chat).await.unwrap()["wakeup"].is_null());
    }
}

#[tokio::test]
async fn legacy_status_contexts_follow_github_required_and_pending_states() {
    for (status, ready) in [
        ("EXPECTED", false),
        ("PENDING", false),
        ("SUCCESS", true),
        ("ERROR", true),
    ] {
        let f = Fixture::new().await;
        let context = json!({ "__typename": "StatusContext", "context": "CI", "state": status, "isRequired": true });
        state(&f, &json!({ "pr": pr(&[context], &["CI"]) }));
        let result = f.schedule(condition(&f)).await;
        assert_eq!(result["deferred"], !ready);
        if ready {
            assert_eq!(
                result["observation"]["conclusion"],
                if status == "SUCCESS" {
                    "success"
                } else {
                    "failure"
                }
            );
        }
    }
}

#[tokio::test]
async fn pushes_and_closure_are_detected_during_the_final_identity_read_and_after_restart() {
    for action in ["push", "closed", "merged"] {
        let f = Fixture::new().await;
        let initial = pr(&[check("CI", "COMPLETED", Some("SUCCESS"), true)], &["CI"]);
        let mut current = initial.clone();
        let expected = if action == "push" {
            current["headRefOid"] = "2222222222222222222222222222222222222222".into();
            "superseded"
        } else {
            current["state"] = action.to_ascii_uppercase().into();
            "closed"
        };
        state(&f, &json!({ "pr": initial, "finalPr": current }));
        let result = f.schedule(condition(&f)).await;
        assert_eq!(result["deferred"], false);
        assert_eq!(result["observation"]["state"], expected);
        pending(&f);
        f.schedule(condition(&f)).await;
        f.finish().await;
        state(&f, &json!({ "pr": current }));
        let restarted = f.restart().await;
        conditions::poll_once(&restarted).await.unwrap();
        assert_eq!(
            restarted.chat_detail(&f.chat).await.unwrap()["wakeup"]["observation"]["state"],
            expected
        );
        restarted.chat_tick(&HashSet::new()).await.unwrap();
        assert!(restarted.chat_detail(&f.chat).await.unwrap()["wakeup"].is_null());
    }
}

#[tokio::test]
async fn check_pagination_and_limits_preserve_complete_head_bound_results() {
    let f = Fixture::new().await;
    let first = vec![check("Optional", "COMPLETED", Some("SUCCESS"), false); 100];
    let mut initial = pr(&first, &["CI"]);
    let contexts = &mut initial["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"];
    contexts["totalCount"] = 101.into();
    contexts["pageInfo"] = json!({ "hasNextPage": true, "endCursor": "page-two" });
    let mut next = pr(&[check("CI", "COMPLETED", Some("SUCCESS"), true)], &["CI"]);
    next["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]["totalCount"] =
        101.into();
    state(&f, &json!({ "pr": initial, "pages": { "page-two": next } }));
    assert_eq!(
        f.schedule(condition(&f)).await["observation"]["conclusion"],
        "success"
    );
    assert!(
        f.calls()
            .iter()
            .any(|c| c["body"]["variables"]["cursor"] == "page-two")
    );
    initial["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]["totalCount"] =
        501.into();
    state(&f, &json!({ "pr": initial }));
    let result = wait_for::schedule(
        &f.s,
        &f.token,
        &json!({ "conditions": [condition(&f)], "reason": "More", "timeoutSeconds": 420 }),
    )
    .await;
    assert_eq!(result.unwrap_err().status, 409);
}

#[tokio::test]
async fn changed_rules_during_observation_stay_pending_and_branch_names_are_path_encoded() {
    let f = Fixture::new().await;
    let mut source = pr(&[check("CI", "COMPLETED", Some("SUCCESS"), true)], &["CI"]);
    source["baseRefName"] = "release/v1".into();
    state(
        &f,
        &json!({ "pr": source, "rulesAfter": [{ "type": "required_status_checks", "parameters": {
        "required_status_checks": [{ "context": "New check", "integration_id": null }],
    } }] }),
    );
    assert_eq!(f.schedule(condition(&f)).await["deferred"], true);
    assert!(f.calls().iter().any(|c| {
        c["path"]
            .as_str()
            .is_some_and(|a| a.contains("/release%2Fv1?"))
    }));
}

#[tokio::test]
async fn incomplete_graphql_results_never_expose_errors_or_schedule_success() {
    let f = Fixture::new().await;
    state(&f, &json!({ "pr": pr(&[], &[]), "graphqlError": true }));
    let result = wait_for::schedule(
        &f.s,
        &f.token,
        &json!({ "conditions": [condition(&f)], "reason": "Errors", "timeoutSeconds": 420 }),
    )
    .await;
    let error = result.unwrap_err();
    assert_eq!(error.status, 502);
    assert!(!error.message.contains("fixture-secret"));
    assert!(f.s.chat_detail(&f.chat).await.unwrap()["wakeup"].is_null());
}

#[tokio::test]
async fn pr_waits_keep_cancellation_permission_and_deadline_contracts() {
    for action in ["message", "permission", "deadline"] {
        let f = Fixture::new().await;
        pending(&f);
        f.schedule(condition(&f)).await;
        f.finish().await;
        match action {
            "message" => {
                f.s.chat_send(&f.chat, json!({ "id": id(), "text": "Change of plan" }))
                    .await
                    .unwrap();
            }
            "permission" => {
                connections::set_enabled(
                    &f.s,
                    "github",
                    &f.instance,
                    &cairn_contracts::PluginEnabledInput { enabled: false },
                )
                .await
                .unwrap();
            }
            "deadline" => {
                f.due().await;
            }
            _ => unreachable!(),
        }
        let calls = f.calls().len();
        conditions::poll_once(&f.s).await.unwrap();
        assert_eq!(f.calls().len(), calls);
        if action == "deadline" {
            let detail = f.tick().await;
            assert!(detail["wakeup"].is_null());
            assert!(
                detail["run"]["chatExecution"]["text"]
                    .as_str()
                    .unwrap()
                    .contains("timed_out")
            );
        } else if action == "permission" {
            assert_eq!(
                f.s.chat_detail(&f.chat).await.unwrap()["wakeup"]["observation"]["state"],
                "unavailable"
            );
            assert_eq!(f.tick().await["run"]["status"], "queued");
        } else if action == "message" {
            assert!(f.s.chat_detail(&f.chat).await.unwrap()["wakeup"].is_null());
        }
    }
}

#[tokio::test]
async fn pr_ids_and_commit_sha_are_canonical_and_invalid_sources_are_rejected() {
    let f = Fixture::new().await;
    pending(&f);
    let mut input = condition(&f);
    input["prNumber"] = json!(7.0);
    let wait = f.schedule(input).await;
    assert_eq!(wait, f.schedule(condition(&f)).await);
    for value in [0.0, 2_147_483_648.0, 1.5] {
        let mut source = condition(&f);
        source["prNumber"] = json!(value);
        let result = wait_for::schedule(
            &f.s,
            &f.token,
            &json!({ "conditions": [source], "reason": "Invalid", "timeoutSeconds": 420 }),
        )
        .await;
        assert_eq!(result.unwrap_err().status, 400);
    }
}
