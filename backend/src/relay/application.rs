//! DataChannel ingress uses the same dispatcher and credited stream implementation as the tunnel.
use super::{StreamOutput, dispatch, request_failure};
use crate::{
    direct::DirectLease,
    error::{Error, Result},
};
use axum::Router;
use leo_relay_protocol::{Frame, MAX_IN_FLIGHT, MAX_STREAMS, MAX_STREAMS_PER_ACCOUNT, stream_path};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};
use tokio::{
    sync::{Semaphore, mpsc},
    task::{AbortHandle, JoinSet},
};

#[derive(Clone)]
pub(crate) struct DirectTraffic {
    requests: Arc<Semaphore>,
    streams: Arc<Semaphore>,
    accounts: Arc<Mutex<HashMap<String, Weak<Semaphore>>>>,
}

impl Default for DirectTraffic {
    fn default() -> Self {
        Self {
            requests: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
            streams: Arc::new(Semaphore::new(MAX_STREAMS)),
            accounts: Arc::default(),
        }
    }
}

impl DirectTraffic {
    pub(crate) async fn serve(
        &self,
        router: Router,
        lease: DirectLease,
        mut input: mpsc::Receiver<Frame>,
        output: mpsc::Sender<Frame>,
    ) -> Result<()> {
        let account_slots = {
            let mut accounts = self.accounts.lock().unwrap();
            accounts.retain(|_, slots| slots.strong_count() > 0);
            let entry = accounts.entry(lease.claims.account_id.clone()).or_default();
            let slots = entry
                .upgrade()
                .unwrap_or_else(|| Arc::new(Semaphore::new(MAX_STREAMS_PER_ACCOUNT)));
            *entry = Arc::downgrade(&slots);
            slots
        };
        let mut jobs = JoinSet::new();
        let mut tasks = HashMap::new();
        let mut active = HashMap::<String, (AbortHandle, Arc<Semaphore>)>::new();
        loop {
            tokio::select! {
                biased;
                () = lease.closed.cancelled() => return Ok(()),
                completed = jobs.join_next_with_id(), if !jobs.is_empty() => {
                    let task = match completed.unwrap() {
                        Ok((id, ())) => id,
                        Err(error) => error.id(),
                    };
                    if let Some(id) = tasks.remove(&task) { active.remove(&id); }
                }
                frame = input.recv() => {
                    match frame {
                        Some(Frame::Request(mut request)) => {
                            let id = request.id.clone();
                            if id.is_empty() || id.len() > 128 || active.contains_key(&id) {
                                return Err(Error::bad("Invalid or duplicate request ID."));
                            }
                            let stream = stream_path(&request.path);
                            let permits = self.requests.clone().try_acquire_owned().ok().and_then(|request_slot| {
                                if stream {
                                    let stream_slot = self.streams.clone().try_acquire_owned().ok()?;
                                    let account_slot = account_slots.clone().try_acquire_owned().ok()?;
                                    Some((request_slot, Some((stream_slot, account_slot))))
                                } else { Some((request_slot, None)) }
                            });
                            let Some(permits) = permits else {
                                output.try_send(Frame::Response(request_failure(id, 503, "Installation busy.")))
                                    .map_err(|_| Error::unavailable("Direct response queue full."))?;
                                continue;
                            };
                            // Only the verified lease supplies identity and capabilities.
                            request.account_id.clone_from(&lease.claims.account_id);
                            request.role = lease.claims.role;
                            request.mcp_scopes = None;
                            request.public_artifact = None;
                            let credit = Arc::new(Semaphore::new(0));
                            let streaming = Some(StreamOutput { frames: output.clone(), credit: credit.clone() });
                            let router = router.clone();
                            let output = output.clone();
                            let reply_id = id.clone();
                            let task = jobs.spawn(async move {
                                let _permits = permits;
                                let response = match dispatch(router, request, streaming).await {
                                    Ok(Some(response)) => response,
                                    Ok(None) => return,
                                    Err(error) => request_failure(reply_id, error.status, &error.message),
                                };
                                let _ = output.send(Frame::Response(response)).await;
                            });
                            tasks.insert(task.id(), id.clone());
                            active.insert(id, (task, credit));
                        }
                        Some(Frame::StreamCredit { id }) => {
                            if let Some((_, credit)) = active.get(&id) && credit.available_permits() == 0 { credit.add_permits(1); }
                        }
                        Some(Frame::Cancel { id }) => {
                            if let Some((task, _)) = active.remove(&id) { task.abort(); }
                        }
                        None => return Ok(()),
                        _ => return Err(Error::bad("Unexpected direct application frame.")),
                    }
                }
            }
        }
    }
}
