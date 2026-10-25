//! Owner-requested preparation outlives short HTTP requests. Only recent operation
//! results are kept in memory; executable/package locks remain authoritative.
use crate::{
    error::{Error, Result},
    service::Service,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::Future,
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;

/// A delayed management action belongs to the owner who admitted it, including
/// that exact grant of access. Reclaiming an installation does not resume it.
pub struct Owner(cairn_protocol::TaskAuthorGrant);

impl Owner {
    pub(crate) fn account_id(&self) -> &str {
        &self.0.account_id
    }

    pub(crate) async fn current(s: &Service) -> Result<Self> {
        crate::relay::task_author_policy(s)
            .await?
            .map(|policy| Self(policy.owner))
            .ok_or_else(|| Error::forbidden("The installation has no verified owner."))
    }

    pub(crate) async fn for_account(s: &Service, account: &str) -> Result<Self> {
        let owner = Self::current(s).await?;
        if owner.0.account_id != account {
            return Err(Error::forbidden(
                "This action requires the current installation owner.",
            ));
        }
        Ok(owner)
    }

    pub(crate) async fn recheck(&self, s: &Service) -> Result<()> {
        let owner = Self::current(s).await?;
        if self.0.account_id != owner.0.account_id || self.0.access_id != owner.0.access_id {
            return Err(Error::forbidden(
                "The installation owner's access changed. Retry the action.",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Serialize)]
struct Operation {
    id: String,
    state: &'static str,
    result: Option<Value>,
    error: Option<String>,
}

pub struct Operations {
    slots: Arc<Semaphore>,
    entries: Mutex<VecDeque<Operation>>,
}

impl Default for Operations {
    fn default() -> Self {
        Self {
            slots: Arc::new(Semaphore::new(4)),
            entries: Mutex::new(VecDeque::new()),
        }
    }
}

impl Operations {
    pub fn start(
        &self,
        s: Arc<Service>,
        work: impl Future<Output = Result<Value>> + Send + 'static,
    ) -> Result<Value> {
        let permit =
            self.slots.clone().try_acquire_owned().map_err(|_| {
                Error::too_many_requests("Plugin preparation is busy. Retry shortly.")
            })?;
        let id = uuid::Uuid::new_v4().to_string();
        {
            let mut entries = self.entries.lock().unwrap();
            if entries.len() >= 64 {
                let expired = entries
                    .iter()
                    .position(|operation| operation.state != "running");
                if let Some(expired) = expired {
                    entries.remove(expired);
                }
            }
            entries.push_back(Operation {
                id: id.clone(),
                state: "running",
                result: None,
                error: None,
            });
        }
        let operation_id = id.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let result = tokio::select! {
                () = s.shutdown.cancelled() => {
                    Err(Error::unavailable("Preparation was interrupted. Refresh the plugin and retry."))
                },
                result = tokio::time::timeout(std::time::Duration::from_secs(540), work) => {
                    result.unwrap_or_else(|_| {
                        Err(Error::unavailable("Preparation timed out. Refresh the plugin and retry."))
                    })
                },
            };
            let mut entries = s.plugins.operations.entries.lock().unwrap();
            if let Some(index) = entries.iter().position(|entry| entry.id == operation_id) {
                let mut operation = entries.remove(index).unwrap();
                match result {
                    Ok(result) => {
                        operation.state = "completed";
                        operation.result = Some(result);
                    }
                    Err(error) => {
                        operation.state = "failed";
                        operation.error = Some(error.message);
                    }
                }
                // Completion order preserves a slow job's fresh result as well
                // as recent fast jobs. Running work is never evicted.
                entries.push_back(operation);
            }
        });
        Ok(json!({ "operation": id }))
    }

    pub fn get(&self, id: &str) -> Result<Value> {
        let entries = self.entries.lock().unwrap();
        let operation = entries.iter().find(|entry| entry.id == id).ok_or_else(|| {
            Error::not_found("This operation is no longer available. Refresh the plugin.")
        })?;
        Ok(serde_json::to_value(operation)?)
    }
}
