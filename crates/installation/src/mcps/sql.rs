//! Current connection state. No records/kv adapter or historical reader.
use super::record::{GrantScope, McpServer, PendingAuthorization, RunGrant};
use crate::{
    config::now,
    error::Result,
    store::{Db, Store},
    vault::Vault,
};
use rusqlite::{OptionalExtension, params};
use std::collections::BTreeMap;

pub(crate) const SCHEMA: &str = "
CREATE TABLE mcp_servers(
 id TEXT PRIMARY KEY NOT NULL,
 name TEXT NOT NULL CHECK(length(trim(name)) BETWEEN 1 AND 100),
 transport TEXT NOT NULL CHECK(transport IN ('http','stdio')),
 url TEXT NOT NULL CHECK(length(url)<=2000),
 command TEXT NOT NULL CHECK(length(command)<=1000),
 auth TEXT NOT NULL CHECK(auth IN ('none','bearer','oauth')),
 client_id TEXT NOT NULL CHECK(length(client_id)<=500),
 allow_private_network INTEGER NOT NULL CHECK(allow_private_network IN (0,1)),
 enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
 all_tools INTEGER NOT NULL CHECK(all_tools IN (0,1)),
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
 revision INTEGER NOT NULL CHECK(revision BETWEEN 0 AND 9007199254740991),
 state TEXT NOT NULL CHECK(state IN ('untested','connected','needs-auth','error')),
 checked_at INTEGER CHECK(checked_at BETWEEN 0 AND 9007199254740991),
 error TEXT NOT NULL,
 updated_at INTEGER NOT NULL,
 CHECK(transport!='stdio' OR (length(trim(command))>0 AND auth='none'))
);
CREATE INDEX mcp_servers_updated ON mcp_servers(updated_at DESC,id);
CREATE TABLE mcp_args(server_id TEXT NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 99),value TEXT NOT NULL CHECK(length(value)<=4000),PRIMARY KEY(server_id,position)) WITHOUT ROWID;
CREATE TABLE mcp_scopes(server_id TEXT NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,position INTEGER NOT NULL CHECK(position>=0),value TEXT NOT NULL CHECK(length(value)>0),PRIMARY KEY(server_id,position)) WITHOUT ROWID;
CREATE TABLE mcp_enabled_tools(server_id TEXT NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 499),name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 200),PRIMARY KEY(server_id,position)) WITHOUT ROWID;
-- Third-party tool descriptors are opaque protocol documents, not Cairn decisions.
CREATE TABLE mcp_discovery_cache(server_id TEXT PRIMARY KEY NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,tools TEXT NOT NULL CHECK(json_valid(tools) AND json_type(tools)='array'));
CREATE TABLE mcp_credentials(server_id TEXT PRIMARY KEY NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,ciphertext BLOB NOT NULL CHECK(length(ciphertext)>=28)) WITHOUT ROWID;
CREATE TABLE mcp_run_grants(digest TEXT PRIMARY KEY NOT NULL,run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,message_id TEXT,workspace INTEGER NOT NULL CHECK(workspace IN (0,1)),installation INTEGER NOT NULL CHECK(installation IN (0,1)),expires_at INTEGER) WITHOUT ROWID;
CREATE INDEX mcp_run_grants_run ON mcp_run_grants(run_id);
CREATE TABLE mcp_grant_servers(digest TEXT NOT NULL REFERENCES mcp_run_grants(digest) ON DELETE CASCADE,server_id TEXT NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,revision INTEGER NOT NULL CHECK(revision>=0),all_tools INTEGER NOT NULL CHECK(all_tools IN (0,1)),PRIMARY KEY(digest,server_id)) WITHOUT ROWID;
CREATE TABLE mcp_grant_tools(digest TEXT NOT NULL,server_id TEXT NOT NULL,position INTEGER NOT NULL CHECK(position>=0),name TEXT NOT NULL,PRIMARY KEY(digest,server_id,position),FOREIGN KEY(digest,server_id) REFERENCES mcp_grant_servers(digest,server_id) ON DELETE CASCADE) WITHOUT ROWID;
CREATE TABLE mcp_pending_oauth(digest TEXT PRIMARY KEY NOT NULL,server_id TEXT NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,revision INTEGER NOT NULL CHECK(revision>=0),session TEXT NOT NULL,nonce TEXT NOT NULL,native INTEGER NOT NULL CHECK(native IN (0,1)),expires_at INTEGER NOT NULL,callback BLOB CHECK(callback IS NULL OR length(callback)>=28)) WITHOUT ROWID;
CREATE INDEX mcp_pending_server_session ON mcp_pending_oauth(server_id,session);
";

const COLUMNS: &str = "id,name,transport,url,command,auth,client_id,allow_private_network,enabled,all_tools,created_at,revision,state,checked_at,error";

fn enum_column<T: serde::de::DeserializeOwned>(
    row: &rusqlite::Row<'_>,
    column: usize,
) -> rusqlite::Result<T> {
    let value: String = row.get(column)?;
    serde_json::from_value(value.into()).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn json_column<T: serde::de::DeserializeOwned>(
    row: &rusqlite::Row<'_>,
    column: usize,
) -> rusqlite::Result<T> {
    let value: String = row.get(column)?;
    serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<McpServer> {
    Ok(McpServer {
        id: row.get(0)?,
        name: row.get(1)?,
        transport: enum_column(row, 2)?,
        url: row.get(3)?,
        command: row.get(4)?,
        auth: enum_column(row, 5)?,
        client_id: row.get(6)?,
        allow_private_network: row.get(7)?,
        enabled: row.get(8)?,
        enabled_tools: if row.get::<_, bool>(9)? {
            None
        } else {
            Some(vec![])
        },
        created_at: row.get(10)?,
        revision: row.get::<_, i64>(11)? as u64,
        state: enum_column(row, 12)?,
        checked_at: row.get(13)?,
        error: row.get(14)?,
        tools: json_column(row, 15)?,
        ..Default::default()
    })
}

impl Db<'_> {
    pub fn mcp_server(&self, id: &str) -> Result<Option<McpServer>> {
        // One statement owns the point-read snapshot, including ordered selections.
        // These arrays are query projections of rows, never a second stored model.
        let query=format!("SELECT {COLUMNS},COALESCE(cache.tools,'[]'),
            (SELECT json_group_array(value) FROM (SELECT value FROM mcp_args WHERE server_id=mcp_servers.id ORDER BY position)),
            COALESCE((SELECT group_concat(value,' ') FROM (SELECT value FROM mcp_scopes WHERE server_id=mcp_servers.id ORDER BY position)),''),
            (SELECT json_group_array(name) FROM (SELECT name FROM mcp_enabled_tools WHERE server_id=mcp_servers.id ORDER BY position))
            FROM mcp_servers LEFT JOIN mcp_discovery_cache AS cache ON cache.server_id=mcp_servers.id WHERE id=?");
        Ok(self
            .0
            .prepare_cached(&query)?
            .query_row([id], |record| {
                let mut server = row(record)?;
                server.args = json_column(record, 16)?;
                server.scopes = record.get(17)?;
                if server.enabled_tools.is_some() {
                    server.enabled_tools = Some(json_column(record, 18)?);
                }
                Ok(server)
            })
            .optional()?)
    }

    pub fn mcp_servers(&self) -> Result<Vec<McpServer>> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            let result = Db(&tx).mcp_servers()?;
            tx.commit()?;
            return Ok(result);
        }
        let mut servers = self
            .0
            .prepare_cached(&format!(
                "SELECT {COLUMNS},COALESCE(cache.tools,'[]') FROM mcp_servers LEFT JOIN mcp_discovery_cache AS cache ON cache.server_id=mcp_servers.id ORDER BY updated_at DESC,id"
            ))?
            .query_map([], row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        self.hydrate_mcps(&mut servers)?;
        Ok(servers)
    }

    fn hydrate_mcps(&self, servers: &mut [McpServer]) -> Result<()> {
        let indexes: BTreeMap<_, _> = servers
            .iter()
            .enumerate()
            .map(|(index, s)| (s.id.clone(), index))
            .collect();
        for (table, column) in [
            ("mcp_args", "value"),
            ("mcp_scopes", "value"),
            ("mcp_enabled_tools", "name"),
        ] {
            let mut statement = self.0.prepare_cached(&format!(
                "SELECT server_id,{column} FROM {table} ORDER BY server_id,position"
            ))?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let key: String = row.get(0)?;
                let value: String = row.get(1)?;
                let Some(index) = indexes.get(&key) else {
                    continue;
                };
                let server = &mut servers[*index];
                match table {
                    "mcp_args" => server.args.push(value),
                    "mcp_scopes" => {
                        if !server.scopes.is_empty() {
                            server.scopes.push(' ');
                        }
                        server.scopes.push_str(&value);
                    }
                    _ => server.enabled_tools.get_or_insert_default().push(value),
                }
            }
        }
        Ok(())
    }

    pub fn save_mcp_server(&self, s: &McpServer) -> Result<()> {
        let transport = match s.transport {
            super::record::Transport::Http => "http",
            super::record::Transport::Stdio => "stdio",
        };
        let auth = serde_json::to_value(s.auth)?;
        let state = serde_json::to_value(s.state)?;
        let revision = i64::try_from(s.revision).map_err(crate::error::Error::internal)?;
        self.0.prepare_cached("INSERT INTO mcp_servers VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16) ON CONFLICT(id) DO UPDATE SET name=excluded.name,transport=excluded.transport,url=excluded.url,command=excluded.command,auth=excluded.auth,client_id=excluded.client_id,allow_private_network=excluded.allow_private_network,enabled=excluded.enabled,all_tools=excluded.all_tools,created_at=excluded.created_at,revision=excluded.revision,state=excluded.state,checked_at=excluded.checked_at,error=excluded.error,updated_at=excluded.updated_at")?.execute(params![
            s.id,
            s.name,
            transport,
            s.url,
            s.command,
            auth.as_str(),
            s.client_id,
            s.allow_private_network,
            s.enabled,
            s.enabled_tools.is_none(),
            s.created_at,
            revision,
            state.as_str(),
            s.checked_at,
            s.error,
            now()
        ])?;
        for (table, column, values) in [
            ("mcp_args", "value", s.args.clone()),
            (
                "mcp_scopes",
                "value",
                s.scopes.split_whitespace().map(str::to_owned).collect(),
            ),
            (
                "mcp_enabled_tools",
                "name",
                s.enabled_tools.clone().unwrap_or_default(),
            ),
        ] {
            self.0
                .prepare_cached(&format!("DELETE FROM {table} WHERE server_id=?"))?
                .execute([&s.id])?;
            let mut insert = self.0.prepare_cached(&format!(
                "INSERT INTO {table}(server_id,position,{column}) VALUES(?,?,?)"
            ))?;
            for (position, value) in values.iter().enumerate() {
                insert.execute(params![s.id, position as i64, value])?;
            }
        }
        self.0.prepare_cached("INSERT INTO mcp_discovery_cache VALUES(?,?) ON CONFLICT(server_id) DO UPDATE SET tools=excluded.tools")?.execute(params![s.id,serde_json::to_string(&s.tools)?])?;
        Ok(())
    }

    pub fn delete_mcp_server(&self, id: &str) -> Result<()> {
        self.0.execute("DELETE FROM mcp_servers WHERE id=?", [id])?;
        Ok(())
    }

    pub fn mcp_credentials(&self, id: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .0
            .prepare_cached("SELECT ciphertext FROM mcp_credentials WHERE server_id=?")?
            .query_row([id], |r| r.get(0))
            .optional()?)
    }

    pub fn save_mcp_credentials(&self, id: &str, bytes: &[u8]) -> Result<()> {
        self.0.prepare_cached("INSERT INTO mcp_credentials VALUES(?,?) ON CONFLICT(server_id) DO UPDATE SET ciphertext=excluded.ciphertext")?.execute(params![id,bytes])?;
        Ok(())
    }

    pub fn delete_mcp_credentials(&self, id: &str) -> Result<()> {
        self.0
            .execute("DELETE FROM mcp_credentials WHERE server_id=?", [id])?;
        Ok(())
    }

    pub fn mcp_run_grant(&self, digest: &str) -> Result<Option<RunGrant>> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            let result = Db(&tx).mcp_run_grant(digest)?;
            tx.commit()?;
            return Ok(result);
        }
        let grant = self.0.prepare_cached("SELECT run_id,message_id,workspace,installation FROM mcp_run_grants WHERE digest=? AND (expires_at IS NULL OR expires_at>?)")?
            .query_row(params![digest, now()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get(2)?,
                    row.get(3)?,
                ))
            })
            .optional()?;
        let Some((run_id, message_id, workspace, installation)) = grant else {
            return Ok(None);
        };
        let mut servers = BTreeMap::new();
        let mut statement = self.0.prepare_cached(
            "SELECT server_id,revision,all_tools FROM mcp_grant_servers WHERE digest=?",
        )?;
        let scopes = statement.query_map([digest], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)? as u64,
                r.get::<_, bool>(2)?,
            ))
        })?;
        for scope in scopes {
            let (id, revision, all) = scope?;
            servers.insert(
                id,
                GrantScope {
                    revision,
                    tools: if all { None } else { Some(vec![]) },
                },
            );
        }
        let mut statement = self.0.prepare_cached(
            "SELECT server_id,name FROM mcp_grant_tools WHERE digest=? ORDER BY server_id,position",
        )?;
        let tools = statement.query_map([digest], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for tool in tools {
            let (id, name) = tool?;
            if let Some(scope) = servers.get_mut(&id) {
                scope.tools.get_or_insert_default().push(name);
            }
        }
        Ok(Some(RunGrant {
            run_id,
            message_id,
            servers,
            workspace,
            installation,
        }))
    }

    pub fn save_mcp_run_grant(
        &self,
        digest: &str,
        grant: &RunGrant,
        expires: Option<i64>,
    ) -> Result<()> {
        self.0
            .execute("DELETE FROM mcp_run_grants WHERE digest=?", [digest])?;
        self.0
            .prepare_cached("INSERT INTO mcp_run_grants VALUES(?,?,?,?,?,?)")?
            .execute(params![
                digest,
                grant.run_id,
                grant.message_id,
                grant.workspace,
                grant.installation,
                expires
            ])?;
        for (id, scope) in &grant.servers {
            self.0
                .prepare_cached("INSERT INTO mcp_grant_servers VALUES(?,?,?,?)")?
                .execute(params![
                    digest,
                    id,
                    i64::try_from(scope.revision).map_err(crate::error::Error::internal)?,
                    scope.tools.is_none()
                ])?;
            for (position, name) in scope.tools.iter().flatten().enumerate() {
                self.0
                    .prepare_cached("INSERT INTO mcp_grant_tools VALUES(?,?,?,?)")?
                    .execute(params![digest, id, position as i64, name])?;
            }
        }
        Ok(())
    }

    pub fn revoke_mcp_run(&self, id: &str) -> Result<()> {
        self.0
            .prepare_cached("DELETE FROM mcp_run_grants WHERE run_id=?")?
            .execute([id])?;
        Ok(())
    }

    pub fn cancel_mcp_oauth(&self, id: &str) -> Result<()> {
        self.0
            .prepare_cached("DELETE FROM mcp_pending_oauth WHERE server_id=?")?
            .execute([id])?;
        Ok(())
    }

    pub fn pending_mcp_oauth(
        &self,
        key: &str,
        vault: &Vault,
    ) -> Result<Option<PendingAuthorization>> {
        let result = self.0.prepare_cached("SELECT server_id,revision,session,nonce,native,expires_at,callback FROM mcp_pending_oauth WHERE digest=? AND expires_at>?")?
            .query_row(params![key, now()], |row| {
                let pending = PendingAuthorization {
                    connection_id: row.get(0)?,
                    revision: row.get::<_, i64>(1)? as u64,
                    session: row.get(2)?,
                    nonce: row.get(3)?,
                    native: row.get(4)?,
                    expires_at: row.get(5)?,
                    callback: None,
                };
                Ok((pending, row.get::<_, Option<Vec<u8>>>(6)?))
            })
            .optional()?;
        result
            .map(|(mut pending, bytes)| {
                pending.callback = bytes
                    .map(|bytes| -> Result<_> {
                        Ok(serde_json::from_slice(&vault.decrypt_bytes(key, &bytes)?)?)
                    })
                    .transpose()?;
                Ok(pending)
            })
            .transpose()
    }

    pub fn save_pending_mcp_oauth(
        &self,
        key: &str,
        pending: &PendingAuthorization,
        vault: &Vault,
    ) -> Result<()> {
        let callback = pending
            .callback
            .as_ref()
            .map(|c| -> Result<_> { vault.encrypt_bytes(key, &serde_json::to_vec(c)?) })
            .transpose()?;
        let revision = i64::try_from(pending.revision).map_err(crate::error::Error::internal)?;
        self.0.prepare_cached("INSERT INTO mcp_pending_oauth VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(digest) DO UPDATE SET callback=excluded.callback")?.execute(params![
            key,
            pending.connection_id,
            revision,
            pending.session,
            pending.nonce,
            pending.native,
            pending.expires_at,
            callback
        ])?;
        Ok(())
    }

    pub fn pending_mcp_session(
        &self,
        id: &str,
        session: &str,
        vault: &Vault,
    ) -> Result<Option<PendingAuthorization>> {
        let key: Option<String> = self.0.prepare_cached("SELECT digest FROM mcp_pending_oauth WHERE server_id=? AND session=? AND native=1 AND expires_at>?")?
            .query_row(params![id, session, now()], |row| row.get(0))
            .optional()?;
        key.map(|key| self.pending_mcp_oauth(&key, vault))
            .transpose()
            .map(Option::flatten)
    }

    pub fn delete_pending_mcp_oauth(&self, key: &str) -> Result<()> {
        self.0
            .execute("DELETE FROM mcp_pending_oauth WHERE digest=?", [key])?;
        Ok(())
    }
}

impl Store {
    pub async fn mcp_server(&self, id: &str) -> Result<Option<McpServer>> {
        let id = id.to_owned();
        self.read(move |db| db.mcp_server(&id)).await
    }

    pub async fn mcp_servers(&self) -> Result<Vec<McpServer>> {
        self.read(|db| db.mcp_servers()).await
    }

    pub async fn mcp_run_grant(&self, digest: &str) -> Result<Option<RunGrant>> {
        let digest = digest.to_owned();
        self.read(move |db| db.mcp_run_grant(&digest)).await
    }
}
