//! Capacity waits outlive code workers, while admitted moves retain their durable
//! placement/fencing state. Only bounded recent tool results live in memory.
use super::moves;
use crate::{
    error::{Error, Result},
    service::Service,
};
use cairn_contracts::{NodeMoveOperation, NodeMoveOperationState as State, NodeMoveStarted};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

#[derive(Clone, PartialEq)]
pub(crate) enum Audience {
    Public(String),
    Installation(String),
}

struct Entry {
    operation: NodeMoveOperation,
    audience: Audience,
    authorization: moves::Authorization,
    cancel_requested: bool,
    finished: Option<Instant>,
}

pub(crate) struct Operations {
    slots: Arc<Semaphore>,
    entries: Mutex<HashMap<String, Entry>>,
    waiting: Mutex<HashSet<String>>,
}

impl Default for Operations {
    fn default() -> Self {
        Self {
            slots: Arc::new(Semaphore::new(8)),
            entries: Mutex::default(),
            waiting: Mutex::default(),
        }
    }
}

impl Operations {
    pub(crate) fn claim_wait(&self, run: &str) -> Result<()> {
        if !self.waiting.lock().unwrap().insert(run.to_owned()) {
            return Err(Error::conflict("A movement is already pending."));
        }
        Ok(())
    }

    pub(crate) fn finish_wait(&self, run: &str) {
        self.waiting.lock().unwrap().remove(run);
    }

    pub(crate) fn start(
        &self,
        service: Service,
        run: Value,
        args: Value,
        authorization: moves::Authorization,
        audience: Audience,
    ) -> Result<Value> {
        if service.shutdown.is_cancelled() {
            return Err(Error::unavailable("Master is stopping."));
        }
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::too_many_requests("Node moves are busy. Retry shortly."))?;
        let id = uuid::Uuid::new_v4();
        let key = id.to_string();
        {
            let mut entries = self.entries.lock().unwrap();
            if entries.len() >= 64 {
                let expired = entries
                    .iter()
                    .filter_map(|(id, entry)| entry.finished.map(|finished| (id.clone(), finished)))
                    .min_by_key(|(_, finished)| *finished)
                    .map(|(id, _)| id);
                if let Some(expired) = expired {
                    entries.remove(&expired);
                } else {
                    return Err(Error::too_many_requests(
                        "Node moves are busy. Retry shortly.",
                    ));
                }
            }
            entries.insert(
                key.clone(),
                Entry {
                    operation: NodeMoveOperation {
                        id,
                        state: State::Running,
                        result: None,
                        error: None,
                        error_status: None,
                    },
                    audience,
                    authorization: authorization.clone(),
                    cancel_requested: false,
                    finished: None,
                },
            );
        }
        let origin = crate::native::policy::origin();
        // The configured capacity bound remains authoritative inside moves. The
        // extra bound covers its existing controller/DB preparation and cleanup.
        let timeout =
            Duration::from_secs(args["waitSeconds"].as_u64().unwrap_or(0).min(3600) + 120);
        tokio::spawn(async move {
            let _permit = permit;
            let work = crate::native::policy::DELEGATION.scope(
                origin,
                Box::pin(moves::request_authorized(
                    &service,
                    &run,
                    &args,
                    authorization.clone(),
                )),
            );
            let result = tokio::time::timeout(timeout, work)
                .await
                .unwrap_or_else(|_| {
                    authorization.cancel();
                    Err(Error::unavailable(
                        "Movement preparation timed out. Refresh placement and retry.",
                    ))
                })
                .and_then(|value| serde_json::from_value(value).map_err(Error::from));
            let mut entries = service.node_move_operations.entries.lock().unwrap();
            if let Some(entry) = entries.get_mut(&key) {
                match result {
                    Ok(result) => {
                        entry.operation.state = State::Completed;
                        entry.operation.result = Some(result);
                    }
                    Err(error)
                        if entry.cancel_requested
                            && !authorization.admitted()
                            && error.message == moves::CANCELLED_CAPACITY_WAIT =>
                    {
                        entry.operation.state = State::Cancelled;
                    }
                    Err(error) => {
                        entry.operation.state = State::Failed;
                        entry.operation.error_status = Some(error.status);
                        entry.operation.error = Some(error.message);
                    }
                }
                entry.finished = Some(Instant::now());
            }
        });
        Ok(serde_json::to_value(NodeMoveStarted { operation: id })?)
    }

    async fn authorize(&self, service: &Service, id: &str, audience: &Audience) -> Result<()> {
        let authorization = {
            let entries = self.entries.lock().unwrap();
            let entry = entries.get(id).ok_or_else(|| {
                Error::not_found("This move result is no longer available. Refresh placement.")
            })?;
            if &entry.audience != audience {
                return Err(Error::forbidden(
                    "This node move belongs to another caller.",
                ));
            }
            entry.authorization.clone()
        };
        authorization.recheck_access(service).await?;
        service
            .store
            .read(move |db| authorization.check_access(db))
            .await
    }

    pub(crate) async fn get(
        &self,
        service: &Service,
        id: &str,
        audience: &Audience,
    ) -> Result<Value> {
        self.authorize(service, id, audience).await?;
        let entries = self.entries.lock().unwrap();
        let entry = entries
            .get(id)
            .ok_or_else(|| Error::not_found("Move result expired."))?;
        Ok(serde_json::to_value(&entry.operation)?)
    }

    pub(crate) async fn cancel(
        &self,
        service: &Service,
        id: &str,
        audience: &Audience,
    ) -> Result<Value> {
        self.authorize(service, id, audience).await?;
        let mut entries = self.entries.lock().unwrap();
        let entry = entries
            .get_mut(id)
            .ok_or_else(|| Error::not_found("Move result expired."))?;
        if entry.operation.state == State::Running {
            if entry.authorization.admitted() {
                return Err(Error::conflict(
                    "The move was already admitted. Refresh placement.",
                ));
            }
            entry.cancel_requested = true;
            entry.authorization.cancel();
        }
        Ok(serde_json::to_value(&entry.operation)?)
    }
}
