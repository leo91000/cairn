//! Installation settings, overview, audit and storage on HTTP and both tools. Storage
//! views never contain credentials; configuring them requires the human owner.
use super::{ResumeContext, decode};
use crate::{
    error::{Error, Result},
    service::Service,
    store::Db,
};
use cairn_contracts::*;
use serde_json::{Value, json};

pub(crate) struct SettingsOperation {
    pub name: &'static str,
    scope: &'static str,
    description: &'static str,
}

pub(crate) const SETTINGS_OPERATIONS: &[SettingsOperation] = &[
    SettingsOperation {
        name: "get_installation_settings",
        scope: "read",
        description: "Read the installation's public and MCP URLs, workspace roots, worker home, host concurrency, version, commit and protocol.",
    },
    SettingsOperation {
        name: "get_installation_overview",
        scope: "read",
        description: "Count retained runs by status, agents, projects and missions, with host concurrency.",
    },
    SettingsOperation {
        name: "list_audit",
        scope: "read",
        description: "List the 100 most recent installation changes, newest first. Each detail is the JSON recorded with its action.",
    },
    SettingsOperation {
        name: "get_storage_settings",
        scope: "read",
        description: "Read the default S3 storage for new disks: bucket, endpoint, region, whether it is integrated or managed by server environment variables. Credentials are never returned.",
    },
    SettingsOperation {
        name: "save_storage_settings",
        scope: "manage",
        description: "Configure the default S3 storage for new disks after a privacy, write, read and deletion check. Existing disks keep their storage. Storage credentials are configured only by the human installation owner in the Cairn apps; agents and MCP connections receive an explicit refusal.",
    },
    SettingsOperation {
        name: "check_storage",
        scope: "manage",
        description: "Write, read and delete a small probe object in the configured storage. Returns only whether it succeeded; credentials are never returned.",
    },
];

pub(crate) fn settings_operation(name: &str) -> Option<&'static SettingsOperation> {
    SETTINGS_OPERATIONS.iter().find(|op| op.name == name)
}

impl SettingsOperation {
    pub(crate) fn descriptor(&self) -> Value {
        let (input, output) = match self.name {
            "get_installation_settings" => (
                schema_for::<GetInstallationSettingsInput>(Contract::Deserialize),
                schema_for::<InstallationSettings>(Contract::Serialize),
            ),
            "get_installation_overview" => (
                schema_for::<GetInstallationOverviewInput>(Contract::Deserialize),
                schema_for::<InstallationOverview>(Contract::Serialize),
            ),
            "list_audit" => (
                schema_for::<ListAuditInput>(Contract::Deserialize),
                schema_for::<ListAuditOutput>(Contract::Serialize),
            ),
            "get_storage_settings" => (
                schema_for::<GetStorageSettingsInput>(Contract::Deserialize),
                schema_for::<StorageSettings>(Contract::Serialize),
            ),
            "save_storage_settings" => (
                schema_for::<SaveStorageSettingsInput>(Contract::Deserialize),
                schema_for::<StorageSettings>(Contract::Serialize),
            ),
            _ => (
                schema_for::<CheckStorageInput>(Contract::Deserialize),
                schema_for::<StorageCheck>(Contract::Serialize),
            ),
        };
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": input,
            "outputSchema": output,
            "_meta": { "securitySchemes": [{ "type": "oauth2", "scopes": [self.scope] }] },
            "annotations": {
                "readOnlyHint": self.scope == "read",
                "destructiveHint": self.name == "save_storage_settings",
                "openWorldHint": self.scope != "read",
            },
        })
    }

    pub(crate) async fn invoke(
        &self,
        s: &Service,
        input: Value,
        context: ResumeContext,
    ) -> Result<Value> {
        // HTTP enforces the owner role for settings and audit before dispatch.
        let (human, bearer) = match context {
            ResumeContext::Human(_) => (true, None),
            ResumeContext::Public(scopes, _) => {
                if !scopes.iter().any(|scope| scope == self.scope) {
                    return Err(Error::forbidden(format!(
                        "The {} scope is required.",
                        self.scope
                    )));
                }
                (false, None)
            }
            ResumeContext::Installation(bearer) => {
                crate::installation_mcp::authorize(s, &bearer).await?;
                (false, Some(bearer))
            }
        };
        let authorize = crate::installation_mcp::management_authorization(bearer.as_deref());
        match self.name {
            "get_installation_settings" => {
                let _: GetInstallationSettingsInput = decode(input)?;
                Ok(serde_json::to_value(installation_settings(s))?)
            }
            "get_storage_settings" => {
                let _: GetStorageSettingsInput = decode(input)?;
                Ok(serde_json::to_value(crate::object_storage::settings(s)?)?)
            }
            "save_storage_settings" => {
                if !human {
                    return Err(Error::forbidden(
                        "Only the installation owner can configure storage credentials, in the Cairn apps.",
                    ));
                }
                Ok(serde_json::to_value(
                    crate::object_storage::save_settings(s, decode(input)?).await?,
                )?)
            }
            "check_storage" => {
                let _: CheckStorageInput = decode(input)?;
                // Network I/O never runs inside a database transaction.
                crate::object_storage::Storage::configured(s)?
                    .probe()
                    .await?;
                Ok(serde_json::to_value(StorageCheck { ok: true })?)
            }
            name => {
                let audit = name == "list_audit";
                if audit {
                    let _: ListAuditInput = decode(input)?;
                } else {
                    let _: GetInstallationOverviewInput = decode(input)?;
                }
                let concurrency = s.config.concurrency as u32;
                s.store
                    .read(move |db| {
                        let tx = rusqlite::Transaction::new_unchecked(
                            db.0,
                            rusqlite::TransactionBehavior::Deferred,
                        )?;
                        let db = Db(&tx);
                        authorize(&db)?;
                        let result = if audit {
                            serde_json::to_value(ListAuditOutput {
                                entries: db.audit_entries(100)?,
                            })?
                        } else {
                            serde_json::to_value(db.overview(concurrency)?)?
                        };
                        tx.commit()?;
                        Ok(result)
                    })
                    .await
            }
        }
    }
}

pub(crate) fn installation_settings(s: &Service) -> InstallationSettings {
    InstallationSettings {
        public_url: s.config.public_url.clone(),
        mcp_url: format!("{}/mcp", s.config.public_url),
        workspace_roots: s
            .config
            .workspace_roots
            .iter()
            .map(|root| root.display().to_string())
            .collect(),
        home: s.config.home.display().to_string(),
        concurrency: s.config.concurrency as u32,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        commit: std::env::var("APP_COMMIT").unwrap_or_else(|_| "development".into()),
        protocol: "2026-07-28".to_owned(),
    }
}

impl Db<'_> {
    pub fn audit_entries(&self, limit: u32) -> Result<Vec<AuditEntry>> {
        let mut statement = self.0.prepare_cached(
            "SELECT id,created_at,action,detail FROM audit ORDER BY id DESC LIMIT ?",
        )?;
        let entries = statement
            .query_map([limit], |row| {
                Ok(AuditEntry {
                    id: row.get(0)?,
                    created_at: row.get(1)?,
                    action: row.get(2)?,
                    detail: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(entries)
    }

    pub fn overview(&self, concurrency: u32) -> Result<InstallationOverview> {
        let count = |table: &str| -> Result<u32> {
            Ok(self
                .0
                .prepare_cached(&format!("SELECT count(*) FROM {table}"))?
                .query_row([], |row| row.get(0))?)
        };
        let runs_by_status = self
            .0
            .prepare_cached("SELECT status,count(*) FROM runs GROUP BY status")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(InstallationOverview {
            runs_by_status,
            agents: count("agents")?,
            projects: count("projects")?,
            missions: count("missions")?,
            concurrency,
        })
    }
}
