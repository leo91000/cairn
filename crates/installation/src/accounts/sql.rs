//! Account persistence through the existing SQLite writer. No document store fallback.
use super::{Account, AccountState, Exhaustion, Lease, usage::{Usage, Window, Resets}};
use crate::{config::now, error::Result, provider::Provider, store::{Db, Store}};
use rusqlite::{OptionalExtension, params};

pub(crate) const SCHEMA: &str = "
CREATE TABLE coding_agent_accounts(
 id TEXT PRIMARY KEY NOT NULL,
 provider TEXT NOT NULL CHECK(provider IN ('codex','claude','opencode')),
 name TEXT NOT NULL CHECK(length(trim(name)) BETWEEN 1 AND 100),
 enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
 email TEXT, plan TEXT, identity TEXT,
 created_at INTEGER CHECK(created_at BETWEEN 0 AND 9007199254740991),
 checked_at INTEGER CHECK(checked_at BETWEEN 0 AND 9007199254740991),
 state TEXT NOT NULL CHECK(state IN ('pending','ready','error')),
 error TEXT NOT NULL,
 last_used_at INTEGER CHECK(last_used_at BETWEEN 0 AND 9007199254740991),
 max_concurrent_runs INTEGER CHECK(max_concurrent_runs BETWEEN 1 AND 9007199254740991),
 reset_error TEXT,
 priority INTEGER CHECK(priority BETWEEN 0 AND 9007199254740991),
 model_provider TEXT, model_provider_name TEXT,
 updated_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX coding_agent_accounts_identity ON coding_agent_accounts(provider,identity) WHERE identity IS NOT NULL;
CREATE INDEX coding_agent_accounts_updated ON coding_agent_accounts(updated_at DESC,id);
CREATE TABLE account_usage(
 account_id TEXT NOT NULL REFERENCES coding_agent_accounts(id) ON DELETE CASCADE,
 phase TEXT NOT NULL CHECK(phase IN ('current','exhausted')),
 allowed INTEGER NOT NULL CHECK(allowed IN (0,1)),
 checked_at INTEGER CHECK(checked_at BETWEEN 0 AND 9007199254740991),
 attempted_at INTEGER CHECK(attempted_at BETWEEN 0 AND 9007199254740991),
 error TEXT, use_balance INTEGER NOT NULL CHECK(use_balance IN (0,1)),
 resets_available INTEGER CHECK(resets_available BETWEEN 0 AND 9007199254740991),
 credits TEXT CHECK(credits IS NULL OR (json_valid(credits) AND json_type(credits)='array')),
 PRIMARY KEY(account_id,phase)
) WITHOUT ROWID;
CREATE TABLE account_usage_windows(
 account_id TEXT NOT NULL, phase TEXT NOT NULL,
 position INTEGER NOT NULL CHECK(position>=0),
 id TEXT NOT NULL, label TEXT NOT NULL,
 used_percent REAL,
 resets_at INTEGER CHECK(resets_at BETWEEN 0 AND 9007199254740991),
 duration_mins INTEGER,
 models TEXT NOT NULL CHECK(json_valid(models) AND json_type(models)='array'),
 reached INTEGER NOT NULL CHECK(reached IN (0,1)),
 PRIMARY KEY(account_id,phase,position),
 FOREIGN KEY(account_id,phase) REFERENCES account_usage(account_id,phase) ON DELETE CASCADE
) WITHOUT ROWID;
CREATE TABLE account_exhaustion(
 account_id TEXT PRIMARY KEY NOT NULL REFERENCES coding_agent_accounts(id) ON DELETE CASCADE,
 at INTEGER NOT NULL CHECK(at BETWEEN 0 AND 9007199254740991),
 model TEXT NOT NULL
) WITHOUT ROWID;
-- Background title/avatar work also leases accounts and has no runs row.
-- Process-local credentials are invalidated on restart, before admission resumes.
CREATE TABLE account_leases(
 run_id TEXT PRIMARY KEY NOT NULL,
 account_id TEXT NOT NULL REFERENCES coding_agent_accounts(id) ON DELETE RESTRICT,
 provider TEXT NOT NULL CHECK(provider IN ('codex','claude','opencode')),
 model TEXT NOT NULL, home TEXT NOT NULL
);
CREATE INDEX account_leases_account ON account_leases(account_id,run_id);
";

const COLUMNS: &str = "id,provider,name,enabled,email,plan,identity,created_at,checked_at,state,error,last_used_at,max_concurrent_runs,reset_error,priority,model_provider,model_provider_name";

fn account_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Account> {
    let provider: String = row.get(1)?;
    let state: String = row.get(9)?;
    Ok(Account {
        id: row.get(0)?,
        provider: match provider.as_str() { "codex" => Provider::Codex, "claude" => Provider::Claude, _ => Provider::Opencode },
        name: row.get(2)?, enabled: row.get(3)?, email: row.get(4)?, plan: row.get(5)?, identity: row.get(6)?,
        created_at: row.get(7)?, checked_at: row.get(8)?,
        state: match state.as_str() { "pending" => AccountState::Pending, "ready" => AccountState::Ready, _ => AccountState::Error },
        error: row.get(10)?, last_used_at: row.get(11)?, max_concurrent_runs: row.get(12)?, reset_error: row.get(13)?,
        priority: row.get(14)?, model_provider: row.get(15)?, model_provider_name: row.get(16)?, usage: None, exhausted: None,
    })
}

fn lease_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Lease> {
    let provider: String = row.get(2)?;
    Ok(Lease { run_id: row.get(0)?, account_id: row.get(1)?, provider: match provider.as_str() { "codex"=>Provider::Codex,"claude"=>Provider::Claude,_=>Provider::Opencode }, model: row.get(3)?, home: row.get::<_,String>(4)?.into() })
}

impl Db<'_> {
    pub fn coding_account(&self, id: &str) -> Result<Option<Account>> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(self.0, rusqlite::TransactionBehavior::Deferred)?;
            let result = Db(&tx).coding_account(id)?; tx.commit()?; return Ok(result);
        }
        let mut account = self.0.prepare_cached(&format!("SELECT {COLUMNS} FROM coding_agent_accounts WHERE id=?"))?.query_row([id], account_row).optional()?;
        if let Some(account) = &mut account { self.hydrate_accounts(std::slice::from_mut(account), Some(id))?; }
        Ok(account)
    }

    pub fn coding_accounts(&self) -> Result<Vec<Account>> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(self.0, rusqlite::TransactionBehavior::Deferred)?;
            let result = Db(&tx).coding_accounts()?; tx.commit()?; return Ok(result);
        }
        let mut accounts = self.0.prepare_cached(&format!("SELECT {COLUMNS} FROM coding_agent_accounts ORDER BY updated_at DESC,id"))?.query_map([], account_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        self.hydrate_accounts(&mut accounts, None)?;
        Ok(accounts)
    }

    fn hydrate_accounts(&self, accounts: &mut [Account], id: Option<&str>) -> Result<()> {
        let mut indexes = std::collections::HashMap::new();
        for (i,a) in accounts.iter().enumerate() { indexes.insert(a.id.clone(), i); }
        let filter = if id.is_some() { " WHERE account_id=?1" } else { "" };
        let args: Vec<&str> = id.into_iter().collect();
        let mut query = self.0.prepare_cached(&format!("SELECT account_id,at,model FROM account_exhaustion{filter}"))?;
        let mut rows = query.query(rusqlite::params_from_iter(&args))?;
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            if let Some(i) = indexes.get(&key) { accounts[*i].exhausted = Some(Exhaustion {at: row.get(1)?, model: row.get(2)?, usage: None}); }
        }
        let mut query = self.0.prepare_cached(&format!("SELECT account_id,phase,allowed,checked_at,attempted_at,error,use_balance,resets_available,credits FROM account_usage{filter}"))?;
        let mut rows = query.query(rusqlite::params_from_iter(&args))?;
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?; let phase: String = row.get(1)?;
            if let Some(i) = indexes.get(&key) {
                let available: Option<u64> = row.get(7)?;
                let credits: Option<String> = row.get(8)?;
                let usage = Usage { allowed: row.get(2)?, checked_at: row.get(3)?, attempted_at: row.get(4)?, error: row.get(5)?, use_balance: row.get(6)?, resets: available.map(|available| Ok::<_,crate::error::Error>(Resets { available, credits: serde_json::from_str(credits.as_deref().unwrap_or("[]"))? })).transpose()?, windows: Vec::new() };
                if phase=="current" { accounts[*i].usage = Some(usage); }
                else if let Some(e) = &mut accounts[*i].exhausted { e.usage = Some(usage); }
            }
        }
        let mut query = self.0.prepare_cached(&format!("SELECT account_id,phase,id,label,used_percent,resets_at,duration_mins,models,reached FROM account_usage_windows{filter} ORDER BY account_id,phase,position"))?;
        let mut rows = query.query(rusqlite::params_from_iter(&args))?;
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?; let phase: String = row.get(1)?;
            if let Some(i) = indexes.get(&key) {
                let usage = if phase=="current" { accounts[*i].usage.as_mut() } else { accounts[*i].exhausted.as_mut().and_then(|e| e.usage.as_mut()) };
                if let Some(usage) = usage { usage.windows.push(Window { id: row.get(2)?, label: row.get(3)?, used_percent: row.get(4)?, resets_at: row.get(5)?, duration_mins: row.get(6)?, models: serde_json::from_str(&row.get::<_,String>(7)?)?, reached: row.get(8)? }); }
            }
        }
        Ok(())
    }

    pub fn save_coding_account(&self, a: &Account) -> Result<()> {
        let state = match a.state { AccountState::Pending=>"pending", AccountState::Ready=>"ready", AccountState::Error=>"error" };
        self.0.execute("INSERT INTO coding_agent_accounts(id,provider,name,enabled,email,plan,identity,created_at,checked_at,state,error,last_used_at,max_concurrent_runs,reset_error,priority,model_provider,model_provider_name,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18) ON CONFLICT(id) DO UPDATE SET name=excluded.name,enabled=excluded.enabled,email=excluded.email,plan=excluded.plan,identity=excluded.identity,checked_at=excluded.checked_at,state=excluded.state,error=excluded.error,last_used_at=excluded.last_used_at,max_concurrent_runs=excluded.max_concurrent_runs,reset_error=excluded.reset_error,priority=excluded.priority,model_provider=excluded.model_provider,model_provider_name=excluded.model_provider_name,updated_at=excluded.updated_at",params![a.id,a.provider.as_str(),a.name,a.enabled,a.email,a.plan,a.identity,a.created_at,a.checked_at,state,a.error,a.last_used_at,a.max_concurrent_runs,a.reset_error,a.priority,a.model_provider,a.model_provider_name,now()])?;
        self.0.execute("DELETE FROM account_usage WHERE account_id=?",[&a.id])?;
        self.0.execute("DELETE FROM account_exhaustion WHERE account_id=?",[&a.id])?;
        if let Some(e) = &a.exhausted { self.0.execute("INSERT INTO account_exhaustion(account_id,at,model) VALUES (?,?,?)",params![a.id,e.at,e.model])?; }
        for (phase,usage) in [("current",a.usage.as_ref()),("exhausted",a.exhausted.as_ref().and_then(|e|e.usage.as_ref()))] {
            let Some(u) = usage else { continue; };
            let available = u.resets.as_ref().map(|r|r.available);
            let credits = u.resets.as_ref().map(|r|serde_json::to_string(&r.credits)).transpose()?;
            self.0.execute("INSERT INTO account_usage(account_id,phase,allowed,checked_at,attempted_at,error,use_balance,resets_available,credits) VALUES (?,?,?,?,?,?,?,?,?)",params![a.id,phase,u.allowed,u.checked_at,u.attempted_at,u.error,u.use_balance,available,credits])?;
            let mut insert = self.0.prepare_cached("INSERT INTO account_usage_windows(account_id,phase,position,id,label,used_percent,resets_at,duration_mins,models,reached) VALUES (?,?,?,?,?,?,?,?,?,?)")?;
            for (position,w) in u.windows.iter().enumerate() { insert.execute(params![a.id,phase,position as i64,w.id,w.label,w.used_percent,w.resets_at,w.duration_mins,serde_json::to_string(&w.models)?,w.reached])?; }
        }
        Ok(())
    }

    pub fn remove_coding_account(&self, id: &str) -> Result<()> {
        self.0.execute("DELETE FROM coding_agent_accounts WHERE id=?",[id])?; Ok(())
    }

    pub fn account_leases(&self) -> Result<Vec<Lease>> {
        Ok(self.0.prepare_cached("SELECT run_id,account_id,provider,model,home FROM account_leases ORDER BY run_id")?.query_map([],lease_row)?.collect::<rusqlite::Result<_>>()?)
    }

    pub fn save_account_lease(&self, lease: &Lease) -> Result<()> {
        self.0.execute("INSERT INTO account_leases(run_id,account_id,provider,model,home) VALUES (?,?,?,?,?) ON CONFLICT(run_id) DO UPDATE SET home=excluded.home",params![lease.run_id,lease.account_id,lease.provider.as_str(),lease.model,lease.home.to_string_lossy()])?; Ok(())
    }

    pub fn remove_account_lease(&self, run: &str) -> Result<()> { self.0.execute("DELETE FROM account_leases WHERE run_id=?",[run])?; Ok(()) }
}

impl Store {
    pub async fn coding_account(&self, id: &str) -> Result<Option<Account>> { let id=id.to_owned(); self.read(move |db|db.coding_account(&id)).await }
    pub async fn coding_accounts(&self) -> Result<Vec<Account>> { self.read(|db|db.coding_accounts()).await }
    pub async fn save_coding_account(&self, account: &Account) -> Result<()> { let account=account.clone(); self.transaction(move |db|db.save_coding_account(&account)).await }
}
