//! Durable references to jobs owned by one admitted environment. Commands and
//! credentials stay in the environment; only bounded completion evidence persists.
use super::checkpoint::RunCheckpoint;
use crate::background_jobs::{
    Outcome,
    protocol::{Event, Request},
};
use crate::{
    error::{Error, Result},
    service::Service,
    store::Db,
    validation::text,
};
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};

pub(crate) const SCHEMA: &str = "CREATE TABLE background_jobs(
 request_id TEXT PRIMARY KEY NOT NULL,
 run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
 attempt_id TEXT NOT NULL,
 job_id TEXT UNIQUE,
 payload_hash TEXT NOT NULL,
 outcome TEXT CHECK(outcome IS NULL OR json_valid(outcome))
) WITHOUT ROWID;
CREATE INDEX background_jobs_run ON background_jobs(run_id,attempt_id);";

pub(crate) async fn track(
    s: &Service,
    run: &Value,
    attempt: &str,
    request_id: &str,
    job_id: &str,
) -> Result<()> {
    let attempt = attempt.to_owned();
    let (run, request_id, job_id) = (
        text(run, "id").to_owned(),
        request_id.to_owned(),
        job_id.to_owned(),
    );
    s.store.transaction(move |db| {
        db.0.execute("UPDATE background_jobs SET job_id=? WHERE request_id=? AND run_id=? AND attempt_id=? AND (job_id IS NULL OR job_id=?)", rusqlite::params![job_id,request_id,run,attempt,job_id])?;
        let actual: String = db.0.query_row("SELECT job_id FROM background_jobs WHERE request_id=? AND run_id=? AND attempt_id=?", rusqlite::params![request_id,run,attempt], |row| row.get(0))?;
        if actual != job_id { return Err(Error::conflict("Conflicting job acknowledgement.")); }
        Ok(())
    }).await
}

pub(crate) async fn refused(s: &Service, run: &Value, attempt: &str, request: &str) -> Result<()> {
    let (id, attempt, request) = (
        text(run, "id").to_owned(),
        attempt.to_owned(),
        request.to_owned(),
    );
    s.store.write(move |db| {
        db.0.execute("UPDATE background_jobs SET outcome=? WHERE run_id=? AND attempt_id=? AND request_id=? AND job_id IS NULL AND outcome IS NULL",
            rusqlite::params![serde_json::to_string(&Outcome::Unknown { message: "Job admission was refused.".into() })?,id,attempt,request])?;
        Ok(())
    }).await
}

pub(crate) fn access(db: &Db<'_>, run: &Value, job: &str) -> Result<()> {
    crate::validation::uuid(job)?;
    let owns: bool = db.0.query_row(
        "SELECT EXISTS(SELECT 1 FROM background_jobs WHERE run_id=? AND job_id=?)",
        rusqlite::params![text(run, "id"), job],
        |row| row.get(0),
    )?;
    if !owns {
        return Err(Error::forbidden("Job does not belong to this run."));
    }
    Ok(())
}

pub(crate) fn result(
    db: &Db<'_>,
    run: &Value,
    job: &str,
) -> Result<Option<cairn_contracts::WaitObservation>> {
    use rusqlite::OptionalExtension;
    access(db, run, job)?;
    let value: Option<String> =
        db.0.query_row(
            "SELECT outcome FROM background_jobs WHERE run_id=? AND job_id=?",
            rusqlite::params![text(run, "id"), job],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let Some(value) = value else { return Ok(None) };
    let outcome: Outcome = serde_json::from_str(&value)?;
    let (state, message) = match outcome {
        Outcome::Running => return Ok(None),
        Outcome::Finished { .. } => ("completed", None),
        Outcome::Unknown { ref message } => ("unavailable", Some(message.clone())),
    };
    Ok(Some(cairn_contracts::WaitObservation {
        state: state.into(),
        kind: Some("job_finished".into()),
        message,
        job_id: Some(job.into()),
        job_result: Some(cairn_contracts::PluginWaitData(serde_json::from_value(
            serde_json::to_value(outcome)?,
        )?)),
        ..Default::default()
    }))
}

pub(crate) async fn refresh(s: &Service, run: &Value) -> Result<()> {
    refresh_redacted(s, run, &[]).await
}

pub(crate) async fn refresh_redacted(s: &Service, run: &Value, history: &[String]) -> Result<()> {
    let id = text(run, "id").to_owned();
    // Reconcile a lost start acknowledgement without launching a replacement.
    // A negative lookup can race the original request, so leave its reservation
    // pending until an authoritative acknowledgement or environment retirement.
    let pending_id = id.clone();
    let pending = s.store.read(move |db| {
        Ok(db.0.prepare("SELECT request_id,attempt_id FROM background_jobs WHERE run_id=? AND job_id IS NULL AND outcome IS NULL")?
            .query_map([pending_id], |row| Ok((row.get::<_, String>(0)?,row.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }).await?;
    for (call_id, attempt) in pending {
        let current = RunCheckpoint::load(&s.store, text(run, "id")).await?;
        if current.as_ref().and_then(RunCheckpoint::runner_id) != Some(attempt.as_str()) {
            let id = text(run, "id").to_owned();
            s.store
                .write(move |db| unknown(db, &id, &attempt, "The job's environment was replaced."))
                .await?;
            continue;
        }
        if let Event::JobLookup {
            job_id: Some(job), ..
        } = super::held::request_bound(
            s,
            run,
            Some(&attempt),
            &Request::LookupJob {
                request_id: crate::config::id(),
                call_id: call_id.clone(),
            },
        )
        .await?
        {
            track(s, run, &attempt, &call_id, &job).await?;
        }
    }
    let rows = s.store.read(move |db| {
        Ok(db.0.prepare("SELECT job_id,attempt_id FROM background_jobs WHERE run_id=? AND job_id IS NOT NULL AND (outcome IS NULL OR json_extract(outcome,'$.state')='running')")?
            .query_map([id], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }).await?;
    if rows.is_empty() {
        return Ok(());
    }
    let current = RunCheckpoint::load(&s.store, text(run, "id")).await?;
    let mut secrets = history.to_vec();
    let known = s
        .worker
        .job_redactions
        .lock()
        .await
        .get(text(run, "id"))
        .cloned();
    if let Some(account) = s.accounts.lease(s, text(run, "id")).await? {
        secrets.extend(s.accounts.redactions(s, &account).await?);
    }
    if s.worker
        .job_redactions
        .lock()
        .await
        .contains_key(text(run, "id"))
    {
        let projects = crate::service::run_projects(run);
        let credentials = match current
            .as_ref()
            .and_then(|saved| super::held::Observation::capture(run, saved))
        {
            Some(observation) => {
                crate::plugins::run_credentials::for_environment_observation(
                    s,
                    run,
                    &projects,
                    observation,
                )
                .await?
            }
            None => crate::plugins::run_credentials::for_execution(s, run, &projects).await?,
        };
        secrets.extend(credentials.secrets);
    } else {
        // A restarted manager no longer has the full rotating credential
        // history. It must not publish raw retained output as a safe summary.
        let run_id = text(run, "id").to_owned();
        if let Some(attempt) = current
            .as_ref()
            .and_then(RunCheckpoint::runner_id)
            .map(str::to_owned)
        {
            s.store.write(move |db| unknown(db,&run_id,&attempt,"Background supervision was interrupted; output cannot be safely reconstructed.")).await?;
        }
        return Ok(());
    }
    for (job, attempt) in rows {
        let outcome =
            if current.as_ref().and_then(RunCheckpoint::runner_id) != Some(attempt.as_str()) {
                Outcome::Unknown {
                    message: "The job's environment was replaced; completion is unknown.".into(),
                }
            } else {
                match super::held::request_bound(
                    s,
                    run,
                    Some(&attempt),
                    &Request::JobStatus {
                        request_id: crate::config::id(),
                        job_id: job.clone(),
                    },
                )
                .await?
                {
                    Event::JobStatus {
                        job_id, outcome, ..
                    } if job_id == job => outcome,
                    Event::Refused { message, .. } => Outcome::Unknown { message },
                    _ => return Err(Error::bad_gateway("Invalid job observation.")),
                }
            };
        let history = known
            .clone()
            .ok_or_else(|| Error::conflict("Job credential history was lost."))?;
        let secrets = secrets.clone();
        s.store.write(move |db| {
            let value = safe_outcome(outcome, &secrets, &history)?;
            db.0.execute("UPDATE background_jobs SET outcome=? WHERE job_id=? AND attempt_id=? AND (outcome IS NULL OR json_extract(outcome,'$.state')='running')", rusqlite::params![value,job,attempt])?;
            Ok(())
        }).await?;
    }
    Ok(())
}

fn safe_outcome(
    mut outcome: Outcome,
    secrets: &[String],
    history: &crate::accounts::output_redactions::History,
) -> Result<String> {
    if let Outcome::Finished { receipt } = &mut outcome
        && receipt.log_truncated
    {
        // Cutting raw bytes can remove the beginning of a credential. No
        // complete-string redactor can prove the retained fragment is safe.
        receipt.log_tail = "Log exceeded its limit; truncated output was omitted.".into();
    }
    let mut secrets = secrets.to_vec();
    secrets.extend(history.snapshot().iter().cloned());
    let value = crate::run_output::payload(&serde_json::to_value(outcome)?, &secrets);
    serde_json::to_string(&value).map_err(Error::from)
}

pub(crate) fn unknown(db: &Db<'_>, run: &str, attempt: &str, reason: &str) -> Result<()> {
    let outcome = serde_json::to_string(&json!({ "state": "unknown", "message": reason }))?;
    db.0.execute("UPDATE background_jobs SET outcome=? WHERE run_id=? AND attempt_id=? AND (outcome IS NULL OR json_extract(outcome,'$.state')='running')", rusqlite::params![outcome,run,attempt])?;
    Ok(())
}

/// Observe outside a store transaction: slow controller I/O never holds SQLite.
pub(crate) async fn refresh_all(s: &Service) -> Result<()> {
    let runs = s.store.read(|db| {
        Ok(db.0.prepare("SELECT DISTINCT run_id FROM background_jobs WHERE outcome IS NULL OR json_extract(outcome,'$.state')='running'")?
            .query_map([], |row| row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }).await?;
    stream::iter(runs).for_each_concurrent(8, |id| async move {
        if let Ok(run) = s.store.run(&id).await
            && let Err(error) = refresh(s, &run).await {
            tracing::debug!(run_id = id, error = %error, "Background job observation unavailable");
        }
    }).await;
    Ok(())
}

pub(crate) async fn reserve(
    s: &Service,
    bearer: &str,
    run: &Value,
    attempt: &str,
    request_id: &str,
    payload: &str,
) -> Result<()> {
    let attempt = attempt.to_owned();
    let (run_id, request_id, payload, bearer) = (
        text(run, "id").to_owned(),
        request_id.to_owned(),
        payload.to_owned(),
        bearer.to_owned(),
    );
    s.store.transaction(move |db| {
        let current = crate::project_workspaces::authorize_in(db,&bearer)?;
        if text(&current,"id") != run_id || RunCheckpoint::load_in(db,&run_id)?.as_ref().and_then(RunCheckpoint::runner_id) != Some(attempt.as_str()) {
            return Err(Error::conflict("Environment changed before job admission."));
        }
        let count: i64 = db.0.query_row("SELECT count(*) FROM background_jobs WHERE run_id=? AND attempt_id=? AND request_id!=?", rusqlite::params![run_id,attempt,request_id], |row| row.get(0))?;
        if count >= 64 { return Err(Error::too_many_requests("Environment job limit reached.")); }
        db.0.execute("INSERT INTO background_jobs(request_id,run_id,attempt_id,payload_hash) VALUES(?,?,?,?) ON CONFLICT(request_id) DO NOTHING", rusqlite::params![request_id,run_id,attempt,payload])?;
        let matches: bool = db.0.query_row("SELECT payload_hash=? AND attempt_id=? AND run_id=? FROM background_jobs WHERE request_id=?", rusqlite::params![payload,attempt,run_id,request_id], |row| row.get(0))?;
        if !matches { return Err(Error::conflict("This request previously started a different job.")); }
        Ok(())
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_publication_uses_current_history_and_omits_raw_truncated_fragments() {
        let history = crate::accounts::output_redactions::History::default();
        let before_request = history.snapshot();
        let secret = "synthetic-private-credential-34567890abcdef";
        history.extend([secret.into()]);
        let receipt = |text: &str, truncated| Outcome::Finished {
            receipt: crate::background_jobs::Receipt {
                exit_code: Some(0),
                signal: None,
                elapsed_ms: 1,
                stopped: false,
                timed_out: false,
                log_tail: text.into(),
                log_truncated: truncated,
            },
        };
        let value = safe_outcome(receipt(secret, false), &before_request, &history).unwrap();
        assert!(!value.contains(secret));
        assert!(value.contains("[redacted]"));
        let value =
            safe_outcome(receipt("34567890abcdef", true), &before_request, &history).unwrap();
        assert!(!value.contains("34567890abcdef"));
        assert!(value.contains("omitted"));
    }

    #[tokio::test]
    async fn completion_is_scoped_and_unknown_cannot_replace_finished_work() {
        use crate::run_fixture::DbRunFixture;
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store.transaction(|db| {
            db.add_run_fixture(&json!({ "id":"run", "taskId":"task", "status":"running" }),None)?;
            db.0.execute("INSERT INTO background_jobs VALUES('request','run','attempt','77d595dc-97b7-4c13-923d-e163baf49ea8','hash',?)",[serde_json::to_string(&Outcome::Finished { receipt: crate::background_jobs::Receipt { exit_code:Some(0),signal:None,elapsed_ms:1,stopped:false,timed_out:false,log_tail:"done".into(),log_truncated:false } })?])?;
            let run = json!({"id":"run"});
            assert!(access(db,&json!({"id":"another"}),"77d595dc-97b7-4c13-923d-e163baf49ea8").is_err());
            unknown(db,"run","attempt","lost")?;
            let observed = result(db,&run,"77d595dc-97b7-4c13-923d-e163baf49ea8")?.unwrap();
            assert_eq!(observed.state,"completed");
            assert_eq!(observed.kind.as_deref(),Some("job_finished"));
            Ok(())
        }).await.unwrap();
    }
}
