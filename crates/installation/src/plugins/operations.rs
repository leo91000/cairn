//! Owner-requested preparation outlives short HTTP requests. Only recent operation
//! results are kept in memory; executable/package locks remain authoritative.
use crate::{
    error::{Error, Result},
    service::Service,
};
use cairn_contracts::{
    PluginOperation as Operation, PluginOperationResult, PluginOperationState as State,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;

// A preparation can be dropped while its transaction is still queued on the
// database actor. Capture this token when enqueueing writes, then check it on
// that actor before admitting publication. Ordinary writes retain their existing
// commit-after-caller-cancellation behavior.
tokio::task_local! {
    static PREPARATION: tokio_util::sync::CancellationToken;
}

pub(crate) fn publication_cancellation() -> Option<tokio_util::sync::CancellationToken> {
    PREPARATION.try_with(Clone::clone).ok()
}

pub(crate) fn check_publication() -> Result<()> {
    if publication_cancellation().is_some_and(|token| token.is_cancelled()) {
        return Err(Error::conflict("Preparation was cancelled."));
    }
    Ok(())
}

#[derive(Clone, PartialEq)]
pub enum Audience {
    Owner,
    Public,
    Installation(String),
}

impl Audience {
    pub(crate) fn from_bearer(bearer: Option<&str>, owner: bool) -> Self {
        match bearer {
            Some(bearer) => Self::Installation(crate::auth::hex_digest(bearer)),
            None if owner => Self::Owner,
            None => Self::Public,
        }
    }
}

struct Entry {
    operation: Operation,
    audience: Audience,
    cancellation: tokio_util::sync::CancellationToken,
    resource: Option<Arc<super::resources::Read>>,
    result_authority: Option<crate::capabilities::plugin_packages::Authority>,
}

pub struct Operations {
    slots: Arc<Semaphore>,
    entries: Mutex<HashMap<String, Entry>>,
}

impl Default for Operations {
    fn default() -> Self {
        Self {
            slots: Arc::new(Semaphore::new(4)),
            entries: Mutex::new(HashMap::new()),
        }
    }
}

impl Operations {
    pub fn start(
        &self,
        s: Arc<Service>,
        audience: Audience,
        work: impl Future<Output = Result<PluginOperationResult>> + Send + 'static,
    ) -> Result<Value> {
        self.start_with_resource(s, audience, None, None, work)
    }

    pub(crate) fn start_resource_read(
        &self,
        s: Arc<Service>,
        audience: Audience,
        resource: Arc<super::resources::Read>,
        work: impl Future<Output = Result<PluginOperationResult>> + Send + 'static,
    ) -> Result<Value> {
        self.start_with_resource(s, audience, Some(resource), None, work)
    }

    pub(crate) fn start_with_result_authority(
        &self,
        s: Arc<Service>,
        audience: Audience,
        authority: crate::capabilities::plugin_packages::Authority,
        work: impl Future<Output = Result<PluginOperationResult>> + Send + 'static,
    ) -> Result<Value> {
        self.start_with_resource(s, audience, None, Some(authority), work)
    }

    fn start_with_resource(
        &self,
        s: Arc<Service>,
        audience: Audience,
        resource: Option<Arc<super::resources::Read>>,
        result_authority: Option<crate::capabilities::plugin_packages::Authority>,
        work: impl Future<Output = Result<PluginOperationResult>> + Send + 'static,
    ) -> Result<Value> {
        let permit =
            self.slots.clone().try_acquire_owned().map_err(|_| {
                Error::too_many_requests("Plugin preparation is busy. Retry shortly.")
            })?;
        let id = uuid::Uuid::new_v4().to_string();
        let cancellation = tokio_util::sync::CancellationToken::new();
        {
            let mut entries = self.entries.lock().unwrap();
            if entries.len() >= 64 {
                let expired = entries
                    .iter()
                    .find(|(_, operation)| operation.operation.state != State::Running)
                    .map(|(id, _)| id.clone());
                if let Some(expired) = expired {
                    entries.remove(&expired);
                }
            }
            entries.insert(
                id.clone(),
                Entry {
                    audience,
                    resource,
                    result_authority,
                    cancellation: cancellation.clone(),
                    operation: Operation {
                        id: id.clone(),
                        state: State::Running,
                        result: None,
                        error: None,
                        error_status: None,
                    },
                },
            );
        }
        let operation_id = id.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _cancellation_guard = cancellation.clone().drop_guard();
            let work = PREPARATION.scope(cancellation.clone(), work);
            let result = tokio::select! {
                biased;
                () = cancellation.cancelled() => return,
                () = s.shutdown.cancelled() => {
                    cancellation.cancel();
                    Err(Error::unavailable("Preparation was interrupted. Refresh the plugin and retry."))
                },
                result = tokio::time::timeout(std::time::Duration::from_secs(540), work) => {
                    result.unwrap_or_else(|_| {
                        cancellation.cancel();
                        Err(Error::unavailable("Preparation timed out. Refresh the plugin and retry."))
                    })
                },
            };
            if let Some(operation) = s
                .plugins
                .operations
                .entries
                .lock()
                .unwrap()
                .get_mut(&operation_id)
            {
                let operation = &mut operation.operation;
                if operation.state != State::Running {
                    return;
                }
                match result {
                    Ok(result) => {
                        operation.state = State::Completed;
                        operation.result = Some(result);
                    }
                    Err(error) => {
                        operation.state = State::Failed;
                        operation.error_status = Some(error.status);
                        operation.error = Some(error.message);
                    }
                }
            }
        });
        Ok(json!({ "operation": id }))
    }

    pub(crate) fn authorize(&self, id: &str, bearer: Option<&str>, owner: bool) -> Result<()> {
        let entries = self.entries.lock().unwrap();
        let entry = entries.get(id).ok_or_else(|| {
            Error::not_found("This operation is no longer available. Refresh the plugin.")
        })?;
        if !owner && entry.audience != Audience::from_bearer(bearer, false) {
            return Err(Error::forbidden(
                "This preparation belongs to another caller.",
            ));
        }
        Ok(())
    }

    pub(crate) fn result_authority(
        &self,
        id: &str,
    ) -> Result<Option<crate::capabilities::plugin_packages::Authority>> {
        let entries = self.entries.lock().unwrap();
        let entry = entries
            .get(id)
            .ok_or_else(|| Error::not_found("Operation not found."))?;
        Ok(entry.result_authority.clone())
    }

    pub(crate) fn resource_read(
        &self,
        id: &str,
        resource_only: bool,
    ) -> Result<Option<Arc<super::resources::Read>>> {
        let entries = self.entries.lock().unwrap();
        let entry = entries
            .get(id)
            .ok_or_else(|| Error::not_found("Operation not found."))?;
        if resource_only && entry.resource.is_none() {
            return Err(Error::forbidden("This is not a resource read operation."));
        }
        Ok(entry.resource.clone())
    }

    pub(crate) fn cancel(&self, id: &str) -> Result<()> {
        let mut entries = self.entries.lock().unwrap();
        let entry = entries
            .get_mut(id)
            .ok_or_else(|| Error::not_found("Operation not found."))?;
        if entry.operation.state == State::Running {
            entry.operation.state = State::Cancelled;
            entry.cancellation.cancel();
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Value> {
        let entries = self.entries.lock().unwrap();
        let operation = entries.get(id).ok_or_else(|| {
            Error::not_found("This operation is no longer available. Refresh the plugin.")
        })?;
        Ok(serde_json::to_value(&operation.operation)?)
    }
}
