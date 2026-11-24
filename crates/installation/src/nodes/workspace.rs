//! Master-owned run scope for remote seed files, inboxes, results, access-token
//! relay and run-scoped MCP.
mod skill_cache;

use crate::{
    error::{Error, Result},
    http::{App, Input},
    run_status::RunStatus,
    service::Service,
    validation::text,
};
use axum::{
    Json,
    body::Body,
    extract::{Request, State},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};

const MAX_RESULT_BYTES: usize = 1_000_000;
const MAX_AUTH_REQUEST_BYTES: usize = 32768;
const SEED_BUDGET_BYTES: u64 = 32 * 1024 * 1024 * 1024;
const SEED_ALLOCATION: &str = "x-cairn-seed-allocation";
const SEED_OUTGREW_RESERVATION: &str = "Native seed outgrew its staging reservation.";

/// The provider's configuration directory inside the conversation home, where its
/// credential broker listens. Each coding agent's adapter names it, as for microVMs.
pub(crate) fn provider_home(plan: &Value) -> &'static str {
    plan["chat"]["provider"]
        .as_str()
        .and_then(|provider| crate::provider::Provider::parse(provider).ok())
        .unwrap_or(crate::provider::Provider::Codex)
        .adapter()
        .state_directory()
}

async fn authorize(s: &Service, node: &str, attempt: &str) -> Result<(Value, Value)> {
    crate::validation::uuid(attempt)?;
    let bytes = tokio::fs::read(
        s.config
            .data_dir
            .join("runner-plans")
            .join(format!("{attempt}.json")),
    )
    .await?;
    let plan: Value = serde_json::from_slice(&bytes)?;
    let run = s.store.run(text(&plan, "runId")).await?;
    let checkpoint = super::checkpoint(s, text(&plan, "runId")).await?;
    let agent = s.agent_snapshot(super::run_agent(&run)).await?;
    let ownership = s
        .store
        .attempt(attempt)
        .await?
        .ok_or_else(|| Error::forbidden("Remote execution scope expired."))?;
    let record = s.node_boundary(node).await?;
    let native_allowed = if crate::native::policy::node_native(&record) {
        let (run, record) = (run.clone(), record.clone());
        s.store
            .read(move |db| crate::native::policy::allows(db, &run, &record))
            .await?
    } else {
        true
    };
    if !native_allowed
        || ownership.released
        || ownership.node_id != node
        || checkpoint["nodeId"] != node
        || checkpoint["runnerId"] != attempt
        || run["status"] != RunStatus::Running
        || !run["cancelRequestedAt"].is_null()
        || !super::agent_allows(&agent, node)
    {
        return Err(Error::forbidden("Remote execution scope expired."));
    }
    Ok((run, plan))
}

pub async fn handle(State(app): State<App>, request: Request) -> Result<Response> {
    let input = Input::read(request).await?;
    let s = &app.service;
    let node = super::transport::authenticate(s, &input.headers).await?;
    let parts = input
        .path
        .trim_start_matches("/internal/node-workspace/")
        .split('/')
        .collect::<Vec<_>>();
    let [attempt, operation] = parts.as_slice() else {
        return Err(Error::bad("Invalid workspace operation."));
    };
    if input.method != "POST" {
        return Err(Error::method_not_allowed("Method not allowed."));
    }
    if *attempt == "cache" {
        return crate::project_archives::serve_cached(
            s,
            &node,
            operation,
            text(&input.body, "hash"),
        )
        .await;
    }
    let (run, plan) = authorize(s, &node, attempt).await?;
    let root = s.config.data_dir.join("runs").join(text(&run, "id"));
    match *operation {
        "lease" => {
            let lease_ms = renew_lease(s, attempt).await?;
            Ok(Json(json!({ "remainingMs": lease_ms })).into_response())
        }
        "plan" => {
            let mut descriptor = json!({ "plan": plan, "dataRoot": s.config.data_dir,
                "credentialFiles": crate::execution::credential_files(&root).await });
            if plan["backend"] == "native"
                && let Some(archives) =
                    crate::project_archives::run_archives(s, &run, &root, &plan).await?
            {
                descriptor["projectArchives"] = serde_json::to_value(archives)?;
            }
            Ok(Json(descriptor).into_response())
        }
        "inbox" => {
            let bytes = crate::chat_inbox::read(
                &root.join("chat-input"),
                crate::chat_inbox::TRANSPORT_LIMIT,
            )
            .await;
            let bytes = match bytes {
                Ok(bytes) => bytes.unwrap_or_else(|| b"[]".to_vec()),
                // This response is a change detector, not the inbox transfer.
                // Keep refresh_inbox seeding the file so the controller's bounded
                // reader reports overflow to the run instead of retaining stale input.
                Err(error) if error.status == 400 => {
                    return Ok(Json(json!({ "oversized": true })).into_response());
                }
                Err(error) => return Err(error),
            };
            let messages: Value = serde_json::from_slice(&bytes)?;
            Ok(Json(json!({ "messages": messages })).into_response())
        }
        "seed" => {
            let archives = plan["backend"] == "native"
                && input.body["projectArchives"] == true
                && crate::project_archives::run_archives(s, &run, &root, &plan)
                    .await?
                    .is_some();
            seed_source(&run, &plan, &root, &input.body, archives).await
        }
        "archive" => crate::project_archives::serve_run(s, &run, &root, &plan, &input.body).await,
        "auth" => {
            let socket = root
                .join("home")
                .join(provider_home(&plan))
                .join("cairn-auth.sock");
            let value = relay_auth(&socket, &input.body).await?;
            Ok(Json(value).into_response())
        }
        "mcp" => crate::mcps::channel::relayed(s, text(&run, "id"), &input.body).await,
        "result" => {
            let result = text(&input.body, "result");
            if result.len() > MAX_RESULT_BYTES {
                return Err(Error::bad("Result exceeds limit."));
            }
            crate::skills::atomic_write(&root.join("output/result.md"), result.as_bytes()).await?;
            Ok(Json(json!({ "ok": true })).into_response())
        }
        _ => Err(Error::not_found("Unknown workspace operation.")),
    }
}

/// Extends the attempt's lease and returns its duration.
async fn renew_lease(s: &Service, attempt: &str) -> Result<u64> {
    let id = attempt.to_owned();
    let lease_ms = super::settings::lease_ms(s).await?;
    s.store
        .transaction(move |db| {
            let mut record = db
                .attempt(&id)?
                .ok_or_else(|| Error::conflict("Attempt removed."))?;
            if record.released {
                return Err(Error::conflict("Attempt released."));
            }
            record.lease_expires_at = Some(crate::config::now() + lease_ms as i64);
            record.lease_duration_ms = Some(record.lease_duration_ms.unwrap_or(0).max(lease_ms));
            db.save_attempt(&record)?;
            Ok(())
        })
        .await?;
    super::record_lease(s, attempt, lease_ms).await;
    Ok(lease_ms)
}

/// Compute the manager's pre-response skill digest and staging estimate.
/// The disposable native qualification measures this same work separately.
#[doc(hidden)]
pub fn native_seed_offer(
    source: &Path,
    skipped_project: Option<&Path>,
) -> (Option<String>, Option<u64>) {
    let key = skill_cache::key(&source.join("home"));
    let allocation = super::files::staging_size(source, |path| {
        skipped_project != Some(path)
            && path != Path::new(crate::conversation_deletion::retired::MANAGER_NAMESPACE)
    })
    .ok()
    .map(|bytes| {
        let margin = if key.is_some() {
            skill_cache::CACHE_WRITE_MARGIN
        } else {
            0
        };
        bytes
            .saturating_add(margin)
            .saturating_add(crate::native::disk::MIB)
    });
    (key, allocation)
}

/// Streams the selected staging directory of the run to the node.
async fn seed_source(
    run: &Value,
    plan: &Value,
    root: &Path,
    selection: &Value,
    archives: bool,
) -> Result<Response> {
    let source = match text(selection, "kind") {
        "initial" => root.to_path_buf(),
        "inbox" => root.join("chat-input"),
        "runtime" if plan["environmentProtocol"] == 1 => root.join("home"),
        "project" => {
            let project = text(selection, "projectId");
            crate::validation::uuid(project)?;
            if !crate::service::run_projects(run)
                .iter()
                .any(|p| p["id"] == project)
            {
                return Err(Error::forbidden("Project outside run scope."));
            }
            crate::execution::project_seed_source(root, plan, project).await?
        }
        _ => return Err(Error::bad("Invalid seed selection.")),
    };
    if tokio::fs::canonicalize(&source).await? != source {
        return Err(Error::bad("Workspace seed changed location."));
    }
    // A node may omit projects only when its disk or verified archives supply them.
    let resumed_vm = plan["backend"] == "firecracker" && plan["resumeWorkspace"] == true;
    let skip_projects = text(selection, "kind") == "initial"
        && selection["workspace"] == false
        && (resumed_vm || archives);
    let project_root = if skip_projects {
        plan["projectRoot"]
            .as_str()
            .and_then(|path| Path::new(path).strip_prefix(root).ok())
            .filter(|path| !path.as_os_str().is_empty())
            .map(Path::to_path_buf)
    } else {
        None
    };
    let (mut writer, reader) = tokio::io::duplex(131072);
    let attempt = text(plan, "id").to_owned();
    let initial = text(selection, "kind") == "initial";
    let runtime = text(selection, "kind") == "runtime";
    let (skill_key, allocation) =
        if initial && plan["backend"] == "native" && selection[skill_cache::NEGOTIATE].is_array() {
            let root = source.clone();
            let skipped = project_root.clone();
            tokio::task::spawn_blocking(move || native_seed_offer(&root, skipped.as_deref()))
                .await
                .map_err(Error::internal)?
        } else {
            (None, None)
        };
    let skill_hit = skill_key.as_ref().is_some_and(|key| {
        selection[skill_cache::NEGOTIATE]
            .as_array()
            .into_iter()
            .flatten()
            .any(|offered| offered.as_str() == Some(key))
    });
    tokio::spawn(async move {
        // A failed transfer ends the stream early, which the receiver rejects.
        if let Err(error) = super::files::send_filtered(&source, &mut writer, |path| {
            (!runtime || path != Path::new(".agents/skills"))
                && (!skill_hit || path != Path::new("home/.agents/skills"))
                && project_root.as_deref() != Some(path)
                && (!initial
                    || path != Path::new(crate::conversation_deletion::retired::MANAGER_NAMESPACE))
        })
        .await
        {
            tracing::warn!(
                attempt,
                status = error.status,
                "Workspace seed source transfer failed"
            );
        }
    });
    let mut response = Response::new(Body::from_stream(tokio_util::io::ReaderStream::new(reader)));
    if let Some(key) = skill_key {
        response
            .headers_mut()
            .insert(skill_cache::KEY, key.parse().map_err(Error::internal)?);
        if skill_hit {
            response
                .headers_mut()
                .insert(skill_cache::HIT, axum::http::HeaderValue::from_static("1"));
        }
    }
    if let Some(bytes) = allocation {
        response.headers_mut().insert(
            SEED_ALLOCATION,
            bytes.to_string().parse().map_err(Error::internal)?,
        );
    }
    Ok(response)
}

async fn relay_auth(path: &Path, value: &Value) -> Result<Value> {
    if serde_json::to_vec(value)?.len() > MAX_AUTH_REQUEST_BYTES {
        return Err(Error::bad("Invalid authentication request."));
    }
    tokio::time::timeout(Duration::from_secs(45), async {
        let mut stream = BufReader::new(tokio::net::UnixStream::connect(path).await?);
        crate::microvm::wire::write(stream.get_mut(), value).await?;
        crate::microvm::wire::read(&mut stream)
            .await?
            .ok_or_else(|| Error::unavailable("Authentication unavailable."))
    })
    .await
    .map_err(|_| Error::unavailable("Authentication unavailable."))?
}

pub async fn fetch(
    client: &reqwest::Client,
    master: &url::Url,
    token: &str,
    attempt: &str,
    operation: &str,
    body: &Value,
) -> Result<reqwest::Response> {
    let response = client
        .post(
            master
                .join(&format!("internal/node-workspace/{attempt}/{operation}"))
                .map_err(Error::internal)?,
        )
        .bearer_auth(token)
        .timeout(Duration::from_secs(300))
        .json(body)
        .send()
        .await
        .map_err(|_| Error::unavailable("Workspace connection interrupted."))?;
    if !response.status().is_success() {
        return Err(Error::new(
            response.status().as_u16(),
            "Remote workspace access rejected.",
        ));
    }
    Ok(response)
}

pub async fn seed(
    client: &reqwest::Client,
    master: &url::Url,
    token: &str,
    attempt: &str,
    selection: Value,
    target: &Path,
) -> Result<()> {
    seed_with_stop(
        client,
        master,
        token,
        attempt,
        selection,
        target,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
}

pub(crate) fn check_preparing(stop: &tokio_util::sync::CancellationToken) -> Result<()> {
    if stop.is_cancelled() {
        return Err(Error::unavailable("Node workspace preparation stopped."));
    }
    Ok(())
}

/// Cancel waits that have no owned writes. Writers observe the same token
/// between reads and drain accepted I/O before releasing their leases.
pub(crate) async fn await_preparation<T>(
    stop: &tokio_util::sync::CancellationToken,
    operation: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    check_preparing(stop)?;
    stop.run_until_cancelled(operation)
        .await
        .ok_or_else(|| Error::unavailable("Node workspace preparation stopped."))?
}

pub(super) async fn seed_with_stop(
    client: &reqwest::Client,
    master: &url::Url,
    token: &str,
    attempt: &str,
    mut selection: Value,
    target: &Path,
    stop: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    use futures_util::StreamExt;
    // The manager can authorize and open the source while unrelated local
    // storage operations run. No bytes enter staging before disk admission.
    let native_initial = crate::native::enabled() && selection["kind"] == "initial";
    let cache = skill_cache::directory();
    if native_initial && selection.get(skill_cache::NEGOTIATE).is_none() {
        selection[skill_cache::NEGOTIATE] = json!(skill_cache::offered(&cache));
    }
    let mut response = await_preparation(
        stop,
        fetch(client, master, token, attempt, "seed", &selection),
    )
    .await?;
    let mut skill_hit = native_initial
        && response
            .headers()
            .get(skill_cache::HIT)
            .is_some_and(|v| v == "1");
    let mut skill_key = response
        .headers()
        .get(skill_cache::KEY)
        .and_then(|value| value.to_str().ok())
        .filter(|key| skill_cache::valid(key))
        .map(str::to_owned);
    if skill_hit
        && !skill_key.as_ref().is_some_and(|key| {
            selection[skill_cache::NEGOTIATE]
                .as_array()
                .is_some_and(|offered| offered.iter().any(|value| value == key))
        })
    {
        return Err(Error::bad(
            "Manager omitted native skills without a matching cache offer.",
        ));
    }
    if skill_hit
        && skill_key
            .as_ref()
            .is_none_or(|key| !cache.join(key).is_file())
    {
        // An offered entry can be evicted while the manager prepares the reply.
        selection[skill_cache::NEGOTIATE] = Value::Null;
        response = await_preparation(
            stop,
            fetch(client, master, token, attempt, "seed", &selection),
        )
        .await?;
        if response.headers().contains_key(skill_cache::HIT) {
            return Err(Error::bad(
                "Manager omitted native skills after a cache miss.",
            ));
        }
        skill_hit = false;
        skill_key = None;
    }
    let mut storage_guard = if crate::native::enabled() {
        Some(await_preparation(stop, crate::native::disk::lock(&crate::native::state_dir())).await?)
    } else {
        None
    };
    let (budget, sample) = if crate::native::enabled() {
        let measured = std::time::Instant::now();
        let sample = await_preparation(stop, crate::native::disk::staging_sample()).await?;
        // Inbox refresh copies into an inode already mounted by the workload;
        // its received temporary tree and the replacement can coexist briefly.
        let available = if selection["kind"] == "inbox" {
            sample.available_bytes / 2
        } else {
            sample.available_bytes
        };
        (
            SEED_BUDGET_BYTES.min(available),
            sample.used_bytes.map(|bytes| (bytes, measured)),
        )
    } else {
        (SEED_BUDGET_BYTES, None)
    };
    let allocation = response
        .headers()
        .get(SEED_ALLOCATION)
        .and_then(|value| value.to_str().ok()?.parse::<u64>().ok())
        .filter(|bytes| native_initial && *bytes > 0 && *bytes <= budget);
    let parent = target
        .parent()
        .ok_or_else(|| Error::bad("Invalid seed directory."))?;
    let mut admission = if crate::native::enabled() {
        let state = crate::native::state_dir();
        Some(crate::native::ingress::Admission::staging_with_sample(
            &super::node_data_dir(),
            &state,
            target,
            sample,
        )?)
    } else {
        None
    };
    let entry_overhead = 2 * super::files::ENTRY_ALLOCATION;
    if let Some(admission) = &mut admission {
        admission.admit(entry_overhead)?;
    }
    crate::skills::private_dir(parent).await?;
    let temp = tempfile::TempDir::new_in(parent)?;
    if let Some(admission) = &mut admission {
        admission.retarget_staging(temp.path())?;
    }
    let (mut admission, reservation) = match (admission, allocation) {
        (Some(admission), Some(bytes)) if admission.reserve_staging(bytes)? => {
            let reservation = crate::native::disk::Preparation::reserve_staging(
                &crate::native::state_dir(),
                &crate::config::id(),
                bytes,
                &super::node_data_dir(),
                temp.path(),
            )
            .await?;
            (
                Some(admission.with_new_reservation(&reservation)?),
                Some(reservation),
            )
        }
        (admission, _) => (admission, None),
    };
    let mut remaining = reservation
        .as_ref()
        .map(|reserved| reserved.bytes.saturating_sub(entry_overhead));
    let reserved = remaining.is_some();
    if reserved {
        drop(storage_guard.take());
    }
    let mut admit = |bytes| {
        if let Some(remaining) = &mut remaining {
            *remaining = remaining
                .checked_sub(bytes)
                .ok_or_else(|| Error::new(507, SEED_OUTGREW_RESERVATION))?;
        }
        admission
            .as_mut()
            .expect("native staging admission")
            .admit(bytes)
    };
    // Initial native seeds are private until the complete HTTP body has arrived
    // and the destination filesystem has accepted one writeback barrier.
    let barrier = if native_initial {
        Some(tokio::fs::File::open(temp.path()).await?.into_std().await)
    } else {
        None
    };
    let mut transport_interrupted = false;
    let mut received_bytes = 0u64;
    let stream = Box::pin(response.bytes_stream().take_until(stop.cancelled()));
    let stream = stream.map(|result| match result {
        Ok(bytes) => {
            received_bytes = received_bytes.saturating_add(bytes.len() as u64);
            Ok(bytes)
        }
        Err(error) => {
            transport_interrupted = true;
            tracing::warn!(
                attempt,
                received_bytes,
                timeout = error.is_timeout(),
                connect = error.is_connect(),
                body = error.is_body(),
                decode = error.is_decode(),
                "Workspace seed HTTP stream interrupted"
            );
            Err(std::io::Error::other(
                "Workspace seed HTTP stream interrupted.",
            ))
        }
    });
    let mut reader = BufReader::new(tokio_util::io::StreamReader::new(stream));
    let received = async {
        if crate::native::enabled() {
            let keep = selection["keepArchives"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let data = super::node_data_dir();
            let multiplier = if selection["kind"] == "inbox" { 2 } else { 1 };
            super::files::receive_admitted(
                &mut reader,
                temp.path(),
                budget,
                if reserved { budget } else { SEED_BUDGET_BYTES },
                |needed| {
                    let (data, keep) = (data.clone(), keep.clone());
                    async move {
                        if reserved {
                            return Ok(0);
                        }
                        Ok(crate::project_archives::reclaim_for_foreground(
                            &data,
                            needed.saturating_mul(multiplier),
                            &keep,
                        )
                        .await?
                            / multiplier)
                    }
                },
                if barrier.is_some() {
                    super::files::Durability::StagedTree
                } else {
                    super::files::Durability::PerFile
                },
                &mut admit,
            )
            .await?;
        } else {
            super::files::receive(&mut reader, temp.path(), budget).await?;
        }

        // The protocol marker alone cannot prove the HTTP body arrived intact.
        let mut trailing = [0; 1];
        if reader.read(&mut trailing).await? != 0 {
            return Err(Error::bad("Unexpected data after workspace transfer."));
        }
        Ok(())
    }
    .await;
    drop(reader);
    check_preparing(stop)?;
    if transport_interrupted {
        return Err(Error::bad_gateway(
            "Workspace transfer was interrupted; source files are preserved.",
        ));
    }
    if let Err(error) = received {
        if error.message == SEED_OUTGREW_RESERVATION && reserved {
            drop(temp);
            drop(reservation);
            selection[skill_cache::NEGOTIATE] = Value::Null;
            return Box::pin(seed_with_stop(
                client, master, token, attempt, selection, target, stop,
            ))
            .await;
        }
        return Err(error);
    }
    let staged = barrier.is_some();
    if let Some(barrier) = barrier {
        if let Some(key) = skill_key {
            let home = temp.path().join("home");
            if skill_hit {
                if let Err(error) = skill_cache::install(&cache, &key, &home, &mut admit).await {
                    if error.status == 507 {
                        return Err(error);
                    }
                    tracing::debug!(
                        status = error.status,
                        "Native skill cache entry could not be reused"
                    );
                    let _ = tokio::fs::remove_file(cache.join(&key)).await;
                    drop(temp);
                    drop(reservation);
                    drop(storage_guard);
                    selection[skill_cache::NEGOTIATE] = Value::Null;
                    return Box::pin(seed_with_stop(
                        client, master, token, attempt, selection, target, stop,
                    ))
                    .await;
                }
            } else {
                let _cache_write = if storage_guard.is_none() {
                    Some(
                        await_preparation(
                            stop,
                            crate::native::disk::lock(&crate::native::state_dir()),
                        )
                        .await?,
                    )
                } else {
                    None
                };
                if let Err(error) = skill_cache::remember(&cache, &key, &home, &mut admit).await {
                    tracing::debug!(
                        status = error.status,
                        "Native skill cache population deferred"
                    );
                }
            }
        }
        prepare_native_inbox(&temp.path().join("chat-input")).await?;
        crate::native::block::sync_opened(barrier).await?;
    }
    check_preparing(stop)?;
    // Preserve the directory inode mounted into a native workload. Replacing a
    // bind-mounted directory would leave it reading the previous inbox forever.
    if crate::native::enabled() && selection["kind"] == "inbox" {
        return install_native_inbox(temp.path(), target).await;
    }
    if selection["kind"] == "runtime" {
        return install_runtime_files(temp.path(), target).await;
    }
    // Only host staging files are replaced. Guest disks remain authoritative.
    if target.exists() {
        tokio::fs::remove_dir_all(target).await?;
    }
    tokio::fs::rename(temp.path(), target).await?;
    if staged {
        tokio::fs::File::open(parent).await?.sync_all().await?;
    }
    Ok(())
}

/// Inbox bytes stay in private host staging and are mounted read-only into the
/// unprivileged workload. Every entry must be readable before publication.
pub(crate) async fn prepare_native_inbox(root: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let mut pending = vec![root.to_owned()];
    while let Some(path) = pending.pop() {
        let metadata = match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) => metadata,
            Err(error) if path == root && error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).await?;
            let mut entries = tokio::fs::read_dir(path).await?;
            while let Some(entry) = entries.next_entry().await? {
                pending.push(entry.path());
            }
        } else if metadata.is_file() && metadata.nlink() == 1 {
            tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).await?;
        } else {
            return Err(Error::bad(
                "Native inbox contains an alias or unsupported entry.",
            ));
        }
    }
    Ok(())
}

async fn inbox_directory(path: &Path) -> Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(Error::bad("Native inbox directory was redirected.")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tokio::fs::create_dir(path).await?;
        }
        Err(error) => return Err(error.into()),
    }
    // Never temporarily chmod a mounted inbox to 0700 during a refresh.
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).await?;
    Ok(())
}

/// Publish controller information into an already mounted read-only inbox.
/// Its directory stays traversable and final bytes are readable before rename.
pub(crate) async fn publish_native_notice(root: &Path, bytes: &[u8]) -> Result<()> {
    inbox_directory(root).await?;
    let (root, bytes) = (root.to_owned(), bytes.to_owned());
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new_in(&root)?;
        file.write_all(&bytes)?;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o444))?;
        file.as_file().sync_all()?;
        file.persist(root.join("disk-notice.json"))
            .map_err(|error| error.error)?;
        std::fs::File::open(root)?.sync_all()
    })
    .await
    .map_err(Error::internal)??;
    Ok(())
}

async fn install_native_inbox(source: &Path, target: &Path) -> Result<()> {
    prepare_native_inbox(source).await?;
    let messages = source.join("messages.json");
    match tokio::fs::symlink_metadata(&messages).await {
        Ok(metadata) if metadata.is_file() => {}
        // Non-chat runs can have no messages yet. Match the master's absent
        // inbox semantics without replacing a directory mounted by the workload.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Ok(_) => return Err(Error::bad("Native inbox messages are missing.")),
        Err(error) => return Err(error.into()),
    }
    inbox_directory(target).await?;
    let mut pending = vec![(source.to_owned(), target.to_owned())];
    while let Some((directory, destination)) = pending.pop() {
        let mut entries = tokio::fs::read_dir(directory).await?;
        while let Some(entry) = entries.next_entry().await? {
            if entry.path() == messages {
                continue;
            }
            let path = destination.join(entry.file_name());
            if entry.file_type().await?.is_dir() {
                inbox_directory(&path).await?;
                pending.push((entry.path(), path));
                continue;
            }
            match tokio::fs::symlink_metadata(&path).await {
                Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
                    return Err(Error::bad("Native inbox file was redirected."));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            tokio::fs::rename(entry.path(), path).await?;
        }
    }
    // All attachments and their readable modes are installed before a message
    // can reference them. The source file is already synchronized and 0444;
    // publishing a 0600 replacement before chmod would race the agent's reader.
    tokio::fs::rename(messages, target.join("messages.json")).await?;
    Ok(())
}

pub async fn auth_listener(
    path: PathBuf,
    client: reqwest::Client,
    master: url::Url,
    token: String,
    attempt: String,
    stop: tokio_util::sync::CancellationToken,
) -> Result<()> {
    if path.exists() {
        tokio::fs::remove_file(&path).await?;
    }
    let listener = tokio::net::UnixListener::bind(&path)?;
    tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await?;
    loop {
        let accepted = tokio::select! {
            () = stop.cancelled() => break,
            result = listener.accept() => result,
        };
        let Ok((socket, _)) = accepted else { break };
        let operation = async {
            let mut stream = BufReader::new(socket);
            let request = crate::microvm::wire::read(&mut stream)
                .await?
                .ok_or_else(|| Error::bad("Empty authentication request."))?;
            let value: Value = fetch(&client, &master, &token, &attempt, "auth", &request)
                .await?
                .json()
                .await
                .map_err(Error::internal)?;
            crate::microvm::wire::write(stream.get_mut(), &value).await?;
            stream.get_mut().shutdown().await?;
            Ok::<_, Error>(())
        };
        tokio::select! {
            () = stop.cancelled() => break,
            _ = tokio::time::timeout(Duration::from_secs(45), operation) => {}
        }
    }
    let _ = tokio::fs::remove_file(path).await;
    Ok(())
}

#[cfg(test)]
mod native_inbox_tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[tokio::test]
    async fn controller_notice_keeps_live_inbox_and_user_steering_readable() {
        let root = tempfile::tempdir().unwrap();
        let inbox = root.path().join("inbox");
        tokio::fs::create_dir(&inbox).await.unwrap();
        tokio::fs::write(inbox.join("messages.json"), b"[]")
            .await
            .unwrap();
        prepare_native_inbox(&inbox).await.unwrap();
        let inode = tokio::fs::metadata(&inbox).await.unwrap().ino();
        for text in [b"first notice".as_slice(), b"updated notice".as_slice()] {
            publish_native_notice(&inbox, text).await.unwrap();
            assert_eq!(tokio::fs::metadata(&inbox).await.unwrap().ino(), inode);
            assert_eq!(
                tokio::fs::metadata(&inbox)
                    .await
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
            for name in ["disk-notice.json", "messages.json"] {
                assert_eq!(
                    tokio::fs::metadata(inbox.join(name))
                        .await
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o444
                );
            }
            assert_eq!(
                tokio::fs::read(inbox.join("messages.json")).await.unwrap(),
                b"[]"
            );
            assert_eq!(
                tokio::fs::read(inbox.join("disk-notice.json"))
                    .await
                    .unwrap(),
                text
            );
        }
    }

    #[tokio::test]
    async fn empty_native_inbox_refresh_waits_for_messages_without_replacing_the_mount() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("target");
        crate::skills::private_dir(&source).await.unwrap();
        crate::skills::private_dir(&target).await.unwrap();
        let inode = tokio::fs::metadata(&target).await.unwrap().ino();
        tokio::fs::write(target.join("messages.json"), b"[]")
            .await
            .unwrap();

        install_native_inbox(&source, &target).await.unwrap();
        assert_eq!(
            tokio::fs::read(target.join("messages.json")).await.unwrap(),
            b"[]"
        );
        assert_eq!(tokio::fs::metadata(&target).await.unwrap().ino(), inode);

        tokio::fs::write(source.join("messages.json"), b"[1]")
            .await
            .unwrap();
        install_native_inbox(&source, &target).await.unwrap();
        assert_eq!(
            tokio::fs::read(target.join("messages.json")).await.unwrap(),
            b"[1]"
        );
        assert_eq!(tokio::fs::metadata(&target).await.unwrap().ino(), inode);
    }

    #[tokio::test]
    async fn native_refresh_publishes_readable_attachments_before_messages() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("inbox");
        crate::skills::private_dir(&target).await.unwrap();
        let inode = tokio::fs::metadata(&target).await.unwrap().ino();
        for name in ["initial", "follow-up"] {
            let source = root.path().join(name);
            let image = source.join("attachments").join(name).join("image.png");
            crate::skills::private_dir(image.parent().unwrap())
                .await
                .unwrap();
            tokio::fs::write(&image, b"synthetic image").await.unwrap();
            tokio::fs::set_permissions(&image, std::fs::Permissions::from_mode(0o600))
                .await
                .unwrap();
            tokio::fs::write(source.join("messages.json"), name)
                .await
                .unwrap();
            install_native_inbox(&source, &target).await.unwrap();
            let received = target.join("attachments").join(name).join("image.png");
            assert_eq!(
                tokio::fs::read(&received).await.unwrap(),
                b"synthetic image"
            );
            assert_eq!(
                tokio::fs::metadata(&received)
                    .await
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o444
            );
            assert_eq!(
                tokio::fs::metadata(received.parent().unwrap())
                    .await
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
            assert_eq!(
                tokio::fs::read(target.join("messages.json")).await.unwrap(),
                name.as_bytes()
            );
            assert_eq!(tokio::fs::metadata(&target).await.unwrap().ino(), inode);
        }
        assert!(target.join("attachments/initial/image.png").exists());
    }

    #[tokio::test]
    async fn native_initial_inbox_is_readable_without_relaxing_neighboring_secrets() {
        let root = tempfile::tempdir().unwrap();
        let inbox = root.path().join("chat-input");
        let image = inbox.join("attachments/image/screenshot.png");
        crate::skills::private_dir(image.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&image, b"image").await.unwrap();
        tokio::fs::set_permissions(&image, std::fs::Permissions::from_mode(0o600))
            .await
            .unwrap();
        let secret = root.path().join("credential");
        tokio::fs::write(&secret, b"synthetic secret")
            .await
            .unwrap();
        tokio::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600))
            .await
            .unwrap();
        prepare_native_inbox(&inbox).await.unwrap();
        for path in [&inbox, &inbox.join("attachments"), image.parent().unwrap()] {
            assert_eq!(
                tokio::fs::metadata(path)
                    .await
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
        }
        assert_eq!(
            tokio::fs::metadata(&image)
                .await
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o444
        );
        assert_eq!(
            tokio::fs::metadata(secret)
                .await
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[tokio::test]
    async fn native_inbox_rejects_redirected_attachments_before_publishing_messages() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("target");
        let outside = root.path().join("private");
        for path in [&source, &target, &outside] {
            crate::skills::private_dir(path).await.unwrap();
        }
        tokio::fs::write(source.join("messages.json"), b"new")
            .await
            .unwrap();
        tokio::fs::write(target.join("messages.json"), b"old")
            .await
            .unwrap();
        tokio::fs::symlink(&outside, source.join("attachments"))
            .await
            .unwrap();
        assert!(install_native_inbox(&source, &target).await.is_err());
        assert_eq!(
            tokio::fs::read(target.join("messages.json")).await.unwrap(),
            b"old"
        );
        assert_eq!(
            tokio::fs::metadata(&outside)
                .await
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        tokio::fs::remove_file(source.join("attachments"))
            .await
            .unwrap();
        crate::skills::private_dir(&source.join("attachments"))
            .await
            .unwrap();
        tokio::fs::symlink(&outside, target.join("attachments"))
            .await
            .unwrap();
        assert!(install_native_inbox(&source, &target).await.is_err());
        assert_eq!(
            tokio::fs::read(target.join("messages.json")).await.unwrap(),
            b"old"
        );
    }

    #[tokio::test]
    async fn native_refresh_preserves_the_bound_directory_and_unprivileged_read_access() {
        let root = tempfile::tempdir().unwrap();
        let (source, target) = (root.path().join("source"), root.path().join("inbox"));
        crate::skills::private_dir(&source).await.unwrap();
        crate::skills::private_dir(&target).await.unwrap();
        let inode = tokio::fs::metadata(&target).await.unwrap().ino();
        for message in [b"first".as_slice(), b"follow-up".as_slice()] {
            tokio::fs::write(source.join("messages.json"), message)
                .await
                .unwrap();
            install_native_inbox(&source, &target).await.unwrap();
            let metadata = tokio::fs::metadata(&target).await.unwrap();
            assert_eq!(metadata.ino(), inode);
            assert_eq!(metadata.permissions().mode() & 0o777, 0o755);
            assert_eq!(
                tokio::fs::metadata(target.join("messages.json"))
                    .await
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o444
            );
            assert_eq!(
                tokio::fs::read(target.join("messages.json")).await.unwrap(),
                message
            );
        }
    }
}

#[cfg(test)]
mod seed_tests {
    use super::*;

    #[tokio::test]
    async fn initial_seed_never_marks_a_remote_stage_as_manager_owned() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        tokio::fs::write(
            source
                .path()
                .join(crate::conversation_deletion::retired::MANAGER_NAMESPACE),
            b"manager-only",
        )
        .await
        .unwrap();
        tokio::fs::write(source.path().join("input.txt"), b"required input")
            .await
            .unwrap();
        let response = seed_source(
            &json!({}),
            &json!({ "id":crate::config::id() }),
            source.path(),
            &json!({ "kind":"initial" }),
            false,
        )
        .await
        .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 1_000_000)
            .await
            .unwrap();
        let mut reader = tokio::io::BufReader::new(bytes.as_ref());
        super::super::files::receive(&mut reader, target.path(), 1_000_000)
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read(target.path().join("input.txt"))
                .await
                .unwrap(),
            b"required input"
        );
        assert!(
            !target
                .path()
                .join(crate::conversation_deletion::retired::MANAGER_NAMESPACE)
                .exists()
        );
    }
}

/// Runtime refresh never removes the parent inode or the live broker sockets.
/// Only regular generated files from the manager replace their matching paths.
async fn install_runtime_files(source: &Path, target: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let mut pending = vec![(source.to_owned(), target.to_owned())];
    while let Some((source, target)) = pending.pop() {
        let metadata = tokio::fs::symlink_metadata(&source).await?;
        if metadata.is_dir() {
            if let Ok(metadata) = tokio::fs::symlink_metadata(&target).await {
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(Error::forbidden("Runtime directory was redirected."));
                }
            } else {
                crate::skills::private_dir(&target).await?;
            }
            let mut entries = tokio::fs::read_dir(&source).await?;
            while let Some(entry) = entries.next_entry().await? {
                pending.push((entry.path(), target.join(entry.file_name())));
            }
        } else if metadata.is_file() {
            if let Ok(metadata) = tokio::fs::symlink_metadata(&target).await
                && (!metadata.is_file() || metadata.file_type().is_symlink())
            {
                return Err(Error::forbidden("Runtime file was redirected."));
            }
            let owner = tokio::fs::metadata(&target)
                .await
                .ok()
                .or(tokio::fs::metadata(
                    target
                        .parent()
                        .ok_or_else(|| Error::bad("Missing runtime parent."))?,
                )
                .await
                .ok());
            crate::skills::atomic_write(&target, &tokio::fs::read(&source).await?).await?;
            if let Some(owner) = owner {
                std::os::unix::fs::lchown(&target, Some(owner.uid()), Some(owner.gid()))?;
            }
        } else {
            return Err(Error::forbidden(
                "Runtime refresh contains an unsupported entry.",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod provider_home_tests {
    use super::provider_home;
    use serde_json::json;

    #[test]
    fn each_coding_agent_relays_credentials_from_its_own_state_directory() {
        for (provider, home) in [
            ("codex", ".codex"),
            ("claude", ".claude"),
            ("opencode", ".opencode"),
            ("antigravity", ".antigravity"),
        ] {
            assert_eq!(
                provider_home(&json!({ "chat": { "provider": provider } })),
                home
            );
        }
        assert_eq!(provider_home(&json!({})), ".codex");
    }
}
