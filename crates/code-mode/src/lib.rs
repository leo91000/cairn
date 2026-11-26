//! Isolated QuickJS execution worker and framed JSON IPC protocol for MCP code mode.
mod worker;

pub use worker::run;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_FRAME: usize = 8 * 1024 * 1024;
pub const MODEL_RESULT_LIMIT: usize = 1024 * 1024;

pub enum Completed {
    Value(Value),
    Truncated { value_bytes: usize },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Frame {
    Start {
        code: String,
        input: Value,
        names: Vec<String>,
    },
    Call {
        name: String,
        args: Value,
    },
    Reply {
        value: Value,
    },
    Denied {
        message: String,
        status: u16,
    },
    Done {
        value: Value,
    },
    Truncated {
        value_bytes: usize,
    },
    Failure {
        message: String,
    },
}
