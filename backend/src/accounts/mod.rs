//! One pool of coding-agent accounts.
//!
//! The pool owns what every coding agent shares: account records, pause and parallel-run
//! limits, selection by remaining capacity, run leases, usage polling and sign-in
//! orchestration. Each coding agent's [`Driver`] owns what differs: its official CLI's sign-in
//! protocol, credential storage, usage reading and the credentials handed to a run.
pub mod broker;
pub mod claude;
pub mod codex;
pub(crate) mod lenient;
mod record;
mod sign_in;
pub mod usage;

pub use record::{Account, AccountState, Exhaustion};
pub use sign_in::{Login, Phase, Progress, SignInState};

use crate::{
    config::now,
    error::{Error, Result, required},
    http::Input,
    provider::Provider,
    service::Service,
    skills::private_dir,
    store::{Db, merge},
    validation::{text, uuid},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sign_in::SignIn;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::{Mutex, OnceCell};
use usage::Usage;

pub const KIND: &str = "agentAccounts";
const MAX_ACCOUNTS: usize = 10;
const PARALLEL_RUNS: u64 = 4;
const LOW: f64 = 10.;
const SIGN_IN_MS: i64 = 15 * 60_000;

/// A run's hold on one account slot. The run reaches credentials through its broker in `home`.
#[derive(Clone, Debug)]
pub struct Lease {
    pub account_id: String,
    pub provider: Provider,
    pub run_id: String,
    pub home: PathBuf,
    pub model: String,
}

#[async_trait::async_trait]
pub trait Driver: Send + Sync {
    /// Cleans up interrupted work and migrates state from earlier versions, once per start.
    async fn initialize(&self, s: &Service) -> Result<()>;

    /// Codex runs on the host's own login until a managed account is added.
    async fn managed(&self, s: &Service) -> Result<bool>;

    /// Records that an account was added, in the transaction that adds it.
    fn added(&self, _db: &mut Db<'_>) -> Result<()> {
        Ok(())
    }

    /// Signs in with the official CLI inside `home`, publishing its link and code to `login`.
    async fn authorize(&self, s: &Service, home: &Path, login: &Login) -> Result<()>;

    /// Verifies the identity signed in within `home`, then keeps its credentials for `id`.
    /// Caller holds the selection and account locks.
    async fn adopt(&self, s: &Service, id: &str, home: &Path) -> Result<()>;

    /// Reads identity and usage into the record. `models` currently run on the account.
    /// Caller holds the account lock.
    async fn refresh(&self, s: &Service, id: &str, models: &[String]) -> Result<()>;

    async fn due(&self, s: &Service, account: &Account, attempted: i64) -> Result<bool>;

    /// How long usage stays current for scheduling and display.
    fn fresh_for(&self) -> i64;

    /// Codex never schedules on unknown usage; Claude Code's usage may be unavailable.
    fn requires_usage(&self) -> bool;

    async fn supports(&self, s: &Service, id: &str, model: &str) -> Result<bool>;

    /// The private home a run's CLI reads its credentials from.
    fn home(&self, s: &Service, run_id: &str) -> PathBuf;

    /// Prepares `home` for a lease. Refresh credentials never enter it.
    async fn prepare(&self, s: &Service, id: &str, home: &Path) -> Result<()>;

    /// Removes credentials a run may have left in `home`.
    async fn clear(&self, home: &Path) -> Result<()>;

    /// Removes credentials left in any home of a run interrupted by a restart.
    async fn recover(&self, s: &Service, run_id: &str) -> Result<()>;

    /// Answers a run's broker request with access-only credentials.
    async fn access(&self, s: &Service, lease: &Lease, request: &broker::Request) -> Result<Value>;

    async fn redactions(&self, s: &Service, id: &str) -> Result<Vec<String>>;

    /// Deletes the credentials of a removed account.
    async fn forget(&self, s: &Service, id: &str) -> Result<()>;
}

impl Provider {
    pub fn driver(self) -> &'static dyn Driver {
        match self {
            Provider::Codex => &codex::Codex,
            Provider::Claude => &claude::Claude,
        }
    }
}

#[derive(Default)]
pub struct Accounts {
    leases: Mutex<HashMap<String, Lease>>,
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    initialized: OnceCell<()>,
    attempted: Mutex<HashMap<String, i64>>,
    polling: Mutex<()>,
    selection: Mutex<()>,
    connecting: Mutex<Option<String>>,
    signing_in: Mutex<Option<SignIn>>,
}

/// What an account needs or does, as shown beside it on Connections.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum Status {
    SignIn,
    Reconnect,
    Paused,
    Unavailable,
    Waiting,
    Full,
    Next,
    Low,
    Ready,
}

/// What Connections shows beside the record itself.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Summary {
    resets_at: Option<i64>,
    remaining_percent: Option<f64>,
    stale: bool,
    status: Status,
    active_run_ids: Vec<String>,
    max_concurrent_runs: usize,
    exhausted: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Overview {
    accounts: Vec<Value>,
    sign_in: Value,
    required: Vec<Provider>,
}

/// Codex accounts and runs saved before accounts were shared across coding agents. Runs once.
pub fn migrate(db: &mut Db<'_>) -> Result<()> {
    let key = "migration:agent-accounts";
    if db.kv(key)?.is_some() {
        return Ok(());
    }
    db.0.execute(
        "UPDATE records SET kind=?1, data=json_set(data,'$.provider','codex') WHERE kind='codexAccounts'",
        [KIND],
    )?;
    db.0.execute(
        "UPDATE runs SET data=json_remove(json_set(data,'$.accountId',json_extract(data,'$.codexAccountId'),'$.accountName',json_extract(data,'$.codexAccountName')),'$.codexAccountId','$.codexAccountName','$.codexAuthMode')
         WHERE json_type(data,'$.codexAccountId') IS NOT NULL OR json_type(data,'$.codexAccountName') IS NOT NULL OR json_type(data,'$.codexAuthMode') IS NOT NULL",
        [],
    )?;
    db.set(key, &Value::Bool(true), None)
}

fn stored(db: &Db<'_>) -> Result<Vec<Account>> {
    db.list(KIND)?
        .into_iter()
        .map(Account::from_value)
        .collect()
}

pub(crate) async fn save(s: &Service, account: &Account) -> Result<()> {
    s.store.put(KIND, account.to_value()).await?;
    Ok(())
}

/// Saves a signed-in account unless another account of its coding agent has the same identity.
/// Both are checked and saved together, even when two sign-ins finish together.
pub(crate) async fn claim(s: &Service, account: Account) -> Result<()> {
    s.store
        .transaction(move |db| {
            let duplicate = stored(db)?.iter().any(|a| {
                a.id != account.id
                    && a.provider == account.provider
                    && a.identity == account.identity
            });
            if duplicate {
                return Err(Error::conflict(
                    "This account is already connected. Reconnect the existing account instead.",
                ));
            }
            db.put(KIND, &account.to_value())?;
            Ok(())
        })
        .await
}

/// A reconnection signed in with another identity than the account's own.
pub(crate) fn different_account() -> Error {
    Error::conflict("Sign-in belongs to a different account. Add it as a new account instead.")
}

/// Clears exhaustion once usage read after it shows the account recovered; otherwise that
/// usage becomes what the next reading is compared with. Without any usage to compare, a
/// coding agent that can run on unknown usage tries the account again after `fresh_for`.
pub(crate) fn replenish(account: &mut Account, driver: &dyn Driver) {
    let Some(exhausted) = &account.exhausted else {
        return;
    };
    let (at, model) = (exhausted.at, exhausted.model.as_str());
    let read_since = account
        .usage
        .as_ref()
        .and_then(|u| u.checked_at)
        .is_some_and(|checked| checked > at);
    let recovered = if read_since {
        let before = exhausted.usage.clone().unwrap_or_default();
        let current = account.usage();
        Usage::recovered(&before, &current, model)
            || (before.remaining(model).is_none() && current.available(model))
    } else {
        !driver.requires_usage() && now() - at >= driver.fresh_for()
    };
    if recovered {
        account.exhausted = None;
    } else if read_since {
        let current = account.usage.clone();
        if let Some(exhausted) = &mut account.exhausted {
            exhausted.usage = current;
        }
    }
}

fn fresh(account: &Account, driver: &dyn Driver) -> bool {
    account
        .usage
        .as_ref()
        .and_then(|u| u.checked_at)
        .is_some_and(|at| now() - at <= driver.fresh_for())
}

/// Remaining capacity for `model`, or `None` when usage is unknown or out of date.
fn capacity(account: &Account, driver: &dyn Driver, model: &str) -> Option<f64> {
    if !fresh(account, driver) {
        return None;
    }
    account.usage.as_ref()?.remaining(model)
}

/// Whether usage and state let the account take a new `model` run, ignoring its slots.
fn schedulable(account: &Account, driver: &dyn Driver, model: &str) -> bool {
    let current = fresh(account, driver);
    let blocked = current && account.usage().blocked(model);
    let has_capacity =
        capacity(account, driver, model).map_or(!driver.requires_usage(), |n| n > 0.);
    account.is_active()
        && account.exhausted.is_none()
        && (current || !driver.requires_usage())
        && !blocked
        && has_capacity
}

fn running(leases: &HashMap<String, Lease>, id: &str) -> usize {
    leases.values().filter(|l| l.account_id == id).count()
}

fn valid_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(Error::bad("Choose a name of 1–100 characters."));
    }
    Ok(name)
}

pub(crate) async fn remove_file(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

pub(crate) async fn remove_directory(path: &Path) -> Result<()> {
    match tokio::fs::remove_dir_all(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

impl Accounts {
    pub async fn initialize(&self, s: &Service) -> Result<()> {
        self.initialized
            .get_or_try_init(|| async {
                // Sign-ins never survive a restart.
                remove_directory(&s.config.data_dir.join("account-login")).await?;
                for provider in Provider::ALL {
                    provider.driver().initialize(s).await?;
                }
                self.recover_runs(s).await
            })
            .await?;
        Ok(())
    }

    /// Leases live in memory, so after a restart only recovering runs may still own
    /// credentials; removes anything the others left behind.
    async fn recover_runs(&self, s: &Service) -> Result<()> {
        let Ok(mut entries) = tokio::fs::read_dir(s.config.data_dir.join("runs")).await else {
            return Ok(());
        };
        while let Some(entry) = entries.next_entry().await? {
            let id = entry.file_name().to_string_lossy().into_owned();
            if uuid::Uuid::parse_str(&id).is_err() {
                continue;
            }
            if let Ok(run) = s.store.run(&id).await
                && run["recoveryPending"] != true
            {
                self.recover_run(s, &run).await?;
            }
        }
        Ok(())
    }

    pub async fn lock(&self, id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        self.locks
            .lock()
            .await
            .entry(id.into())
            .or_default()
            .clone()
            .lock_owned()
            .await
    }

    pub async fn get(&self, s: &Service, id: &str) -> Result<Value> {
        required(s.store.get(KIND, id).await?, "Account not found.")
    }

    pub async fn account(&self, s: &Service, id: &str) -> Result<Account> {
        Account::from_value(self.get(s, id).await?)
    }

    async fn find(&self, s: &Service, id: &str) -> Result<Option<Account>> {
        s.store
            .get(KIND, id)
            .await?
            .map(Account::from_value)
            .transpose()
    }

    pub async fn records(&self, s: &Service, provider: Provider) -> Result<Vec<Value>> {
        Ok(s.store
            .list(KIND)
            .await?
            .into_iter()
            .filter(|a| a["provider"] == provider.as_str())
            .collect())
    }

    /// The accounts of `provider`, most recently saved first.
    pub async fn of(&self, s: &Service, provider: Provider) -> Result<Vec<Account>> {
        let mut accounts = self.all(s).await?;
        accounts.retain(|a| a.provider == provider);
        Ok(accounts)
    }

    async fn all(&self, s: &Service) -> Result<Vec<Account>> {
        s.store.read(|db| stored(db)).await
    }

    pub async fn lease(&self, run_id: &str) -> Option<Lease> {
        self.leases.lock().await.get(run_id).cloned()
    }

    /// The account's leases, ordered by run.
    pub async fn active(&self, id: &str) -> Vec<Lease> {
        let mut leases = self
            .leases
            .lock()
            .await
            .values()
            .filter(|l| l.account_id == id)
            .cloned()
            .collect::<Vec<_>>();
        leases.sort_by(|a, b| a.run_id.cmp(&b.run_id));
        leases
    }

    async fn recovering(&self, s: &Service, id: &str) -> Result<bool> {
        let id = id.to_owned();
        s.store
            .read(move |db| {
                Ok(db
                    .active()?
                    .iter()
                    .any(|r| r["recoveryPending"] == true && r["accountId"] == id))
            })
            .await
    }

    /// Signing in, or recovering a run: nothing else may use the account's credentials.
    pub async fn busy(&self, s: &Service, id: &str) -> Result<bool> {
        Ok(self.connecting.lock().await.as_deref() == Some(id) || self.recovering(s, id).await?)
    }

    /// Accounts able to take a new `model` run now, best first: the most remaining capacity,
    /// then the least recently used.
    async fn eligible(
        &self,
        s: &Service,
        provider: Provider,
        model: &str,
        leases: &HashMap<String, Lease>,
    ) -> Result<Vec<Account>> {
        let driver = provider.driver();
        let connecting = self.connecting.lock().await.clone();
        let mut candidates = Vec::new();
        for account in self.of(s, provider).await? {
            let id = account.id.as_str();
            if schedulable(&account, driver, model)
                && running(leases, id) < account.parallel_runs()
                && connecting.as_deref() != Some(id)
                && !self.recovering(s, id).await?
                && driver.supports(s, id, model).await?
            {
                candidates.push(account);
            }
        }
        let rank = |a: &Account| capacity(a, driver, model).unwrap_or(-1.);
        candidates.sort_by(|a, b| {
            rank(b)
                .total_cmp(&rank(a))
                .then(
                    a.last_used_at
                        .unwrap_or(0)
                        .cmp(&b.last_used_at.unwrap_or(0)),
                )
                .then(a.id.cmp(&b.id))
        });
        Ok(candidates)
    }

    pub async fn acquire(
        &self,
        s: &Service,
        run_id: &str,
        provider: Provider,
        model: &str,
    ) -> Result<Option<Lease>> {
        self.initialize(s).await?;
        let driver = provider.driver();
        if !driver.managed(s).await? {
            return Ok(None);
        }
        self.poll(s, true).await?;
        let _selection = self.selection.lock().await;
        let leases = self.leases.lock().await;
        if leases.contains_key(run_id) {
            return Err(Error::conflict("This run already holds an account."));
        }
        let candidates = self.eligible(s, provider, model, &leases).await?;
        drop(leases);
        let Some(mut account) = candidates.into_iter().next() else {
            return Err(self.waiting(s, provider).await?);
        };
        let lease = Lease {
            account_id: account.id.clone(),
            provider,
            run_id: run_id.into(),
            model: model.into(),
            home: driver.home(s, run_id),
        };
        let _guard = self.lock(&lease.account_id).await;
        driver.prepare(s, &lease.account_id, &lease.home).await?;
        self.leases
            .lock()
            .await
            .insert(lease.run_id.clone(), lease.clone());
        account.last_used_at = Some(now());
        save(s, &account).await?;
        Ok(Some(lease))
    }

    /// Why no account can take a run now.
    async fn waiting(&self, s: &Service, provider: Provider) -> Result<Error> {
        let accounts = self.of(s, provider).await?;
        let leases = self.leases.lock().await;
        let label = provider.label();
        let ready = accounts.iter().filter(|a| a.is_ready()).collect::<Vec<_>>();
        let full = |a: &&Account| a.enabled && running(&leases, &a.id) >= a.parallel_runs();
        let message = if ready.is_empty() && accounts.iter().any(|a| a.state == AccountState::Error)
        {
            format!("Reconnect your {label} account in Connections to continue.")
        } else if ready.is_empty() {
            format!("Connect a {label} account in Connections before running this agent.")
        } else if ready.iter().all(|a| !a.enabled) {
            format!("Every {label} account is paused. Resume one in Connections to continue.")
        } else if ready.iter().any(full) {
            format!(
                "Waiting for a free {label} account slot. The run starts when another finishes."
            )
        } else {
            format!("Waiting for a {label} account with available usage.")
        };
        Ok(Error::conflict(message))
    }

    /// Whether runs of `provider` wait for the user to connect, reconnect or resume an account.
    pub async fn needs_attention(&self, s: &Service, provider: Provider) -> Result<bool> {
        Ok(provider.driver().managed(s).await?
            && !self.of(s, provider).await?.iter().any(Account::is_active))
    }

    pub async fn release(&self, lease: &Lease) -> Result<()> {
        let _guard = self.lock(&lease.account_id).await;
        let cleared = lease.provider.driver().clear(&lease.home).await;
        self.leases.lock().await.remove(&lease.run_id);
        cleared
    }

    /// Moves a lease to the home its run actually uses, such as inside an isolated workspace.
    pub async fn relocate(&self, s: &Service, lease: &mut Lease, home: &Path) -> Result<()> {
        let _guard = self.lock(&lease.account_id).await;
        if lease.home == home {
            return Ok(());
        }
        let driver = lease.provider.driver();
        driver.prepare(s, &lease.account_id, home).await?;
        driver.clear(&lease.home).await?;
        lease.home = home.to_owned();
        self.leases
            .lock()
            .await
            .insert(lease.run_id.clone(), lease.clone());
        Ok(())
    }

    pub async fn recover_run(&self, s: &Service, run: &Value) -> Result<()> {
        let account = text(run, "accountId");
        let run_id = text(run, "id");
        let _guard = if account.is_empty() {
            None
        } else {
            Some(self.lock(account).await)
        };
        // A conversation can switch coding agents, so clear every provider's run home.
        for provider in Provider::ALL {
            provider.driver().recover(s, run_id).await?;
        }
        self.leases
            .lock()
            .await
            .retain(|_, lease| lease.run_id != run_id);
        Ok(())
    }

    /// Answers a run's broker request.
    pub async fn access(&self, s: &Service, lease: &Lease, request: &Value) -> Result<Value> {
        let request = broker::Request::deserialize(request).unwrap_or_default();
        let ended = self
            .leases
            .lock()
            .await
            .get(&lease.run_id)
            .is_none_or(|l| l.account_id != lease.account_id || l.home != lease.home);
        if ended {
            return Err(Error::conflict("This account lease has ended."));
        }
        lease.provider.driver().access(s, lease, &request).await
    }

    pub async fn redactions(&self, s: &Service, lease: &Lease) -> Result<Vec<String>> {
        lease
            .provider
            .driver()
            .redactions(s, &lease.account_id)
            .await
    }

    pub async fn exhausted(&self, s: &Service, id: &str, model: &str) -> Result<()> {
        let _guard = self.lock(id).await;
        let (id, model) = (id.to_owned(), model.to_owned());
        s.store
            .transaction(move |db| {
                let mut account =
                    Account::from_value(required(db.get(KIND, &id)?, "Account not found.")?)?;
                account.exhausted = Some(Exhaustion {
                    at: now(),
                    model,
                    usage: account.usage.clone(),
                });
                db.put(KIND, &account.to_value())?;
                db.audit("account.exhausted", &json!({ "id": id }))
            })
            .await
    }

    pub async fn poll(&self, s: &Service, only_due: bool) -> Result<()> {
        let Ok(_guard) = self.polling.try_lock() else {
            return Ok(());
        };
        self.initialize(s).await?;
        let mut due = Vec::new();
        for account in self.all(s).await? {
            if account.state == AccountState::Pending {
                continue;
            }
            let attempted = self
                .attempted
                .lock()
                .await
                .get(&account.id)
                .copied()
                .unwrap_or(0);
            if !only_due
                || account
                    .provider
                    .driver()
                    .due(s, &account, attempted)
                    .await?
            {
                due.push(account.id);
            }
        }
        for batch in due.chunks(2) {
            futures_util::future::try_join_all(batch.iter().map(|id| self.refresh(s, id))).await?;
        }
        Ok(())
    }

    pub async fn refresh(&self, s: &Service, id: &str) -> Result<()> {
        let _guard = self.lock(id).await;
        let Some(account) = self.find(s, id).await? else {
            return Ok(());
        };
        if self.busy(s, id).await? {
            return Ok(());
        }
        self.attempted.lock().await.insert(id.into(), now());
        let models = self
            .active(id)
            .await
            .into_iter()
            .map(|l| l.model)
            .collect::<Vec<_>>();
        let provider = account.provider;
        let driver = provider.driver();
        if let Err(error) = driver.refresh(s, id, &models).await {
            let mut account = self.account(s, id).await?;
            account.state = AccountState::Error;
            account.error = if error.status < 500 {
                error.message
            } else {
                format!(
                    "Unable to read this {} account. Reconnect it and try again.",
                    provider.label()
                )
            };
            return save(s, &account).await;
        }
        let mut account = self.account(s, id).await?;
        if account.exhausted.is_some() {
            replenish(&mut account, driver);
            save(s, &account).await?;
        }
        Ok(())
    }

    /// Accounts as shown on Connections. `status` summarizes what the account needs or does.
    pub async fn list(&self, s: &Service) -> Result<Vec<Value>> {
        let mut accounts = self.all(s).await?;
        accounts.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.name.cmp(&b.name)));
        let leases = self.leases.lock().await.clone();
        let mut next = Vec::new();
        for provider in Provider::ALL {
            if let Some(first) = self.eligible(s, provider, "", &leases).await?.first() {
                next.push(first.id.clone());
            }
        }
        Ok(accounts
            .into_iter()
            .map(|account| {
                let is_next = next.contains(&account.id);
                view(&account, &leases, is_next)
            })
            .collect())
    }

    /// Every account, the sign-in in progress, and the coding agents whose runs wait for the
    /// user to connect, reconnect or resume an account.
    pub async fn overview(&self, s: &Service) -> Result<Value> {
        let mut required = Vec::new();
        for provider in Provider::ALL {
            if self.needs_attention(s, provider).await? {
                required.push(provider);
            }
        }
        let overview = Overview {
            accounts: self.list(s).await?,
            sign_in: self.sign_in().await,
            required,
        };
        Ok(serde_json::to_value(overview)?)
    }

    pub async fn sign_in(&self) -> Value {
        self.signing_in
            .lock()
            .await
            .as_ref()
            .map_or(Value::Null, SignIn::view)
    }

    /// Adds an account that is not signed in yet.
    pub async fn create(&self, s: &Service, provider: Provider, name: &str) -> Result<Value> {
        let name = valid_name(name)?.to_owned();
        s.store
            .transaction(move |db| {
                let count = stored(db)?
                    .iter()
                    .filter(|a| a.provider == provider)
                    .count();
                if count >= MAX_ACCOUNTS {
                    return Err(Error::bad(format!(
                        "A maximum of {MAX_ACCOUNTS} {} accounts can be connected.",
                        provider.label()
                    )));
                }
                let account = Account::new(provider, &name, AccountState::Pending).to_value();
                db.put(KIND, &account)?;
                provider.driver().added(db)?;
                Ok(account)
            })
            .await
    }

    /// Adds an account and starts its sign-in.
    pub async fn add(&self, s: &Arc<Service>, provider: Provider, name: &str) -> Result<Value> {
        self.initialize(s).await?;
        let _selection = self.selection.lock().await;
        let mut current = self.signing_in.lock().await;
        if current.as_ref().is_some_and(SignIn::busy) {
            return Err(Error::conflict("Another sign-in is in progress."));
        }
        let account = Account::from_value(self.create(s, provider, name).await?)?;
        self.begin(s, &mut current, &account).await
    }

    pub async fn reconnect(&self, s: &Arc<Service>, id: &str) -> Result<Value> {
        self.initialize(s).await?;
        let _selection = self.selection.lock().await;
        let mut current = self.signing_in.lock().await;
        if current.as_ref().is_some_and(SignIn::busy) {
            return Err(Error::conflict("Another sign-in is in progress."));
        }
        if self.recovering(s, id).await? || !self.active(id).await.is_empty() {
            return Err(Error::conflict(
                "Wait for this account’s runs to finish before reconnecting it.",
            ));
        }
        let account = self.account(s, id).await?;
        self.begin(s, &mut current, &account).await
    }

    async fn begin(
        &self,
        s: &Arc<Service>,
        current: &mut Option<SignIn>,
        account: &Account,
    ) -> Result<Value> {
        let id = account.id.clone();
        let _guard = self.lock(&id).await;
        let home = s.config.data_dir.join("account-login").join(&id);
        private_dir(&home).await?;
        // Fence the account before the sign-in can finish and release it.
        *self.connecting.lock().await = Some(id.clone());
        let created = account.state == AccountState::Pending;
        let sign_in = sign_in::start(s, id, account.provider, home, created);
        let view = sign_in.view();
        *current = Some(sign_in);
        Ok(view)
    }

    /// Cancels the current sign-in. An account that never finished signing in is removed.
    pub async fn cancel(&self, s: &Service) -> Result<()> {
        let Some(sign_in) = self.signing_in.lock().await.take() else {
            return Ok(());
        };
        sign_in.login.stop.cancel();
        let mut complete = sign_in.complete.clone();
        while !*complete.borrow() {
            if complete.changed().await.is_err() {
                break;
            }
        }
        if sign_in.created
            && self
                .find(s, &sign_in.account_id)
                .await?
                .is_some_and(|a| a.state == AccountState::Pending)
        {
            self.remove(s, &sign_in.account_id).await?;
        }
        Ok(())
    }

    pub async fn submit_code(&self, code: &str) -> Result<()> {
        let current = self.signing_in.lock().await;
        current
            .as_ref()
            .filter(|c| c.waits_for_code())
            .ok_or_else(|| Error::conflict("This sign-in is no longer waiting for a code."))?
            .submit(code)
    }

    pub async fn update(&self, s: &Service, id: &str, input: &Value) -> Result<Value> {
        let _guard = self.lock(id).await;
        if self.connecting.lock().await.as_deref() == Some(id) {
            return Err(Error::conflict(
                "Finish or cancel sign-in before editing this account.",
            ));
        }
        let mut account = self.account(s, id).await?;
        if let Some(name) = input.get("name") {
            account.name = valid_name(name.as_str().unwrap_or(""))?.into();
        }
        if let Some(enabled) = input.get("enabled") {
            account.enabled = enabled
                .as_bool()
                .ok_or_else(|| Error::bad("Choose whether this account is used."))?;
        }
        if let Some(limit) = input.get("maxConcurrentRuns") {
            let limit = limit
                .as_u64()
                .filter(|n| *n > 0)
                .ok_or_else(|| Error::bad("Parallel runs must be a positive integer."))?;
            account.max_concurrent_runs = Some(limit);
        }
        save(s, &account).await?;
        Ok(self
            .list(s)
            .await?
            .into_iter()
            .find(|a| a["id"] == id)
            .unwrap_or(Value::Null))
    }

    pub async fn remove(&self, s: &Service, id: &str) -> Result<()> {
        let _selection = self.selection.lock().await;
        let _guard = self.lock(id).await;
        let account = self.account(s, id).await?;
        let signing_in = self
            .signing_in
            .lock()
            .await
            .as_ref()
            .is_some_and(|c| c.busy() && c.account_id == id);
        if signing_in || self.recovering(s, id).await? || !self.active(id).await.is_empty() {
            return Err(Error::conflict(
                "Wait for this account’s runs or sign-in to finish before removing it.",
            ));
        }
        let provider = account.provider;
        provider.driver().forget(s, id).await?;
        let id = id.to_owned();
        s.store
            .transaction(move |db| {
                db.remove(KIND, &id)?;
                db.audit(
                    "account.removed",
                    &json!({ "id": id, "provider": provider }),
                )
            })
            .await
    }
}

fn status(account: &Account, driver: &dyn Driver, runs: usize, next: bool) -> Status {
    let current = fresh(account, driver);
    let left = capacity(account, driver, "");
    let waiting =
        account.exhausted.is_some() || (current && account.usage().blocked("")) || left == Some(0.);
    match account.state {
        AccountState::Pending => return Status::SignIn,
        AccountState::Error => return Status::Reconnect,
        AccountState::Ready => {}
    }
    if !account.enabled {
        Status::Paused
    } else if driver.requires_usage() && !current {
        Status::Unavailable
    } else if waiting {
        Status::Waiting
    } else if runs >= account.parallel_runs() {
        Status::Full
    } else if next {
        Status::Next
    } else if left.is_some_and(|n| n < LOW) {
        Status::Low
    } else {
        Status::Ready
    }
}

fn view(account: &Account, leases: &HashMap<String, Lease>, next: bool) -> Value {
    let driver = account.provider.driver();
    let mut runs = leases
        .values()
        .filter(|l| l.account_id == account.id)
        .map(|l| l.run_id.clone())
        .collect::<Vec<_>>();
    runs.sort();
    let usage = account.usage();
    let summary = Summary {
        resets_at: usage.limiting("").and_then(|w| w.resets_at),
        remaining_percent: usage.remaining(""),
        stale: !fresh(account, driver),
        status: status(account, driver, runs.len(), next),
        active_run_ids: runs,
        max_concurrent_runs: account.parallel_runs(),
        exhausted: account.exhausted.is_some(),
    };
    let mut value = account.to_value();
    if let Some(fields) = value.as_object_mut() {
        fields.remove("identity");
    }
    merge(
        &mut value,
        &serde_json::to_value(summary).unwrap_or(Value::Null),
    );
    value
}

pub async fn routes(s: &Arc<Service>, input: &Input) -> Result<Value> {
    let segments = input
        .path
        .trim_start_matches("/api/accounts")
        .trim_start_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    let accounts = &s.accounts;
    match (input.method.as_str(), segments.as_slice()) {
        ("GET", []) => {
            accounts.initialize(s).await?;
            accounts.overview(s).await
        }
        ("POST", []) => {
            let provider = Provider::parse(input.string("provider", 20)?)?;
            accounts.add(s, provider, input.string("name", 200)?).await
        }
        ("POST", ["refresh"]) => {
            accounts.poll(s, false).await?;
            accounts.overview(s).await
        }
        ("POST", ["sign-in", "code"]) => {
            let code = input.string("code", 4096)?.trim();
            if code.is_empty() || code.chars().any(char::is_whitespace) {
                return Err(Error::bad(
                    "Paste the authorization code shown by the sign-in page.",
                ));
            }
            accounts.submit_code(code).await?;
            Ok(json!({ "submitted": true }))
        }
        ("DELETE", ["sign-in"]) => {
            accounts.cancel(s).await?;
            Ok(json!({ "cancelled": true }))
        }
        ("POST", [id, "sign-in"]) => {
            uuid(id)?;
            accounts.reconnect(s, id).await
        }
        ("PATCH", [id]) => {
            uuid(id)?;
            accounts.update(s, id, &input.body).await
        }
        ("DELETE", [id]) => {
            uuid(id)?;
            accounts.remove(s, id).await?;
            Ok(json!({ "deleted": true }))
        }
        _ => Err(Error::not_found("Unknown account operation.")),
    }
}
