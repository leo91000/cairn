use super::*;
use crate::capabilities::ResumeContext;

pub(crate) struct MissionOperation {
    pub name: &'static str,
    scope: &'static str,
    description: &'static str,
}

pub(crate) const MISSION_OPERATIONS: &[MissionOperation] = &[
    MissionOperation {
        name: "list_tasks",
        scope: "read",
        description: "List missions and their current schedule state.",
    },
    MissionOperation {
        name: "get_task",
        scope: "read",
        description: "Read a mission.",
    },
    MissionOperation {
        name: "create_task",
        scope: "manage",
        description: "Create a mission; creationId makes retries return the same commitment. Settings contain all mission fields.",
    },
    MissionOperation {
        name: "replace_task",
        scope: "manage",
        description: "Replace all mission settings at the expected revision, retaining its author and membership.",
    },
    MissionOperation {
        name: "set_task_state",
        scope: "manage",
        description: "Set enabled and archived at the expected revision. Members may pause/archive other authors' commitments but cannot change their content or re-enable them.",
    },
    MissionOperation {
        name: "delete_task",
        scope: "manage",
        description: "Delete a mission without active work. Only the installation owner can delete missions.",
    },
    MissionOperation {
        name: "preview_schedule",
        scope: "read",
        description: "Preview the next three occurrences of a five-field cron expression in an IANA timezone.",
    },
    MissionOperation {
        name: "list_task_activity",
        scope: "read",
        description: "Latest run of each mission. unread, preview and dismissal describe the Cairn account that authorized this connection; executions have no account delegation and receive none.",
    },
];

pub(crate) fn mission_operation(name: &str) -> Option<&'static MissionOperation> {
    MISSION_OPERATIONS.iter().find(|op| op.name == name)
}

impl MissionOperation {
    pub(crate) fn descriptor(&self) -> Value {
        let input = match self.name {
            "create_task" => schema_for::<CreateMissionInput>(Contract::Deserialize),
            "replace_task" => schema_for::<ReplaceMissionInput>(Contract::Deserialize),
            "set_task_state" => schema_for::<MissionStateInput>(Contract::Deserialize),
            "preview_schedule" => schema_for::<SchedulePreviewInput>(Contract::Deserialize),
            "list_tasks" | "list_task_activity" => {
                schema_for::<ListMissionsInput>(Contract::Deserialize)
            }
            _ => schema_for::<MissionInput>(Contract::Deserialize),
        };
        let output = match self.name {
            "list_tasks" => schema_for::<ListMissionsOutput>(Contract::Serialize),
            "delete_task" => schema_for::<DeleteMissionOutput>(Contract::Serialize),
            "preview_schedule" => schema_for::<SchedulePreviewOutput>(Contract::Serialize),
            // Run summaries retain the run domain's existing wire shape until A5/A6.
            "list_task_activity" => schema_for::<Vec<Value>>(Contract::Serialize),
            _ => schema_for::<Mission>(Contract::Serialize),
        };
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": input,
            "outputSchema": output,
            "_meta": {
                "securitySchemes": [{ "type": "oauth2", "scopes": [self.scope] }],
            },
            "annotations": {
                "readOnlyHint": self.scope == "read",
                "destructiveHint": self.scope != "read",
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
        let actor = match context {
            ResumeContext::Human(account) => Actor::Human(InstallationIdentity::trusted(
                InstallationRole::Owner,
                &account,
            )),
            ResumeContext::Public(scopes, _) => {
                if !scopes.iter().any(|scope| scope == self.scope) {
                    return Err(Error::forbidden(format!(
                        "The {} scope is required.",
                        self.scope
                    )));
                }
                Actor::Public
            }
            ResumeContext::Installation(bearer) => {
                crate::installation_mcp::authorize(s, &bearer).await?;
                Actor::Installation(bearer)
            }
        };
        self.invoke_as(s, input, actor).await
    }

    pub(crate) async fn invoke_as(&self, s: &Service, input: Value, actor: Actor) -> Result<Value> {
        fn decode<T: serde::de::DeserializeOwned>(input: Value) -> Result<T> {
            serde_json::from_value(input).map_err(|e| Error::bad(e.to_string()))
        }
        match self.name {
            "create_task" => Ok(serde_json::to_value(
                s.missions
                    .save(s, SaveMission::Create(decode(input)?), actor)
                    .await?,
            )?),
            "replace_task" => Ok(serde_json::to_value(
                s.missions
                    .save(s, SaveMission::Replace(decode(input)?), actor)
                    .await?,
            )?),
            "set_task_state" => Ok(serde_json::to_value(
                s.missions
                    .save(s, SaveMission::State(decode(input)?), actor)
                    .await?,
            )?),
            "delete_task" => {
                s.missions.delete(s, decode(input)?, actor).await?;
                Ok(serde_json::to_value(DeleteMissionOutput { deleted: true })?)
            }
            "preview_schedule" => {
                let input: SchedulePreviewInput = decode(input)?;
                if input.cron.chars().count() > 100 || input.timezone.chars().count() > 100 {
                    return Err(Error::bad("Enter a valid cron expression and timezone."));
                }
                let occurrences = next_occurrences(&input.cron, &input.timezone, now(), 3)?;
                // Even a preparation-only operation rechecks an execution grant before returning.
                s.store
                    .read(move |db| {
                        actor.authorize(db)?;
                        Ok(serde_json::to_value(SchedulePreviewOutput { occurrences })?)
                    })
                    .await
            }
            "list_task_activity" => {
                let _: ListMissionsInput = decode(input)?;
                // Read and set-aside states belong to the verified account. An execution
                // has none of its own and never borrows the owner's.
                let account = match &actor {
                    Actor::Human(identity) => Some(identity.account_id.clone()),
                    Actor::Public => crate::native::policy::caller(),
                    Actor::Installation(_) => None,
                };
                if let Some(account) = &account {
                    crate::reads::begin(s, account).await?;
                }
                s.store
                    .read(move |db| {
                        let transaction = rusqlite::Transaction::new_unchecked(
                            db.0,
                            rusqlite::TransactionBehavior::Deferred,
                        )?;
                        let db = Db(&transaction);
                        actor.authorize(&db)?;

                        let query = format!(
                            "SELECT {} FROM run_views AS runs WHERE id IN (
                            SELECT (SELECT id FROM runs WHERE task_id=missions.id
                                ORDER BY created_at DESC,id DESC LIMIT 1) FROM missions
                        ) ORDER BY created_at DESC,id DESC",
                            crate::store::RUN_SUMMARY,
                        );
                        let mut runs = db.json_rows(&query, [])?;
                        db.hydrate_run_wakeups(&mut runs)?;
                        crate::reads::Reads::load_for(&db, account.as_deref())?
                            .runs(&db, &mut runs)?;
                        transaction.commit()?;
                        Ok(runs.into())
                    })
                    .await
            }
            "list_tasks" => {
                let _: ListMissionsInput = decode(input)?;
                s.store
                    .read(move |db| {
                        let tx = rusqlite::Transaction::new_unchecked(
                            db.0,
                            rusqlite::TransactionBehavior::Deferred,
                        )?;
                        let db = Db(&tx);
                        actor.authorize(&db)?;
                        let output = ListMissionsOutput {
                            tasks: db.missions()?,
                        };
                        tx.commit()?;
                        Ok(serde_json::to_value(output)?)
                    })
                    .await
            }
            _ => {
                let input: MissionInput = decode(input)?;
                s.store
                    .read(move |db| {
                        let tx = rusqlite::Transaction::new_unchecked(
                            db.0,
                            rusqlite::TransactionBehavior::Deferred,
                        )?;
                        let db = Db(&tx);
                        actor.authorize(&db)?;
                        let output =
                            required(db.mission(&input.task_id.to_string())?, "Mission not found")?;
                        tx.commit()?;
                        Ok(serde_json::to_value(output)?)
                    })
                    .await
            }
        }
    }
}
