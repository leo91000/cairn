use super::*;
use std::process::Stdio;
use tokio::process::Command;

pub(crate) static JOBS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

pub(crate) fn command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    unsafe {
        cmd.pre_exec(|| {
            for (resource, limit) in [
                (libc::RLIMIT_CPU, 20),
                (libc::RLIMIT_AS, 1024 * 1024 * 1024),
                (libc::RLIMIT_FSIZE, 16 * 1024 * 1024),
            ] {
                let value = libc::rlimit {
                    rlim_cur: limit,
                    rlim_max: limit,
                };
                if libc::setrlimit(resource, &value) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    cmd
}

/// Reads dimensions and duration of a media file into the record.
async fn probe(path: &Path, artifact: &mut Deliverable) -> Result<()> {
    let output = command("ffprobe")
        .args([
            "-v",
            "error",
            "-protocol_whitelist",
            "file,pipe",
            "-select_streams",
            "v:0",
            "-show_entries",
            "format=duration:stream=width,height",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .await?;
    if !output.status.success() || output.stdout.len() >= 16384 {
        return Ok(());
    }
    let info: Value = serde_json::from_slice(&output.stdout)?;
    artifact.width = info["streams"][0]["width"]
        .as_u64()
        .and_then(|n| n.try_into().ok());
    artifact.height = info["streams"][0]["height"]
        .as_u64()
        .and_then(|n| n.try_into().ok());
    if let Ok(duration) = text(&info["format"], "duration").parse::<f64>()
        && duration.is_finite()
        && duration >= 0.0
    {
        artifact.duration = Some(duration);
    }
    Ok(())
}

/// Renders a 960px JPEG of the first PDF page or video frame.
pub(crate) async fn thumbnail(pdf: bool, path: &Path, target: &Path) -> Result<bool> {
    let status = if pdf {
        command("pdftoppm")
            .args(["-f", "1", "-singlefile", "-scale-to", "960", "-jpeg"])
            .arg(path)
            .arg(target.with_extension(""))
            .status()
            .await?
    } else {
        command("ffmpeg")
            .args([
                "-v",
                "error",
                "-nostdin",
                "-y",
                "-protocol_whitelist",
                "file,pipe",
                "-threads",
                "1",
                "-i",
            ])
            .arg(path)
            .args([
                "-frames:v",
                "1",
                "-vf",
                "scale=960:960:force_original_aspect_ratio=decrease",
                "-threads",
                "1",
                "-f",
                "image2",
            ])
            .arg(target)
            .status()
            .await?
    };
    Ok(status.success() && target.is_file())
}

/// Renders a JPEG of at most 960px of a raster image, flattening transparency onto white
/// and never enlarging small images.
pub(crate) async fn image_thumbnail(path: &Path, target: &Path) -> Result<bool> {
    let status = command("ffmpeg")
        .args([
            "-v",
            "error",
            "-nostdin",
            "-y",
            "-protocol_whitelist",
            "file,pipe",
            // Codec thread limits do not bound the complex filter graph's pool.
            // Its automatic per-core workers can exceed the process address limit.
            "-filter_complex_threads",
            "1",
            "-threads",
            "1",
            "-i",
        ])
        .arg(path)
        .args([
            "-filter_complex",
            "[0:v]scale='min(960,iw)':'min(960,ih)':force_original_aspect_ratio=decrease,\
             split[background][image];[background]drawbox=c=white:t=fill[white];\
             [white][image]overlay=format=auto",
            "-frames:v",
            "1",
            "-threads",
            "1",
            "-f",
            "image2",
        ])
        .arg(target)
        .status()
        .await?;
    Ok(status.success() && target.is_file())
}

/// Reads the page count of a PDF.
pub(crate) async fn pdf_pages(path: &Path) -> Result<Option<u64>> {
    let output = command("pdfinfo").arg(path).output().await?;
    if !output.status.success() || output.stdout.len() >= 65536 {
        return Ok(None);
    }
    let pages = String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("Pages:"))
        .and_then(|count| count.trim().parse().ok());
    Ok(pages)
}

/// Copies the preview fields onto the stored record, if it still exists.
async fn store(s: &Service, artifact: Deliverable) -> Result<()> {
    s.store
        .transaction(move |db| db.save_artifact_preview(&artifact))
        .await
}

pub async fn prepare(s: Service, mut artifact: Deliverable) -> Result<()> {
    if artifact.preview_status != PreviewStatus::Pending.as_str() {
        return Ok(());
    }
    let _permit = JOBS.acquire().await.map_err(Error::internal)?;
    let run = artifact.run_id.clone();
    let active = s
        .store
        .read(move |db| crate::conversation_lifecycle::require_active_run(db, &run))
        .await;
    if active.is_err() {
        return Ok(());
    }
    let directory = s.config.data_dir.join("artifacts");
    let path = directory.join(&artifact.id);
    let target = directory.join(format!("{}.jpg", &artifact.id));
    let kind = artifact.kind.clone();
    let job = async {
        if kind != "pdf" {
            probe(&path, &mut artifact).await?;
        }
        if kind == "audio" {
            return Ok(false);
        }
        thumbnail(kind == "pdf", &path, &target).await
    };
    let status = match tokio::time::timeout(Duration::from_secs(25), job).await {
        Ok(Ok(true)) => PreviewStatus::Ready,
        Ok(Ok(false)) if kind == "audio" => PreviewStatus::None,
        _ => PreviewStatus::Unavailable,
    };
    artifact.preview_status = status.as_str().to_owned();
    store(&s, artifact).await
}

pub async fn recover(s: Arc<Service>) {
    // A request/process can disappear between fsync and the SQLite commit. Only
    // collect unreferenced files older than any allowed transfer, under the commit
    // lock; recent in-flight publications and every committed version are retained.
    if let Err(error) = reconcile(&s).await {
        tracing::warn!(error = %error, "artifact file reconciliation failed");
    }
    let Ok(items) = s.store.artifacts(None).await else {
        return;
    };
    for item in items {
        if item.preview_status == PreviewStatus::Pending.as_str()
            && let Err(error) = prepare((*s).clone(), item).await
        {
            tracing::warn!(error = %error, "artifact preview failed");
        }
    }
}

async fn reconcile(s: &Service) -> Result<()> {
    let _guard = s.artifacts.commit.lock().await;
    let known: std::collections::HashSet<String> = s
        .store
        .artifacts(None)
        .await?
        .into_iter()
        .map(|item| item.id)
        .collect();
    let directory = s.config.data_dir.join("artifacts");
    if !directory.is_dir() {
        return Ok(());
    }
    let mut files = tokio::fs::read_dir(directory).await?;
    while let Some(entry) = files.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        let identifier = name.strip_suffix(".jpg").unwrap_or(&name);
        if known.contains(identifier) || !(uuid(identifier).is_ok() || name.starts_with(".tmp")) {
            continue;
        }
        let metadata = tokio::fs::symlink_metadata(entry.path()).await?;
        if metadata.is_file()
            && metadata.modified()?.elapsed().unwrap_or_default() > Duration::from_secs(3600)
        {
            tokio::fs::remove_file(entry.path()).await?;
        }
    }
    Ok(())
}
