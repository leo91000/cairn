//! Private authority for accepted management work, never a transport credential.
use super::*;
use crate::{chats::Conversation, error::required, validation::text};
use cairn_contracts::{ConversationLifecycle, RunStatus};
use serde_json::Value;
use std::sync::Weak;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub(crate) struct Jobs(Mutex<Vec<Weak<Accepted>>>);

impl Jobs {
    fn register(&self, job: &Arc<Accepted>) {
        let mut jobs = self.0.lock().unwrap();
        jobs.retain(|job| job.strong_count() > 0);
        jobs.push(Arc::downgrade(job));
    }

    pub(crate) fn revoke_run(&self, id: &str) {
        self.revoke(|job| {
            job.run_id == id || job.requester.as_ref().is_some_and(|p| p.run_id == id)
        });
    }

    pub(crate) fn revoke_chat(&self, id: &str) {
        self.revoke(|job| {
            job.chat.as_ref().is_some_and(|c| c.id == id)
                || job.requester.as_ref().is_some_and(|p| p.chat.id == id)
        });
    }

    fn revoke(&self, matches: impl Fn(&Accepted) -> bool) {
        self.0.lock().unwrap().retain(|weak| {
            let Some(job) = weak.upgrade() else {
                return false;
            };
            if matches(&job) {
                job.stop.cancel();
            }
            true
        });
    }
}

struct Instance {
    id: String,
    revision: String,
    enabled: bool,
    plugin: String,
    active: Option<String>,
    digest: Option<String>,
}

struct Profile {
    id: String,
    revision: String,
    instances: Vec<Instance>,
}

impl Profile {
    fn capture(db: &Db<'_>, id: &str) -> Result<Self> {
        let profile = required(db.agent(id)?, "Initiating agent removed")?;
        let mut instances = Vec::new();
        for grant in db.agent_plugin_grants(id)?.grants {
            let instance = required(
                db.plugin_instance(&grant.instance_id.to_string())?,
                "Plugin instance removed",
            )?;
            let package = db.plugin_package(&instance.plugin_id)?;
            let digest = package
                .active
                .as_ref()
                .and_then(|v| package.approvals.get(v))
                .cloned();
            instances.push(Instance {
                id: instance.id,
                revision: instance.revision,
                enabled: instance.enabled,
                plugin: instance.plugin_id,
                active: package.active,
                digest,
            });
        }
        Ok(Self {
            id: id.into(),
            revision: profile.revision,
            instances,
        })
    }

    fn authorize(&self, db: &Db<'_>) -> Result<()> {
        if required(db.agent(&self.id)?, "Initiating agent removed")?.revision != self.revision {
            return Err(revoked());
        }
        for expected in &self.instances {
            let instance = required(db.plugin_instance(&expected.id)?, "Plugin instance removed")?;
            let package = db.plugin_package(&expected.plugin)?;
            let digest = package
                .active
                .as_ref()
                .and_then(|v| package.approvals.get(v));
            if instance.revision != expected.revision
                || instance.enabled != expected.enabled
                || package.active != expected.active
                || digest != expected.digest.as_ref()
            {
                return Err(revoked());
            }
        }
        Ok(())
    }
}

struct Requester {
    chat: Conversation,
    run_id: String,
    profile: Profile,
}

pub(crate) struct Accepted {
    run_id: String,
    agent: Value,
    profile: Profile,
    chat: Option<Conversation>,
    requester: Option<Requester>,
    task: Option<(String, i64)>,
    native: Vec<(&'static str, Value)>,
    stop: CancellationToken,
}

fn revoked() -> Error {
    Error::forbidden("The accepted job's initiating authority was revoked.")
}

impl Accepted {
    pub(crate) fn agent_id(&self) -> &str {
        &self.profile.id
    }

    pub(crate) fn requester_profile(&self) -> Option<(String, String)> {
        self.requester.as_ref().map(|requester| {
            (
                requester.profile.id.clone(),
                requester.profile.revision.clone(),
            )
        })
    }

    pub(crate) async fn capture(s: &Service, bearer: &str) -> Result<Arc<Self>> {
        let bearer = bearer.to_owned();
        let job = s
            .store
            .read(move |db| {
                // Capture only from a currently valid request, before its bearer expires.
                let run = crate::installation_mcp::authorize_in(db, &bearer)?;
                let run_id = text(&run, "id").to_owned();
                let chat = db.conversation_for_run(&run_id)?;
                let requester = chat
                    .as_ref()
                    .and_then(|c| c.requested_by.as_ref())
                    .map(|lineage| {
                        let parent = required(
                            db.conversation(&lineage.chat_id)?,
                            "Requesting conversation removed",
                        )?;
                        if parent.lifecycle != ConversationLifecycle::Active {
                            return Err(revoked());
                        }
                        let profile = Profile::capture(db, &parent.agent_id)?;
                        Ok(Requester {
                            chat: parent,
                            profile,
                            run_id: lineage.run_id.clone(),
                        })
                    })
                    .transpose()?;
                let task = if chat.is_none() {
                    run["taskId"]
                        .as_str()
                        .map(|id| {
                            let task = required(db.mission(id)?, "Initiating task removed")?;
                            Ok::<_, Error>((id.to_owned(), task.created_at))
                        })
                        .transpose()?
                } else {
                    None
                };
                let agent = run["snapshot"]["agent"].clone();
                let profile = Profile::capture(db, text(&agent, "id"))?;
                let native = [
                    "nativePrincipals",
                    "nativeAuthorityNodes",
                    "nativeParentRunId",
                    "nativeParentAccess",
                    "nativeProvenanceUnknown",
                    "nativePluginGrants",
                ]
                .into_iter()
                .map(|field| (field, run[field].clone()))
                .collect();
                Ok(Arc::new(Self {
                    run_id,
                    agent,
                    profile,
                    chat,
                    requester,
                    task,
                    native,
                    stop: CancellationToken::new(),
                }))
            })
            .await?;
        s.accepted_jobs.register(&job);
        job.recheck(s).await?;
        Ok(job)
    }

    pub(crate) fn authorize(&self, db: &Db<'_>) -> Result<()> {
        if self.stop.is_cancelled() {
            return Err(revoked());
        }
        let result = self.check(db);
        if result.is_err() {
            self.stop.cancel();
        }
        result
    }

    fn check(&self, db: &Db<'_>) -> Result<()> {
        let run = required(db.run(&self.run_id)?, "Initiating run removed")?;
        if !run["cancelRequestedAt"].is_null()
            || run["status"] == RunStatus::Cancelled
            || !crate::installation_mcp::agent_allowed(&self.agent, &run["snapshot"]["agent"])
            || self
                .native
                .iter()
                .any(|(field, expected)| &run[*field] != expected)
        {
            return Err(revoked());
        }
        self.profile.authorize(db)?;
        let current = required(
            db.agent_snapshot(&self.profile.id)?,
            "Initiating agent removed",
        )?;
        if crate::service::policy(&self.agent)["installation"] != true
            || !crate::installation_mcp::agent_allowed(&current, &self.agent)
        {
            return Err(revoked());
        }
        if let Some(chat) = &self.chat {
            let current = required(
                db.conversation(&chat.id)?,
                "Initiating conversation removed",
            )?;
            if current.lifecycle != ConversationLifecycle::Active
                || current.created_at != chat.created_at
                || current.agent_id != chat.agent_id
                || current.requested_by != chat.requested_by
            {
                return Err(revoked());
            }
        }
        if let Some(parent) = &self.requester {
            let parent_chat = required(
                db.conversation(&parent.chat.id)?,
                "Requesting conversation removed",
            )?;
            let parent_run = required(db.run(&parent.run_id)?, "Requesting run removed")?;
            if parent_chat.lifecycle != ConversationLifecycle::Active
                || parent_chat.created_at != parent.chat.created_at
                || parent_chat.agent_id != parent.profile.id
                || !parent_run["cancelRequestedAt"].is_null()
                || parent_run["status"] == RunStatus::Cancelled
            {
                return Err(revoked());
            }
            parent.profile.authorize(db)?;
            let requester = required(
                db.agent_snapshot(&parent.profile.id)?,
                "Requesting agent removed",
            )?;
            if crate::service::policy(&requester)["delegation"] != true
                || !crate::service::covers(&requester, &current)
                || !crate::plugins::grants::contribution_ceiling_in(
                    db,
                    &parent.profile.id,
                    &self.profile.id,
                )?
            {
                return Err(revoked());
            }
        }
        if let Some((id, created)) = &self.task
            && required(db.mission(id)?, "Initiating task removed")?.created_at != *created
        {
            return Err(revoked());
        }
        Ok(())
    }

    pub(crate) async fn recheck(self: &Arc<Self>, s: &Service) -> Result<()> {
        let proof = self.clone();
        s.store.read(move |db| proof.authorize(db)).await?;
        if let Some(parent) = &self.requester {
            let proof = self.clone();
            let authority = Authority::default().with_guard(move |db| proof.authorize(db));
            let coverage = crate::plugins::grants::check_coverage_authorized(
                s,
                &parent.profile.id,
                &self.profile.id,
                Some(authority),
            )
            .await?;
            let proof = self.clone();
            s.store
                .read(move |db| {
                    proof.authorize(db)?;
                    coverage.authorize(db)
                })
                .await?;
        }
        Ok(())
    }

    pub(super) async fn cancelled(&self) {
        self.stop.cancelled().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_grant_fixture::{Fixture, contribution, support};
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};

    async fn accepted() -> (Fixture, String, Value, Authorization) {
        let (f, bearer) = Fixture::delegated_manager().await;
        let run = crate::installation_mcp::authorize(&f.s, &bearer)
            .await
            .unwrap();
        let job = crate::installation_mcp::management_authorization(&f.s, Some(&bearer))
            .await
            .unwrap()
            .accepted()
            .await
            .unwrap();
        (f, bearer, run, job)
    }

    async fn complete(f: &Fixture, run: &Value) {
        f.s.store
            .change_run(
                text(run, "id"),
                crate::runs::RunChanges {
                    status: Some(RunStatus::Succeeded),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        f.s.mcps.revoke_run(&f.s, text(run, "id")).await.unwrap();
    }

    #[tokio::test]
    async fn normal_completion_retains_only_the_accepted_job_not_its_transport() {
        let (f, bearer, run, job) = accepted().await;
        complete(&f, &run).await;
        assert!(
            crate::installation_mcp::authorize(&f.s, &bearer)
                .await
                .is_err()
        );
        job.recheck().await.unwrap();
        f.s.store
            .transaction(move |db| job.authorize(db))
            .await
            .unwrap();
        f.s.shutdown.cancel();
    }

    #[tokio::test]
    async fn completed_jobs_recheck_semantic_delegation_after_an_async_wait() {
        let (f, _, run, job) = accepted().await;
        complete(&f, &run).await;
        job.recheck().await.unwrap();
        f.moved_parent.store(true, Ordering::SeqCst);
        assert_eq!(job.recheck().await.unwrap_err().status, 403);
        f.s.shutdown.cancel();
    }

    #[tokio::test]
    async fn completed_jobs_cannot_adopt_reduced_grants_or_changed_instances() {
        for change in ["child", "requester", "instance"] {
            let (f, _, run, job) = accepted().await;
            complete(&f, &run).await;
            match change {
                "child" => {
                    f.remove_for(text(&run["snapshot"]["agent"], "id")).await;
                }
                "requester" => {
                    f.set(json!([contribution(
                        "tool",
                        "grant_probe_other",
                        support()
                    )]))
                    .await;
                }
                _ => f.marker("changed-after-acceptance").await,
            }
            assert_eq!(job.recheck().await.unwrap_err().status, 403, "{change}");
            f.s.shutdown.cancel();
        }
    }

    #[tokio::test]
    async fn explicit_stop_revokes_completed_jobs_even_if_the_chat_resumes() {
        let (f, _, run, job) = accepted().await;
        let run_id = text(&run, "id").to_owned();
        let chat =
            f.s.store
                .read(move |db| db.conversation_for_run(&run_id))
                .await
                .unwrap()
                .unwrap();
        complete(&f, &run).await;
        f.s.worker
            .stop_chat_live(&f.s, &chat.id, Authorization::default())
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), job.cancelled())
            .await
            .unwrap();
        assert_eq!(job.recheck().await.unwrap_err().status, 403);
        f.s.shutdown.cancel();
    }

    #[tokio::test]
    async fn requester_deletion_revokes_without_an_own_agent_fallback() {
        let (f, _, run, job) = accepted().await;
        let run_id = text(&run, "id").to_owned();
        let chat =
            f.s.store
                .read(move |db| db.conversation_for_run(&run_id))
                .await
                .unwrap()
                .unwrap();
        complete(&f, &run).await;
        let requester = chat.requested_by.unwrap();
        f.s.chat_trash_live(&requester.chat_id, true, Authorization::default())
            .await
            .unwrap();
        assert_eq!(job.recheck().await.unwrap_err().status, 403);
        f.s.shutdown.cancel();
    }

    #[tokio::test]
    async fn retention_preserves_additional_guards_and_does_not_consume_admission_callbacks() {
        let (f, bearer) = Fixture::delegated_manager().await;
        let allowed = Arc::new(AtomicBool::new(true));
        let check = allowed.clone();
        let authority = crate::installation_mcp::management_authorization(&f.s, Some(&bearer))
            .await
            .unwrap()
            .with_guard(move |_| {
                if check.load(Ordering::SeqCst) {
                    Ok(())
                } else {
                    Err(revoked())
                }
            });
        let job = authority.accepted().await.unwrap();
        allowed.store(false, Ordering::SeqCst);
        assert_eq!(job.recheck().await.unwrap_err().status, 403);
        let called = Arc::new(AtomicBool::new(false));
        let callback = called.clone();
        let request = Authorization::once(move |_| {
            assert!(!callback.swap(true, Ordering::SeqCst));
            Ok(())
        });
        let detached = request.clone().accepted().await.unwrap();
        assert!(!called.load(Ordering::SeqCst));
        f.s.store
            .transaction(move |db| request.authorize(db))
            .await
            .unwrap();
        f.s.store
            .transaction(move |db| detached.authorize(db))
            .await
            .unwrap();
        assert!(called.load(Ordering::SeqCst));
        f.s.shutdown.cancel();
    }

    #[tokio::test]
    async fn opaque_execution_authorities_cannot_be_reconstructed_as_jobs() {
        let (f, bearer) = Fixture::delegated_manager().await;
        let (_, authority) = crate::installation_mcp::authorize_with_authority(&f.s, &bearer)
            .await
            .unwrap();
        assert_eq!(
            Authorization::live(&f.s, authority)
                .accepted()
                .await
                .err()
                .unwrap()
                .status,
            403
        );
        f.s.shutdown.cancel();
    }

    #[tokio::test]
    async fn accepted_source_jobs_keep_the_original_invocation_after_normal_completion() {
        for change in ["complete", "ancestry", "grant", "delete", "delivery"] {
            let (f, bearer) = Fixture::delegated_manager().await;
            let installed = crate::common::package_install(&f.s, &json!({
                "manifest": { "id": "retained-source", "name": "Retained source", "version": "1", "apiVersion": 2,
                    "agentAccess": {}, "capabilities": ["http"], "hosts": ["service.example.test"],
                    "projectSources": [{ "name": "repositories", "title": "Repositories" }],
                },
                "source": r#"return { projects = function(input)
                    cairn.http.request({instance=input.instance.id,url="https://service.example.test/hold"})
                    cairn.http.request({instance=input.instance.id,url="https://service.example.test/upload"})
                    return {items=cairn.array({{id="private",name="Private repository"}})}
                end }"#,
            })).await.unwrap();
            crate::common::package_approve(
                &f.s,
                "retained-source",
                "1",
                text(&installed, "digest"),
            )
            .await
            .unwrap();
            let instance = crate::common::save_plugin(
                &f.s,
                "retained-source",
                &json!({ "name": "Source", "enabled": true }),
                None,
            )
            .await
            .unwrap();
            let caller = crate::installation_mcp::authorize(&f.s, &bearer)
                .await
                .unwrap();
            for agent in [
                crate::config::MAIN_AGENT_ID,
                text(&caller["snapshot"]["agent"], "id"),
            ] {
                let current = f.api("GET", json!({ "agentId": agent })).await.unwrap();
                let mut grants = current["grants"].as_array().unwrap().clone();
                grants.push(json!({
                    "instanceId": instance["id"],
                    "access": {
                        "type": "selected",
                        "contributions": [contribution(
                            "projectSource",
                            "repositories",
                            json!([]),
                        )],
                    },
                }));
                f.api(
                    "PUT",
                    json!({
                        "agentId": agent,
                        "revision": current["revision"],
                        "grants": grants,
                        "runCredentialInstances": [],
                    }),
                )
                .await
                .unwrap();
            }
            let pending = crate::capabilities::resource_operation("list_source_projects")
                .unwrap()
                .invoke(
                    &f.s,
                    json!({
                        "pluginId": "retained-source",
                        "source": "repositories",
                        "instance": instance["id"],
                    }),
                    crate::capabilities::ResumeContext::Installation(bearer.clone()),
                )
                .await
                .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(10), f.entered.notified())
                .await
                .unwrap();
            if change != "delivery" {
                complete(&f, &caller).await;
            }
            match change {
                "ancestry" => f.moved_parent.store(true, Ordering::SeqCst),
                "grant" => {
                    f.remove_for(text(&caller["snapshot"]["agent"], "id")).await;
                }
                "delete" => {
                    let run_id = text(&caller, "id").to_owned();
                    let chat =
                        f.s.store
                            .read(move |db| db.conversation_for_run(&run_id))
                            .await
                            .unwrap()
                            .unwrap();
                    f.s.chat_trash_live(
                        &chat.requested_by.unwrap().chat_id,
                        true,
                        Authorization::default(),
                    )
                    .await
                    .unwrap();
                }
                _ => {}
            }
            f.release.notify_one();
            let operation = text(&pending, "operation");
            let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let result = f.s.plugins.operations.get(operation).unwrap();
                    if result["state"] != "running" {
                        break result;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                result["state"],
                if matches!(change, "complete" | "delivery") {
                    "completed"
                } else {
                    "failed"
                },
                "{change}: {result}"
            );
            assert_eq!(
                f.uploads.load(Ordering::SeqCst),
                usize::from(matches!(change, "complete" | "delivery")),
                "{change}"
            );
            if change == "delivery" {
                let read =
                    crate::capabilities::plugin_package_operation("get_plugin_operation").unwrap();
                let input = json!({ "operationId": operation });
                let response = read
                    .invoke(
                        &f.s,
                        input.clone(),
                        crate::capabilities::ResumeContext::Installation(bearer.clone()),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    response["result"]["page"]["items"][0]["name"],
                    "Private repository"
                );
                f.remove_for(text(&caller["snapshot"]["agent"], "id")).await;
                assert!(
                    crate::installation_mcp::authorize(&f.s, &bearer)
                        .await
                        .is_ok()
                );
                let error = read
                    .invoke(
                        &f.s,
                        input,
                        crate::capabilities::ResumeContext::Installation(bearer.clone()),
                    )
                    .await
                    .unwrap_err();
                assert_eq!(error.status, 403);
            } else {
                assert!(
                    crate::installation_mcp::authorize(&f.s, &bearer)
                        .await
                        .is_err()
                );
            }
            f.s.shutdown.cancel();
        }
    }
}
