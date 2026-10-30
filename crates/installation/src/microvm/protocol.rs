//! Host/guest control messages carried by [`super::wire`] frames.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

/// How archive bytes follow an import request on its connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Encoding {
    Binary,
}

/// A request sent by the host. Both components run the same executable.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum GuestRequest {
    Status,
    ReuseEntrypoint {
        digest: String,
    },
    WarmCodex,
    Shutdown,
    Freeze,
    Thaw,
    MountWorkspace,
    RestoreClone {
        identity: CloneIdentity,
    },
    #[serde(rename_all = "camelCase")]
    Clock {
        epoch_ms: i64,
    },
    ArtifactExport {
        path: String,
        root: PathBuf,
    },
    Import {
        target: String,
        replace: bool,
        encoding: Encoding,
        #[serde(default, rename = "traceId", skip_serializing_if = "Option::is_none")]
        trace_id: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    ProjectImport {
        target: String,
        read_only: bool,
        encoding: Encoding,
        #[serde(default, rename = "traceId", skip_serializing_if = "Option::is_none")]
        trace_id: Option<String>,
    },
    Run {
        plan: Value,
    },
}

/// Answer of the `status` operation.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GuestStatus {
    pub version: u32,
    pub executable_digest: String,
    pub initialized: bool,
    pub codex_ready: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloneIdentity {
    pub id: String,
    pub guest: String,
    pub gateway: String,
    pub mac: String,
    pub entropy: String,
}

/// Acknowledgement of an operation. Absent fields are never written.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Reply {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

impl Reply {
    pub fn ok(ok: bool) -> Self {
        Self {
            ok: Some(ok),
            ..Self::default()
        }
    }

    /// The guest is waiting for the project archive.
    pub fn ready() -> Self {
        Self {
            ready: Some(true),
            ..Self::default()
        }
    }

    /// An artifact of `size` bytes follows this reply.
    pub fn export(size: u64) -> Self {
        Self {
            ok: Some(true),
            size: Some(size),
            ..Self::default()
        }
    }

    pub fn succeeded(&self) -> bool {
        self.ok == Some(true)
    }

    pub fn is_ready(&self) -> bool {
        self.ready == Some(true)
    }
}

/// Run stream from the guest, also persisted line by line as the attempt log.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Event {
    Output {
        stderr: bool,
        /// Base64 process output.
        data: String,
    },
    Exit {
        code: Option<i64>,
        result: String,
    },
    FinalizationFailed,
    Heartbeat,
    #[serde(other)]
    Unknown,
}
