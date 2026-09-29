use super::*;
use std::process::Stdio;
use tokio::process::Command;

pub(crate) static JOBS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

fn command(program: &str) -> Command {
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

pub async fn prepare(s: Service, mut artifact: Value) -> Result<()> {
    if artifact["previewStatus"] != "pending" {
        return Ok(());
    }
    let _permit = JOBS.acquire().await.map_err(Error::internal)?;
    let run = text(&artifact, "runId").to_owned();
    if s.store
        .read(move |db| crate::conversation_lifecycle::require_active_run(db, &run))
        .await
        .is_err()
    {
        return Ok(());
    }
    let directory = s.config.data_dir.join("artifacts");
    let path = directory.join(text(&artifact, "id"));
    let target = directory.join(format!("{}.jpg", text(&artifact, "id")));
    let job = async {
        if artifact["kind"] != "pdf" {
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
                .arg(&path)
                .output()
                .await?;
            if output.status.success() && output.stdout.len() < 16384 {
                let info: Value = serde_json::from_slice(&output.stdout)?;
                artifact["width"] = info["streams"][0]["width"].clone();
                artifact["height"] = info["streams"][0]["height"].clone();
                if let Ok(duration) = text(&info["format"], "duration").parse::<f64>()
                    && duration.is_finite()
                    && duration >= 0.0
                {
                    artifact["duration"] = json!(duration);
                }
            }
        }
        if artifact["kind"] == "audio" {
            return Ok::<bool, Error>(false);
        }
        let status = if artifact["kind"] == "pdf" {
            command("pdftoppm")
                .args(["-f", "1", "-singlefile", "-scale-to", "960", "-jpeg"])
                .arg(&path)
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
                .arg(&path)
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
                .arg(&target)
                .status()
                .await?
        };
        Ok(status.success() && target.is_file())
    };
    artifact["previewStatus"] = match tokio::time::timeout(Duration::from_secs(25), job).await {
        Ok(Ok(true)) => "ready",
        Ok(Ok(false)) if artifact["kind"] == "audio" => "none",
        _ => "unavailable",
    }
    .into();
    s.store
        .transaction(move |db| {
            let key = format!(
                "artifact:{}:{}",
                text(&artifact, "runId"),
                text(&artifact, "id")
            );
            if let Some(mut current) = db.kv(&key)? {
                for field in ["previewStatus", "width", "height", "duration"] {
                    if let Some(value) = artifact.get(field) {
                        current[field] = value.clone();
                    }
                }
                db.set(&key, &current, None)?;
            }
            Ok(())
        })
        .await
}

pub async fn recover(s: Arc<Service>) {
    // A request/process can disappear between fsync and the SQLite commit. Only
    // collect unreferenced files older than any allowed transfer, under the commit
    // lock; recent in-flight publications and every committed version are retained.
    let _ = reconcile(&s).await;
    if let Ok(items) = s.store.keys("artifact:").await {
        for (_, item) in items {
            if item["previewStatus"] == "pending" {
                let _ = prepare((*s).clone(), item).await;
            }
        }
    }
}

async fn reconcile(s: &Service) -> Result<()> {
    let _guard = s.artifacts.commit.lock().await;
    let known: std::collections::HashSet<String> = s
        .store
        .keys("artifact:")
        .await?
        .into_iter()
        .map(|(_, item)| text(&item, "id").to_owned())
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
