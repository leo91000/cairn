//! Distribution is authenticated by the installation origin and an approved binary digest.
use crate::{
    error::{Error, Result},
    http::{App, Input},
    validation::text,
};
use axum::{
    Json,
    body::Body,
    extract::{Request, State},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{io::AsyncReadExt, sync::OnceCell};

static BINARY: OnceCell<(String, u64)> = OnceCell::const_new();
static TOOLS: OnceCell<(String, u64)> = OnceCell::const_new();
/// The second tool bundle (Antigravity's `agy`), outside the first bundle's budget
/// that installed supervisors enforce. Images without it serve no extra tools.
static EXTRA_TOOLS: OnceCell<Option<(String, u64)>> = OnceCell::const_new();
const EXTRA_TOOLS_FILE: &str = "/opt/cairn-native-extra-tools.zip";
const EXTRA_TOOLS_PATH: &str = "/internal/nodes/native-extra-tools";

async fn artifact_digest(path: &str, maximum: u64) -> Result<(String, u64)> {
    let mut file = tokio::fs::File::open(path).await?;
    let size = file.metadata().await?.len();
    if size > maximum {
        return Err(Error::unavailable(
            "Native artifact exceeds its distribution size limit.",
        ));
    }
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 65536];
    loop {
        let count = file.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok((hex::encode(digest.finalize()), size))
}

pub async fn metadata() -> Result<Value> {
    let (digest, size) = BINARY
        .get_or_try_init(|| artifact_digest("/proc/self/exe", 256 * 1024 * 1024))
        .await?;
    let tools = TOOLS
        .get_or_try_init(|| artifact_digest("/opt/cairn-native-tools.zip", 512 * 1024 * 1024))
        .await?;
    let extra = EXTRA_TOOLS
        .get_or_try_init(|| async {
            if !tokio::fs::try_exists(EXTRA_TOOLS_FILE).await? {
                return Ok::<_, Error>(None);
            }
            artifact_digest(EXTRA_TOOLS_FILE, 256 * 1024 * 1024)
                .await
                .map(Some)
        })
        .await?;
    let mut release = json!({
        "sha256": digest, "size": size, "version": env!("CARGO_PKG_VERSION"),
        "os": "linux", "arch": "x86_64", "nativeHostProtocol": 2, "executionProtocol": 2,
        "runtimeId": std::env::var("APP_RUNTIME_ID").unwrap_or_else(|_| "development".into()),
        "binaryPath": "/internal/nodes/native-binary",
        "toolsPath": "/internal/nodes/native-tools", "toolsSha256": tools.0, "toolsSize": tools.1
    });
    // Older supervisors ignore these fields and install no Antigravity CLI.
    if let Some((digest, size)) = extra {
        release["extraToolsPath"] = EXTRA_TOOLS_PATH.into();
        release["extraToolsSha256"] = digest.as_str().into();
        release["extraToolsSize"] = (*size).into();
    }
    Ok(release)
}

pub async fn download(State(app): State<App>, request: Request) -> Result<Response> {
    let path = request.uri().path().to_owned();
    if request.method() != "POST" {
        return Err(Error::method_not_allowed("Use an authenticated POST."));
    }
    let input = Input::read(request).await?;
    let s = &app.service;
    if let Some(code) = input.body["code"].as_str() {
        let invitation = s
            .store
            .kv(&format!("node-enrollment:{}", crate::auth::digest(code)))
            .await?
            .unwrap_or_default();
        if code.len() != 43 || invitation["backend"] != "native" {
            return Err(Error::unauthorized("Invalid native enrollment code."));
        }
    } else {
        let node = crate::nodes::transport::authenticate(s, &input.headers).await?;
        if !super::policy::node_native(&s.node_boundary(&node).await?) {
            return Err(Error::forbidden(
                "This node is not enrolled for native execution.",
            ));
        }
    }
    let release = metadata().await?;
    if path.ends_with("native-release") {
        return Ok(Json(release).into_response());
    }
    let artifact = if path == EXTRA_TOOLS_PATH {
        if release["extraToolsSha256"].is_null() {
            return Err(Error::not_found("This release has no extra native tools."));
        }
        Some(("extraToolsSize", "extraToolsSha256", EXTRA_TOOLS_FILE))
    } else if path.ends_with("native-tools") {
        Some(("toolsSize", "toolsSha256", "/opt/cairn-native-tools.zip"))
    } else if path.ends_with("native-binary") {
        Some(("size", "sha256", "/proc/self/exe"))
    } else {
        None
    };
    if let Some((size, digest, file)) = artifact {
        let size = &release[size];
        let digest = text(&release, digest);
        let file = tokio::fs::File::open(file).await?;
        return Ok((
            [
                ("content-type", "application/octet-stream".to_owned()),
                ("content-length", size.to_string()),
                ("x-cairn-sha256", digest.to_owned()),
            ],
            Body::from_stream(tokio_util::io::ReaderStream::new(file)),
        )
            .into_response());
    }
    Err(Error::not_found("Unknown native release operation."))
}
