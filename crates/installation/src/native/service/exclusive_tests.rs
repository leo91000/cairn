use super::{Attempt, Controller, tests::fixture};
use crate::{
    config::{id, now},
    microvm::suspension::Reason,
    native::suspension::{Suspension, TestCpu},
    runner::exclusive::{Acquisition, Release, Reservations},
};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

async fn attempt(controller: &Controller, ready: bool) -> (String, Arc<TestCpu>, Arc<Suspension>) {
    let id = id();
    let cpu = Arc::new(TestCpu::default());
    let suspension = Arc::new(Suspension::testing(&controller.state, &id, cpu.clone()));
    if ready {
        suspension.bind().await.unwrap();
    }
    controller
        .leases
        .lock()
        .await
        .insert(id.clone(), crate::nodes::boot_ms() + 60_000);
    controller.active.lock().await.insert(
        id.clone(),
        Attempt {
            run: crate::config::id(),
            expires: None,
            stop: CancellationToken::new(),
            started: ready,
            protection: None,
            suspension: suspension.clone(),
        },
    );
    (id, cpu, suspension)
}

fn request(owner: &str) -> Acquisition {
    Acquisition {
        id: id(),
        epoch: 1,
        owner_attempt_id: owner.into(),
        member_attempt_ids: vec![],
        expires_at: now() + 5000,
        remaining_ms: Some(5000),
        reason: "Native compilation".into(),
        minimum_available_memory_mi_b: None,
    }
}

#[tokio::test]
async fn native_protocol_reserves_members_blocks_admission_and_preserves_storage_fences() {
    let (_root, controller) = fixture().await;
    let (owner, owner_cpu, _) = attempt(&controller, true).await;
    let (member, member_cpu, _) = attempt(&controller, true).await;
    let (peer, peer_cpu, peer_pause) = attempt(&controller, true).await;
    peer_pause.set_reason(&Reason::Storage, true).await.unwrap();
    let mut acquisition = request(&owner);
    acquisition.member_attempt_ids.push(member);
    let acquired = controller
        .acquire_exclusive(acquisition.clone())
        .await
        .unwrap();
    assert_eq!(acquired["reservation"]["state"], "acquired");
    assert_eq!(acquired["reservation"]["pausedAttemptIds"], json!([peer]));
    assert!(owner_cpu.transitions.lock().await.is_empty());
    assert!(member_cpu.transitions.lock().await.is_empty());
    assert_eq!(controller.start(&id()).await.unwrap_err().status, 409);
    controller.allow_exclusive_admission(&owner).await.unwrap();
    tokio::time::sleep(Duration::from_millis(15)).await;
    let duration = controller.administrative_pause(&peer).await.unwrap();
    assert!(duration["administrativePausedMs"].as_u64().unwrap() >= 15);
    controller
        .release_exclusive(Release {
            id: acquisition.id,
            epoch: acquisition.epoch,
        })
        .await
        .unwrap();
    assert!(controller.inspect_exclusive().await.unwrap()["reservation"].is_null());
    assert_eq!(*peer_cpu.transitions.lock().await, vec![true]);
    peer_pause
        .set_reason(&Reason::Storage, false)
        .await
        .unwrap();
    assert_eq!(*peer_cpu.transitions.lock().await, vec![true, false]);
    assert_eq!(*owner_cpu.transitions.lock().await, vec![true, false]);
}

#[tokio::test]
async fn native_acquisition_waits_for_preparation_and_rolls_back_a_failed_freeze() {
    let (_root, controller) = fixture().await;
    let (owner, _, _) = attempt(&controller, true).await;
    let (peer, cpu, pause) = attempt(&controller, false).await;
    let acquisition = request(&owner);
    let pending_controller = controller.clone();
    let pending =
        tokio::spawn(async move { pending_controller.acquire_exclusive(acquisition).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if controller.inspect_exclusive().await.unwrap()["reservation"]["pausedAttemptIds"]
                == json!([peer])
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!pending.is_finished());
    pause.bind().await.unwrap();
    assert_eq!(
        pending.await.unwrap().unwrap()["reservation"]["state"],
        "acquired"
    );
    // Failure must never publish an acquired reservation or leave admission shut.
    let held = controller.inspect_exclusive().await.unwrap();
    controller
        .release_exclusive(
            serde_json::from_value(json!({ "id": held["reservation"]["id"], "epoch": 1 })).unwrap(),
        )
        .await
        .unwrap();
    cpu.fail_next
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let mut failed = request(&owner);
    failed.epoch = 2;
    assert!(controller.acquire_exclusive(failed).await.is_err());
    assert!(controller.inspect_exclusive().await.unwrap()["reservation"].is_null());
    assert_eq!(
        *cpu.transitions.lock().await,
        vec![true, false, true, false]
    );
    let mut invalid = request(&owner);
    invalid.epoch = 3;
    invalid.member_attempt_ids.push(id());
    assert!(controller.acquire_exclusive(invalid).await.is_err());
    assert!(controller.inspect_exclusive().await.unwrap()["reservation"].is_null());
}

#[tokio::test]
async fn native_expiration_renews_only_on_extension_and_resumes_peers() {
    let (_root, controller) = fixture().await;
    let (owner, _, _) = attempt(&controller, true).await;
    let (_peer, cpu, _) = attempt(&controller, true).await;
    let mut acquisition = request(&owner);
    acquisition.expires_at = now() + 1000;
    acquisition.remaining_ms = Some(1000);
    controller
        .acquire_exclusive(acquisition.clone())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    controller
        .acquire_exclusive(acquisition.clone())
        .await
        .unwrap();
    acquisition.expires_at += 500;
    acquisition.remaining_ms = Some(1470);
    controller.acquire_exclusive(acquisition).await.unwrap();
    let monitor = tokio::spawn(controller.clone().monitor_exclusive());
    tokio::time::timeout(Duration::from_secs(5), async {
        while !controller.inspect_exclusive().await.unwrap()["reservation"].is_null() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(*cpu.transitions.lock().await, vec![true, false]);
    controller.stop.cancel();
    monitor.await.unwrap();
}

#[tokio::test]
async fn native_tombstones_survive_restart_and_expired_members_cannot_acquire() {
    let (_root, controller) = fixture().await;
    let (owner, _, _) = attempt(&controller, true).await;
    let acquisition = request(&owner);
    controller
        .release_exclusive(Release {
            id: acquisition.id.clone(),
            epoch: 1,
        })
        .await
        .unwrap();
    let mut restored = controller.clone();
    restored.exclusive = Arc::default();
    restored.recover_exclusive().await.unwrap();
    assert!(restored.acquire_exclusive(acquisition).await.is_err());
    let mut acquisition = request(&owner);
    acquisition.epoch = 2;
    restored.leases.lock().await.remove(&owner);
    assert!(restored.acquire_exclusive(acquisition).await.is_err());
}

#[tokio::test]
async fn native_confirmed_pause_extends_execution_deadline_but_never_a_lease() {
    let (_root, controller) = fixture().await;
    let (peer, _, pause) = attempt(&controller, true).await;
    let deadline = now() + 500;
    let reason = Reason::Exclusive {
        reservation_id: id(),
        epoch: 1,
    };
    pause.set_reason(&reason, true).await.unwrap();
    tokio::time::sleep(Duration::from_millis(550)).await;
    assert!(!controller.expired(&peer, Some(deadline)).await);
    controller
        .leases
        .lock()
        .await
        .insert(peer.clone(), crate::nodes::boot_ms());
    assert!(!controller.leased(&peer).await);
    pause.set_reason(&reason, false).await.unwrap();
    assert!(!controller.expired(&peer, Some(deadline)).await);
    tokio::time::sleep(Duration::from_millis(550)).await;
    assert!(controller.expired(&peer, Some(deadline)).await);
}
