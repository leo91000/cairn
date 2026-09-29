//! Host/guest control messages carried by [`super::wire`] frames.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

/// How archive bytes follow an import request on its connection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Encoding {
    Binary,
    /// Base64 [`ArchiveFrame`]s, understood by every guest version.
    #[default]
    Json,
}

impl Encoding {
    pub fn of(binary: bool) -> Self {
        if binary { Self::Binary } else { Self::Json }
    }
}

/// A request sent by the host. Missing fields default the way older guests read them.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum GuestRequest {
    Status,
    Shutdown,
    Freeze,
    Thaw,
    #[serde(rename_all = "camelCase")]
    Clock {
        epoch_ms: i64,
    },
    ArtifactExport {
        #[serde(default)]
        path: String,
        #[serde(default)]
        root: PathBuf,
    },
    Import {
        #[serde(default)]
        target: String,
        #[serde(default)]
        replace: bool,
        #[serde(default)]
        encoding: Encoding,
    },
    #[serde(rename_all = "camelCase")]
    ProjectImport {
        #[serde(default)]
        target: String,
        #[serde(default)]
        read_only: bool,
        #[serde(default)]
        encoding: Encoding,
    },
    Run {
        plan: Value,
    },
}

/// Answer of the `status` operation.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GuestStatus {
    pub version: u32,
    pub binary_imports: bool,
    pub filesystem_snapshots: bool,
    pub initialized: bool,
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

/// JSON-encoded archive stream following an import request.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ArchiveFrame {
    Chunk {
        #[serde(default)]
        data: String,
    },
    End,
}

/// Run stream from the guest, also persisted line by line as the attempt log.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Event {
    Output {
        #[serde(default)]
        stderr: bool,
        /// Base64 process output.
        #[serde(default)]
        data: String,
    },
    Exit {
        #[serde(default)]
        code: Option<i64>,
        #[serde(default)]
        result: String,
    },
    Heartbeat,
    #[serde(other)]
    Unknown,
}
