//! Revocable per-version links, read only through the authenticated relay.
use super::*;
use crate::store::Db;

const NOT_FOUND: &str = "Public file not found";

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

pub(super) fn apply(
    db: &Db<'_>,
    record: &mut Deliverable,
    visibility: &str,
    origin: &str,
) -> Result<()> {
    if visibility == "public" {
        crate::conversation_lifecycle::require_active_run(db, &record.run_id)?;
        let token = record
            .public_token
            .clone()
            .filter(|token| !token.is_empty())
            .unwrap_or_else(id);
        record.public_url = Some(format!(
            "{}/artifacts/{token}",
            origin.trim_end_matches('/')
        ));
        record.public_token = Some(token);
    } else {
        record.public_token = None;
        record.public_url = None;
    }
    record.visibility = Some(visibility.to_owned());
    Ok(())
}

pub async fn set(
    s: &Service,
    run: &str,
    artifact: &str,
    value: &str,
    bearer: Option<&str>,
) -> Result<Value> {
    let bearer = bearer.map(str::to_owned);
    let expected_run = run.to_owned();
    set_authorized(s, run, artifact, value, move |db| {
        if let Some(token) = bearer {
            let authorized = crate::project_workspaces::authorize_in(db, &token)?;
            if authorized["id"] != expected_run {
                return Err(Error::forbidden("Artifact is outside this run."));
            }
        }
        Ok(())
    })
    .await
}

pub(crate) async fn set_authorized<F>(
    s: &Service,
    run: &str,
    artifact: &str,
    value: &str,
    authorize: F,
) -> Result<Value>
where
    F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
{
    uuid(run)?;
    uuid(artifact)?;
    let run = run.to_owned();
    let artifact = artifact.to_owned();
    let value = checked(Some(value))?.to_owned();
    let origin = if value == "public" {
        public_origin(s).await?
    } else {
        String::new()
    };
    s.store
        .transaction(move |db| {
            authorize(db)?;
            required(db.run(&run)?, "Run not found")?;
            let mut record = required(db.artifact(&run, &artifact)?, "Artifact not found")?;
            apply(db, &mut record, &value, &origin)?;
            db.save_artifact_visibility(&record)?;
            Ok(serde_json::to_value(record)?)
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
            let record = required(db.public_artifact(&token)?, NOT_FOUND)?;
            required(db.run(&record.run_id)?, NOT_FOUND)?;
            crate::conversation_lifecycle::require_active_run(db, &record.run_id)
                .map_err(|_| Error::not_found(NOT_FOUND))?;
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
    let (origin, installation) = crate::relay::beacon_address(&s.config.data_dir)
        .await?
        .ok_or_else(|| {
            Error::unavailable("Claim this installation before sharing public files.")
        })?;
    Ok(format!("{origin}/api/public/installations/{installation}"))
}
