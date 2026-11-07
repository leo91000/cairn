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
        name: "get_task_storage_limits",
        scope: "read",
        description: "Read the installation's per-task disk growth limit and revision. Workspace and managed temporary files share the limit; existing larger allowances are preserved.",
    },
    SettingsOperation {
        name: "save_task_storage_limits",
        scope: "manage",
        description: "Save the installation's per-task disk growth limit at the expected revision. Applies to future growth without shrinking existing workspaces. Task agents may reduce the limit but cannot increase it.",
    },
    SettingsOperation {
        name: "get_coding_agent_configuration",
        scope: "read",
        description: "Read effective editable native Codex, Claude Code or OpenCode configuration, installation inheritance, form fields, revision and managed restrictions. Credentials are never included.",
    },
    SettingsOperation {
        name: "preview_coding_agent_configuration",
        scope: "read",
        description: "Validate a native document or form changes without saving. Returns the same effective document and form fields, preserving advanced options.",
    },
    SettingsOperation {
        name: "save_coding_agent_configuration",
        scope: "manage",
        description: "Save revision-guarded native engine defaults or agent customizations. Applies at the next message without interrupting an active turn. Cannot expand Cairn resource grants or replace managed credentials, transport or tools.",
    },
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
            "get_task_storage_limits" => (
                schema_for::<GetTaskStorageLimitsInput>(Contract::Deserialize),
                schema_for::<TaskStorageLimits>(Contract::Serialize),
            ),
            "save_task_storage_limits" => (
                schema_for::<TaskStorageLimits>(Contract::Deserialize),
                schema_for::<TaskStorageLimits>(Contract::Serialize),
            ),
            "get_coding_agent_configuration" => (
                schema_for::<CodingAgentConfigurationInput>(Contract::Deserialize),
                schema_for::<CodingAgentConfiguration>(Contract::Serialize),
            ),
            "preview_coding_agent_configuration" | "save_coding_agent_configuration" => (
                schema_for::<EditCodingAgentConfigurationInput>(Contract::Deserialize),
                schema_for::<CodingAgentConfiguration>(Contract::Serialize),
            ),
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
                "destructiveHint": matches!(self.name, "save_storage_settings" | "save_task_storage_limits"),
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
        if matches!(
            self.name,
            "get_task_storage_limits" | "save_task_storage_limits"
        ) {
            let may_increase = !matches!(&context, ResumeContext::Installation(_));
            let authority =
                super::plugin_packages::Authority::for_grants(s, context, self.scope).await?;
            let authorize = crate::authorization::Authorization::live(s, authority);
            let name = self.name;
            return s
                .store
                .transaction(move |db| {
                    authorize.authorize(db)?;
                    let result = if name == "get_task_storage_limits" {
                        let _: GetTaskStorageLimitsInput = decode(input)?;
                        crate::task_storage::view(db)?
                    } else {
                        crate::task_storage::save(db, decode(input)?, may_increase)?
                    };
                    Ok(serde_json::to_value(result)?)
                })
                .await;
        }
        if matches!(
            self.name,
            "get_coding_agent_configuration"
                | "preview_coding_agent_configuration"
                | "save_coding_agent_configuration"
        ) {
            let authority =
                super::plugin_packages::Authority::for_grants(s, context, self.scope).await?;
            let authorize = crate::authorization::Authorization::live(s, authority);
            let name = self.name;
            return s
                .store
                .transaction(move |db| {
                    authorize.authorize(db)?;
                    let result = if name == "get_coding_agent_configuration" {
                        crate::coding_agent_configuration::view(db, decode(input)?)?
                    } else {
                        crate::coding_agent_configuration::edit(
                            db,
                            &decode(input)?,
                            name == "save_coding_agent_configuration",
                        )?
                    };
                    Ok(serde_json::to_value(result)?)
                })
                .await;
        }
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
        let authorize =
            crate::installation_mcp::management_authorization(s, bearer.as_deref()).await?;
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
                // Boxed: the S3 SDK futures would otherwise enlarge every
                // dispatcher that can reach this capability.
                Ok(serde_json::to_value(
                    Box::pin(crate::object_storage::save_settings(s, decode(input)?)).await?,
                )?)
            }
            "check_storage" => {
                let _: CheckStorageInput = decode(input)?;
                // Network I/O never runs inside a database transaction.
                let storage = crate::object_storage::Storage::configured(s)?;
                Box::pin(storage.probe_live(authorize)).await?;
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
                        authorize.authorize(&db)?;
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
