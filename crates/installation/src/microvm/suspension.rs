//! Durable CPU suspension reasons. Removing one owner never releases another.
use super::{host, vm::AttemptRecord};
use crate::{
    error::{Error, Result},
    skills::atomic_write,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
    time::Duration,
};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reason {
    Storage,
    Capture,
    Retention,
    Revoked,
    Exclusive { reservation_id: String, epoch: u64 },
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// Desired aggregate CPU state, which may still await acknowledgment.
    pub paused: bool,
    pub acknowledged: bool,
    pub administrative_suspended_ms: u64,
    pub reasons: Vec<Reason>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    #[serde(default)]
    vm_id: Option<String>,
    #[serde(default)]
    ready: Option<bool>,
    #[serde(default)]
    reasons: BTreeSet<Reason>,
    #[serde(default)]
    confirmed_paused: Option<bool>,
    #[serde(default)]
    administrative_since: Option<i64>,
    #[serde(default)]
    administrative_suspended_ms: u64,
}

#[derive(Deserialize, Serialize)]
struct Owner {
    attempt: String,
}

type Locks = HashMap<PathBuf, Weak<AsyncMutex<()>>>;

static LOCKS: OnceLock<Mutex<Locks>> = OnceLock::new();

fn lock(path: &Path) -> Result<Arc<AsyncMutex<()>>> {
    let mut locks = LOCKS
        .get_or_init(Mutex::default)
        .lock()
        .map_err(Error::internal)?;
    locks.retain(|_, value| value.strong_count() > 0);
    if let Some(lock) = locks.get(path).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    let lock = Arc::new(AsyncMutex::new(()));
    locks.insert(path.to_owned(), Arc::downgrade(&lock));
    Ok(lock)
}

async fn transition_lock(path: &Path) -> Result<tokio::sync::OwnedMutexGuard<()>> {
    tokio::time::timeout(Duration::from_secs(1), lock(path)?.lock_owned())
        .await
        .map_err(|_| Error::timeout("VM transition is waiting for another controller operation."))
}

fn record_path(state: &Path, attempt: &str) -> Result<PathBuf> {
    if attempt.is_empty()
        || attempt.len() > 128
        || !attempt
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(Error::bad("Invalid execution identity for suspension."));
    }
    Ok(state.join(format!("{attempt}.suspension.json")))
}

fn jail(state: &Path, vm_id: &str) -> Result<PathBuf> {
    crate::validation::uuid(vm_id)?;
    Ok(state.join("jails/firecracker").join(vm_id).join("root"))
}

async fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn save<T: Serialize>(path: &Path, record: &T) -> Result<()> {
    atomic_write(path, &serde_json::to_vec(record)?).await?;
    let directory = path.parent().unwrap().to_owned();
    tokio::task::spawn_blocking(move || std::fs::File::open(directory)?.sync_all())
        .await
        .map_err(Error::internal)??;
    Ok(())
}

impl Record {
    fn administrative(&self) -> bool {
        self.reasons
            .iter()
            .any(|reason| matches!(reason, Reason::Exclusive { .. }))
    }

    fn effective_paused(&self) -> bool {
        self.reasons
            .iter()
            .any(|reason| self.ready != Some(false) || !matches!(reason, Reason::Exclusive { .. }))
    }

    fn account(&mut self, bound: bool, now: i64) {
        if bound
            && self.ready != Some(false)
            && self.confirmed_paused == Some(true)
            && self.administrative()
        {
            self.administrative_since.get_or_insert(now);
        } else if let Some(since) = self.administrative_since.take() {
            self.administrative_suspended_ms = self
                .administrative_suspended_ms
                .saturating_add(now.saturating_sub(since).max(0) as u64);
        }
    }

    fn status(&self, bound: bool) -> Status {
        let paused = !self.reasons.is_empty();
        let current = if bound {
            self.administrative_since.map_or(0, |since| {
                crate::config::now().saturating_sub(since).max(0) as u64
            })
        } else {
            0
        };
        Status {
            paused,
            acknowledged: bound
                && self.ready != Some(false)
                && self.confirmed_paused == Some(paused),
            administrative_suspended_ms: self.administrative_suspended_ms.saturating_add(current),
            reasons: self.reasons.iter().cloned().collect(),
        }
    }
}

async fn apply(path: &Path, jail: &Path, record: &mut Record) -> Result<Status> {
    let paused = record.effective_paused();
    if record.confirmed_paused != Some(paused) {
        // Persist the requested state before sending it. A lost response leaves
        // confirmation unknown and the next caller repeats the latest request.
        record.confirmed_paused = None;
        save(path, record).await?;
        host::set_vm_state(
            &jail.join("api.sock"),
            if paused { "Paused" } else { "Resumed" },
        )
        .await?;
        record.confirmed_paused = Some(paused);
    }
    // A requested or lost pause response does not establish CPU suspension.
    // Start compensation only after acknowledgment; stop after resume settles.
    record.account(true, crate::config::now());
    save(path, record).await?;
    Ok(record.status(true))
}

/// Safe under the broker's control lock: this owns a separate attempt/VM lock.
/// Before boot, the reason is durable but `acknowledged` is false.
pub async fn set_reason(
    state: &Path,
    attempt: &str,
    reason: &Reason,
    enabled: bool,
) -> Result<Status> {
    if !enabled && *reason == Reason::Revoked {
        return Err(Error::conflict("A revoked execution cannot resume."));
    }
    let path = record_path(state, attempt)?;
    let _attempt = transition_lock(&path).await?;
    let mut record: Record = read(&path).await?.unwrap_or_default();
    if enabled {
        record.reasons.insert(reason.clone());
    } else {
        record.reasons.remove(reason);
    }

    let binding: Option<AttemptRecord> = read(&state.join(format!("{attempt}.vm.json"))).await?;
    let Some(binding) = binding else {
        record.account(false, crate::config::now());
        save(&path, &record).await?;
        return Ok(record.status(false));
    };
    let starting = record.ready == Some(false);
    let administrative_only = record
        .reasons
        .iter()
        .all(|reason| matches!(reason, Reason::Exclusive { .. }));
    if starting && administrative_only {
        // Initial guest RPCs must finish before their CPUs can be parked. The
        // exclusive caller waits for bind_attempt after the Run request.
        let current_jail = jail(state, &binding.vm_id)?;
        let owner_path = current_jail.join("suspension-owner.json");
        let _vm = transition_lock(&owner_path).await?;
        let owns_vm = read::<Owner>(&owner_path)
            .await?
            .is_some_and(|owner| owner.attempt == attempt);
        if owns_vm && record.confirmed_paused != Some(false) {
            // Storage protection can end during preparation. Resume that
            // fence while leaving administrative acquisition unacknowledged.
            return apply(&path, &current_jail, &mut record).await;
        }
        save(&path, &record).await?;
        return Ok(record.status(false));
    }
    let jail = jail(state, &binding.vm_id)?;
    let owner_path = jail.join("suspension-owner.json");
    let _vm = transition_lock(&owner_path).await?;
    if let Some(owner) = read::<Owner>(&owner_path).await? {
        if owner.attempt != attempt {
            return Err(Error::conflict("Execution no longer owns this VM."));
        }
    } else {
        // Compatibility with already active VMs from before arbiter startup.
        save(
            &owner_path,
            &Owner {
                attempt: attempt.to_owned(),
            },
        )
        .await?;
        record.confirmed_paused.get_or_insert(false);
    }
    record.vm_id = Some(binding.vm_id);
    apply(&path, &jail, &mut record).await
}

pub async fn settle_reason(
    state: &Path,
    attempt: &str,
    reason: &Reason,
    enabled: bool,
    stop: &CancellationToken,
) -> Result<Status> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let result = tokio::select! {
            () = stop.cancelled() => {
                return Err(Error::conflict("Execution stopped during VM transition."));
            },
            result = tokio::time::timeout_at(deadline, set_reason(state, attempt, reason, enabled)) => {
                result.map_err(|_| {
                    Error::timeout("VM suspension transition did not settle before its deadline.")
                })?
            },
        };
        match result {
            Ok(status) if status.acknowledged => return Ok(status),
            Ok(_) if tokio::time::Instant::now() < deadline => {}
            Err(error) if error.status == 408 && tokio::time::Instant::now() < deadline => {}
            Err(error) => return Err(error),
            Ok(_) => {
                return Err(Error::timeout(
                    "VM suspension is not yet bound to an execution.",
                ));
            }
        }
        tokio::select! {
            () = stop.cancelled() => {
                return Err(Error::conflict("Execution stopped during VM transition."));
            },
            () = tokio::time::sleep(Duration::from_millis(100)) => {},
        }
    }
}

pub async fn inspect(state: &Path, attempt: &str) -> Result<Status> {
    let path = record_path(state, attempt)?;
    // Atomic publication makes this read coherent without waiting behind a
    // guest RPC or reclamation. Lease/deadline enforcement must remain live.
    let record: Record = read(&path).await?.unwrap_or_default();
    let bound = if let Some(vm_id) = &record.vm_id {
        read::<Owner>(&jail(state, vm_id)?.join("suspension-owner.json"))
            .await?
            .is_some_and(|owner| owner.attempt == attempt)
    } else {
        false
    };
    Ok(record.status(bound))
}

pub async fn administrative_suspended_ms(state: &Path, attempt: &str) -> Result<u64> {
    Ok(inspect(state, attempt).await?.administrative_suspended_ms)
}

/// Adopt the old monitor's known state once, for an already active VM. Existing
/// durable owners always take precedence over this compatibility observation.
pub(crate) async fn adopt_storage_state(
    state: &Path,
    attempt: &str,
    paused: bool,
    pending: bool,
) -> Result<()> {
    let path = record_path(state, attempt)?;
    let mutex = lock(&path)?;
    let _attempt = mutex.lock().await;
    if read::<Record>(&path).await?.is_some() {
        return Ok(());
    }
    let binding: AttemptRecord = read(&state.join(format!("{attempt}.vm.json")))
        .await?
        .ok_or_else(|| Error::conflict("VM suspension is not bound."))?;
    let owner_path = jail(state, &binding.vm_id)?.join("suspension-owner.json");
    let mutex = lock(&owner_path)?;
    let _vm = mutex.lock().await;
    if let Some(owner) = read::<Owner>(&owner_path).await? {
        if owner.attempt != attempt {
            return Err(Error::conflict("Execution no longer owns this VM."));
        }
    } else {
        save(
            &owner_path,
            &Owner {
                attempt: attempt.to_owned(),
            },
        )
        .await?;
    }
    let mut record = Record {
        vm_id: Some(binding.vm_id),
        confirmed_paused: (!pending).then_some(paused),
        ..Record::default()
    };
    if paused || pending {
        record.reasons.insert(Reason::Storage);
    }
    save(&path, &record).await
}

/// Keeps a confirmed fence stable while host-side memory reclamation operates.
/// This does not take the broker control lock, so callers may already hold it.
pub struct PausedGuard {
    _attempt: tokio::sync::OwnedMutexGuard<()>,
    _vm: tokio::sync::OwnedMutexGuard<()>,
}

pub async fn hold_paused(state: &Path, attempt: &str, reason: &Reason) -> Result<PausedGuard> {
    let path = record_path(state, attempt)?;
    let attempt_guard = lock(&path)?.lock_owned().await;
    let record: Record = read(&path).await?.unwrap_or_default();
    if record.ready == Some(false)
        || record.confirmed_paused != Some(true)
        || !record.reasons.contains(reason)
    {
        return Err(Error::conflict(
            "VM suspension has not been confirmed for this owner.",
        ));
    }
    let vm_id = record
        .vm_id
        .as_deref()
        .ok_or_else(|| Error::conflict("VM suspension is not bound."))?;
    let owner_path = jail(state, vm_id)?.join("suspension-owner.json");
    let vm_guard = lock(&owner_path)?.lock_owned().await;
    if !read::<Owner>(&owner_path)
        .await?
        .is_some_and(|owner| owner.attempt == attempt)
    {
        return Err(Error::conflict("Execution no longer owns this VM."));
    }
    Ok(PausedGuard {
        _attempt: attempt_guard,
        _vm: vm_guard,
    })
}

/// Activate CPU control after guest setup and sending its Run request. An
/// exclusive acquisition cannot be acknowledged while initial RPCs are running.
pub(crate) async fn bind_attempt(state: &Path, attempt: &str, vm_id: &str) -> Result<Status> {
    let path = record_path(state, attempt)?;
    let mutex = lock(&path)?;
    let _attempt = mutex.lock().await;
    let jail = jail(state, vm_id)?;
    let owner_path = jail.join("suspension-owner.json");
    let mutex = lock(&owner_path)?;
    let _vm = mutex.lock().await;
    let mut record: Record = read(&path).await?.unwrap_or_default();
    if let Some(owner) = read::<Owner>(&owner_path).await?
        && owner.attempt != attempt
    {
        let old_path = record_path(state, &owner.attempt)?;
        let old: Record = read(&old_path).await?.unwrap_or_default();
        if !old.reasons.is_empty() || old.confirmed_paused != Some(false) {
            return Err(Error::conflict(
                "Previous execution still holds this VM suspended.",
            ));
        }
    }
    record.vm_id = Some(vm_id.to_owned());
    record.ready = Some(true);
    // Boot and retained resume both leave CPUs running before this binding.
    record.confirmed_paused = Some(false);
    save(
        &owner_path,
        &Owner {
            attempt: attempt.to_owned(),
        },
    )
    .await?;
    apply(&path, &jail, &mut record).await
}

pub(crate) async fn prepare_attempt(state: &Path, attempt: &str, vm_id: &str) -> Result<()> {
    let path = record_path(state, attempt)?;
    let mutex = lock(&path)?;
    let _attempt = mutex.lock().await;
    let mut record: Record = read(&path).await?.unwrap_or_default();
    let owner_path = jail(state, vm_id)?.join("suspension-owner.json");
    let mutex = lock(&owner_path)?;
    let _vm = mutex.lock().await;
    if let Some(owner) = read::<Owner>(&owner_path).await?
        && owner.attempt != attempt
    {
        let old: Record = read(&record_path(state, &owner.attempt)?)
            .await?
            .unwrap_or_default();
        if !old.reasons.is_empty() || old.confirmed_paused != Some(false) {
            return Err(Error::conflict(
                "Previous execution still holds this VM suspended.",
            ));
        }
    }
    record.vm_id = Some(vm_id.to_owned());
    record.ready = Some(false);
    record.confirmed_paused = Some(false);
    save(&path, &record).await?;
    // Transfer the physical owner before imports. The new lease's storage
    // monitor can then protect this retained VM without reviving old callbacks.
    save(
        &owner_path,
        &Owner {
            attempt: attempt.to_owned(),
        },
    )
    .await
}

/// Guest RPCs must finish before a competing CPU pause begins. The wire read
/// itself remains alive without a guard while CPUs are suspended.
pub(crate) async fn hold_running(state: &Path, attempt: &str) -> Result<Option<PausedGuard>> {
    let path = record_path(state, attempt)?;
    let attempt_guard = lock(&path)?.lock_owned().await;
    let record: Record = read(&path).await?.unwrap_or_default();
    if !record.reasons.is_empty() || record.confirmed_paused != Some(false) {
        return Ok(None);
    }
    let Some(vm_id) = record.vm_id.as_deref() else {
        return Ok(None);
    };
    let owner_path = jail(state, vm_id)?.join("suspension-owner.json");
    let vm_guard = lock(&owner_path)?.lock_owned().await;
    if !read::<Owner>(&owner_path)
        .await?
        .is_some_and(|owner| owner.attempt == attempt)
    {
        return Ok(None);
    }
    Ok(Some(PausedGuard {
        _attempt: attempt_guard,
        _vm: vm_guard,
    }))
}

fn state_for_jail(jail: &Path) -> Result<&Path> {
    jail.parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(|| Error::bad("Invalid VM jail path."))
}

pub(super) async fn inspect_jail(jail: &Path) -> Result<Option<Status>> {
    let Some(owner) = read::<Owner>(&jail.join("suspension-owner.json")).await? else {
        return Ok(None);
    };
    inspect(state_for_jail(jail)?, &owner.attempt)
        .await
        .map(Some)
}

/// Called only after the VMM has exited and been reaped. No budget compensation
/// may continue after physical ownership disappears or the controller restarts.
pub(super) async fn finish_jail(jail: &Path) -> Result<()> {
    let owner_path = jail.join("suspension-owner.json");
    let Some(owner) = read::<Owner>(&owner_path).await? else {
        return Ok(());
    };
    let path = record_path(state_for_jail(jail)?, &owner.attempt)?;
    let _attempt = transition_lock(&path).await?;
    let _vm = transition_lock(&owner_path).await?;
    if !read::<Owner>(&owner_path)
        .await?
        .is_some_and(|current| current.attempt == owner.attempt)
    {
        return Err(Error::conflict("Execution no longer owns this VM."));
    }
    let mut record: Record = read(&path).await?.unwrap_or_default();
    record.account(false, crate::config::now());
    record.vm_id = None;
    record.confirmed_paused = None;
    record.ready = Some(false);
    save(&path, &record).await
}

pub(super) async fn set_retention(jail: &Path, enabled: bool) -> Result<Status> {
    let owner: Owner = read(&jail.join("suspension-owner.json"))
        .await?
        .ok_or_else(|| Error::conflict("VM has no suspension owner."))?;
    let state = state_for_jail(jail)?;
    let status = set_reason(state, &owner.attempt, &Reason::Retention, enabled).await?;
    if !enabled && status.paused {
        return Err(Error::conflict(
            "Another suspension owner still holds this VM.",
        ));
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
        net::{UnixListener, UnixStream},
    };

    fn exclusive(epoch: u64) -> Reason {
        Reason::Exclusive {
            reservation_id: "reservation".to_owned(),
            epoch,
        }
    }

    async fn fixture() -> (tempfile::TempDir, String, UnixListener) {
        let root = tempfile::tempdir().unwrap();
        let vm_id = crate::config::id();
        let directory = jail(root.path(), &vm_id).unwrap();
        tokio::fs::create_dir_all(&directory).await.unwrap();
        save(
            &root.path().join("attempt.vm.json"),
            &AttemptRecord {
                vm_id: vm_id.clone(),
                run_id: String::new(),
            },
        )
        .await
        .unwrap();
        let listener = UnixListener::bind(directory.join("api.sock")).unwrap();
        let initial = bind_attempt(root.path(), "attempt", &vm_id).await.unwrap();
        assert!(initial.acknowledged);
        assert!(!initial.paused);
        (root, vm_id, listener)
    }

    async fn receive(stream: UnixStream) -> (BufReader<UnixStream>, String) {
        let mut stream = BufReader::new(stream);
        let mut length = 0;
        loop {
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length: ") {
                length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        stream.read_exact(&mut body).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        (stream, value["state"].as_str().unwrap().to_owned())
    }

    async fn acknowledge(listener: &UnixListener, expected: &str) {
        let (mut stream, state) = receive(listener.accept().await.unwrap().0).await;
        assert_eq!(state, expected);
        stream
            .get_mut()
            .write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn overlapping_storage_capture_and_exclusive_have_one_aggregate_transition() {
        let (root, _, listener) = fixture().await;
        let server = tokio::spawn(async move {
            acknowledge(&listener, "Paused").await;
            acknowledge(&listener, "Resumed").await;
        });
        set_reason(root.path(), "attempt", &Reason::Storage, true)
            .await
            .unwrap();
        set_reason(root.path(), "attempt", &exclusive(1), true)
            .await
            .unwrap();
        set_reason(root.path(), "attempt", &Reason::Capture, true)
            .await
            .unwrap();
        let storage_recovered = set_reason(root.path(), "attempt", &Reason::Storage, false)
            .await
            .unwrap();
        assert!(storage_recovered.paused && storage_recovered.acknowledged);
        let captured = set_reason(root.path(), "attempt", &Reason::Capture, false)
            .await
            .unwrap();
        assert_eq!(captured.reasons, vec![exclusive(1)]);
        assert!(captured.paused);
        let resumed = set_reason(root.path(), "attempt", &exclusive(1), false)
            .await
            .unwrap();
        assert!(!resumed.paused && resumed.acknowledged);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn lost_pause_reply_remains_durable_and_uncredited_until_confirmed() {
        let (root, _, listener) = fixture().await;
        let server = tokio::spawn(async move {
            let (_, state) = receive(listener.accept().await.unwrap().0).await;
            assert_eq!(state, "Paused");
            acknowledge(&listener, "Paused").await;
            acknowledge(&listener, "Resumed").await;
        });
        let error = set_reason(root.path(), "attempt", &exclusive(1), true)
            .await
            .unwrap_err();
        assert_eq!(error.status, 408);
        let pending = inspect(root.path(), "attempt").await.unwrap();
        assert!(pending.paused && !pending.acknowledged);
        assert_eq!(pending.administrative_suspended_ms, 0);
        let record: Record = read(&record_path(root.path(), "attempt").unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.administrative_since, None);
        assert!(record.reasons.contains(&exclusive(1)));
        assert!(
            set_reason(root.path(), "attempt", &exclusive(1), true)
                .await
                .unwrap()
                .acknowledged
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(
            set_reason(root.path(), "attempt", &exclusive(1), false)
                .await
                .unwrap()
                .administrative_suspended_ms
                >= 10
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn releasing_old_epoch_does_not_release_replacement_reservation() {
        let (root, _, listener) = fixture().await;
        let server = tokio::spawn(async move {
            acknowledge(&listener, "Paused").await;
            acknowledge(&listener, "Resumed").await;
        });
        set_reason(root.path(), "attempt", &exclusive(1), true)
            .await
            .unwrap();
        set_reason(root.path(), "attempt", &exclusive(2), true)
            .await
            .unwrap();
        set_reason(root.path(), "attempt", &exclusive(1), false)
            .await
            .unwrap();
        let duplicate = set_reason(root.path(), "attempt", &exclusive(1), false)
            .await
            .unwrap();
        assert!(duplicate.paused && duplicate.acknowledged);
        assert_eq!(duplicate.reasons, vec![exclusive(2)]);
        set_reason(root.path(), "attempt", &exclusive(2), false)
            .await
            .unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn pending_exclusivity_waits_until_guest_run_setup_finishes() {
        let root = tempfile::tempdir().unwrap();
        let vm_id = crate::config::id();
        let directory = jail(root.path(), &vm_id).unwrap();
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let listener = UnixListener::bind(directory.join("api.sock")).unwrap();
        let pending = set_reason(root.path(), "attempt", &exclusive(1), true)
            .await
            .unwrap();
        assert!(!pending.acknowledged);
        prepare_attempt(root.path(), "attempt", &vm_id)
            .await
            .unwrap();
        save(
            &root.path().join("attempt.vm.json"),
            &AttemptRecord {
                vm_id: vm_id.clone(),
                run_id: String::new(),
            },
        )
        .await
        .unwrap();
        let preparing = set_reason(root.path(), "attempt", &exclusive(1), true)
            .await
            .unwrap();
        assert!(!preparing.acknowledged);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), listener.accept())
                .await
                .is_err()
        );
        let server = tokio::spawn(async move {
            acknowledge(&listener, "Paused").await;
        });
        assert!(
            bind_attempt(root.path(), "attempt", &vm_id)
                .await
                .unwrap()
                .acknowledged
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn storage_recovery_during_setup_does_not_park_pending_administrative_work() {
        let root = tempfile::tempdir().unwrap();
        let vm_id = crate::config::id();
        let directory = jail(root.path(), &vm_id).unwrap();
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let listener = UnixListener::bind(directory.join("api.sock")).unwrap();
        prepare_attempt(root.path(), "attempt", &vm_id)
            .await
            .unwrap();
        save(
            &root.path().join("attempt.vm.json"),
            &AttemptRecord {
                vm_id: vm_id.clone(),
                run_id: String::new(),
            },
        )
        .await
        .unwrap();
        let server = tokio::spawn(async move {
            acknowledge(&listener, "Paused").await;
            acknowledge(&listener, "Resumed").await;
            acknowledge(&listener, "Paused").await;
        });
        set_reason(root.path(), "attempt", &Reason::Storage, true)
            .await
            .unwrap();
        set_reason(root.path(), "attempt", &exclusive(1), true)
            .await
            .unwrap();
        assert!(
            hold_paused(root.path(), "attempt", &exclusive(1))
                .await
                .is_err()
        );
        let recovering = set_reason(root.path(), "attempt", &Reason::Storage, false)
            .await
            .unwrap();
        assert!(!recovering.acknowledged);
        assert_eq!(recovering.administrative_suspended_ms, 0);
        let record: Record = read(&record_path(root.path(), "attempt").unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.confirmed_paused, Some(false));
        assert!(
            bind_attempt(root.path(), "attempt", &vm_id)
                .await
                .unwrap()
                .acknowledged
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn retained_setup_transfers_storage_ownership_before_guest_imports() {
        let (root, vm_id, listener) = fixture().await;
        prepare_attempt(root.path(), "replacement", &vm_id)
            .await
            .unwrap();
        save(
            &root.path().join("replacement.vm.json"),
            &AttemptRecord {
                vm_id: vm_id.clone(),
                run_id: String::new(),
            },
        )
        .await
        .unwrap();
        let server = tokio::spawn(async move {
            acknowledge(&listener, "Paused").await;
            acknowledge(&listener, "Resumed").await;
        });
        let protected = set_reason(root.path(), "replacement", &Reason::Storage, true)
            .await
            .unwrap();
        assert!(protected.paused);
        assert!(!protected.acknowledged);
        assert_eq!(
            set_reason(root.path(), "attempt", &Reason::Storage, false)
                .await
                .unwrap_err()
                .status,
            409
        );
        set_reason(root.path(), "replacement", &Reason::Storage, false)
            .await
            .unwrap();
        assert!(
            bind_attempt(root.path(), "replacement", &vm_id)
                .await
                .unwrap()
                .acknowledged
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn late_previous_attempt_cannot_resume_reused_physical_vm() {
        let (root, vm_id, listener) = fixture().await;
        save(
            &root.path().join("replacement.vm.json"),
            &AttemptRecord {
                vm_id: vm_id.clone(),
                run_id: String::new(),
            },
        )
        .await
        .unwrap();
        bind_attempt(root.path(), "replacement", &vm_id)
            .await
            .unwrap();
        let server = tokio::spawn(async move {
            acknowledge(&listener, "Paused").await;
        });
        set_reason(root.path(), "replacement", &exclusive(2), true)
            .await
            .unwrap();
        let error = set_reason(root.path(), "attempt", &exclusive(1), false)
            .await
            .unwrap_err();
        assert_eq!(error.status, 409);
        assert!(inspect(root.path(), "replacement").await.unwrap().paused);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn reclamation_guard_holds_pause_until_memory_operation_finishes() {
        let (root, _, listener) = fixture().await;
        let server = tokio::spawn(async move {
            acknowledge(&listener, "Paused").await;
            acknowledge(&listener, "Resumed").await;
        });
        set_reason(root.path(), "attempt", &exclusive(1), true)
            .await
            .unwrap();
        let guard = hold_paused(root.path(), "attempt", &exclusive(1))
            .await
            .unwrap();
        let state = root.path().to_owned();
        let release =
            tokio::spawn(async move { set_reason(&state, "attempt", &exclusive(1), false).await });
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!release.is_finished());
        drop(guard);
        assert!(!release.await.unwrap().unwrap().paused);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn revoked_execution_cannot_be_resumed_by_storage_recovery() {
        let (root, _, listener) = fixture().await;
        let server = tokio::spawn(async move {
            acknowledge(&listener, "Paused").await;
        });
        set_reason(root.path(), "attempt", &Reason::Revoked, true)
            .await
            .unwrap();
        let recovered = set_reason(root.path(), "attempt", &Reason::Storage, false)
            .await
            .unwrap();
        assert!(recovered.paused && recovered.acknowledged);
        assert_eq!(
            set_reason(root.path(), "attempt", &Reason::Revoked, false)
                .await
                .unwrap_err()
                .status,
            409
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn disappeared_binding_stops_accrual_and_reaped_vm_finalizes_it() {
        let (root, vm_id, listener) = fixture().await;
        let server = tokio::spawn(async move {
            acknowledge(&listener, "Paused").await;
        });
        set_reason(root.path(), "attempt", &exclusive(1), true)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        let directory = jail(root.path(), &vm_id).unwrap();
        finish_jail(&directory).await.unwrap();
        let finished = inspect(root.path(), "attempt").await.unwrap();
        assert!(!finished.acknowledged);
        assert!(finished.administrative_suspended_ms >= 10);
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(
            finished.administrative_suspended_ms,
            inspect(root.path(), "attempt")
                .await
                .unwrap()
                .administrative_suspended_ms
        );
        // A controller crash can remove the physical binding without running
        // finalization. Never add elapsed wall time to that orphaned record.
        let path = record_path(root.path(), "attempt").unwrap();
        let mut record: Record = read(&path).await.unwrap().unwrap();
        record.vm_id = Some(vm_id);
        record.administrative_since = Some(crate::config::now() - 60_000);
        save(&path, &record).await.unwrap();
        tokio::fs::remove_file(directory.join("suspension-owner.json"))
            .await
            .unwrap();
        assert_eq!(
            inspect(root.path(), "attempt")
                .await
                .unwrap()
                .administrative_suspended_ms,
            finished.administrative_suspended_ms
        );
        server.await.unwrap();
    }

    #[test]
    fn only_confirmed_administrative_pauses_extend_execution_budget() {
        let mut record = Record {
            confirmed_paused: Some(true),
            ..Record::default()
        };
        record.reasons.insert(Reason::Storage);
        record.account(true, 100);
        assert_eq!(record.administrative_since, None);
        record.reasons.insert(exclusive(1));
        record.account(true, 200);
        assert_eq!(record.administrative_since, Some(200));
        record.account(true, 300);
        record.reasons.remove(&exclusive(1));
        record.account(true, 400);
        assert_eq!(record.administrative_suspended_ms, 200);
        assert_eq!(record.administrative_since, None);
        record.confirmed_paused = None;
        record.reasons.insert(exclusive(2));
        record.account(true, 500);
        assert_eq!(record.administrative_since, None);
    }

    /// Local metadata measurement; the single CPU pause uses a synthetic API.
    #[tokio::test]
    #[ignore = "explicit local suspension metadata benchmark"]
    async fn suspension_metadata_measurements() {
        use std::{
            io::{Read, Seek},
            os::fd::AsRawFd,
            time::Instant,
        };

        fn parent_cpu_ms() -> f64 {
            let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
            assert_eq!(
                unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
                0
            );
            let usage = unsafe { usage.assume_init() };
            (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as f64 * 1000.0
                + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1000.0
        }

        fn distribution(mut samples: Vec<f64>) -> serde_json::Value {
            samples.sort_by(f64::total_cmp);
            let count = samples.len();
            serde_json::json!({
                "medianMs": samples[count / 2],
                "p95Ms": samples[count * 95 / 100],
                "meanMs": samples.iter().sum::<f64>() / count as f64,
                "samples": count
            })
        }

        fn reads(path: &Path) -> serde_json::Value {
            let size = std::fs::metadata(path).unwrap().len() as usize;
            let mut bytes = vec![0; size];
            let mut file = std::fs::File::open(path).unwrap();
            let mut warm = Vec::new();
            for _ in 0..500 {
                let started = Instant::now();
                file.rewind().unwrap();
                file.read_exact(&mut bytes).unwrap();
                warm.push(started.elapsed().as_secs_f64() * 1000.0);
            }
            let mut fresh = Vec::new();
            for _ in 0..500 {
                // Advisory only: tmpfs retains its backing data. This does not
                // claim a physically cold disk or production network read.
                unsafe {
                    libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
                }
                let started = Instant::now();
                let mut fresh_file = std::fs::File::open(path).unwrap();
                fresh_file.read_exact(&mut bytes).unwrap();
                fresh.push(started.elapsed().as_secs_f64() * 1000.0);
            }
            serde_json::json!({
                "encodedBytes": size,
                "warmRead": distribution(warm),
                "freshDescriptorRead": distribution(fresh)
            })
        }

        let (root, vm_id, listener) = fixture().await;
        let server = tokio::spawn(async move {
            acknowledge(&listener, "Paused").await;
        });
        set_reason(root.path(), "attempt", &Reason::Storage, true)
            .await
            .unwrap();
        server.await.unwrap();
        let reference_path = root.path().join("attempt.vm.json");
        let reference = AttemptRecord {
            vm_id: vm_id.clone(),
            run_id: String::new(),
        };
        let mut reference_writes = Vec::new();
        let reference_cpu_started = parent_cpu_ms();
        for _ in 0..100 {
            let started = Instant::now();
            save(&reference_path, &reference).await.unwrap();
            reference_writes.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let reference_cpu = parent_cpu_ms() - reference_cpu_started;
        let mut writes = Vec::new();
        let cpu_started = parent_cpu_ms();
        for index in 0..100 {
            let started = Instant::now();
            set_reason(root.path(), "attempt", &exclusive(1), index % 2 == 0)
                .await
                .unwrap();
            writes.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let cpu = parent_cpu_ms() - cpu_started;
        let reference_reads = reads(&reference_path);
        let suspension_reads = reads(&record_path(root.path(), "attempt").unwrap());
        let private_owner_reads = reads(
            &jail(root.path(), &vm_id)
                .unwrap()
                .join("suspension-owner.json"),
        );
        let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        let cpu_model = cpuinfo
            .lines()
            .find_map(|line| line.strip_prefix("model name\t: "))
            .unwrap_or("unknown");
        println!(
            "{}",
            serde_json::json!({
                "benchmark": "suspension-metadata",
                "workload": "100 atomic fsynced reference writes; 100 alternating exclusive reason mutations while storage fence stays acknowledged; 500 warm and 500 fresh descriptor reads per metadata file",
                "coldReadPolicy": "POSIX_FADV_DONTNEED advisory before fresh descriptor read; OS cache may remain warm, especially on tmpfs",
                "api": "one synthetic local Firecracker pause before timing; zero VM state requests during measured mutations",
                "host": {
                    "cpuModel": cpu_model,
                    "availableParallelism": std::thread::available_parallelism().map_or(0, std::num::NonZeroUsize::get),
                    "kernel": std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default().trim(),
                    "memoryTotal": std::fs::read_to_string("/proc/meminfo").unwrap_or_default().lines().find(|line| line.starts_with("MemTotal:")).unwrap_or("unknown")
                },
                "reference": { "read": reference_reads, "write": distribution(reference_writes), "writeParentCpuMs": reference_cpu },
                "suspension": { "read": suspension_reads, "write": distribution(writes), "writeParentCpuMs": cpu },
                "privateOwner": private_owner_reads
            })
        );
    }
}
