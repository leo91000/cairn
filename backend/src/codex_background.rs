//! Isolated, subscription-backed Codex work that yields to foreground executions.
//! Callers own prompts and results; this module owns credentials and lease cleanup.
use crate::{
    accounts::{broker, codex::Client},
    config::id,
    error::{Error, Result},
    provider::Provider,
    rpc::{Incoming, Session},
    service::Service,
    validation::text,
};
use futures_util::future::BoxFuture;
use serde_json::Value;
use std::path::Path;
use tokio_util::sync::CancellationToken;

pub const STOPPED: &str = "Background Codex execution stopped.";
pub const YIELDED: &str = "Background Codex execution yielded to foreground work.";

pub const UNAVAILABLE: &str =
    "No Codex account is available. Check Connections and its usage limits, then try again.";

pub async fn should_yield(s: &Service) -> Result<bool> {
    s.store
        .read(|db| {
            Ok(db.kv("deployment-lease")?.is_some()
                || db.0.query_row(
                    "SELECT EXISTS(SELECT 1 FROM runs WHERE status='queued'
             AND COALESCE(json_extract(data,'$.snapshot.agent.provider'),'codex')!='claude')",
                    [],
                    |row| row.get::<_, bool>(0),
                )?)
        })
        .await
}

async fn yield_requested(s: &Service) -> Result<()> {
    // Subscribe before reading so a queued conversation cannot be missed between them.
    let mut changes = s.store.subscribe();
    loop {
        if should_yield(s).await? {
            return Ok(());
        }
        changes
            .changed()
            .await
            .map_err(|_| Error::unavailable(STOPPED))?;
    }
}

pub async fn run<T>(
    s: &Service,
    model: &str,
    stop: &CancellationToken,
    operation: impl for<'a> FnOnce(&'a mut Session, &'a Path) -> BoxFuture<'a, Result<T>>,
) -> Result<T> {
    if model.is_empty() {
        return Err(Error::bad(
            "Background Codex work requires an explicit model.",
        ));
    }
    if stop.is_cancelled() || s.shutdown.is_cancelled() {
        return Err(Error::unavailable(STOPPED));
    }
    if should_yield(s).await? {
        return Err(Error::unavailable(YIELDED));
    }
    let lease_id = id();
    // Acquisition and cleanup must complete even when cancellation arrives: dropping
    // acquisition midway could leave a newly registered lease without an owner.
    let mut lease = s
        .accounts
        .acquire(s, &lease_id, Provider::Codex, model)
        .await
        .map_err(|_| Error::unavailable(UNAVAILABLE))?
        .ok_or_else(|| Error::unavailable(UNAVAILABLE))?;
    let result = async {
        if stop.is_cancelled() || s.shutdown.is_cancelled() {
            return Err(Error::unavailable(STOPPED));
        }
        let directory = tempfile::Builder::new()
            .prefix("leo-codex-background-")
            .tempdir()?;
        let home = directory.path().join("codex");
        let cwd = directory.path().join("work");
        crate::skills::private_dir(&cwd).await?;
        s.accounts.relocate(s, &mut lease, &home).await?;
        let _broker = broker::serve(s, &lease).await?;
        let mut config = s.config.clone();
        config.home = directory.path().to_owned();
        let mut session = Session::codex(&config, &home, &[], Some(&cwd)).await?;
        let authenticated = async {
            let mut auth = Client::from_socket(home.join(broker::SOCKET))
                .ok_or_else(|| Error::unavailable("Missing background authentication."))?;
            auth.login(&mut session).await?;
            session.auth = Some(auth);
            operation(&mut session, &cwd).await
        };
        let result = tokio::select! {
            biased;
            _ = stop.cancelled() => Err(Error::unavailable(STOPPED)),
            _ = s.shutdown.cancelled() => Err(Error::unavailable(STOPPED)),
            _ = yield_requested(s) => Err(Error::unavailable(YIELDED)),
            result = authenticated => result,
        };
        session.close().await;
        result
    }
    .await;
    let released = s.accounts.release(&lease).await;
    let _ = tokio::fs::remove_dir_all(s.config.data_dir.join("runs").join(&lease_id)).await;
    released.and(result)
}

/// Read a turn without losing notifications that precede its acknowledgement.
/// The caller returns Some(result) once its requested output is complete.
pub async fn turn<T>(
    session: &mut Session,
    params: Value,
    mut receive: impl FnMut(&Incoming) -> Result<Option<T>>,
) -> Result<T> {
    let thread_id = text(&params, "threadId").to_owned();
    if thread_id.is_empty() {
        return Err(Error::bad("Missing background thread."));
    }
    let rpc = session.rpc.clone();
    let request = rpc.request("turn/start", params);
    tokio::pin!(request);
    let mut acknowledged = false;
    loop {
        tokio::select! {
            result = &mut request,
            if !acknowledged => { result?; acknowledged = true; }
            incoming = session.incoming.recv() => {
                let incoming = incoming.ok_or_else(|| Error::bad("Background session disconnected."))?;
                if session.handle_auth(&incoming).await? { continue }
                if let Some(id) = incoming.id { rpc.reject(id).await?; continue }
                if incoming.params["threadId"] != thread_id { continue }
                if let Some(result) = receive(&incoming)? { return Ok(result) }
                if incoming.method == "turn/completed" {
                    return Err(Error::bad("Codex finished without the requested output."));
                }
            }
        }
    }
}
