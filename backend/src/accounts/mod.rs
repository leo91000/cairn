//! One pool of coding-agent accounts.
//!
//! The pool owns what every coding agent shares: account records, pause and parallel-run
//! limits, selection by remaining capacity, run leases, usage polling and sign-in
//! orchestration. Each coding agent's [`Driver`] owns what differs: its official CLI's sign-in
//! protocol, credential storage, usage reading and the credentials handed to a run.
pub mod broker;
pub mod claude;
pub mod codex;
pub mod usage;

use crate::{
    config::{id, now},
    error::{Error, Result, required},
    http::Input,
    provider::Provider,
    service::Service,
    skills::private_dir,
    store::merge,
    validation::{text, uuid},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::{Mutex, OnceCell, mpsc, watch};
use tokio_util::sync::CancellationToken;

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
    fn added(&self, _db: &mut crate::store::Db<'_>) -> Result<()> {
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

    async fn due(&self, s: &Service, account: &Value, attempted: i64) -> Result<bool>;

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
    async fn access(&self, s: &Service, lease: &Lease, request: &Value) -> Result<Value>;

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

/// A sign-in in progress, as seen by a driver.
pub struct Login {
    pub(crate) view: watch::Sender<Value>,
    pub stop: CancellationToken,
    codes: Mutex<mpsc::Receiver<String>>,
}

impl Login {
    pub fn update(&self, change: impl FnOnce(&mut Value)) {
        self.view.send_modify(change);
    }

    /// The next authorization code the user pastes, for CLIs that ask for one.
    pub async fn code(&self) -> Option<String> {
        self.codes.lock().await.recv().await
    }
}

struct SignIn {
    account_id: String,
    provider: Provider,
    login: Arc<Login>,
    input: mpsc::Sender<String>,
    complete: watch::Receiver<bool>,
    created: bool,
}

impl SignIn {
    fn busy(&self) -> bool {
        !*self.complete.borrow()
    }

    fn view(&self) -> Value {
        let mut view = self.login.view.borrow().clone();
        view["accountId"] = self.account_id.clone().into();
        view["provider"] = self.provider.as_str().into();
        view
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

/// Codex accounts and runs saved before accounts were shared across coding agents. Runs once.
pub fn migrate(db: &mut crate::store::Db<'_>) -> Result<()> {
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
    db.set(key, &json!(true), None)
}

/// A new account, before its first sign-in or import.
pub(crate) fn record(provider: Provider, name: &str, state: &str) -> Value {
    json!({
        "id": id(),
        "provider": provider,
        "name": name,
        "enabled": true,
        "email": null,
        "plan": null,
        "identity": null,
        "createdAt": now(),
        "checkedAt": null,
        "state": state,
        "error": "",
        "usage": null,
        "lastUsedAt": null,
        "exhausted": null,
        "maxConcurrentRuns": PARALLEL_RUNS
    })
}

/// Saves a signed-in account unless another account of its coding agent has the same identity.
/// Both are checked and saved together, even when two sign-ins finish together.
pub(crate) async fn claim(s: &Service, account: Value) -> Result<()> {
    s.store
        .transaction(move |db| {
            if db.list(KIND)?.iter().any(|a| {
                a["id"] != account["id"]
                    && a["provider"] == account["provider"]
                    && a["identity"] == account["identity"]
            }) {
                return Err(Error::new(
                    409,
                    "This account is already connected. Reconnect the existing account instead.",
                ));
            }
            db.put(KIND, &account).map(|_| ())
        })
        .await
}

/// A reconnection signed in with another identity than the account's own.
pub(crate) fn different_account() -> Error {
    Error::new(
        409,
        "Sign-in belongs to a different account. Add it as a new account instead.",
    )
}

/// Clears exhaustion once usage read after it shows the account recovered; otherwise that
/// usage becomes what the next reading is compared with. Without any usage to compare, a
/// coding agent that can run on unknown usage tries the account again after `fresh_for`.
pub(crate) fn replenish(account: &mut Value, driver: &dyn Driver) {
    let exhausted = &account["exhausted"];
    if exhausted.is_null() {
        return;
    }
    let model = text(exhausted, "model");
    let at = exhausted["at"].as_i64().unwrap_or(0);
    let current = &account["usage"];
    let recovered = if current["checkedAt"]
        .as_i64()
        .is_some_and(|checked| checked > at)
    {
        usage::recovered(&exhausted["usage"], current, model)
            || (usage::remaining(&exhausted["usage"], model).is_none()
                && !usage::blocked(current, model)
                && usage::remaining(current, model).is_some_and(|n| n > 0.))
    } else {
        !driver.requires_usage() && now() - at >= driver.fresh_for()
    };
    if recovered {
        account["exhausted"] = Value::Null;
    } else if current["checkedAt"]
        .as_i64()
        .is_some_and(|checked| checked > at)
    {
        account["exhausted"]["usage"] = current.clone();
    }
}

fn fresh(account: &Value, driver: &dyn Driver) -> bool {
    account["usage"]["checkedAt"]
        .as_i64()
        .is_some_and(|at| now() - at <= driver.fresh_for())
}

/// Remaining capacity for `model`, or `None` when usage is unknown or out of date.
fn capacity(account: &Value, driver: &dyn Driver, model: &str) -> Option<f64> {
    fresh(account, driver)
        .then(|| usage::remaining(&account["usage"], model))
        .flatten()
}

fn parallel_runs(account: &Value) -> usize {
    account["maxConcurrentRuns"]
        .as_u64()
        .unwrap_or(PARALLEL_RUNS) as usize
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
                // Leases live in memory, so after a restart only recovering runs may still
                // own credentials; remove anything the others left behind.
                if let Ok(mut entries) = tokio::fs::read_dir(s.config.data_dir.join("runs")).await {
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
                }
                Ok(())
            })
            .await
            .map(|_| ())
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

    pub async fn records(&self, s: &Service, provider: Provider) -> Result<Vec<Value>> {
        Ok(s.store
            .list(KIND)
            .await?
            .into_iter()
            .filter(|a| a["provider"] == provider.as_str())
            .collect())
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
    ) -> Result<Vec<Value>> {
        let driver = provider.driver();
        let connecting = self.connecting.lock().await.clone();
        let mut candidates = Vec::new();
        for account in self.records(s, provider).await? {
            let id = text(&account, "id");
            let current = fresh(&account, driver);
            let left = capacity(&account, driver, model);
            if account["enabled"] == true
                && account["state"] == "ready"
                && account["exhausted"].is_null()
                && (current || !driver.requires_usage())
                && !(current && usage::blocked(&account["usage"], model))
                && left.map_or(!driver.requires_usage(), |n| n > 0.)
                && leases.values().filter(|l| l.account_id == id).count() < parallel_runs(&account)
                && connecting.as_deref() != Some(id)
                && !self.recovering(s, id).await?
                && driver.supports(s, id, model).await?
            {
                candidates.push(account);
            }
        }
        let driver_capacity = |a: &Value| capacity(a, driver, model).unwrap_or(-1.);
        candidates.sort_by(|a, b| {
            driver_capacity(b)
                .total_cmp(&driver_capacity(a))
                .then(
                    a["lastUsedAt"]
                        .as_i64()
                        .unwrap_or(0)
                        .cmp(&b["lastUsedAt"].as_i64().unwrap_or(0)),
                )
                .then(text(a, "id").cmp(text(b, "id")))
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
            return Err(Error::new(409, "This run already holds an account."));
        }
        let candidates = self.eligible(s, provider, model, &leases).await?;
        drop(leases);
        let Some(mut account) = candidates.into_iter().next() else {
            return Err(self.waiting(s, provider).await?);
        };
        let lease = Lease {
            account_id: text(&account, "id").into(),
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
        account["lastUsedAt"] = now().into();
        s.store.put(KIND, account).await?;
        Ok(Some(lease))
    }

    /// Why no account can take a run now.
    async fn waiting(&self, s: &Service, provider: Provider) -> Result<Error> {
        let accounts = self.records(s, provider).await?;
        let leases = self.leases.lock().await;
        let label = provider.label();
        let ready = accounts
            .iter()
            .filter(|a| a["state"] == "ready")
            .collect::<Vec<_>>();
        let message = if ready.is_empty() && accounts.iter().any(|a| a["state"] == "error") {
            format!("Reconnect your {label} account in Connections to continue.")
        } else if ready.is_empty() {
            format!("Connect a {label} account in Connections before running this agent.")
        } else if ready.iter().all(|a| a["enabled"] != true) {
            format!("Every {label} account is paused. Resume one in Connections to continue.")
        } else if ready.iter().any(|a| {
            a["enabled"] == true
                && leases
                    .values()
                    .filter(|l| a["id"] == l.account_id.as_str())
                    .count()
                    >= parallel_runs(a)
        }) {
            format!(
                "Waiting for a free {label} account slot. The run starts when another finishes."
            )
        } else {
            format!("Waiting for a {label} account with available usage.")
        };
        Ok(Error::new(409, message))
    }

    /// Whether runs of `provider` wait for the user to connect, reconnect or resume an account.
    pub async fn needs_attention(&self, s: &Service, provider: Provider) -> Result<bool> {
        Ok(provider.driver().managed(s).await?
            && !self
                .records(s, provider)
                .await?
                .iter()
                .any(|a| a["state"] == "ready" && a["enabled"] == true))
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
        let _guard = if account.is_empty() {
            None
        } else {
            Some(self.lock(account).await)
        };
        // A conversation can switch coding agents, so clear every provider's run home.
        for provider in Provider::ALL {
            provider.driver().recover(s, text(run, "id")).await?;
        }
        self.leases
            .lock()
            .await
            .retain(|_, lease| lease.run_id != text(run, "id"));
        Ok(())
    }

    pub async fn access(&self, s: &Service, lease: &Lease, request: &Value) -> Result<Value> {
        if self
            .leases
            .lock()
            .await
            .get(&lease.run_id)
            .is_none_or(|l| l.account_id != lease.account_id || l.home != lease.home)
        {
            return Err(Error::new(409, "This account lease has ended."));
        }
        lease.provider.driver().access(s, lease, request).await
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
                let mut a = required(db.get(KIND, &id)?, "Account not found.")?;
                a["exhausted"] = json!({"at": now(),"model": model,"usage": a["usage"]});
                db.put(KIND, &a)?;
                db.audit("account.exhausted", &json!({"id": id}))
            })
            .await
    }

    pub async fn poll(&self, s: &Service, only_due: bool) -> Result<()> {
        let Ok(_guard) = self.polling.try_lock() else {
            return Ok(());
        };
        self.initialize(s).await?;
        let mut due = Vec::new();
        for account in s.store.list(KIND).await? {
            if account["state"] == "pending" {
                continue;
            }
            let id = text(&account, "id");
            let attempted = self.attempted.lock().await.get(id).copied().unwrap_or(0);
            if !only_due
                || Provider::of_account(&account)
                    .driver()
                    .due(s, &account, attempted)
                    .await?
            {
                due.push(id.to_owned());
            }
        }
        for batch in due.chunks(2) {
            futures_util::future::try_join_all(batch.iter().map(|id| self.refresh(s, id))).await?;
        }
        Ok(())
    }

    pub async fn refresh(&self, s: &Service, id: &str) -> Result<()> {
        let _guard = self.lock(id).await;
        let Some(account) = s.store.get(KIND, id).await? else {
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
        let provider = Provider::of_account(&account);
        let driver = provider.driver();
        if let Err(error) = driver.refresh(s, id, &models).await {
            let mut account = self.get(s, id).await?;
            let message = if error.status < 500 {
                error.message
            } else {
                format!(
                    "Unable to read this {} account. Reconnect it and try again.",
                    provider.label()
                )
            };
            merge(&mut account, &json!({"state": "error","error": message}));
            s.store.put(KIND, account).await?;
            return Ok(());
        }
        let mut account = self.get(s, id).await?;
        if !account["exhausted"].is_null() {
            replenish(&mut account, driver);
            s.store.put(KIND, account).await?;
        }
        Ok(())
    }

    /// Accounts as shown on Connections. `status` summarizes what the account needs or does.
    pub async fn list(&self, s: &Service) -> Result<Vec<Value>> {
        let mut accounts = s.store.list(KIND).await?;
        accounts.sort_by(|a, b| {
            a["createdAt"]
                .as_i64()
                .cmp(&b["createdAt"].as_i64())
                .then(text(a, "name").cmp(text(b, "name")))
        });
        let leases = self.leases.lock().await.clone();
        let mut next = Vec::new();
        for provider in Provider::ALL {
            if let Some(first) = self.eligible(s, provider, "", &leases).await?.first() {
                next.push(text(first, "id").to_owned());
            }
        }
        Ok(accounts
            .into_iter()
            .map(|account| {
                let is_next = next.iter().any(|id| account["id"] == *id);
                view(account, &leases, is_next)
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
        Ok(json!({
            "accounts": self.list(s).await?,
            "signIn": self.sign_in().await,
            "required": required
        }))
    }

    pub async fn sign_in(&self) -> Value {
        self.signing_in
            .lock()
            .await
            .as_ref()
            .map(SignIn::view)
            .unwrap_or(Value::Null)
    }

    /// Adds an account that is not signed in yet.
    pub async fn create(&self, s: &Service, provider: Provider, name: &str) -> Result<Value> {
        let name = valid_name(name)?.to_owned();
        s.store
            .transaction(move |db| {
                if db
                    .list(KIND)?
                    .iter()
                    .filter(|a| a["provider"] == provider.as_str())
                    .count()
                    >= MAX_ACCOUNTS
                {
                    return Err(Error::bad(format!(
                        "A maximum of {MAX_ACCOUNTS} {} accounts can be connected.",
                        provider.label()
                    )));
                }
                let account = record(provider, &name, "pending");
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
            return Err(Error::new(409, "Another sign-in is in progress."));
        }
        let account = self.create(s, provider, name).await?;
        self.begin(s, &mut current, account).await
    }

    pub async fn reconnect(&self, s: &Arc<Service>, id: &str) -> Result<Value> {
        self.initialize(s).await?;
        let _selection = self.selection.lock().await;
        let mut current = self.signing_in.lock().await;
        if current.as_ref().is_some_and(SignIn::busy) {
            return Err(Error::new(409, "Another sign-in is in progress."));
        }
        if self.recovering(s, id).await? || !self.active(id).await.is_empty() {
            return Err(Error::new(
                409,
                "Wait for this account’s runs to finish before reconnecting it.",
            ));
        }
        let account = self.get(s, id).await?;
        self.begin(s, &mut current, account).await
    }

    async fn begin(
        &self,
        s: &Arc<Service>,
        current: &mut Option<SignIn>,
        account: Value,
    ) -> Result<Value> {
        let id = text(&account, "id").to_owned();
        let provider = Provider::of_account(&account);
        let _guard = self.lock(&id).await;
        let home = s.config.data_dir.join("account-login").join(&id);
        private_dir(&home).await?;
        let (input, codes) = mpsc::channel(1);
        let (view, _) = watch::channel(json!({
            "state": "pending",
            "phase": "starting",
            "url": null,
            "code": null,
            "acceptsCode": false,
            "expiresAt": now()+SIGN_IN_MS,
            "error": null
        }));
        let login = Arc::new(Login {
            view,
            stop: CancellationToken::new(),
            codes: Mutex::new(codes),
        });
        let (done, complete) = watch::channel(false);
        let sign_in = SignIn {
            account_id: id.clone(),
            provider,
            login: login.clone(),
            input,
            complete,
            // An account that never finished signing in is removed if the user gives up.
            created: account["state"] == "pending",
        };
        let view = sign_in.view();
        *current = Some(sign_in);
        *self.connecting.lock().await = Some(id.clone());
        let s = s.clone();
        tokio::spawn(async move {
            let driver = provider.driver();
            let result = async {
                driver.authorize(&s, &home, &login).await?;
                login.update(|v| {
                    v["phase"] = "verifying".into();
                    v["url"] = Value::Null;
                    v["code"] = Value::Null;
                    v["acceptsCode"] = false.into();
                });
                // Keep selection fenced until the identity is verified and credentials saved.
                let _selection = s.accounts.selection.lock().await;
                let _guard = s.accounts.lock(&id).await;
                // Drivers report sign-in errors without provider details; verification can fail
                // for internal reasons worth hiding.
                driver.adopt(&s, &id, &home).await.map_err(|error| {
                    if error.status < 500 {
                        error
                    } else {
                        Error::new(error.status, "Unable to complete sign-in. Try again.")
                    }
                })
            }
            .await;
            let stopped = login.stop.is_cancelled() || s.shutdown.is_cancelled();
            if result.is_ok() {
                let _ = s
                    .store
                    .audit("account.connected", json!({"id":id,"provider":provider}))
                    .await;
            }
            let _ = remove_directory(&home).await;
            *s.accounts.connecting.lock().await = None;
            // A terminal view lets clients immediately select, reconnect or remove
            // the account. Finish cleanup and release its fence before publishing it.
            let _ = done.send(true);
            login.update(|v| {
                v["url"] = Value::Null;
                v["code"] = Value::Null;
                v["acceptsCode"] = false.into();
                v["expiresAt"] = Value::Null;
                v["state"] = match &result {
                    Ok(()) => "complete",
                    Err(_) if stopped => "cancelled",
                    Err(_) => "failed",
                }
                .into();
                if let Err(error) = &result
                    && !stopped
                {
                    v["error"] = error.message.clone().into();
                }
            });
        });
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
            && s.store
                .get(KIND, &sign_in.account_id)
                .await?
                .is_some_and(|a| a["state"] == "pending")
        {
            self.remove(s, &sign_in.account_id).await?;
        }
        Ok(())
    }

    pub async fn submit_code(&self, code: &str) -> Result<()> {
        let current = self.signing_in.lock().await;
        let sign_in = current
            .as_ref()
            .filter(|c| c.busy() && c.login.view.borrow()["acceptsCode"] == true)
            .ok_or_else(|| Error::new(409, "This sign-in is no longer waiting for a code."))?;
        sign_in
            .input
            .try_send(code.into())
            .map_err(|_| Error::new(409, "A code is already being checked."))
    }

    pub async fn update(&self, s: &Service, id: &str, input: &Value) -> Result<Value> {
        let _guard = self.lock(id).await;
        if self.connecting.lock().await.as_deref() == Some(id) {
            return Err(Error::new(
                409,
                "Finish or cancel sign-in before editing this account.",
            ));
        }
        let mut account = self.get(s, id).await?;
        if let Some(name) = input.get("name") {
            account["name"] = valid_name(name.as_str().unwrap_or(""))?.into();
        }
        if let Some(enabled) = input.get("enabled") {
            account["enabled"] = enabled
                .as_bool()
                .ok_or_else(|| Error::bad("Choose whether this account is used."))?
                .into();
        }
        if let Some(limit) = input.get("maxConcurrentRuns") {
            account["maxConcurrentRuns"] = limit
                .as_u64()
                .filter(|n| *n > 0)
                .ok_or_else(|| Error::bad("Parallel runs must be a positive integer."))?
                .into();
        }
        s.store.put(KIND, account).await?;
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
        let account = self.get(s, id).await?;
        let signing_in = self
            .signing_in
            .lock()
            .await
            .as_ref()
            .is_some_and(|c| c.busy() && c.account_id == id);
        if signing_in || self.recovering(s, id).await? || !self.active(id).await.is_empty() {
            return Err(Error::new(
                409,
                "Wait for this account’s runs or sign-in to finish before removing it.",
            ));
        }
        let provider = Provider::of_account(&account);
        provider.driver().forget(s, id).await?;
        let id = id.to_owned();
        s.store
            .transaction(move |db| {
                db.remove(KIND, &id)?;
                db.audit("account.removed", &json!({"id": id,"provider": provider}))
            })
            .await
    }
}

fn view(mut account: Value, leases: &HashMap<String, Lease>, next: bool) -> Value {
    let driver = Provider::of_account(&account).driver();
    let mut runs = leases
        .values()
        .filter(|l| account["id"] == l.account_id.as_str())
        .map(|l| l.run_id.clone())
        .collect::<Vec<_>>();
    runs.sort();
    let current = fresh(&account, driver);
    let usage = account["usage"].clone();
    let usage = &usage;
    let left = capacity(&account, driver, "");
    let status = if account["state"] == "pending" {
        "signIn"
    } else if account["state"] == "error" {
        "reconnect"
    } else if account["enabled"] != true {
        "paused"
    } else if driver.requires_usage() && !current {
        "unavailable"
    } else if !account["exhausted"].is_null()
        || (current && usage::blocked(usage, ""))
        || left == Some(0.)
    {
        "waiting"
    } else if runs.len() >= parallel_runs(&account) {
        "full"
    } else if next {
        "next"
    } else if left.is_some_and(|n| n < LOW) {
        "low"
    } else {
        "ready"
    };
    account["resetsAt"] = usage::limiting(usage, "")
        .map(|w| w["resetsAt"].clone())
        .unwrap_or(Value::Null);
    account["remainingPercent"] = usage::remaining(usage, "")
        .map(Value::from)
        .unwrap_or(Value::Null);
    account["stale"] = (!current).into();
    account["status"] = status.into();
    account["activeRunIds"] = json!(runs);
    account["maxConcurrentRuns"] = parallel_runs(&account).into();
    account["exhausted"] = (!account["exhausted"].is_null()).into();
    account.as_object_mut().unwrap().remove("identity");
    account
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
            Ok(json!({"submitted": true}))
        }
        ("DELETE", ["sign-in"]) => {
            accounts.cancel(s).await?;
            Ok(json!({"cancelled": true}))
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
            Ok(json!({"deleted": true}))
        }
        _ => Err(Error::new(404, "Unknown account operation.")),
    }
}
