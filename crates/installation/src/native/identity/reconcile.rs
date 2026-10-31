//! Ownership changes clear set-ID bits and file capabilities. Journal original
//! metadata before chown, then replay it after a crash before advancing the map.
use super::{Identity, translate};
use crate::native::bootstrap::{Cost, Counter, Trace};
use crate::{
    error::{Error, Result},
    native::{atomic_write, block},
};
use serde::{Deserialize, Serialize};
use std::{
    ffi::{CString, OsString},
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{MetadataExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
};

const MARKER: &str = ".cairn-identity.json";
const TRANSITION: &str = ".cairn-identity.transition.json";
const PENDING: &str = ".cairn-identity.pending.json";

const ATTRIBUTES: [&str; 3] = [
    "system.posix_acl_access",
    "system.posix_acl_default",
    "security.capability",
];

#[derive(Deserialize, Serialize)]
struct Transition {
    previous: Option<Identity>,
    target: Identity,
}

#[derive(Deserialize, Serialize)]
struct Entry {
    relative: Vec<u8>,
    inode: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    symlink: bool,
    attributes: Vec<(String, Vec<u8>)>,
    needs_apply: bool,
}

fn cpath(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(Error::internal)
}

fn attribute(path: &Path, name: &str) -> Result<Option<Vec<u8>>> {
    let path = cpath(path)?;
    let name = CString::new(name).map_err(Error::internal)?;
    let mut bytes = vec![0; 65536];
    let size = unsafe {
        libc::lgetxattr(
            path.as_ptr(),
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    if size < 0 {
        let error = std::io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(libc::ENODATA | libc::EOPNOTSUPP)) {
            return Ok(None);
        }
        return Err(error.into());
    }
    bytes.truncate(size as usize);
    Ok(Some(bytes))
}

fn mapped(id: u32, group: bool, previous: Option<&Identity>, target: &Identity) -> u32 {
    match previous {
        Some(old) if group && id == old.gid() => target.gid(),
        Some(old) if !group && id == old.uid() => target.uid(),
        Some(old) if group => {
            translate(id, old.root_gid, old.subgid, target.root_gid, target.subgid)
        }
        Some(old) => translate(id, old.root_uid, old.subuid, target.root_uid, target.subuid),
        None if group => target.gid(),
        None => target.uid(),
    }
}

fn remap_attribute(
    name: &str,
    bytes: &mut [u8],
    previous: Option<&Identity>,
    target: &Identity,
) -> Result<()> {
    // A legacy disk has no namespace mapping. Its ACL qualifiers are opaque
    // host IDs, not all aliases of the file owner; retain their exact values.
    if previous.is_none() {
        return Ok(());
    }
    if name == "security.capability" {
        if bytes.len() < 4 {
            return Err(Error::conflict("Invalid persisted file capability."));
        }
        let revision = u32::from_le_bytes(bytes[..4].try_into().unwrap()) & 0xff000000;
        match (revision, bytes.len()) {
            (0x03000000, 24) => {
                let root = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
                let root = mapped(root, false, previous, target);
                bytes[20..24].copy_from_slice(&root.to_le_bytes());
            }
            (0x02000000, 20) | (0x01000000, 12) => (),
            _ => return Err(Error::conflict("Unsupported persisted file capability.")),
        }
    } else {
        if bytes.len() < 4
            || !(bytes.len() - 4).is_multiple_of(8)
            || bytes[..4] != 2u32.to_le_bytes()
        {
            return Err(Error::conflict("Invalid persisted POSIX ACL."));
        }
        for entry in bytes[4..].chunks_exact_mut(8) {
            let tag = u16::from_le_bytes(entry[..2].try_into().unwrap());
            if tag == 2 || tag == 8 {
                let id = u32::from_le_bytes(entry[4..8].try_into().unwrap());
                let id = mapped(id, tag == 8, previous, target);
                entry[4..8].copy_from_slice(&id.to_le_bytes());
            }
        }
    }
    Ok(())
}

fn capture(data: &Path, path: &Path, transition: &Transition) -> Result<Entry> {
    let metadata = std::fs::symlink_metadata(path)?;
    let mut attributes = Vec::new();
    let mut changed = false;
    if !metadata.file_type().is_symlink() {
        for name in ATTRIBUTES {
            if let Some(mut bytes) = attribute(path, name)? {
                let original = bytes.clone();
                remap_attribute(
                    name,
                    &mut bytes,
                    transition.previous.as_ref(),
                    &transition.target,
                )?;
                changed |= original != bytes;
                attributes.push((name.to_owned(), bytes));
            }
        }
    }
    let uid = mapped(
        metadata.uid(),
        false,
        transition.previous.as_ref(),
        &transition.target,
    );
    let gid = mapped(
        metadata.gid(),
        true,
        transition.previous.as_ref(),
        &transition.target,
    );
    let needs_apply = changed || uid != metadata.uid() || gid != metadata.gid();
    Ok(Entry {
        relative: path
            .strip_prefix(data)
            .map_err(Error::internal)?
            .as_os_str()
            .as_bytes()
            .to_vec(),
        inode: metadata.ino(),
        uid,
        gid,
        mode: metadata.mode(),
        symlink: metadata.file_type().is_symlink(),
        attributes,
        needs_apply,
    })
}

fn apply(data: &Path, entry: &Entry) -> Result<()> {
    let relative = PathBuf::from(OsString::from_vec(entry.relative.clone()));
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(Error::conflict("Invalid ownership journal path."));
    }
    let path = data.join(relative);
    let metadata = std::fs::symlink_metadata(&path)?;
    if metadata.ino() != entry.inode || metadata.file_type().is_symlink() != entry.symlink {
        return Err(Error::conflict(
            "Ownership journal inode changed while the disk was stopped.",
        ));
    }
    if metadata.uid() != entry.uid || metadata.gid() != entry.gid {
        std::os::unix::fs::lchown(&path, Some(entry.uid), Some(entry.gid))?;
    }
    if entry.symlink {
        return Ok(());
    }
    // ACLs precede chmod; file capabilities must be restored after chmod/chown.
    for (name, bytes) in entry
        .attributes
        .iter()
        .filter(|(name, _)| name != "security.capability")
    {
        set_attribute(&path, name, bytes)?;
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(entry.mode))?;
    for (name, bytes) in entry
        .attributes
        .iter()
        .filter(|(name, _)| name == "security.capability")
    {
        set_attribute(&path, name, bytes)?;
    }
    Ok(())
}

fn set_attribute(path: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let path = cpath(path)?;
    let name = CString::new(name).map_err(Error::internal)?;
    if unsafe {
        libc::lsetxattr(
            path.as_ptr(),
            name.as_ptr(),
            bytes.as_ptr().cast(),
            bytes.len(),
            0,
        )
    } < 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

async fn publish(path: &Path, bytes: &[u8], observation: &Trace) -> Result<()> {
    observation
        .measure(Cost::JournalPublication, atomic_write(path, bytes))
        .await?;
    observation.add(Counter::DirectorySyncsCompleted, 1);
    Ok(())
}

async fn remove(data: &Path, name: &str, observation: &Trace) -> Result<()> {
    tokio::fs::remove_file(data.join(name)).await?;
    tokio::fs::File::open(data).await?.sync_all().await?;
    observation.add(Counter::DirectorySyncsCompleted, 1);
    Ok(())
}

async fn replay(data: &Path, observation: &Trace) -> Result<()> {
    let bytes = match observation
        .measure(Cost::PendingReadback, tokio::fs::read(data.join(PENDING)))
        .await
    {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    observation.add(Counter::PendingBytesRead, bytes.len() as u64);
    let entries: Vec<Entry> = serde_json::from_slice(&bytes)?;
    for entry in entries {
        let _operation = observation.operation(Cost::Apply);
        apply(data, &entry)?;
        observation.add(Counter::EntriesApplied, 1);
    }
    observation.measure(Cost::Syncfs, block::sync(data)).await?;
    observation.add(Counter::SyncfsCompleted, 1);
    observation
        .measure(Cost::PendingRetire, remove(data, PENDING, observation))
        .await?;
    observation.add(Counter::PendingRetired, 1);
    Ok(())
}

async fn migrate(data: &Path, transition: &Transition, observation: &Trace) -> Result<()> {
    replay(data, observation).await?;
    let mut pending = vec![data.to_owned()];
    let mut batch = Vec::new();
    while let Some(path) = pending.pop() {
        {
            let _operation = observation.operation(Cost::Scan);
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                for entry in std::fs::read_dir(&path)? {
                    let entry = entry?;
                    if path == data
                        && [MARKER, TRANSITION, PENDING]
                            .iter()
                            .any(|name| entry.file_name() == *name)
                    {
                        continue;
                    }
                    pending.push(entry.path());
                }
            }
        }
        observation.add(Counter::EntriesSeen, 1);
        let entry = {
            let _operation = observation.operation(Cost::Capture);
            capture(data, &path, transition)?
        };
        // On retries an already migrated inode needs no new transaction. The
        // durable pending batch has already restored its original mode/xattrs.
        if entry.needs_apply {
            observation.add(Counter::EntriesChanged, 1);
            batch.push(entry);
        }
        if batch.len() == 32 || pending.is_empty() && !batch.is_empty() {
            let bytes = serde_json::to_vec(&batch)?;
            publish(&data.join(PENDING), &bytes, observation).await?;
            observation.add(Counter::PendingBytesWritten, bytes.len() as u64);
            observation.add(Counter::BatchesPublished, 1);
            replay(data, observation).await?;
            batch.clear();
        }
    }
    publish(
        &data.join(MARKER),
        &serde_json::to_vec(&transition.target)?,
        observation,
    )
    .await?;
    remove(data, TRANSITION, observation).await
}

/// The transition itself travels with the disk. Complete a partially applied
/// mapping before starting another, even when recovery happens on a third node.
pub(in crate::native) async fn reconcile(
    data: &Path,
    identity: &Identity,
    observation: &Trace,
) -> Result<()> {
    match tokio::fs::read(data.join(TRANSITION)).await {
        Ok(bytes) => {
            observation.add(Counter::ReplayTransition, 1);
            migrate(
                data,
                &serde_json::from_slice::<Transition>(&bytes)?,
                observation,
            )
            .await?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let previous = match tokio::fs::read(data.join(MARKER)).await {
        Ok(bytes) => Some(serde_json::from_slice::<Identity>(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if previous.as_ref() == Some(identity) {
        observation.add(Counter::MarkerMatch, 1);
        return Ok(());
    }
    let transition = Transition {
        previous,
        target: identity.clone(),
    };
    publish(
        &data.join(TRANSITION),
        &serde_json::to_vec(&transition)?,
        observation,
    )
    .await?;
    observation.add(Counter::NewTransition, 1);
    migrate(data, &transition, observation).await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn reconcile(data: &Path, identity: &Identity) -> Result<()> {
        super::reconcile(data, identity, &Trace::default()).await
    }

    fn count(report: &serde_json::Value, counter: Counter) -> u64 {
        let counter = serde_json::to_value(counter).unwrap();
        report["counters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|value| value["counter"] == counter)
            .unwrap()["value"]
            .as_u64()
            .unwrap()
    }

    #[tokio::test]
    async fn observation_counts_real_batches_and_preserves_the_identity_fast_path() {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path();
        let previous = Identity {
            user: "old-fixture".into(),
            root_uid: 900,
            root_gid: 901,
            subuid: 100000,
            subgid: 100000,
            host_engine: None,
        };
        let target = Identity {
            user: "new-fixture".into(),
            root_uid: 902,
            root_gid: 903,
            subuid: 200000,
            subgid: 200000,
            host_engine: None,
        };
        atomic_write(&data.join(MARKER), &serde_json::to_vec(&previous).unwrap())
            .await
            .unwrap();
        let mut acl = 2u32.to_le_bytes().to_vec();
        for (tag, permissions, id) in [
            (1u16, 6u16, u32::MAX),
            (2, 4, 100010),
            (4, 4, u32::MAX),
            (16, 4, u32::MAX),
            (32, 0, u32::MAX),
        ] {
            acl.extend(tag.to_le_bytes());
            acl.extend(permissions.to_le_bytes());
            acl.extend(id.to_le_bytes());
        }
        for index in 0..33 {
            let path = data.join(format!("entry-{index}"));
            std::fs::write(&path, b"unchanged payload").unwrap();
            set_attribute(&path, "system.posix_acl_access", &acl).unwrap();
        }
        let registry = crate::native::bootstrap::Registry::new("fixture-runtime".into(), None);
        let id = crate::config::id();
        let scope = registry.begin(&id);
        super::reconcile(data, &target, &scope.trace())
            .await
            .unwrap();
        let report = serde_json::to_value(registry.get(&id).unwrap()).unwrap();
        assert_eq!(count(&report, Counter::EntriesSeen), 34);
        assert_eq!(count(&report, Counter::EntriesChanged), 33);
        assert_eq!(count(&report, Counter::EntriesApplied), 33);
        assert_eq!(count(&report, Counter::BatchesPublished), 2);
        assert_eq!(count(&report, Counter::SyncfsCompleted), 2);
        assert_eq!(count(&report, Counter::PendingRetired), 2);
        assert_eq!(count(&report, Counter::DirectorySyncsCompleted), 7);
        assert_eq!(
            count(&report, Counter::PendingBytesWritten),
            count(&report, Counter::PendingBytesRead)
        );
        assert!(count(&report, Counter::PendingBytesRead) > 0);
        assert!(!data.join(PENDING).exists());
        assert!(!data.join(TRANSITION).exists());
        assert_eq!(
            std::fs::read(data.join("entry-0")).unwrap(),
            b"unchanged payload"
        );
        let migrated_acl = attribute(&data.join("entry-0"), "system.posix_acl_access")
            .unwrap()
            .unwrap();
        assert_eq!(
            u32::from_le_bytes(migrated_acl[16..20].try_into().unwrap()),
            200010
        );

        let second_id = crate::config::id();
        let second = registry.begin(&second_id);
        super::reconcile(data, &target, &second.trace())
            .await
            .unwrap();
        let fast = serde_json::to_value(registry.get(&second_id).unwrap()).unwrap();
        assert_eq!(count(&fast, Counter::MarkerMatch), 1);
        assert_eq!(count(&fast, Counter::EntriesSeen), 0);
        assert_eq!(count(&fast, Counter::BatchesPublished), 0);
        assert_eq!(count(&fast, Counter::SyncfsCompleted), 0);
    }

    #[test]
    fn capability_and_acl_ids_follow_the_namespace_mapping() {
        let old = Identity {
            user: "old".into(),
            root_uid: 900,
            root_gid: 901,
            subuid: 100000,
            subgid: 200000,
            host_engine: None,
        };
        let target = Identity {
            user: "new".into(),
            root_uid: 902,
            root_gid: 903,
            subuid: 300000,
            subgid: 400000,
            host_engine: None,
        };
        let mut capability = vec![0; 24];
        capability[..4].copy_from_slice(&0x03000001u32.to_le_bytes());
        capability[20..].copy_from_slice(&900u32.to_le_bytes());
        remap_attribute("security.capability", &mut capability, Some(&old), &target).unwrap();
        assert_eq!(&capability[20..], &902u32.to_le_bytes());
        let mut acl = vec![0; 12];
        acl[..4].copy_from_slice(&2u32.to_le_bytes());
        acl[4..6].copy_from_slice(&8u16.to_le_bytes());
        acl[8..].copy_from_slice(&200999u32.to_le_bytes());
        remap_attribute("system.posix_acl_access", &mut acl, Some(&old), &target).unwrap();
        assert_eq!(&acl[8..], &400999u32.to_le_bytes());
    }

    #[test]
    fn legacy_acl_qualifiers_are_preserved_without_collapsing_distinct_users() {
        let target = Identity {
            user: "new".into(),
            root_uid: 902,
            root_gid: 903,
            subuid: 300000,
            subgid: 400000,
            host_engine: None,
        };
        let mut acl = vec![0; 20];
        acl[..4].copy_from_slice(&2u32.to_le_bytes());
        for (entry, id) in acl[4..].chunks_exact_mut(8).zip([800u32, 801]) {
            entry[..2].copy_from_slice(&2u16.to_le_bytes());
            entry[4..].copy_from_slice(&id.to_le_bytes());
        }
        let original = acl.clone();
        remap_attribute("system.posix_acl_access", &mut acl, None, &target).unwrap();
        assert_eq!(acl, original);
    }

    #[test]
    fn administrator_engine_mapping_preserves_namespace_volume_users() {
        let protected = Identity {
            user: "fixture".into(),
            root_uid: 500000000,
            root_gid: 500000001,
            subuid: 100000,
            subgid: 200000,
            host_engine: None,
        };
        let administrator = Identity {
            host_engine: Some((998, 997)),
            ..protected.clone()
        };
        for (group, engine, root, start, host_engine) in [
            (
                false,
                protected.uid(),
                protected.root_uid,
                protected.subuid,
                administrator.uid(),
            ),
            (
                true,
                protected.gid(),
                protected.root_gid,
                protected.subgid,
                administrator.gid(),
            ),
        ] {
            assert_eq!(
                mapped(engine, group, Some(&protected), &administrator),
                host_engine
            );
            assert_eq!(
                mapped(host_engine, group, Some(&administrator), &protected),
                engine
            );
            for owner in [root, start, start + 998, start + 1000, start + 65535] {
                assert_eq!(
                    mapped(owner, group, Some(&protected), &administrator),
                    owner
                );
                assert_eq!(
                    mapped(owner, group, Some(&administrator), &protected),
                    owner
                );
            }
        }
        let mut acl = vec![0; 12];
        acl[..4].copy_from_slice(&2u32.to_le_bytes());
        acl[4..6].copy_from_slice(&2u16.to_le_bytes());
        acl[8..12].copy_from_slice(&protected.uid().to_le_bytes());
        let original = acl.clone();
        remap_attribute(
            "system.posix_acl_access",
            &mut acl,
            Some(&protected),
            &administrator,
        )
        .unwrap();
        assert_eq!(&acl[8..12], &administrator.uid().to_le_bytes());
        remap_attribute(
            "system.posix_acl_access",
            &mut acl,
            Some(&administrator),
            &protected,
        )
        .unwrap();
        assert_eq!(acl, original);
        assert!(administrator.verify_maps("", "").is_err());
    }

    #[tokio::test]
    async fn home_refresh_replays_pending_credential_metadata_before_replacing_its_inode() {
        // A service account below UID/GID999 cannot own the synthetic engine
        // identity without privilege. Use the repository's metadata-only tool
        // on those hosts; ordinary CI UIDs exercise real same-owner operations.
        if unsafe { libc::geteuid() } != 0
            && (unsafe { libc::getuid() } < 999 || unsafe { libc::getgid() } < 999)
        {
            let module = module_path!().split_once("::").unwrap().1;
            let status = tokio::process::Command::new("fakeroot")
                .arg("--")
                .arg(std::env::current_exe().unwrap())
                .arg(format!("{module}::home_refresh_replays_pending_credential_metadata_before_replacing_its_inode"))
                .args(["--exact", "--nocapture"])
                .status()
                .await
                .expect("fakeroot is required for metadata tests under low-numbered service accounts");
            assert!(status.success());
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("data");
        let source = directory.path().join("stage");
        crate::skills::private_dir(&data.join("home/.codex"))
            .await
            .unwrap();
        crate::skills::private_dir(&source.join(".codex"))
            .await
            .unwrap();
        std::fs::write(source.join(".codex/auth.json"), b"synthetic-runtime-only").unwrap();
        let target = data.join("home/.codex/auth.json");
        std::fs::write(&target, b"").unwrap();
        std::fs::write(data.join("home/notes"), b"durable user file").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        let metadata = std::fs::metadata(&target).unwrap();
        let identity = Identity {
            user: "fixture".into(),
            root_uid: metadata.uid() + 131072,
            root_gid: metadata.gid() + 131072,
            subuid: metadata.uid().saturating_sub(999),
            subgid: metadata.gid().saturating_sub(999),
            host_engine: None,
        };
        let transition = Transition {
            previous: Some(identity.clone()),
            target: identity.clone(),
        };
        let entry = capture(&data, &target, &transition).unwrap();
        // An already matching marker must not skip a still-pending transition.
        atomic_write(&data.join(MARKER), &serde_json::to_vec(&identity).unwrap())
            .await
            .unwrap();
        atomic_write(
            &data.join(TRANSITION),
            &serde_json::to_vec(&transition).unwrap(),
        )
        .await
        .unwrap();
        atomic_write(
            &data.join(PENDING),
            &serde_json::to_vec(&vec![entry]).unwrap(),
        )
        .await
        .unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        // Holding this inode prevents unlink/create from accidentally reusing it.
        let old_target = std::fs::File::open(&target).unwrap();

        let registry = crate::native::bootstrap::Registry::new("fixture-runtime".into(), None);
        let id = crate::config::id();
        let scope = registry.begin(&id);
        let bindings =
            crate::native::home::prepare_for_identity(&source, &data, &identity, &scope.trace())
                .await
                .unwrap();

        assert_eq!(old_target.metadata().unwrap().mode() & 0o7777, 0o640);
        let fresh = std::fs::metadata(&target).unwrap();
        assert_ne!(fresh.ino(), old_target.metadata().unwrap().ino());
        assert_eq!((fresh.uid(), fresh.gid()), (identity.uid(), identity.gid()));
        assert!(std::fs::read(&target).unwrap().is_empty());
        assert_eq!(
            std::fs::read(data.join("home/notes")).unwrap(),
            b"durable user file"
        );
        assert!(bindings.iter().any(|binding| binding.target
            == Path::new("/home/node/.codex/auth.json")
            && binding.read_only));
        assert!(!data.join(PENDING).exists());
        assert!(!data.join(TRANSITION).exists());
        let report = serde_json::to_value(registry.get(&id).unwrap()).unwrap();
        assert_eq!(count(&report, Counter::ReplayTransition), 1);
        assert_eq!(count(&report, Counter::MarkerMatch), 1);
        assert_eq!(count(&report, Counter::NewTransition), 0);
        assert_eq!(count(&report, Counter::SyncfsCompleted), 1);
        assert_eq!(
            serde_json::from_slice::<Identity>(&std::fs::read(data.join(MARKER)).unwrap()).unwrap(),
            identity
        );
    }

    #[tokio::test]
    async fn pending_metadata_replay_restores_mode_even_after_ownership_changed() {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path();
        let path = data.join("volume");
        std::fs::write(&path, b"durable").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let metadata = std::fs::metadata(&path).unwrap();
        let identity = Identity {
            user: "fixture".into(),
            root_uid: metadata.uid(),
            root_gid: metadata.gid(),
            subuid: 100000,
            subgid: 200000,
            host_engine: None,
        };
        let entry = capture(
            data,
            &path,
            &Transition {
                previous: Some(identity.clone()),
                target: identity,
            },
        )
        .unwrap();
        atomic_write(
            &data.join(PENDING),
            &serde_json::to_vec(&vec![entry]).unwrap(),
        )
        .await
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        replay(data, &Trace::default()).await.unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o7777, 0o640);
        assert!(!data.join(PENDING).exists());
    }
}
