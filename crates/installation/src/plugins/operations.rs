//! Owner-requested preparation outlives short HTTP requests. Only recent operation
//! results are kept in memory; executable/package locks remain authoritative.
use crate::{
    error::{Error, Result},
    service::Service,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;

#[derive(Clone, Serialize)]
struct Operation {
    id: String,
    state: &'static str,
    result: Option<Value>,
    error: Option<String>,
}

pub struct Operations {
    slots: Arc<Semaphore>,
    entries: Mutex<HashMap<String, Operation>>,
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
                    .find(|(_, operation)| operation.state != "running")
                    .map(|(id, _)| id.clone());
                if let Some(expired) = expired {
                    entries.remove(&expired);
                }
            }
            entries.insert(
                id.clone(),
                Operation {
                    id: id.clone(),
                    state: "running",
                    result: None,
                    error: None,
                },
            );
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
            if let Some(operation) = s
                .plugins
                .operations
                .entries
                .lock()
                .unwrap()
                .get_mut(&operation_id)
            {
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
            }
        });
        Ok(json!({ "operation": id }))
    }

    pub fn get(&self, id: &str) -> Result<Value> {
        let entries = self.entries.lock().unwrap();
        let operation = entries.get(id).ok_or_else(|| {
            Error::not_found("This operation is no longer available. Refresh the plugin.")
        })?;
        Ok(serde_json::to_value(operation)?)
    }
}
