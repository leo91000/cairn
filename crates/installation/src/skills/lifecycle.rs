//! Resource lifecycle shared by the three transports. Files/bundles retain their
//! existing writer; publication rechecks the actor, project and source instance.
use crate::{
    capabilities::{ResumeContext, plugin_packages},
    error::{Error, Result},
    plugins::connections::{self, Connection},
    service::Service,
    store::Db,
};
use cairn_contracts::Project;
use std::path::{Path, PathBuf};

#[derive(Clone, Default)]
pub(crate) struct Authority {
    preparation: plugin_packages::Authority,
    project: Option<Project>,
    instance: Option<Connection>,
    package: Option<(String, String, String)>,
    management: Option<crate::plugins::management::Authority>,
}

impl Authority {
    pub(crate) async fn new(
        s: &Service,
        context: ResumeContext,
        required_scope: &str,
    ) -> Result<Self> {
        let preparation = match context {
            ResumeContext::Human(account) if required_scope == "manage" => {
                plugin_packages::Authority::human(s, &account).await?
            }
            ResumeContext::Human(_) => plugin_packages::Authority::default(),
            ResumeContext::Installation(bearer) => {
                crate::installation_mcp::authorize_with_authority(s, &bearer)
                    .await?
                    .1
            }
            ResumeContext::Public(scopes, credential) => {
                if !scopes.iter().any(|scope| scope == required_scope) {
                    return Err(Error::forbidden(format!(
                        "The {required_scope} scope is required."
                    )));
                }
                if required_scope == "manage" && credential.is_none() {
                    return Err(Error::unauthorized(
                        "A current public MCP authorization is required.",
                    ));
                }
                plugin_packages::Authority::public_scoped(
                    credential,
                    if required_scope == "read" {
                        "read"
                    } else {
                        "manage"
                    },
                )
            }
        };
        let authority = Self {
            preparation,
            ..Default::default()
        };
        authority.recheck(s).await?;
        Ok(authority)
    }

    pub(crate) async fn project(&mut self, s: &Service, scope: &str) -> Result<Option<PathBuf>> {
        if scope == "global" {
            return Ok(None);
        }
        crate::validation::uuid(scope)?;
        let project = s.get_project(scope).await?;
        let path = PathBuf::from(&project.settings.path);
        self.project = Some(project);
        self.recheck(s).await?;
        Ok(Some(path))
    }

    pub(crate) async fn instance(
        &self,
        s: &Service,
        instance: &Connection,
        package: &crate::plugins::registry::Package,
        kind: cairn_contracts::PluginContributionKind,
        name: &str,
    ) -> Result<Self> {
        let mut authority = self.clone();
        authority.instance = Some(instance.clone());
        if !package.builtin {
            authority.package = Some((
                package.manifest.id.clone(),
                package.manifest.version.clone(),
                package.digest.clone(),
            ));
        }
        if let Some(bearer) = &self.preparation.bearer {
            let run = crate::installation_mcp::authorize(s, bearer).await?;
            let agent = crate::validation::text(&run["snapshot"]["agent"], "id");
            let snapshot = crate::plugins::grant_authorization::Snapshot::capture_installation(
                s, agent, bearer, package,
            )
            .await?;
            let invocation = crate::plugins::grant_authorization::Invocation::prepare(
                s,
                snapshot.clone(),
                instance.clone(),
                kind,
                name,
                &serde_json::Value::Null,
            )
            .await?;
            authority.management = Some(crate::plugins::management::Authority::agent_action(
                instance.clone(),
                snapshot,
                Some(invocation),
            ));
        } else {
            authority.management = Some(crate::plugins::management::Authority::resource_read(
                self.preparation.clone(),
                instance.clone(),
            ));
        }
        authority.recheck(s).await?;
        Ok(authority)
    }

    pub(crate) fn management(&self) -> Option<crate::plugins::management::Authority> {
        self.management.clone()
    }

    pub(crate) fn authorize(&self, db: &Db<'_>) -> Result<()> {
        self.preparation.authorize(db)?;
        if let Some(management) = &self.management {
            management.authorize(db)?;
        }
        if let Some(expected) = &self.project {
            let current = db
                .project(&expected.id.to_string())?
                .ok_or_else(|| Error::conflict("The project was removed."))?;
            if current.revision != expected.revision {
                return Err(Error::conflict(
                    "The project changed during this operation. Reload it and retry.",
                ));
            }
        }
        if let Some((id, version, digest)) = &self.package {
            let current = db.plugin_package(id)?;
            if current.active.as_ref() != Some(version)
                || current.approvals.get(version) != Some(digest)
            {
                return Err(Error::conflict(
                    "The source package changed during preparation. Reload it and retry.",
                ));
            }
        }
        if let Some(expected) = &self.instance {
            connections::check_revision(db, expected)?;
            if !expected.enabled {
                return Err(Error::forbidden("This plugin instance is disabled."));
            }
        }
        Ok(())
    }

    pub(crate) async fn check_local(&self, s: &Service) -> Result<()> {
        let authority = self.clone();
        s.store.read(move |db| authority.authorize(db)).await
    }

    pub(crate) async fn recheck(&self, s: &Service) -> Result<()> {
        self.preparation.recheck(s).await?;
        if let Some(management) = &self.management {
            management.recheck(s).await?;
        }
        self.check_local(s).await
    }

    pub(crate) fn publication<'a>(&self, s: &'a Service) -> super::bundle::Authorization<'a> {
        let authority = self.clone();
        let external = self.clone();
        super::bundle::Authorization {
            store: s.store.clone(),
            recheck: Box::pin(async move { external.recheck(s).await }),
            authorize: Box::new(move |db, _| authority.authorize(db)),
        }
    }
}

pub(crate) async fn remove(
    s: &Service,
    scope: &str,
    name: &str,
    project: Option<&Path>,
    authority: Authority,
) -> Result<()> {
    super::name(name)?;
    let _mutation = s.skills.mutations.lock().await;
    let root = s.skills.root(project).await?;
    let _files = super::bundle::access(&root).await.write_owned().await;
    let directory = super::skill_directory(&root, name).await?;
    authority.recheck(s).await?;
    let key = format!("{scope}/{name}");
    let source = super::sources::key(scope, name);
    let detail = serde_json::json!({"scope": scope, "name": name});
    s.store
        .transaction(move |db| {
            authority.authorize(db)?;
            let selected = db.missions()?.iter().any(|task| {
                task.settings
                    .skills
                    .as_ref()
                    .is_some_and(|skills| skills.contains(&key))
            });
            if selected {
                return Err(Error::conflict(
                    "This skill is selected by a task. Update that task first.",
                ));
            }
            std::fs::remove_dir_all(directory)?;
            db.delete(&source)?;
            db.audit("skill.deleted", &detail)
        })
        .await
}
