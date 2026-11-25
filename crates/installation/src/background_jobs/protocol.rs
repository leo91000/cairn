//! Private environment control, never accepted from provider stdout.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Outcome;

pub(crate) const CONTROL_INTERRUPTED: &str = "environment_control_interrupted";
pub(crate) const INVALID_CONTROL_FRAME: &str = "environment_invalid_control_frame";

pub(crate) fn invalid_control_frame() -> crate::error::Error {
    crate::error::Error::coded(
        502,
        INVALID_CONTROL_FRAME,
        "Execution environment sent an invalid control frame.",
    )
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Request {
    StartJob {
        request_id: String,
        name: String,
        command: String,
    },
    JobStatus {
        request_id: String,
        job_id: String,
    },
    Activity {
        request_id: String,
    },
    LookupJob {
        request_id: String,
        call_id: String,
    },
    /// A fresh provider plan for the same live environment. The receiver checks
    /// its immutable run, attempt, mounts and privilege policy before accepting.
    ResumeTurn {
        request_id: String,
        turn_id: String,
        plan: Value,
    },
    Close {
        request_id: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "event",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Event {
    Ready {
        attempt_id: String,
        generation: String,
    },
    Output {
        turn_id: String,
        stderr: bool,
        data: String,
    },
    TurnStarted {
        turn_id: String,
    },
    TurnFinished {
        turn_id: String,
        code: i32,
    },
    TurnFailed {
        turn_id: String,
        message: String,
    },
    JobStarted {
        request_id: String,
        job_id: String,
    },
    JobStatus {
        request_id: String,
        job_id: String,
        outcome: Outcome,
    },
    JobLookup {
        request_id: String,
        job_id: Option<String>,
    },
    Activity {
        request_id: String,
        jobs: bool,
        containers: bool,
    },
    JobChanged {
        job_id: String,
        outcome: Outcome,
    },
    Accepted {
        request_id: String,
    },
    Refused {
        request_id: String,
        message: String,
    },
}
