//! New empty ext4 images become journal-backed before any user command runs.
use super::{Disk, layout::Layout};
use crate::{
    error::{Error, Result},
    validation::text,
};
use serde_json::Value;
use std::path::Path;

pub async fn prepare(
    directory: &Path,
    size: u64,
    context: &Value,
    stop: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    prepare_source(directory, size, context, stop, false, None).await
}

/// Build an anonymous, entirely local disk without minting or borrowing any
/// conversation's S3 authority. Only prepared environments may use this path.
pub async fn prepare_unassigned(
    directory: &Path,
    size: u64,
    policy: &super::policy::Policy,
    stop: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    if directory
        .parent()
        .and_then(Path::file_name)
        .is_none_or(|name| name != "environments")
        || super::runtime::exists(directory)
    {
        return Err(Error::conflict(
            "Anonymous preparation requires a new environment disk.",
        ));
    }
    crate::validation::uuid(
        directory
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(""),
    )?;
    let context = serde_json::json!({ "unassigned": true, "policy": policy });
    prepare_source(directory, size, &context, stop, true, None).await
}

fn selected_layout(size: u64) -> Result<Layout> {
    match std::env::var("CAIRN_DISK_LAYOUT")
        .as_deref()
        .unwrap_or("flat-ext4-v1")
    {
        "flat-ext4-v1" => Ok(Layout::Flat),
        "paired-ext4-v1" => {
            if !cfg!(feature = "ublk")
                || std::env::var("CAIRN_BLOCK_TRANSPORT").as_deref() != Ok("ublk")
            {
                return Err(Error::bad(
                    "Paired disks require the native ublk transport.",
                ));
            }
            // Small legacy resource requests retain their single filesystem.
            if size < 256 * 1024 * 1024 {
                return Ok(Layout::Flat);
            }
            Ok(Layout::paired(size)?)
        }
        _ => Err(Error::bad("Unsupported conversation disk layout.")),
    }
}

async fn prepare_source(
    directory: &Path,
    size: u64,
    context: &Value,
    stop: &tokio_util::sync::CancellationToken,
    prepared: bool,
    requested_layout: Option<Layout>,
) -> Result<()> {
    crate::skills::private_dir(directory).await?;
    let _lock = crate::file_lock::exclusive(&directory.join("lock"), "VM disk is active.")?;
    let marker = directory.join("bootstrap.pending");
    let resize_source = directory.join("resize-source");
    let mut context = context.clone();
    let mut generation = 1;
    let mut layout = Layout::Flat;
    let mut resizing = false;
    if resize_source.exists() && !super::runtime::exists(directory) {
        // Never continue from a raw image that resize2fs may have only partly
        // changed. The original journal stays authoritative until installation.
        tokio::fs::rename(&resize_source, directory.join("lazy")).await?;
        tokio::fs::File::open(directory).await?.sync_all().await?;
    }
    if super::runtime::exists(directory) {
        if marker.exists() {
            // A crash after the atomic install must still reclaim the empty source.
            let _volume = super::runtime::load(directory).await?;
            if directory.join("data.ext4").exists() {
                tokio::fs::remove_file(directory.join("data.ext4")).await?;
            }
            tokio::fs::remove_file(&marker).await?;
            tokio::fs::File::open(directory).await?.sync_all().await?;
        }
        if resize_source.exists() {
            let _volume = super::runtime::load(directory).await?;
            tokio::fs::remove_dir_all(&resize_source).await?;
        }
        let volume = super::runtime::load(directory).await?;
        let current = volume.disk.size();
        layout = volume.disk.layout()?;
        drop(volume);
        if size < current {
            return Err(Error::conflict("A retained VM disk cannot be shrunk."));
        }
        if size == current {
            return Ok(());
        }
        let (previous, next_generation) = super::runtime::rebuild_identity(directory).await?;
        context["grant"] = previous["grant"].clone();
        generation = next_generation;
        // Resize tools still require a local ext4 image. Mark the transition
        // before exporting so a restart can finish rebuilding the journal.
        crate::skills::atomic_write(&marker, b"resizing").await?;
        super::runtime::materialize(directory, stop).await?;
        resizing = true;
    }
    if directory.join("data.ext4").exists() && !marker.exists() {
        return Err(Error::conflict(
            "This conversation still has a legacy local disk and cannot start with S3-backed storage.",
        ));
    }
    let policy: super::policy::Policy = serde_json::from_value(context["policy"].clone())?;
    policy.validate()?;
    let (total, free) = super::policy::space(directory)?;
    if free <= policy.reserve(total) + 64 * 1024 * 1024 {
        return Err(Error::new(
            507,
            "Free disk reserve prevents creating a new environment.",
        ));
    }
    if !resizing {
        layout = if marker.exists() {
            let bytes = tokio::fs::read(&marker).await?;
            if bytes == b"initializing" {
                Layout::Flat
            } else {
                if bytes == b"resizing" {
                    return Err(Error::conflict(
                        "Disk resize requires its original journal.",
                    ));
                }
                let pending: Value = serde_json::from_slice(&bytes)?;
                if pending["phase"] != "initializing" {
                    return Err(Error::conflict(
                        "Disk resize requires its original journal.",
                    ));
                }
                serde_json::from_value(pending["layout"].clone())?
            }
        } else {
            requested_layout.map_or_else(|| selected_layout(size), Ok)?
        };
        layout = layout.grown(size)?;
        let pending = serde_json::json!({ "phase": "initializing", "layout": layout });
        crate::skills::atomic_write(&marker, &serde_json::to_vec(&pending)?).await?;
        // No VM can have run before the journal's atomic installation. Rebuild
        // a partial first bootstrap rather than guessing its filesystem offsets.
        if directory.join("data.ext4").exists() {
            tokio::fs::remove_file(directory.join("data.ext4")).await?;
        }
    }
    let (raw, layout) = super::local::prepare_layout(directory, size, layout).await?;
    let mut manifest = crate::nodes::snapshots::index(&raw).await?;
    layout.store(&mut manifest)?;
    let staging = tempfile::Builder::new()
        .prefix("bootstrap-")
        .tempdir_in(directory)?;
    let root = staging.path().to_owned();
    let mut empty = manifest.clone();
    for block in empty["blocks"].as_array_mut().unwrap() {
        block["hash"] = Value::Null;
    }
    let disk = if prepared {
        super::runtime::create_prepared(&root, &empty, &context).await?
    } else {
        super::runtime::create_at_generation(&root, &empty, &context, generation).await?
    };
    let writer = disk.clone();
    tokio::task::spawn_blocking(move || copy_blocks(&raw, &manifest, &writer, &policy))
        .await
        .map_err(Error::internal)??;
    drop(disk);
    tokio::fs::rename(&root, directory.join("lazy")).await?;
    tokio::fs::File::open(directory).await?.sync_all().await?;
    // The complete image is now in the durable, non-evictable journal.
    tokio::fs::remove_file(directory.join("data.ext4")).await?;
    tokio::fs::remove_file(marker).await?;
    tokio::fs::File::open(directory).await?.sync_all().await?;
    if resize_source.exists() {
        tokio::fs::remove_dir_all(resize_source).await?;
    }
    Ok(())
}

/// Build an offline local-only journal for an anonymous VM's immutable prefix.
/// The caller has frozen/paused the template, and never assigns it a grant.
pub(crate) async fn prepare_template(
    directory: &Path,
    source: std::sync::Arc<super::runtime::Volume>,
    generation: i64,
    policy: &super::policy::Policy,
    stop: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    if source.source.authorization()?.is_some() || directory.join("lazy").exists() {
        return Err(Error::conflict(
            "A template requires a new anonymous journal.",
        ));
    }
    policy.validate()?;
    crate::skills::private_dir(directory).await?;
    let source_disk = source.disk.clone();
    let manifest = tokio::task::spawn_blocking(move || source_disk.capture(generation))
        .await
        .map_err(Error::internal)??;
    crate::nodes::snapshots::validate(&manifest)?;
    let mut empty = manifest.clone();
    for block in empty["blocks"].as_array_mut().unwrap() {
        block["hash"] = Value::Null;
    }
    let context = serde_json::json!({ "unassigned": true, "policy": policy });
    let disk = super::runtime::create_prepared(&directory.join("lazy"), &empty, &context).await?;
    let writer = disk.clone();
    let policy = policy.clone();
    let stop = stop.clone();
    let parent = directory.to_owned();
    tokio::task::spawn_blocking(move || -> Result<()> {
        for block in manifest["blocks"].as_array().unwrap() {
            if stop.is_cancelled() {
                return Err(Error::unavailable("Template preparation stopped."));
            }
            let Some(hash) = block["hash"].as_str() else {
                continue;
            };
            let bytes = source.disk.captured_block(generation, hash)?;
            let (total, free) = super::policy::space(&parent)?;
            if free <= policy.reserve(total) + bytes.len() as u64 * 4 + 1048576 {
                return Err(Error::new(
                    507,
                    "Free disk reserve prevents creating a template.",
                ));
            }
            writer.write_at(block["offset"].as_u64().unwrap(), &bytes)?;
        }
        writer.sync()?;
        Ok(())
    })
    .await
    .map_err(Error::internal)??;
    // The last SQLite connection closes before the journal becomes an offline
    // template; future clones copy complete files, never a live DB/WAL pair.
    drop(disk);
    tokio::fs::File::open(directory).await?.sync_all().await?;
    Ok(())
}

/// Copies the image's non-empty blocks into the journal, keeping the free-space reserve.
fn copy_blocks(
    raw: &Path,
    manifest: &Value,
    writer: &super::LazyDisk,
    policy: &super::policy::Policy,
) -> Result<()> {
    let input = super::LocalDisk::open(raw, false)?;
    for block in manifest["blocks"].as_array().unwrap() {
        if text(block, "hash").is_empty() {
            continue;
        }
        let offset = block["offset"].as_u64().unwrap();
        let mut bytes = vec![0; block["size"].as_u64().unwrap() as usize];
        let _admission = super::cache::admission()?;
        let (total, free) = super::policy::space(raw.parent().unwrap())?;
        if free <= policy.reserve(total) + bytes.len() as u64 * 4 + 1048576 {
            return Err(Error::new(
                507,
                "Free disk reserve prevents initializing the journal.",
            ));
        }
        input.read_at(offset, &mut bytes)?;
        writer.write_at(offset, &bytes)?;
    }
    writer.sync()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn offline_template_keeps_the_sealed_prefix_without_remote_authority() {
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("environments/original");
        let size = 256 * 1024 * 1024;
        let policy: super::super::policy::Policy = serde_json::from_value(json!({
            "reserveMiB": 64,
            "reservePercent": 1
        }))
        .unwrap();
        let context = json!({ "unassigned": true, "policy": policy });
        let stop = CancellationToken::new();
        let layout = Layout::paired(size).unwrap();
        prepare_source(&original, size, &context, &stop, true, Some(layout))
            .await
            .unwrap();
        let volume = super::super::runtime::load(&original).await.unwrap();
        let offset = 16 * 1024 * 1024;
        volume.write_at(offset, b"template-prefix").unwrap();
        let generation = volume.seal().await.unwrap();
        volume.write_at(offset, b"later-mutation!").unwrap();
        let template = root.path().join("environments/template");
        prepare_template(&template, volume.clone(), generation, &policy, &stop)
            .await
            .unwrap();
        let snapshot = super::super::runtime::load(&template).await.unwrap();
        assert_eq!(snapshot.disk.layout().unwrap(), layout);
        assert_eq!(snapshot.size(), size);
        assert!(snapshot.source.authorization().unwrap().is_none());
        let mut bytes = [0; 15];
        snapshot.read_at(offset, &mut bytes).unwrap();
        assert_eq!(&bytes, b"template-prefix");
        snapshot.write_at(offset, b"clone-mutation!").unwrap();
        volume.read_at(offset, &mut bytes).unwrap();
        assert_eq!(&bytes, b"later-mutation!");
        assert_eq!(snapshot.disk.performance()["remoteFetch"]["count"], 0);
        assert!(
            prepare_template(&template, volume, generation, &policy, &stop)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn paired_resize_recovers_original_offsets_files_and_publication_identity() {
        use super::super::{DiskView, LocalDisk};
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("disks/conversation");
        let context = json!({
            "master": "http://127.0.0.1:1/",
            "grant": "fixture",
            "policy": { "reserveMiB": 64, "reservePercent": 1 },
        });
        let size = 256 * 1024 * 1024;
        let layout = Layout::paired(size).unwrap();
        let stop = CancellationToken::new();
        prepare_source(&directory, size, &context, &stop, false, Some(layout))
            .await
            .unwrap();
        let volume = super::super::runtime::load(&directory).await.unwrap();
        assert_eq!(volume.disk.layout().unwrap(), layout);
        let marker = root.path().join("marker");
        tokio::fs::write(&marker, b"persistent workspace and system")
            .await
            .unwrap();
        for (name, offset, length) in [
            ("system", 0, size / 4),
            ("workspace", size / 4, size * 3 / 4),
        ] {
            let view = DiskView::new(volume.clone(), offset, length).unwrap();
            let image = root.path().join(name);
            super::super::export(&view, &image).unwrap();
            crate::microvm::host::command(
                "debugfs",
                &[
                    "-w",
                    "-R",
                    &format!("write {} /marker", marker.display()),
                    image.to_str().unwrap(),
                ],
            )
            .await
            .unwrap();
            let image = LocalDisk::open(&image, false).unwrap();
            let mut original = vec![0; crate::nodes::snapshots::BLOCK as usize];
            let mut changed = original.clone();
            for position in (0..length).step_by(original.len()) {
                view.read_at(position, &mut original).unwrap();
                image.read_at(position, &mut changed).unwrap();
                if original != changed {
                    view.write_at(position, &changed).unwrap();
                }
            }
        }
        let generation = volume.seal().await.unwrap();
        let captured = volume.disk.capture(generation).unwrap();
        assert_eq!(Layout::from_manifest(&captured).unwrap(), layout);
        crate::nodes::snapshots::validate(&captured).unwrap();
        let grant = volume.source.grant_id().unwrap();
        drop(volume);
        crate::skills::atomic_write(&directory.join("bootstrap.pending"), b"resizing")
            .await
            .unwrap();
        super::super::runtime::materialize(&directory, &stop)
            .await
            .unwrap();
        tokio::fs::write(directory.join("data.ext4"), b"partially moved workspace")
            .await
            .unwrap();
        let mut next_context = context.clone();
        next_context["grant"] = "replacement-grant".into();
        // Even a new default/configuration cannot reinterpret an existing disk.
        prepare_source(
            &directory,
            size * 2,
            &next_context,
            &stop,
            false,
            Some(Layout::Flat),
        )
        .await
        .unwrap();
        let volume = super::super::runtime::load(&directory).await.unwrap();
        assert_eq!(volume.source.grant_id().unwrap(), grant);
        assert!(volume.seal().await.unwrap() > generation);
        assert_eq!(
            volume.disk.layout().unwrap(),
            Layout::paired(size * 2).unwrap()
        );
        assert_eq!(volume.size(), size * 2);
        for (name, offset, length) in [
            ("grown-system", 0, size / 2),
            ("grown-workspace", size / 2, size * 3 / 2),
        ] {
            let view = DiskView::new(volume.clone(), offset, length).unwrap();
            let image = root.path().join(name);
            super::super::export(&view, &image).unwrap();
            crate::microvm::host::command("e2fsck", &["-fn", image.to_str().unwrap()])
                .await
                .unwrap();
            let saved = root.path().join(format!("{name}-saved"));
            crate::microvm::host::command(
                "debugfs",
                &[
                    "-R",
                    &format!("dump /marker {}", saved.display()),
                    image.to_str().unwrap(),
                ],
            )
            .await
            .unwrap();
            assert_eq!(
                tokio::fs::read(saved).await.unwrap(),
                b"persistent workspace and system"
            );
        }
        assert!(!directory.join("resize-source").exists());
        assert!(!directory.join("data.ext4").exists());
        assert!(!directory.join("bootstrap.pending").exists());
    }

    #[tokio::test]
    async fn raw_legacy_disk_cannot_start_as_a_new_s3_disk() {
        let root = tempfile::tempdir().unwrap();
        let disk = root.path().join("disks/conversation");
        std::fs::create_dir_all(&disk).unwrap();
        std::fs::write(disk.join("data.ext4"), b"existing work").unwrap();
        let error = prepare(
            &disk,
            128 * 1024 * 1024,
            &Value::Null,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.status, 409);
        assert_eq!(
            std::fs::read(disk.join("data.ext4")).unwrap(),
            b"existing work"
        );
    }

    #[tokio::test]
    async fn anonymous_ext4_disk_bootstraps_without_remote_authority_and_is_adopted_in_place() {
        let state = tempfile::tempdir().unwrap();
        let environment = crate::config::id();
        let directory = state.path().join("environments").join(&environment);
        let policy = super::super::policy::Policy {
            reserve_mi_b: 64,
            reserve_percent: 1,
            ..Default::default()
        };
        let stop = CancellationToken::new();
        prepare_unassigned(&directory, 128 * 1024 * 1024, &policy, &stop)
            .await
            .unwrap();
        let volume = super::super::runtime::load(&directory).await.unwrap();
        let mut signature = [0; 2];
        volume.read_at(1080, &mut signature).unwrap();
        assert_eq!(signature, [0x53, 0xef]);
        assert!(!directory.join("data.ext4").exists());
        assert!(volume.source.grant_id().is_err());
        assert!(!volume.disk.has_remote_base().unwrap());
        let context = super::super::LazyDisk::context(&directory.join("lazy")).unwrap();
        assert!(context.get("master").is_none() && context.get("grant").is_none());
        let physical = crate::file_lock::exclusive(&directory.join("lock"), "busy").unwrap();
        let run = crate::config::id();
        let owner = super::super::environment::assign(state.path(), &environment, &run)
            .await
            .unwrap();
        let authorization = json!({
            "master": "http://127.0.0.1:1/",
            "grant": "real-owner-fixture",
            "policy": policy,
        });
        volume.authorize(&owner, &authorization).await.unwrap();
        volume.read_at(1080, &mut signature).unwrap();
        assert_eq!(signature, [0x53, 0xef]);
        assert_eq!(
            volume.source.grant_id().unwrap(),
            crate::auth::digest("real-owner-fixture")
        );
        assert_eq!(
            super::super::environment::directory(state.path(), &run).unwrap(),
            directory
        );
        drop((owner, physical));
    }

    #[tokio::test]
    async fn interrupted_resize_restarts_from_the_original_journal() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("disks/conversation");
        let policy = super::super::policy::Policy {
            reserve_mi_b: 64,
            reserve_percent: 1,
            ..Default::default()
        };
        let context = json!({
            "master": "http://127.0.0.1:1/",
            "grant": "fixture",
            "policy": policy
        });
        let stop = CancellationToken::new();
        prepare(&directory, 128 * 1024 * 1024, &context, &stop)
            .await
            .unwrap();
        let volume = super::super::runtime::load(&directory).await.unwrap();
        volume
            .disk
            .write_at(33 * 1024 * 1024, b"retained work")
            .unwrap();
        volume.disk.sync().unwrap();
        let before = volume.seal().await.unwrap();
        let original_grant = volume.source.grant_id().unwrap();
        drop(volume);
        crate::skills::atomic_write(&directory.join("bootstrap.pending"), b"resizing")
            .await
            .unwrap();
        super::super::runtime::materialize(&directory, &stop)
            .await
            .unwrap();
        // Simulate a crash after resize2fs started modifying its temporary image.
        tokio::fs::write(directory.join("data.ext4"), b"partly resized image")
            .await
            .unwrap();
        let mut restarted_context = context.clone();
        restarted_context["grant"] = "new-attempt-grant".into();
        prepare(&directory, 256 * 1024 * 1024, &restarted_context, &stop)
            .await
            .unwrap();
        let volume = super::super::runtime::load(&directory).await.unwrap();
        assert_eq!(volume.source.grant_id().unwrap(), original_grant);
        assert!(volume.seal().await.unwrap() > before);
        let mut saved = [0; 13];
        volume.disk.read_at(33 * 1024 * 1024, &mut saved).unwrap();
        assert_eq!(&saved, b"retained work");
        // ext4's block count must grow too, not just the containing file.
        let mut header = [0; 28];
        volume.disk.read_at(1024, &mut header).unwrap();
        let blocks = u32::from_le_bytes(header[4..8].try_into().unwrap()) as u64;
        let block_size = 1024_u64 << u32::from_le_bytes(header[24..28].try_into().unwrap());
        assert_eq!(blocks * block_size, 256 * 1024 * 1024);
        assert!(!directory.join("resize-source").exists());
        assert!(!directory.join("data.ext4").exists());
        assert!(!directory.join("bootstrap.pending").exists());
    }
}
