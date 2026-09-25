//! Revocable, per-version public links. Only this read-only route bypasses login.
use super::*;
use crate::store::Db;

pub fn visibility(args: &Value) -> Result<&str> {
    match args.get("visibility").and_then(Value::as_str) {
        Some(value @ ("private" | "public")) => Ok(value),
        _ => Err(Error::bad("Visibility must be private or public.")),
    }
}

pub fn tool() -> Value {
    json!({
        "name": "set_artifact_visibility",
        "description": "Enable or revoke a public link for an artifact in the current conversation/run. \
    Public links let anyone with the link read this specific file version without signing \
    in. Only make files public when requested by the user. Revoking a link does not \
    remove copies already downloaded. Returns the updated artifact and publicUrl when \
    public.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "artifactId": {
                    "type": "string",
                    "format": "uuid",
                },
                "visibility": {
                    "type": "string",
                    "enum": ["private", "public"],
                },
            },
            "required": ["artifactId", "visibility"],
            "additionalProperties": false,
        },
    })
}

pub(super) fn apply(db: &Db<'_>, record: &mut Value, visibility: &str, origin: &str) -> Result<()> {
    if visibility == "public" {
        crate::conversation_lifecycle::require_active_run(db, text(record, "runId"))?;
        let token = if let Some(token) = record["publicToken"].as_str().filter(|t| !t.is_empty()) {
            token.to_owned()
        } else {
            id()
        };
        db.set(
            &format!("artifact-share:{token}"),
            &json!({
                "runId": record["runId"],
                "id": record["id"],
            }),
            None,
        )?;
        record["publicUrl"] = format!(
            "{}/api/public/artifacts/{token}",
            origin.trim_end_matches('/')
        )
        .into();
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
    let value = visibility(&json!({
        "visibility": value,
    }))?
    .to_owned();
    let bearer = bearer.map(str::to_owned);
    let origin = s.config.public_url.clone();
    s.store
        .transaction(move |db| {
            if let Some(token) = bearer {
                let authorized = crate::project_workspaces::authorize_in(db, &token)?;
                if authorized["id"] != run {
                    return Err(Error::new(403, "Artifact is outside this run."));
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
        ["", "api", "public", "artifacts", token] if uuid(token).is_ok())
}

pub async fn http(s: &Service, token: &str, request: Request) -> Result<Response> {
    if !public_read(request.uri().path(), request.method().as_str()) {
        return Err(Error::new(404, "Public file not found."));
    }
    let token = token.to_owned();
    let record = s
        .store
        .read(move |db| {
            let share = required(
                db.kv(&format!("artifact-share:{token}"))?,
                "Public file not found",
            )?;
            let run = text(&share, "runId");
            required(db.run(run)?, "Public file not found")?;
            let record = required(
                db.kv(&format!("artifact:{run}:{}", text(&share, "id")))?,
                "Public file not found",
            )?;
            if record["visibility"] != "public" || record["publicToken"] != token {
                return Err(Error::new(404, "Public file not found."));
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
