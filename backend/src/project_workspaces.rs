//! Run-scoped project access: authorize, prepare a private seed, then import once.
use crate::{
    auth::hex_digest,
    error::{Error, Result},
    service::{Service, policy, run_projects},
    validation::{text, uuid},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Weak},
    time::Duration,
};
use tokio::sync::Mutex;

#[derive(Default)]
pub struct Projects {
    locks: Mutex<HashMap<String, Weak<Mutex<()>>>>,
}

pub fn catalog(run: &Value) -> Vec<Value> {
    run["snapshot"]["availableProjects"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| run_projects(run))
}

pub fn authorize_in(db: &crate::store::Db<'_>, bearer: &str) -> Result<Value> {
    let key = format!("mcp-grant:{}", hex_digest(bearer));
    let denied = || Error::new(401, "Workspace access expired or was revoked.");
    let grant = db.kv(&key)?.ok_or_else(denied)?;
    let run = db.run(text(&grant, "runId"))?.ok_or_else(denied)?;
    if grant["messageId"] != run["chatExecution"]["messageId"]
        || grant["workspace"] != true
        || run["status"] != "running"
        || !run["cancelRequestedAt"].is_null()
    {
        return Err(denied());
    }
    let current = db
        .get("agents", text(&run["snapshot"]["agent"], "id"))?
        .ok_or_else(denied)?;
    if policy(&current) != policy(&run["snapshot"]["agent"]) {
        return Err(Error::new(403, "Agent permissions changed."));
    }
    Ok(run)
}
pub async fn authorize(s: &Service, bearer: &str) -> Result<Value> {
    let bearer = bearer.to_owned();
    s.store.read(move |db| authorize_in(db, &bearer)).await
}

impl Projects {
    pub async fn open(&self, s: &Service, bearer: &str, project_id: &str) -> Result<Value> {
        uuid(project_id)?;
        let run = authorize(s, bearer).await?;
        let key = format!("{}:{project_id}", text(&run, "id"));
        let lock = {
            let mut locks = self.locks.lock().await;
            locks.retain(|_, lock| lock.strong_count() > 0);
            let lock = locks.get(&key).and_then(Weak::upgrade).unwrap_or_default();
            locks.insert(key, Arc::downgrade(&lock));
            lock
        };
        let _guard = lock.lock().await;
        let run = authorize(s, bearer).await?;
        let project = catalog(&run)
            .into_iter()
            .find(|p| p["id"] == project_id)
            .ok_or_else(|| Error::new(403, "This project is not authorized for this run."))?;
        let current = s
            .store
            .get("projects", project_id)
            .await?
            .ok_or_else(|| Error::new(404, "Project no longer exists."))?;
        if current["path"] != project["path"]
            || current["baseBranch"] != project["baseBranch"]
            || (current["sourceMode"] == "local") != (project["sourceMode"] == "local")
        {
            return Err(Error::new(
                409,
                "Project configuration changed. Start a new conversation.",
            ));
        }
        let checkpoint = s
            .store
            .kv(&format!("run-checkpoint:{}", text(&run, "id")))
            .await?
            .unwrap_or_default();
        let prepared = &checkpoint["prepared"];
        let root = Path::new(text(prepared, "projectRoot"));
        if prepared["backend"] != "firecracker" || !root.is_absolute() {
            return Err(Error::new(
                409,
                "The private workspace is not ready. Retry when the run is active.",
            ));
        }
        let attempt = text(&checkpoint, "runnerId");
        uuid(attempt)?;
        s.store
            .event(
                text(&run, "id"),
                "status",
                &format!("Opening {}", text(&project, "name")),
                None,
            )
            .await?;
        let entry = crate::execution::project_seed(&run, &project, &s.config, root).await?;
        // Recheck the grant after a potentially slow clone and before transferring anything.
        authorize(s, bearer).await?;
        let credential = crate::execution::secret(&s.config.data_dir, "runner-secret").await?;
        let response = s.http.post(format!("{}/runs/{attempt}/projects/{project_id}",crate::nodes::transport::url(s,text(&run,"id")).await?))
            .bearer_auth(credential)
            .json(&json!({"runId":run["id"],"source":entry["path"],"target":entry["path"]}))
            .timeout(Duration::from_secs(300)).send().await
            .map_err(|_| Error::new(503,"Project transfer was interrupted. Retry open_project; saved files are preserved."))?;
        if !response.status().is_success() {
            return Err(Error::new(
                503,
                "Project could not be opened in this VM. Retry when the run is active.",
            ));
        }
        let response: Value = response.json().await.map_err(Error::internal)?;
        if response["ok"] != true {
            return Err(Error::new(503, "Project import was not acknowledged."));
        }
        let id = text(&run, "id").to_owned();
        let entry_copy = entry.clone();
        s.store
            .write(move |db| {
                let mut run = db
                    .run(&id)?
                    .ok_or_else(|| Error::new(404, "Run not found."))?;
                let mut entries = run["workspaces"].as_array().cloned().unwrap_or_default();
                if !entries
                    .iter()
                    .any(|w| w["projectId"] == entry_copy["projectId"])
                {
                    entries.push(entry_copy);
                }
                run["workspaces"] = entries.into();
                db.patch_run(&id, &json!({"workspaces":run["workspaces"]}))?;
                Ok(())
            })
            .await?;
        Ok(
            json!({"projectId":project_id,"name":project["name"],"path":entry["path"],"reused":response["reused"],"revision":entry["revision"]}),
        )
    }
}

pub async fn rpc(s: &Service, bearer: &str, method: &str, params: &Value) -> Result<Value> {
    if method == "tools/call" && params["name"] == "list_nodes" {
        let run = authorize(s, bearer).await?;
        return Ok(match crate::nodes::moves::list(s, &run).await {
            Ok(value) => {
                json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":value})
            }
            Err(error) => {
                json!({"isError":true,"content":[{"type":"text","text":error.message}]})
            }
        });
    }
    if method == "tools/call" && params["name"] == "request_capacity" {
        let run = authorize(s, bearer).await?;
        return Ok(
            match crate::nodes::moves::request(s, &run, &params["arguments"]).await {
                Ok(value) => {
                    json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":value})
                }
                Err(error) => {
                    json!({"isError":true,"content":[{"type":"text","text":error.message}]})
                }
            },
        );
    }
    if method == "tools/call" && params["name"] == "onepassword" {
        return Ok(
            match crate::onepassword::call(s, bearer, &params["arguments"]).await {
                Ok(result) => {
                    json!({"content":[{"type":"text","text":result.to_string()}],"structuredContent":result})
                }
                Err(error) => {
                    json!({"isError":true,"content":[{"type":"text","text":error.message}]})
                }
            },
        );
    }
    if method == "tools/call" && params["name"] == "report_outcome" {
        return Ok(
            match crate::outcome::report(s, bearer, &params["arguments"]).await {
                Ok(result) => {
                    json!({"content":[{"type":"text","text":"Outcome saved."}],"structuredContent":result})
                }
                Err(error) => {
                    json!({"isError":true,"content":[{"type":"text","text":error.message}]})
                }
            },
        );
    }
    if method == "tools/call" && params["name"] == "set_artifact_visibility" {
        return Ok(
            match crate::artifacts::sharing::for_agent(s, bearer, &params["arguments"]).await {
                Ok(result) => {
                    json!({"content":[{"type":"text","text":format!("Artifact {}. {}", text(&result,"visibility"), text(&result,"publicUrl"))}],"structuredContent":result})
                }
                Err(error) => {
                    json!({"isError":true,"content":[{"type":"text","text":error.message}]})
                }
            },
        );
    }
    if method == "tools/call" && params["name"] == "publish_artifact" {
        return Ok(
            match s.artifacts.publish(s, bearer, &params["arguments"]).await {
                Ok(result) => {
                    json!({"content":[{"type":"text","text":format!("Published {}: {}",text(&result,"title"),result["publicUrl"].as_str().unwrap_or(text(&result,"url")))}],"structuredContent":result})
                }
                Err(error) => {
                    json!({"isError":true,"content":[{"type":"text","text":error.message}]})
                }
            },
        );
    }
    match method {
        "tools/list" => {
            let mut catalog = json!({"tools":[{"name":"open_project","description":"Open an authorized project in this conversation's private workspace. Call only when you need its files. Repeated calls reuse existing files and changes. Use the returned path for commands and read its AGENTS.md before editing.","inputSchema":{"type":"object","properties":{"projectId":{"type":"string","format":"uuid"}},"required":["projectId"],"additionalProperties":false}}]});
            catalog["tools"]
                .as_array_mut()
                .unwrap()
                .push(crate::artifacts::tool());
            catalog["tools"]
                .as_array_mut()
                .unwrap()
                .push(crate::nodes::moves::list_tool());
            catalog["tools"]
                .as_array_mut()
                .unwrap()
                .push(crate::nodes::moves::tool());
            catalog["tools"]
                .as_array_mut()
                .unwrap()
                .push(crate::artifacts::sharing::tool());
            catalog["tools"]
                .as_array_mut()
                .unwrap()
                .push(crate::outcome::tool());
            catalog["tools"]
                .as_array_mut()
                .unwrap()
                .push(crate::onepassword::tool());
            Ok(catalog)
        }
        "tools/call" if params["name"] == "open_project" => {
            let result = s
                .projects
                .open(s, bearer, text(&params["arguments"], "projectId"))
                .await;
            Ok(match result {
                Ok(result) => {
                    json!({"content":[{"type":"text","text":format!("{} is ready at {}",text(&result,"name"),text(&result,"path"))}],"structuredContent":result})
                }
                Err(error) => {
                    json!({"isError":true,"content":[{"type":"text","text":error.message}]})
                }
            })
        }
        "resources/list" => Ok(json!({"resources":[]})),
        "resources/templates/list" => Ok(json!({"resourceTemplates":[]})),
        "prompts/list" => Ok(json!({"prompts":[]})),
        _ => Err(Error::new(404, "Unknown workspace operation.")),
    }
}
