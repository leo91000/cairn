use super::*;
use serde_json::json;

#[tokio::test]
async fn delegated_reductions_reresolve_resource_coverage_after_publication_waits() {
    use crate::agent_fixture::AgentFixture;
    use crate::capabilities::{ResumeContext, plugin_packages::Authority};
    use crate::plugin_grant_fixture::{Fixture, contribution, support};
    use crate::validation::text;
    use std::sync::{Arc, atomic::Ordering};

    let f = Fixture::new().await;
    f.marker("live-parent").await;
    f.set(json!([contribution("tool", "grant_probe_post", support())]))
        .await;
    let child =
        f.s.agent_fixture(
            json!({
                "name": "Child",
                "access": { "installation": true },
            }),
            None,
        )
        .await
        .unwrap();
    let child_id = text(&child, "id");
    f.set_for(
        child_id,
        json!([contribution(
            "tool",
            "grant_probe_post",
            json!([{ "resourceType": "thread", "id": "support-thread" }])
        )]),
    )
    .await;
    let (_, bearer) = f.delegated(crate::config::MAIN_AGENT_ID, child_id).await;
    let target =
        f.s.agent_fixture(json!({ "name": "Target" }), None)
            .await
            .unwrap();
    let target_id = text(&target, "id").to_owned();
    f.set_for(
        &target_id,
        json!([contribution("tool", "grant_probe_other", json!([]))]),
    )
    .await;
    let id = target_id.clone();
    let before =
        f.s.store
            .read(move |db| db.agent_plugin_grants(&id))
            .await
            .unwrap();
    let input = serde_json::from_value(json!({
        "agentId": target_id,
        "revision": before.revision,
        "grants": [],
        "runCredentialInstances": [],
    }))
    .unwrap();
    let checked = Arc::new(tokio::sync::Notify::new());
    let completed = checked.clone();
    let authority = Authority::for_grants(&f.s, ResumeContext::Installation(bearer), "manage")
        .await
        .unwrap()
        .with_recheck_guard(move |_| {
            let completed = completed.clone();
            Box::pin(async move {
                completed.notify_one();
                Ok(())
            })
        });
    let publication = f.s.plugins.package_publication.lock().await;
    let service = f.s.clone();
    let change = tokio::spawn(async move {
        replace(
            &service,
            input,
            authority,
            true,
            crate::conversation_writers::EntryWriter::Unknown,
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), checked.notified())
        .await
        .unwrap();
    f.moved_parent.store(true, Ordering::SeqCst);
    drop(publication);
    let error = change.await.unwrap().unwrap_err();
    assert_eq!(error.status, 403);
    let after =
        f.s.store
            .read(move |db| db.agent_plugin_grants(&target_id))
            .await
            .unwrap();
    assert_eq!(
        after, before,
        "a delegated reduction escaped its requester's resource ceiling"
    );
    f.s.shutdown.cancel();
}

#[tokio::test]
async fn descendant_reductions_cannot_widen_after_external_ancestry_moves() {
    use crate::agent_fixture::AgentFixture;
    use crate::capabilities::{ResumeContext, plugin_packages::Authority};
    use crate::plugin_grant_fixture::{Fixture, contribution, support};
    use crate::validation::text;
    use std::sync::{Arc, atomic::Ordering};

    let f = Fixture::new().await;
    f.marker("live-parent").await;
    f.set(json!([contribution("tool", "grant_probe_post", json!([]))]))
        .await;
    let target =
        f.s.agent_fixture(json!({ "name": "Target" }), None)
            .await
            .unwrap();
    let target_id = text(&target, "id").to_owned();
    f.set_for(
        &target_id,
        json!([contribution("tool", "grant_probe_post", support())]),
    )
    .await;
    let id = target_id.clone();
    let before =
        f.s.store
            .read(move |db| db.agent_plugin_grants(&id))
            .await
            .unwrap();
    let input = serde_json::from_value(json!({
        "agentId": target_id,
        "revision": before.revision,
        "grants": [{
            "instanceId": f.instance,
            "access": {
                "type": "selected",
                "contributions": [contribution(
                    "tool",
                    "grant_probe_post",
                    json!([{
                        "resourceType": "thread",
                        "id": "support-thread",
                    }]),
                )],
            },
        }],
        "runCredentialInstances": [],
    }))
    .unwrap();
    let checked = Arc::new(tokio::sync::Notify::new());
    let completed = checked.clone();
    let resolved = f.posts.clone();
    f.posts.store(0, Ordering::SeqCst);
    let authority = Authority::for_grants(
        &f.s,
        ResumeContext::Installation(f.bearer.clone()),
        "manage",
    )
    .await
    .unwrap()
    .with_recheck_guard(move |_| {
        let completed = completed.clone();
        let resolved = resolved.clone();
        Box::pin(async move {
            if resolved.load(Ordering::SeqCst) > 0 {
                completed.notify_one();
            }
            Ok(())
        })
    });
    let publication = f.s.plugins.package_publication.lock().await;
    let service = f.s.clone();
    let change = tokio::spawn(async move {
        replace(
            &service,
            input,
            authority,
            true,
            crate::conversation_writers::EntryWriter::Unknown,
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), checked.notified())
        .await
        .unwrap();
    f.moved_parent.store(true, Ordering::SeqCst);
    drop(publication);
    let error = change.await.unwrap().unwrap_err();
    assert_eq!(error.status, 403);
    let after =
        f.s.store
            .read(move |db| db.agent_plugin_grants(&target_id))
            .await
            .unwrap();
    assert_eq!(
        after, before,
        "a descendant reduction became a grant outside its previous resource root"
    );
    f.s.shutdown.cancel();
}

#[tokio::test]
async fn revoked_public_authority_cannot_widen_grants_after_the_publication_lock() {
    use axum::{Json, Router, routing::post};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let root = tempfile::tempdir().unwrap();
    let config = serde_json::from_value(json!({
        "dataDir": root.path().join("data"),
        "home": root.path().join("home"),
        "workspaceRoots": [root.path()],
        "publicUrl": "http://localhost:4310",
        "host": "127.0.0.1",
        "port": 0,
        "codexBin": "unused",
        "concurrency": 1,
        "logger": false,
        "workerEnabled": false,
        "runnerUrl": "",
    }))
    .unwrap();
    let s = crate::service::Service::new(config).await.unwrap();
    let live = Arc::new(AtomicBool::new(true));
    let checked = Arc::new(tokio::sync::Notify::new());
    let (valid, completed) = (live.clone(), checked.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new().route(
        "/api/relay/00000000-0000-4000-8000-000000000055/plugin-mcp-authorization",
        post(move || {
            let (valid, completed) = (valid.clone(), completed.clone());
            async move {
                // Capture the successful answer before announcing the check.
                let allowed = valid.load(Ordering::SeqCst);
                completed.notify_one();
                Json(allowed)
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let identity = s.config.data_dir.join("installation-relay");
    tokio::fs::create_dir_all(&identity).await.unwrap();
    tokio::fs::write(
        identity.join("identity.json"),
        serde_json::to_vec(&json!({
            "origin": format!("http://{address}"),
            "installationId": "00000000-0000-4000-8000-000000000055",
            "token": "fixture-only",
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    tokio::fs::set_permissions(
        identity.join("identity.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .await
    .unwrap();
    let package = super::super::registry::active(&s, "github").await.unwrap();
    let instance = crate::config::id();
    let id = instance.clone();
    s.store
        .transaction(move |db| {
            db.save_plugin_version("github", &package.manifest.version, &package.digest)?;
            let mut instance =
                super::super::connections::Connection::draft(&id, "github", json!({}), vec![]);
            instance.name = "Fixture".into();
            instance.enabled = true;
            instance.revision = "r1".into();
            db.save_plugin_instance(&instance)
        })
        .await
        .unwrap();
    let agent = crate::config::MAIN_AGENT_ID;
    let before = s
        .store
        .read(move |db| db.agent_plugin_grants(agent))
        .await
        .unwrap();
    let input = serde_json::from_value(json!({
        "agentId": agent,
        "revision": before.revision,
        "grants": [{
            "instanceId": instance,
            "access": { "type": "full" },
        }],
        "runCredentialInstances": [instance],
    }))
    .unwrap();
    let publication = s.plugins.package_publication.lock().await;
    let service = s.clone();
    let mut change = tokio::spawn(async move {
        replace(
            &service,
            input,
            crate::capabilities::plugin_packages::Authority::public_scoped(
                Some("fixture-digest".into()),
                "manage",
            ),
            false,
            crate::conversation_writers::EntryWriter::Unknown,
        )
        .await
    });
    let handshake = checked.notified();
    tokio::pin!(handshake);
    tokio::select! {
        () = &mut handshake => {}
        result = &mut change => {
            panic!("Grant replacement finished before its revocation check: {result:?}");
        }
        () = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
            panic!("Grant replacement did not reach the revocation check within 30 seconds.");
        }
    }
    live.store(false, Ordering::SeqCst);
    drop(publication);
    let error = change.await.unwrap().unwrap_err();
    assert_eq!(error.status, 403);
    let after = s
        .store
        .read(move |db| db.agent_plugin_grants(agent))
        .await
        .unwrap();
    assert_eq!(
        after, before,
        "revoked client published a full grant or supplier"
    );
    server.abort();
    s.shutdown.cancel();
}
