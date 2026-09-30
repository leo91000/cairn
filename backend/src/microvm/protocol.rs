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
        #[serde(default, rename = "traceId", skip_serializing_if = "Option::is_none")]
        trace_id: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    ProjectImport {
        #[serde(default)]
        target: String,
        #[serde(default)]
        read_only: bool,
        #[serde(default)]
        encoding: Encoding,
        #[serde(default, rename = "traceId", skip_serializing_if = "Option::is_none")]
        trace_id: Option<String>,
    },
    Run {
        plan: Value,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn imports_accept_legacy_requests_and_optional_trace_ids() {
        for operation in ["import", "project-import"] {
            let mut message = json!({
                "op": operation,
                "target": "/run/example",
                "encoding": "binary",
            });
            let legacy: GuestRequest = serde_json::from_value(message.clone()).unwrap();
            assert!(
                serde_json::to_value(legacy)
                    .unwrap()
                    .get("traceId")
                    .is_none()
            );

            message["traceId"] = json!("44f35af1-a266-44bc-9659-bfb8ffb040a3");
            let traced: GuestRequest = serde_json::from_value(message).unwrap();
            assert_eq!(
                serde_json::to_value(traced).unwrap()["traceId"],
                "44f35af1-a266-44bc-9659-bfb8ffb040a3"
            );
        }
    }

    #[test]
    fn traced_imports_remain_readable_by_legacy_guests() {
        #[derive(Deserialize)]
        struct LegacyImport {
            target: String,
            #[serde(default)]
            encoding: Encoding,
        }

        for request in [
            GuestRequest::Import {
                target: "/run/example".into(),
                replace: false,
                encoding: Encoding::Binary,
                trace_id: Some("44f35af1-a266-44bc-9659-bfb8ffb040a3".into()),
            },
            GuestRequest::ProjectImport {
                target: "/run/example".into(),
                read_only: true,
                encoding: Encoding::Binary,
                trace_id: Some("44f35af1-a266-44bc-9659-bfb8ffb040a3".into()),
            },
        ] {
            let legacy: LegacyImport =
                serde_json::from_value(serde_json::to_value(request).unwrap()).unwrap();
            assert_eq!(legacy.target, "/run/example");
            assert_eq!(legacy.encoding, Encoding::Binary);
        }
    }
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
