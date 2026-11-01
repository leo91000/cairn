//! Ephemeral credential generations for one active worker execution.
//!
//! This is not a Vault cache or a permanent secret archive. Histories have the
//! same append-only lifetime as the worker's old sensitive Vec; memory grows
//! with distinct observed values. Registry membership and in-flight values are
//! weak, and disappear deterministically when their owners end.
use super::Lease;
use crate::{
    error::{Error, Result},
    provider::Provider,
    service::Service,
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, Weak},
};

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // These locks contain only append/deduplicate bookkeeping, never callbacks
    // or I/O. A panic must not reset an already learned redaction history.
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Shared with the outer failure publisher; a snapshot never holds the lock
/// during Store/log I/O. No Debug/Serialize implementation may expose values.
#[derive(Clone, Default)]
pub(crate) struct History(Arc<Mutex<Arc<Vec<String>>>>);

impl History {
    pub(crate) fn extend(&self, values: impl IntoIterator<Item = String>) {
        // Evaluate caller iterators before taking the history lock.
        let values: Vec<_> = values.into_iter().collect();
        let mut current = locked(&self.0);
        for value in values {
            if !value.is_empty() && !current.contains(&value) {
                Arc::make_mut(&mut current).push(value);
            }
        }
    }

    pub(crate) fn snapshot(&self) -> Arc<Vec<String>> {
        locked(&self.0).clone()
    }
}

#[derive(Clone, Default)]
pub(crate) struct Registry(
    Arc<Mutex<HashMap<String, Account>>>,
    #[cfg(test)] Arc<Mutex<Option<Arc<tests::CommitGate>>>>,
);

#[derive(Default)]
struct Account {
    scopes: HashMap<String, Weak<Scope>>,
    changes: HashMap<String, Weak<Pending>>,
}

impl Account {
    fn prune(&mut self) {
        self.scopes.retain(|_, scope| scope.strong_count() != 0);
        self.changes.retain(|_, change| change.strong_count() != 0);
    }

    fn empty(&self) -> bool {
        self.scopes.is_empty() && self.changes.is_empty()
    }
}

struct Pending(Vec<String>);

struct Scope {
    account: String,
    run: String,
    home: PathBuf,
    state: Mutex<State>,
}

struct State {
    active: bool,
    history: History,
}

impl Scope {
    fn check(&self, state: &State, lease: &Lease) -> Result<()> {
        if !state.active
            || lease.provider != Provider::Codex
            || lease.account_id != self.account
            || lease.run_id != self.run
            || lease.home != self.home
        {
            return Err(Error::conflict("This output credential scope has ended."));
        }
        Ok(())
    }

    fn observe(&self, values: &[String]) {
        let state = locked(&self.state);
        if state.active {
            state.history.extend(values.iter().cloned());
        }
    }
}

/// Owns registration. Cloned handles cannot keep registration active after its
/// worker owner revokes or drops it. Identity is unique to this launch attempt.
pub(crate) struct Observation {
    registry: Registry,
    id: String,
    scope: Arc<Scope>,
}

#[derive(Clone)]
pub(crate) struct Handle {
    registry: Registry,
    scope: Arc<Scope>,
}

impl Observation {
    pub(crate) fn handle(&self) -> Handle {
        Handle {
            registry: self.registry.clone(),
            scope: self.scope.clone(),
        }
    }

    pub(crate) fn active(&self) -> bool {
        locked(&self.scope.state).active
    }

    /// Stops NEW admission. Joining/aborting and awaiting owned readers and the
    /// broker is the async completion barrier, not this synchronous operation.
    pub(crate) fn revoke(&self) {
        locked(&self.scope.state).active = false;
        let mut accounts = locked(&self.registry.0);
        if let Some(account) = accounts.get_mut(&self.scope.account) {
            account.scopes.remove(&self.id);
            account.prune();
            if account.empty() {
                accounts.remove(&self.scope.account);
            }
        }
    }
}

impl Drop for Observation {
    fn drop(&mut self) {
        self.revoke();
    }
}

/// Kept inside the actual Store closure AND its returned T until commit/reply.
/// A cancelled caller cannot make a still-queued generation invisible.
pub(crate) struct Change {
    registry: Registry,
    account: String,
    id: String,
    _pending: Arc<Pending>,
    #[cfg(test)]
    gate: Option<Arc<tests::CommitGate>>,
}

#[cfg(test)]
impl Change {
    pub(crate) fn before_commit_fixture(&self) {
        if let Some(gate) = &self.gate {
            gate.wait();
        }
    }
}

impl Drop for Change {
    fn drop(&mut self) {
        let mut accounts = locked(&self.registry.0);
        if let Some(account) = accounts.get_mut(&self.account) {
            account.changes.remove(&self.id);
            account.prune();
            if account.empty() {
                accounts.remove(&self.account);
            }
        }
    }
}

/// Exact response learned before its first socket byte. An admitted response
/// retains the scope/history until it completes or its broker task is joined.
pub(crate) struct Delivery {
    _change: Change,
    _scope: Arc<Scope>,
}

impl Handle {
    pub(crate) fn check(&self, lease: &Lease) -> Result<()> {
        let state = locked(&self.scope.state);
        self.scope.check(&state, lease)
    }

    pub(crate) fn admit(&self, lease: &Lease, values: Vec<String>) -> Result<Delivery> {
        {
            let state = locked(&self.scope.state);
            self.scope.check(&state, lease)?;
            state.history.extend(values.iter().cloned());
        }
        // Never take registry while holding the scope lock. Revocation after
        // admission is allowed; cleanup must join this delivery before closing.
        Ok(Delivery {
            _change: self.registry.begin(&lease.account_id, values),
            _scope: self.scope.clone(),
        })
    }
}

impl Registry {
    fn register(&self, lease: &Lease, attempt: &str, history: History) -> Observation {
        let scope = Arc::new(Scope {
            account: lease.account_id.clone(),
            run: lease.run_id.clone(),
            home: lease.home.clone(),
            state: Mutex::new(State {
                active: true,
                history,
            }),
        });
        // attempt IDs identify launches, but a random registration suffix also
        // prevents an accidentally reused attempt ID replacing another owner.
        let id = format!("{attempt}:{}", crate::config::id());
        let mut accounts = locked(&self.0);
        let account = accounts.entry(lease.account_id.clone()).or_default();
        account.prune();
        account.scopes.insert(id.clone(), Arc::downgrade(&scope));
        for pending in account.changes.values().filter_map(Weak::upgrade) {
            scope.observe(&pending.0);
        }
        Observation {
            registry: self.clone(),
            id,
            scope,
        }
    }

    pub(crate) fn begin(&self, id: &str, values: Vec<String>) -> Change {
        let pending = Arc::new(Pending(values));
        let change_id = crate::config::id();
        let scopes = {
            let mut accounts = locked(&self.0);
            let account = accounts.entry(id.into()).or_default();
            account.prune();
            account
                .changes
                .insert(change_id.clone(), Arc::downgrade(&pending));
            account
                .scopes
                .values()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>()
        };
        for scope in scopes {
            scope.observe(&pending.0);
        }
        Change {
            registry: self.clone(),
            account: id.into(),
            id: change_id,
            _pending: pending,
            #[cfg(test)]
            gate: locked(&self.1).take(),
        }
    }
}

impl super::Accounts {
    pub(crate) async fn observe_output_credentials(
        &self,
        s: &Service,
        lease: &Lease,
        attempt: &str,
        history: History,
    ) -> Result<Option<Observation>> {
        if lease.provider != Provider::Codex {
            return Ok(None);
        }
        let current = self.lease(s, &lease.run_id).await?;
        if current.is_none_or(|current| {
            current.provider != Provider::Codex
                || current.account_id != lease.account_id
                || current.home != lease.home
        }) {
            return Err(Error::conflict("This account lease has ended."));
        }
        // Register BEFORE the first Vault read. Old/new publication and live
        // pending changes cover writes both before and after this subscription.
        let observation = self
            .output_credentials
            .register(lease, attempt, history.clone());
        history.extend(self.redactions(s, lease).await?);
        Ok(Some(observation))
    }
}

#[cfg(test)]
mod tests;
