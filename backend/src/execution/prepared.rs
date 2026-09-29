//! Execution descriptor saved with a run checkpoint (`prepared`).
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;

/// How a project's working files were provided to a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceKind {
    /// The project directory itself.
    Direct,
    /// A plain copy of a directory without Git history.
    Copy,
    /// An independent Git clone.
    Clone,
    /// A linked Git worktree of the project repository.
    Worktree,
}

impl WorkspaceKind {
    pub fn has_git(self) -> bool {
        matches!(self, Self::Clone | Self::Worktree)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Firecracker,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Firecracker => "firecracker",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub project_id: String,
    pub path: PathBuf,
    pub kind: WorkspaceKind,
    /// Starting commit; `null` for copies and when Git could not report it.
    pub revision: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Mount {
    pub source: PathBuf,
    pub target: PathBuf,
    pub read_only: bool,
}

impl Mount {
    pub fn new(source: impl Into<PathBuf>, target: impl Into<PathBuf>, read_only: bool) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
            read_only,
        }
    }

    /// Exposes a host path at the same location inside the sandbox.
    pub fn same(path: impl Into<PathBuf>, read_only: bool) -> Self {
        let path = path.into();
        Self::new(path.clone(), path, read_only)
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Prepared {
    /// Parent of every workspace; only isolated executions have one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_root: Option<PathBuf>,
    pub cwd: PathBuf,
    pub output: PathBuf,
    pub workspaces: Vec<Workspace>,
    pub isolated: bool,
    pub mounts: Vec<Mount>,
    /// Skill snapshot, with guest paths once installed in an isolated home.
    pub skills: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<Backend>,
}
