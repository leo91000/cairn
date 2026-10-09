//! Revocable per-version links, read only through the authenticated relay.
use super::*;
use crate::store::Db;
use serde::Deserialize;

const NOT_FOUND: &str = "Public file not found";

/// A public link (`artifact-share:{token}`) to one artifact version.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShareLink {
    run_id: String,
    id: String,
}

fn checked(value: Option<&str>) -> Result<&str> {
    match value {
        Some(value @ ("private" | "public")) => Ok(value),
        _ => Err(Error::bad("Visibility must be private or public.")),
    }
}

pub fn visibility(args: &Value) -> Result<&str> {
    checked(args.get("visibility").and_then(Value::as_str))
}

pub fn tool() -> Value {
    json!({
        "name": "set_artifact_visibility",
        "description": "Enable or revoke a public link for an artifact in the current conversation/run. Public links let anyone with the link read this specific file version without signing in. Only make files public when requested by the user. Revoking a link does not remove copies already downloaded. Returns the updated artifact and publicUrl when public.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "artifactId": { "type": "string", "format": "uuid" },
                "visibility": { "type": "string", "enum": ["private", "public"] },
            },
            "required": ["artifactId", "visibility"],
            "additionalProperties": false,
        },
    })
}

pub(super) fn apply(db: &Db<'_>, record: &mut Value, visibility: &str, origin: &str) -> Result<()> {
    if visibility == "public" {
        crate::conversation_lifecycle::require_active_run(db, text(record, "runId"))?;
        let token = record["publicToken"]
            .as_str()
            .filter(|t| !t.is_empty())
            .map_or_else(id, str::to_owned);
        let link = ShareLink {
            run_id: text(record, "runId").to_owned(),
            id: text(record, "id").to_owned(),
        };
        db.set(
            &format!("artifact-share:{token}"),
            &serde_json::to_value(link)?,
            None,
        )?;
        let origin = origin.trim_end_matches('/');
        record["publicUrl"] = format!("{origin}/artifacts/{token}").into();
        record["publicToken"] = token.into();
    } else {
        if let Some(token) = record["publicToken"].as_str() {
            db.delete(&format!("artifact-share:{token}"))?;
        }
        record["publicToken"] = Value::Null;
        record["publicUrl"] = Value::Null;
    }
    record["visibility"] = visibility.into();
    Ok(())
}

pub async fn set(
    s: &Service,
    run: &str,
    artifact: &str,
    value: &str,
    bearer: Option<&str>,
) -> Result<Value> {
    uuid(run)?;
    uuid(artifact)?;
    let run = run.to_owned();
    let artifact = artifact.to_owned();
    let value = checked(Some(value))?.to_owned();
    let bearer = bearer.map(str::to_owned);
    let origin = if value == "public" {
        public_origin(s).await?
    } else {
        String::new()
    };
    s.store
        .transaction(move |db| {
            if let Some(token) = bearer {
                let authorized = crate::project_workspaces::authorize_in(db, &token)?;
                if authorized["id"] != run {
                    return Err(Error::forbidden("Artifact is outside this run."));
                }
            }
            required(db.run(&run)?, "Run not found")?;
            let key = format!("artifact:{run}:{artifact}");
            let mut record = required(db.kv(&key)?, "Artifact not found")?;
            apply(db, &mut record, &value, &origin)?;
            db.set(&key, &record, None)?;
            Ok(record)
        })
        .await
}

pub async fn for_agent(s: &Service, bearer: &str, args: &Value) -> Result<Value> {
    let run = crate::project_workspaces::authorize(s, bearer).await?;
    set(
        s,
        text(&run, "id"),
        text(args, "artifactId"),
        visibility(args)?,
        Some(bearer),
    )
    .await
}

pub fn public_read(path: &str, method: &str) -> bool {
    matches!(method, "GET" | "HEAD")
        && matches!(path.split('/').collect::<Vec<_>>().as_slice(),
        ["", "api", "shared-artifacts", token] if uuid(token).is_ok())
}

pub async fn http(s: &Service, token: &str, request: Request) -> Result<Response> {
    if !public_read(request.uri().path(), request.method().as_str()) {
        return Err(Error::not_found("Public file not found."));
    }
    let token = token.to_owned();
    let record = s
        .store
        .read(move |db| {
            let share = required(db.kv(&format!("artifact-share:{token}"))?, NOT_FOUND)?;
            let share = ShareLink::deserialize(&share)?;
            required(db.run(&share.run_id)?, NOT_FOUND)?;
            let key = format!("artifact:{}:{}", share.run_id, share.id);
            let record = required(db.kv(&key)?, NOT_FOUND)?;
            if record["visibility"] != "public" || record["publicToken"] != token {
                return Err(Error::not_found("Public file not found."));
            }
            Ok(record)
        })
        .await?;
    let mut response = super::serve(s, &record, request).await?;
    response
        .headers_mut()
        .insert("access-control-allow-origin", HeaderValue::from_static("*"));
    response
        .headers_mut()
        .insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    response.headers_mut().insert(
        "x-robots-tag",
        HeaderValue::from_static("noindex, nofollow"),
    );
    Ok(response)
}

pub(super) async fn public_origin(s: &Service) -> Result<String> {
    let (origin, installation) = crate::relay::official_address(&s.config.data_dir)
        .await?
        .ok_or_else(|| {
            Error::unavailable("Claim this installation before sharing public files.")
        })?;
    Ok(format!("{origin}/api/public/installations/{installation}"))
}
