//! Package and program capabilities shared by authenticated transports. Installation
//! management is not the owner's approval of new executable code.
use super::{ResumeContext, decode};
use crate::plugins::{management, operations, program_installation, registry, resources};
use crate::{
    error::{Error, Result},
    service::Service,
    store::Db,
};
use cairn_contracts::*;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) struct PluginPackageOperation {
    pub name: &'static str,
    scope: &'static str,
    description: &'static str,
}

pub(crate) const PLUGIN_PACKAGE_OPERATIONS: &[PluginPackageOperation] = &[
    PluginPackageOperation {
        name: "list_plugins",
        scope: "read",
        description: "List plugin packages, exact versions, approvals, contributions and redacted instances.",
    },
    PluginPackageOperation {
        name: "list_plugin_resources",
        scope: "read",
        description: "List a plugin instance\'s resources by declared type, page and query. Returns an operation id; poll get_plugin_resource_operation. Installation agents also need a grant on the instance.",
    },
    PluginPackageOperation {
        name: "resolve_plugin_resource",
        scope: "read",
        description: "Resolve an instance resource and each ancestor through its plugin. Returns an operation id; poll get_plugin_resource_operation. Installation agents also need a grant on the instance.",
    },
    PluginPackageOperation {
        name: "get_plugin_resource_operation",
        scope: "read",
        description: "Poll a resource read started by list_plugin_resources or resolve_plugin_resource. Current instance grants and initiating authority are rechecked; management preparations are not readable here.",
    },
    PluginPackageOperation {
        name: "install_plugin_package",
        scope: "manage",
        description: "Upload an inert Luau package. Only the owner can approve its exact manifest and code.",
    },
    PluginPackageOperation {
        name: "approve_plugin_package",
        scope: "manage",
        description: "Approve and activate exact reviewed code. Requires the authenticated installation owner; an agent must ask the owner in the apps.",
    },
    PluginPackageOperation {
        name: "activate_plugin_package",
        scope: "manage",
        description: "Activate a previously owner-approved package version. Program preparation may return an operation to poll.",
    },
    PluginPackageOperation {
        name: "delete_plugin_package",
        scope: "manage",
        description: "Remove an installed package after its instances have been removed.",
    },
    PluginPackageOperation {
        name: "list_plugin_programs",
        scope: "read",
        description: "Read installed, previous and candidate program releases of an active package.",
    },
    PluginPackageOperation {
        name: "prepare_plugin_program",
        scope: "manage",
        description: "Check, install the reviewed candidate digest, or restore a program. Explicit program changes require the authenticated owner; installation access confers no system authority.",
    },
    PluginPackageOperation {
        name: "get_plugin_operation",
        scope: "manage",
        description: "Read a plugin preparation result. Resource results also require read access. Agent callers cannot read owner instance preparations.",
    },
    PluginPackageOperation {
        name: "cancel_plugin_operation",
        scope: "manage",
        description: "Cancel a pending plugin preparation. Does not undo an already published result.",
    },
];

pub(crate) fn plugin_package_operation(name: &str) -> Option<&'static PluginPackageOperation> {
    PLUGIN_PACKAGE_OPERATIONS
        .iter()
        .find(|operation| operation.name == name)
}

/// Captured before async preparation and rechecked at publication.
#[derive(Clone, Default)]
pub(crate) struct Authority {
    pub bearer: Option<String>,
    accepted_activation: Option<Arc<crate::authorization::Accepted>>,
    owner: Option<cairn_protocol::TaskAuthorGrant>,
    public_credential: Option<String>,
    public_scope: Option<&'static str>,
    additional_public_scope: Option<&'static str>,
    guard: Option<Arc<LocalGuard>>,
    recheck_guard: Option<Arc<RecheckGuard>>,
}

type LocalGuard = dyn Fn(&Db<'_>) -> Result<()> + Send + Sync;

type RecheckGuard = dyn for<'a> Fn(&'a Service) -> BoxFuture<'a, Result<()>> + Send + Sync;

struct ExecutionPlugin {
    id: String,
    revision: String,
    enabled: bool,
    plugin: String,
    active: Option<String>,
    digest: Option<String>,
}

impl Authority {
    /// Freeze the complete plugin authority used to prepare and launch a turn,
    /// including disabled/inactive grants that must not become live mid-prepare.
    pub(crate) async fn for_run(s: &Service, run: &Value) -> Result<Self> {
        let authority = Self::for_execution(run);
        let agent = crate::validation::text(&run["snapshot"]["agent"], "id").to_owned();
        let captured = authority.clone();
        let (agent, revision, plugins) = s
            .store
            .read(move |db| {
                captured.authorize(db)?;
                let grants = db.agent_plugin_grants(&agent)?;
                let mut plugins = Vec::new();
                for grant in grants.grants {
                    let instance = crate::error::required(
                        db.plugin_instance(&grant.instance_id.to_string())?,
                        "Plugin instance removed",
                    )?;
                    let state = db.plugin_package(&instance.plugin_id)?;
                    let digest = state
                        .active
                        .as_ref()
                        .and_then(|version| state.approvals.get(version))
                        .cloned();
                    plugins.push(ExecutionPlugin {
                        id: instance.id,
                        revision: instance.revision,
                        enabled: instance.enabled,
                        plugin: instance.plugin_id,
                        active: state.active,
                        digest,
                    });
                }
                Ok((agent, grants.revision, plugins))
            })
            .await?;
        let authority = authority.with_guard(move |db| {
            if crate::error::required(db.agent(&agent)?, "Agent not found")?.revision != revision {
                return Err(Error::forbidden(
                    "Agent grants changed during execution preparation.",
                ));
            }
            for expected in &plugins {
                let current = crate::error::required(
                    db.plugin_instance(&expected.id)?,
                    "Plugin instance removed",
                )?;
                let state = db.plugin_package(&expected.plugin)?;
                let digest = state
                    .active
                    .as_ref()
                    .and_then(|version| state.approvals.get(version));
                if current.revision != expected.revision
                    || current.enabled != expected.enabled
                    || state.active != expected.active
                    || digest != expected.digest.as_ref()
                {
                    return Err(Error::forbidden(
                        "Plugin authority changed during execution preparation.",
                    ));
                }
            }
            Ok(())
        });
        crate::delegation::authorize_async_with(s, run, authority).await
    }

    pub(crate) fn for_execution(run: &Value) -> Self {
        let id = crate::validation::text(run, "id").to_owned();
        let execution = crate::waits::execution_id(run).clone();
        Self::default().with_guard(move |db| {
            let run = crate::error::required(db.run(&id)?, "Run not found")?;
            let agent = &run["snapshot"]["agent"];
            let current = crate::error::required(
                db.agent_snapshot(crate::validation::text(agent, "id"))?,
                "Agent not found",
            )?;
            if crate::waits::execution_id(&run) != &execution
                || !run["cancelRequestedAt"].is_null()
                || !crate::run_status::RunStatus::of(&run)
                    .is_some_and(crate::run_status::RunStatus::is_active)
                || crate::waits::Owner::for_run(db, &run)?.run_id() != id
                || !crate::service::covers(&current, agent)
            {
                return Err(Error::forbidden("The calling execution ended or changed."));
            }
            crate::delegation::authorize_run(db, &run)
        })
    }

    pub(crate) fn with_guard(
        mut self,
        guard: impl Fn(&Db<'_>) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        let previous = self.guard.take();
        self.guard = Some(Arc::new(move |db| {
            if let Some(previous) = &previous {
                previous(db)?;
            }
            guard(db)
        }));
        self
    }

    pub(crate) fn with_recheck_guard(
        mut self,
        guard: impl for<'a> Fn(&'a Service) -> BoxFuture<'a, Result<()>> + Send + Sync + 'static,
    ) -> Self {
        let previous = self.recheck_guard.take();
        let guard: Arc<RecheckGuard> = Arc::new(guard);
        self.recheck_guard = Some(Arc::new(move |s| {
            let (previous, guard) = (previous.clone(), guard.clone());
            Box::pin(async move {
                if let Some(previous) = previous {
                    previous(s).await?;
                }
                guard(s).await
            })
        }));
        self
    }

    pub(crate) async fn for_grants(
        s: &Service,
        context: ResumeContext,
        scope: &'static str,
    ) -> Result<Self> {
        let authority = match context {
            ResumeContext::Installation(bearer) => {
                crate::installation_mcp::authorize_with_authority(s, &bearer)
                    .await?
                    .1
            }
            ResumeContext::Human(account) => Self::human(s, &account).await?,
            ResumeContext::Public(scopes, credential) => {
                if !scopes.iter().any(|held| held == scope) {
                    return Err(Error::forbidden(format!("The {scope} scope is required.")));
                }
                let credential = credential.ok_or_else(|| {
                    Error::unauthorized("A current public MCP authorization is required.")
                })?;
                Self {
                    public_credential: Some(credential),
                    public_scope: Some(scope),
                    ..Default::default()
                }
            }
        };
        authority.recheck(s).await?;
        Ok(authority)
    }

    pub(crate) fn public_scoped(public_credential: Option<String>, scope: &'static str) -> Self {
        Self {
            public_credential,
            public_scope: Some(scope),
            ..Default::default()
        }
    }

    pub(crate) async fn human(s: &Service, account: &str) -> Result<Self> {
        let policy = crate::relay::task_author_policy(s)
            .await?
            .ok_or_else(|| Error::forbidden("The installation has no verified owner."))?;
        if policy.owner.account_id != account {
            return Err(Error::forbidden(
                "This action requires the current installation owner.",
            ));
        }
        Ok(Self {
            bearer: None,
            accepted_activation: None,
            owner: Some(policy.owner),
            public_credential: None,
            public_scope: None,
            additional_public_scope: None,
            guard: None,
            recheck_guard: None,
        })
    }

    pub(crate) async fn recheck(&self, s: &Service) -> Result<()> {
        if let Some(job) = &self.accepted_activation {
            job.recheck(s).await?;
        }
        if let Some(credential) = &self.public_credential {
            let required_scope = self.public_scope.unwrap_or("manage");
            crate::relay::authorize_plugin_mcp_scope(s, credential, required_scope).await?;
            if let Some(additional) = self.additional_public_scope
                && additional != required_scope
            {
                crate::relay::authorize_plugin_mcp_scope(s, credential, additional).await?;
            }
        }
        if let Some(expected) = &self.owner {
            let policy = crate::relay::task_author_policy(s).await?.ok_or_else(|| {
                Error::forbidden("The installation owner changed during preparation.")
            })?;
            if expected.account_id != policy.owner.account_id
                || expected.access_id != policy.owner.access_id
            {
                return Err(Error::forbidden(
                    "The installation owner's access changed during preparation. Retry the action.",
                ));
            }
        }
        if let Some(guard) = &self.recheck_guard {
            let authority = self.clone();
            s.store.read(move |db| authority.authorize(db)).await?;
            guard(s).await?;
        }
        let authority = self.clone();
        s.store.read(move |db| authority.authorize(db)).await
    }

    pub(crate) fn authorize(&self, db: &Db<'_>) -> Result<()> {
        if let Some(guard) = &self.guard {
            guard(db)?;
        }
        if let Some(job) = &self.accepted_activation {
            job.authorize(db)?;
        } else if let Some(bearer) = &self.bearer {
            crate::installation_mcp::authorize_in(db, bearer)?;
        }
        Ok(())
    }
}

impl PluginPackageOperation {
    pub(crate) fn descriptor(&self) -> Value {
        let input = match self.name {
            "list_plugins" => schema_for::<ListPluginPackagesInput>(Contract::Deserialize),
            "list_plugin_resources" => {
                schema_for::<ListPluginResourcesInput>(Contract::Deserialize)
            }
            "resolve_plugin_resource" => {
                schema_for::<ResolvePluginResourceInput>(Contract::Deserialize)
            }
            "install_plugin_package" => {
                schema_for::<InstallPluginPackageInput>(Contract::Deserialize)
            }
            "approve_plugin_package" => {
                schema_for::<ApprovePluginPackageInput>(Contract::Deserialize)
            }
            "activate_plugin_package" => {
                schema_for::<ActivatePluginPackageInput>(Contract::Deserialize)
            }
            "list_plugin_programs" => schema_for::<ListPluginProgramsInput>(Contract::Deserialize),
            "prepare_plugin_program" => schema_for::<PluginProgramInput>(Contract::Deserialize),
            "get_plugin_operation"
            | "get_plugin_resource_operation"
            | "cancel_plugin_operation" => {
                schema_for::<PluginOperationInput>(Contract::Deserialize)
            }
            _ => schema_for::<PluginPackageInput>(Contract::Deserialize),
        };
        let output = match self.name {
            "list_plugins" => schema_for::<PluginCatalog>(Contract::Serialize),
            "list_plugin_resources" | "resolve_plugin_resource" => {
                schema_for::<PluginOperationStarted>(Contract::Serialize)
            }
            "list_plugin_programs" => schema_for::<ListPluginProgramsOutput>(Contract::Serialize),
            "get_plugin_operation"
            | "get_plugin_resource_operation"
            | "cancel_plugin_operation" => schema_for::<PluginOperation>(Contract::Serialize),
            "delete_plugin_package" => schema_for::<DeletePluginPackageOutput>(Contract::Serialize),
            "install_plugin_package" => schema_for::<PackageVersion>(Contract::Serialize),
            _ => schema_for::<PluginOperationStarted>(Contract::Serialize),
        };
        json!({
            "name": self.name, "description": self.description,
            "inputSchema": input, "outputSchema": output,
            "_meta": { "securitySchemes": [{ "type": "oauth2", "scopes": [self.scope] }] },
            "annotations": { "readOnlyHint": self.scope == "read" || self.name == "get_plugin_operation", "destructiveHint": self.name == "delete_plugin_package", "openWorldHint": true },
        })
    }

    pub(crate) async fn invoke(
        &self,
        s: &Arc<Service>,
        input: Value,
        context: ResumeContext,
    ) -> Result<Value> {
        // Resource handlers add sizeable futures to this shared dispatcher. Keep
        // that state off its HTTP/MCP callers' stacks, including nested preparation.
        Box::pin(self.invoke_inner(s, input, context)).await
    }

    async fn invoke_inner(
        &self,
        s: &Arc<Service>,
        input: Value,
        context: ResumeContext,
    ) -> Result<Value> {
        let owner = matches!(context, ResumeContext::Human(_));
        let human_account = match &context {
            ResumeContext::Human(account) => Some(account.clone()),
            _ => None,
        };
        // Resource handlers use instance capabilities after this request ends;
        // read scope must retain the human's revocable account/access identity.
        let reads_resources = matches!(
            self.name,
            "list_plugin_resources" | "resolve_plugin_resource" | "get_plugin_resource_operation"
        );
        let public_resource_read = match &context {
            ResumeContext::Public(scopes, _) => Some(scopes.iter().any(|scope| scope == "read")),
            _ => None,
        };
        let mut authority = match context {
            ResumeContext::Installation(bearer) => {
                let (_, live) =
                    crate::installation_mcp::authorize_with_authority(s, &bearer).await?;
                if self.name == "activate_plugin_package" {
                    let job = crate::authorization::Accepted::capture(s, &bearer).await?;
                    live.recheck(s).await?;
                    // The accepted activation keeps its initiating identity and
                    // polling audience, but its private execution proof outlives
                    // normal turn completion. This is not a resource-read proof.
                    Authority {
                        bearer: Some(bearer),
                        accepted_activation: Some(job),
                        ..Default::default()
                    }
                } else {
                    live
                }
            }
            ResumeContext::Public(scopes, credential) => {
                if !scopes.iter().any(|scope| scope == self.scope) {
                    return Err(Error::forbidden(format!(
                        "The {} scope is required.",
                        self.scope
                    )));
                }
                let writes_packages = matches!(
                    self.name,
                    "install_plugin_package" | "activate_plugin_package" | "delete_plugin_package"
                );
                if (writes_packages || reads_resources) && credential.is_none() {
                    return Err(Error::unauthorized(
                        "A current public MCP authorization is required.",
                    ));
                }
                Authority {
                    public_credential: credential,
                    public_scope: Some(self.scope),
                    ..Default::default()
                }
            }
            ResumeContext::Human(account) => {
                if reads_resources
                    || (self.scope == "manage"
                        && !matches!(
                            self.name,
                            "get_plugin_operation" | "cancel_plugin_operation"
                        ))
                {
                    Authority::human(s, &account).await?
                } else {
                    Authority::default()
                }
            }
        };
        let check = authority.clone();
        s.store.read(move |db| check.authorize(db)).await?;
        let mut resource_result = None;
        let result = match self.name {
            "list_plugins" => {
                let _: ListPluginPackagesInput = decode(input)?;
                serde_json::to_value(management::catalog(s).await?)?
            }
            "list_plugin_resources" | "resolve_plugin_resource" => {
                authority.recheck(s).await?;
                let caller = match &authority.bearer {
                    Some(bearer) => Some(crate::installation_mcp::authorize(s, bearer).await?),
                    None => None,
                };
                let agent = caller
                    .as_ref()
                    .map(|run| crate::validation::text(&run["snapshot"]["agent"], "id"));
                let audience =
                    operations::Audience::from_bearer(authority.bearer.as_deref(), owner);
                let service = s.clone();
                if self.name == "list_plugin_resources" {
                    let input: ListPluginResourcesInput = decode(input)?;
                    let read = Arc::new(
                        resources::prepare_list(s, &input, agent, Some(authority.clone())).await?,
                    );
                    let work = read.clone();
                    s.plugins.operations.start_resource_read(
                        s.clone(),
                        audience,
                        read,
                        async move {
                            Ok(PluginOperationResult::ResourcePage {
                                page: Box::new(work.list(&service, input).await?),
                            })
                        },
                    )?
                } else {
                    let input: ResolvePluginResourceInput = decode(input)?;
                    let read = Arc::new(
                        resources::prepare_resolve(s, &input, agent, Some(authority.clone()))
                            .await?,
                    );
                    let work = read.clone();
                    s.plugins.operations.start_resource_read(
                        s.clone(),
                        audience,
                        read,
                        async move {
                            Ok(PluginOperationResult::ResourceAncestry {
                                resolution: Box::new(work.resolve(&service, input).await?),
                            })
                        },
                    )?
                }
            }
            "install_plugin_package" => {
                let input: InstallPluginPackageInput = decode(input)?;
                serde_json::to_value(
                    registry::install_authorized(s, &input, authority.clone()).await?,
                )?
            }
            "approve_plugin_package" => {
                let input: ApprovePluginPackageInput = decode(input)?;
                if !owner {
                    return Err(Error::forbidden(
                        "Only the installation owner can approve plugin code. Review it in Plugins.",
                    ));
                }
                let service = s.clone();
                let auth = authority.clone();
                s.plugins
                    .operations
                    .start(s.clone(), operations::Audience::Owner, async move {
                        let version = registry::approve_authorized(
                            &service,
                            &input.plugin_id,
                            &input.version,
                            &input.digest,
                            auth,
                        )
                        .await?;
                        Ok(PluginOperationResult::Package {
                            version: Box::new(version),
                        })
                    })?
            }
            "activate_plugin_package" => {
                let input: ActivatePluginPackageInput = decode(input)?;
                let service = s.clone();
                let auth = authority.clone();
                s.plugins.operations.start(
                    s.clone(),
                    operations::Audience::from_bearer(authority.bearer.as_deref(), owner),
                    async move {
                        let version = registry::activate_authorized(
                            &service,
                            &input.plugin_id,
                            &input.version,
                            auth,
                        )
                        .await?;
                        Ok(PluginOperationResult::Package {
                            version: Box::new(version),
                        })
                    },
                )?
            }
            "delete_plugin_package" => {
                let input: PluginPackageInput = decode(input)?;
                registry::uninstall_authorized(s, &input.plugin_id, authority.clone()).await?
            }
            "list_plugin_programs" => {
                let input: ListPluginProgramsInput = decode(input)?;
                program_installation::list(s, &input.plugin_id).await?
            }
            "prepare_plugin_program" => {
                let input: PluginProgramInput = decode(input)?;
                if !owner {
                    return Err(Error::forbidden(
                        "Only the installation owner can change plugin programs. Review the release in Plugins.",
                    ));
                }
                let service = s.clone();
                let auth = authority.clone();
                s.plugins
                    .operations
                    .start(s.clone(), operations::Audience::Owner, async move {
                        let (plugin, name, action, data) = match input {
                            PluginProgramInput::Check { plugin_id, program } => {
                                (plugin_id, program, "check", json!({}))
                            }
                            PluginProgramInput::Install {
                                plugin_id,
                                program,
                                digest,
                            } => (plugin_id, program, "install", json!({ "digest": digest })),
                            PluginProgramInput::Rollback { plugin_id, program } => {
                                (plugin_id, program, "rollback", json!({}))
                            }
                        };
                        let program = serde_json::from_value(
                            program_installation::action_authorized(
                                &service, &plugin, &name, action, &data, &auth,
                            )
                            .await?,
                        )?;
                        Ok(PluginOperationResult::Program { program })
                    })?
            }
            "get_plugin_operation"
            | "get_plugin_resource_operation"
            | "cancel_plugin_operation" => {
                let input: PluginOperationInput = decode(input)?;
                s.plugins.operations.authorize(
                    &input.operation_id,
                    authority.bearer.as_deref(),
                    owner,
                )?;
                resource_result = s.plugins.operations.resource_read(
                    &input.operation_id,
                    self.name == "get_plugin_resource_operation",
                )?;
                if resource_result.is_some()
                    && authority.owner.is_none()
                    && let Some(account) = &human_account
                {
                    authority = Authority::human(s, account).await?;
                }
                if resource_result.is_some()
                    && let Some(can_read) = public_resource_read
                {
                    if !can_read {
                        return Err(Error::forbidden(
                            "The read scope is required for resource results.",
                        ));
                    }
                    if authority.public_credential.is_none() {
                        return Err(Error::unauthorized(
                            "A current public MCP authorization is required.",
                        ));
                    }
                    authority.additional_public_scope = Some("read");
                }
                if self.name == "cancel_plugin_operation" {
                    s.plugins.operations.cancel(&input.operation_id)?;
                }
                s.plugins.operations.get(&input.operation_id)?
            }
            _ => return Err(Error::not_found("Unknown plugin package operation.")),
        };
        let check = authority.clone();
        s.store.read(move |db| check.authorize(db)).await?;
        if let Some(read) = resource_result {
            read.authorize_result(s, &authority).await?;
        }
        Ok(result)
    }
}
