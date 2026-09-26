//! Node-side HTTP relay. The master can address only the private VM controller.
use crate::{
    error::{Error, Result},
    validation::text,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

pub async fn run(
    master: url::Url,
    token: String,
    runner: String,
    runner_token: String,
    stop: CancellationToken,
) -> Result<()> {
    let runner = url::Url::parse(&runner).map_err(|_| Error::bad("Invalid local runner URL."))?;
    if runner.scheme() != "http"
        || !runner.host_str().is_some_and(|h| {
            h == "localhost"
                || h.parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        })
        || !runner.username().is_empty()
        || runner.password().is_some()
        || runner.path() != "/"
        || runner.query().is_some()
        || runner.fragment().is_some()
    {
        return Err(Error::bad(
            "The node relay requires a loopback HTTP VM controller.",
        ));
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(Error::internal)?;
    let permits = Arc::new(tokio::sync::Semaphore::new(64));
    let mut tasks = tokio::task::JoinSet::new();
    let executor = Arc::new(super::executor::Executor::default());
    let result = loop {
        let response = tokio::select! {
            _=stop.cancelled()=>break Ok(()),
            result=client.post(master.join("internal/nodes/poll").map_err(Error::internal)?).bearer_auth(&token).json(&json!({})).timeout(Duration::from_secs(25)).send()=>result,
        };
        let command = match response {
            Ok(response) if response.status() == 401 => {
                break Err(Error::new(401, "Node identity revoked."));
            }
            Ok(response) if response.status().is_success() => response.json::<Value>().await.ok(),
            _ => None,
        };
        while tasks.try_join_next().is_some() {}
        let Some(command) = command.filter(Value::is_object) else {
            tokio::select! { _=stop.cancelled()=>break Ok(()),_=tokio::time::sleep(Duration::from_millis(250))=>{} }
            continue;
        };
        let permit = permits
            .clone()
            .acquire_owned()
            .await
            .map_err(Error::internal)?;
        let (http, master, token, runner, runner_token) = (
            client.clone(),
            master.clone(),
            token.clone(),
            runner.clone(),
            runner_token.clone(),
        );
        let executor = executor.clone();
        tasks.spawn(async move {
            let _permit = permit;
            let _ = forward(
                &http,
                &master,
                &token,
                &runner,
                &runner_token,
                command,
                executor,
            )
            .await;
        });
    };
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    for attempt in executor.close().await {
        let _ = client
            .delete(
                runner
                    .join(&format!("runs/{attempt}"))
                    .map_err(Error::internal)?,
            )
            .bearer_auth(&runner_token)
            .timeout(Duration::from_secs(17))
            .send()
            .await;
    }
    result
}
fn route(method: &str, path: &str) -> Result<()> {
    let parts = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    let allowed = match parts.as_slice() {
        ["health"] => method == "GET",
        ["archive-transfers", id, offset] => {
            crate::validation::uuid(id).is_ok()
                && ((method == "DELETE" && *offset == "discard")
                    || (["GET", "POST"].contains(&method) && offset.parse::<u64>().is_ok()))
        }
        ["snapshots", id, hash] => {
            crate::validation::uuid(id).is_ok()
                && ((method == "GET" && super::snapshots::valid_hash(hash))
                    || (method == "DELETE" && *hash == "discard"))
        }
        ["runs", id] => crate::validation::uuid(id).is_ok() && ["POST", "DELETE"].contains(&method),
        ["runs", id, operation] => {
            crate::validation::uuid(id).is_ok()
                && matches!(
                    (method, *operation),
                    ("GET", "logs")
                        | ("POST", "wait")
                        | ("POST", "artifact")
                        | ("POST", "snapshot")
                )
        }
        ["runs", id, "projects", project] => {
            method == "POST"
                && crate::validation::uuid(id).is_ok()
                && crate::validation::uuid(project).is_ok()
        }
        ["disks", id, operation] => {
            method == "POST"
                && crate::validation::uuid(id).is_ok()
                && ["export", "import", "delete", "restore"].contains(operation)
        }
        _ => false,
    };
    if !allowed {
        return Err(Error::bad("Unsupported execution operation."));
    }
    Ok(())
}
async fn forward(
    client: &reqwest::Client,
    master: &url::Url,
    token: &str,
    runner: &url::Url,
    runner_token: &str,
    command: Value,
    executor: Arc<super::executor::Executor>,
) -> Result<()> {
    let id = text(&command, "id");
    crate::validation::uuid(id)?;
    let method = text(&command, "method");
    let path = text(&command, "path");
    if let Some(attempt) = path.strip_prefix("/prepare/") {
        crate::validation::uuid(attempt)?;
        let prepared = tokio::time::timeout(
            Duration::from_secs(280),
            executor.prepare(client, master, token, attempt),
        )
        .await;
        let status = if matches!(prepared, Ok(Ok(()))) {
            200
        } else {
            503
        };
        return send(
            client,
            master,
            token,
            &json!({"id":id,"sequence":0,"status":status,"done":true}),
        )
        .await;
    }
    let operation = async {
        route(method, path)?;
        let bytes = STANDARD
            .decode(text(&command, "body"))
            .map_err(|_| Error::bad("Invalid execution payload."))?;
        if bytes.len()
            > if path.ends_with("/restore") {
                super::snapshots::MAX_MANIFEST_BYTES
            } else {
                2_000_000
            }
        {
            return Err(Error::bad("Execution payload exceeds limit."));
        }
        executor
            .before(client, master, token, method, path, &bytes)
            .await?;
        client
            .request(
                method.parse().map_err(Error::internal)?,
                runner.join(path).map_err(Error::internal)?,
            )
            .bearer_auth(runner_token)
            .header("content-type", "application/json")
            .body(bytes)
            .send()
            .await
            .map_err(|_| Error::new(503, "Local VM controller unavailable."))
    };
    let response = tokio::time::timeout(
        Duration::from_secs(
            if path.ends_with("/restore") || path.ends_with("/export") || path.ends_with("/import")
            {
                7200
            } else if path.ends_with("/snapshot") {
                300
            } else {
                25
            },
        ),
        operation,
    )
    .await;
    let mut response = match response {
        Ok(Ok(response)) => response,
        _ => {
            send(client,master,token,&json!({"id":id,"sequence":0,"status":503,"done":true,"data":STANDARD.encode(b"VM controller unavailable")})).await?;
            return Ok(());
        }
    };
    let head = json!({"id":id,"sequence":0,"status":response.status().as_u16(),"length":response.content_length(),"contentType":response.headers().get("content-type").and_then(|v|v.to_str().ok()).unwrap_or("")});
    send(client, master, token, &head).await?;
    let mut sequence = 1u64;
    while let Some(bytes) = response
        .chunk()
        .await
        .map_err(|_| Error::new(503, "VM response interrupted."))?
    {
        for chunk in bytes.chunks(65536) {
            send(
                client,
                master,
                token,
                &json!({"id":id,"sequence":sequence,"data":STANDARD.encode(chunk)}),
            )
            .await?;
            sequence += 1;
        }
    }
    if let Some(attempt) = path
        .strip_prefix("/runs/")
        .and_then(|v| v.strip_suffix("/wait"))
    {
        super::executor::Executor::result(client, master, token, attempt).await?;
    }
    send(
        client,
        master,
        token,
        &json!({"id":id,"sequence":sequence,"done":true}),
    )
    .await
}
async fn send(
    client: &reqwest::Client,
    master: &url::Url,
    token: &str,
    value: &Value,
) -> Result<()> {
    let response = client
        .post(
            master
                .join("internal/nodes/reply")
                .map_err(Error::internal)?,
        )
        .bearer_auth(token)
        .json(value)
        .timeout(Duration::from_secs(25))
        .send()
        .await
        .map_err(|_| Error::new(503, "Master connection interrupted."))?;
    if !response.status().is_success() {
        return Err(Error::new(409, "Execution response rejected."));
    }
    Ok(())
}
