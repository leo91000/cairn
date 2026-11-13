//! One-time assignment of a prepared local disk after its owner is durable.
use crate::error::{Error, Result};
use serde_json::Value;
use std::sync::{Mutex, RwLock, TryLockError};

pub(crate) struct Assignment {
    identity: RwLock<Option<String>>,
    assigning: Mutex<()>,
}

impl Assignment {
    pub fn new(context: &Value) -> Result<Self> {
        Ok(Self {
            identity: RwLock::new(Some(Self::identity(context)?)),
            assigning: Mutex::new(()),
        })
    }

    pub fn prepared() -> Self {
        Self {
            identity: RwLock::new(None),
            assigning: Mutex::new(()),
        }
    }

    fn identity(context: &Value) -> Result<String> {
        if context["unassigned"] == true {
            return Err(Error::bad("Conversation disk cannot be marked unassigned."));
        }
        if let Some(owner) = context["owner"].as_str().filter(|owner| !owner.is_empty()) {
            return Ok(owner.to_owned());
        }
        // Existing entirely local journals can still carry their old grant.
        // Its digest is only a stable assignment identity; it confers no authority.
        context["grant"]
            .as_str()
            .filter(|grant| !grant.is_empty())
            .map(crate::auth::digest)
            .ok_or_else(|| Error::bad("Missing local disk owner."))
    }

    pub fn assigned(&self) -> Result<Option<String>> {
        Ok(self.identity.read().map_err(Error::internal)?.clone())
    }

    pub fn assign(&self, context: &Value, persist: impl FnOnce() -> Result<()>) -> Result<()> {
        let identity = Self::identity(context)?;
        let _assigning = self.assigning.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => Error::conflict("Disk assignment is in progress."),
            TryLockError::Poisoned(error) => Error::internal(error),
        })?;
        if self.assigned()?.is_some() {
            return Err(Error::conflict("Disk has already been assigned."));
        }
        // Keep health probes independent of the durable assignment's fsync.
        persist()?;
        *self.identity.write().map_err(Error::internal)? = Some(identity);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn assignment_is_visible_only_after_persistence_and_cannot_be_replaced() {
        let assignment = Assignment::prepared();
        let context = json!({ "owner": "conversation" });
        assert!(
            assignment
                .assign(&context, || Err(Error::unavailable("fsync failed")))
                .is_err()
        );
        assert_eq!(assignment.assigned().unwrap(), None);
        assignment.assign(&context, || Ok(())).unwrap();
        assert_eq!(
            assignment.assigned().unwrap().as_deref(),
            Some("conversation")
        );
        assert!(
            assignment
                .assign(&json!({ "owner": "another" }), || panic!(
                    "must preserve owner"
                ))
                .is_err()
        );
        assert_eq!(
            assignment.assigned().unwrap().as_deref(),
            Some("conversation")
        );
    }

    #[test]
    fn assignment_fsync_keeps_probes_and_duplicate_assignment_nonblocking() {
        let assignment = std::sync::Arc::new(Assignment::prepared());
        let (entered, waiting) = std::sync::mpsc::channel();
        let (resume, gate) = std::sync::mpsc::channel();
        let assigning = assignment.clone();
        let worker = std::thread::spawn(move || {
            assigning
                .assign(&json!({ "owner": "conversation" }), || {
                    entered.send(()).unwrap();
                    gate.recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap();
                    Ok(())
                })
                .unwrap();
        });
        waiting
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert_eq!(assignment.assigned().unwrap(), None);
        assert!(
            assignment
                .assign(&json!({ "owner": "another" }), || panic!(
                    "concurrent persistence"
                ))
                .is_err()
        );
        resume.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(
            assignment.assigned().unwrap().as_deref(),
            Some("conversation")
        );
    }
}
