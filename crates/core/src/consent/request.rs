//! The request a consent token is bound to.
//!
//! A [`Request`] is built by the gate and by nothing else: the invocation id is core-issued,
//! the run is one the gate registered, a relative path is resolved against the workspace
//! root, and for a write the content and the state of the file it replaces are snapshotted
//! at build time and the target is confined to the workspace. The caller supplies
//! a [`RequestSpec`], which carries only what the model or the user chose; the fields that
//! make the request unforgeable are the gate's to add. See `docs/decisions.md`, "Consent is
//! a native dialog the core owns".

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest as _, Sha256};

/// Core-issued identity of one request. Unique for the life of the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InvocationId(pub(crate) u64);

impl fmt::Display for InvocationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invocation {}", self.0)
    }
}

/// Core-issued identity of one run, handed out by [`super::Consent::register_run`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RunId(pub(crate) u64);

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "run {}", self.0)
    }
}

/// Which backend a run is on. Every dialog names it, in a label the core generates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Backend {
    Native,
    ClaudeCode,
    Codex,
}

impl Backend {
    pub fn label(self) -> &'static str {
        match self {
            Backend::Native => "native",
            Backend::ClaudeCode => "claude-code",
            Backend::Codex => "codex",
        }
    }
}

/// What a request belongs to: a run, or the application when it has none (a settings write,
/// a workspace-root move). A token dies with whichever it is bound to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Binding {
    Run { id: RunId, backend: Backend },
    Application,
}

/// SHA-256 of some bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha256Digest(pub [u8; 32]);

impl Sha256Digest {
    pub fn of(bytes: &[u8]) -> Self {
        Sha256Digest(Sha256::digest(bytes).into())
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:")?;
        for b in &self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

/// The state of a write's target when the request was built. Verified again, under the
/// workspace write lock, immediately before the write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prior {
    /// A regular file (not a symlink) with this content.
    File(Sha256Digest),
    /// Nothing at the path; the write creates the file.
    Absent,
}

/// A program the core will run: an MCP stdio server, a credential provider command. The
/// `env` is part of the value because the model may have supplied it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// A run configuration supplied inline rather than selected by reference. Base URL and all,
/// since the URL is where a credential is sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineProfile {
    pub name: String,
    pub base_url: String,
    pub model: String,
}

/// The classes of request, one variant per kind of core-owned state.
///
/// Every variant carries the whole of what will run or be stored: for settings the old
/// value as well as the new one, for a write the bytes themselves and not only their hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Class {
    /// A shell command on the native backend. `command` is the exact string the executor
    /// receives. `cwd` and `env` are policy, not something the model chose: bound into the
    /// token, stated once in settings, not rendered on every dialog.
    ShellCommand {
        command: String,
        cwd: PathBuf,
        env: Vec<(String, String)>,
    },
    /// A file write on the native backend. `path` is what the caller asked for, resolved
    /// against the workspace root; `parent` is the canonical form of its directory at build
    /// time, so a path that later resolves elsewhere voids the token.
    FileWrite {
        path: PathBuf,
        parent: PathBuf,
        content: Arc<[u8]>,
        content_hash: Sha256Digest,
        prior: Prior,
    },
    McpServerEntry {
        name: String,
        old: Option<Program>,
        new: Program,
    },
    CredentialProviderCommand {
        old: Option<Program>,
        new: Program,
    },
    GatewayUrl {
        old: Option<String>,
        new: String,
    },
    WorkspaceRoot {
        old: PathBuf,
        new: PathBuf,
    },
    AutoApproveRule {
        old: Option<String>,
        new: String,
    },
    InlineProfileRun {
        profile: InlineProfile,
    },
    /// An approval request a CLI backend delegated to the core, shown as the CLI supplied it
    /// and nothing more. The *allow* reply is the execution on that backend.
    CliCommand {
        cli_request_id: String,
        command: String,
        cwd: PathBuf,
    },
    /// A tool call a CLI-backed run forwards through the bridge to a tool the core polices
    /// (#44).
    BridgeForward {
        tool: String,
        arguments: String,
    },
}

impl Class {
    /// A short label for the dialog title. Core-generated, never model text.
    pub fn label(&self) -> &'static str {
        match self {
            Class::ShellCommand { .. } => "run a shell command",
            Class::FileWrite { .. } => "write a file",
            Class::McpServerEntry { .. } => "change an MCP server entry",
            Class::CredentialProviderCommand { .. } => "change the credential provider command",
            Class::GatewayUrl { .. } => "change the gateway URL",
            Class::WorkspaceRoot { .. } => "move the workspace root",
            Class::AutoApproveRule { .. } => "change an auto-approve rule",
            Class::InlineProfileRun { .. } => "start a run from an inline profile",
            Class::CliCommand { .. } => "allow a command the CLI asked about",
            Class::BridgeForward { .. } => "forward a tool call through the bridge",
        }
    }
}

/// What a caller may supply. Everything that makes the request trustworthy — the id, the
/// backend, resolution, snapshots — is added by the gate.
#[derive(Clone, Debug)]
pub struct RequestSpec {
    pub run: Option<RunId>,
    pub workspace_root: PathBuf,
    pub class: ClassSpec,
}

/// The caller's half of a [`Class`].
#[derive(Clone, Debug)]
pub enum ClassSpec {
    ShellCommand {
        command: String,
        cwd: PathBuf,
        env: Vec<(String, String)>,
    },
    /// `path` may be relative to the workspace root. The gate snapshots the target.
    FileWrite {
        path: PathBuf,
        content: Vec<u8>,
    },
    McpServerEntry {
        name: String,
        old: Option<Program>,
        new: Program,
    },
    CredentialProviderCommand {
        old: Option<Program>,
        new: Program,
    },
    GatewayUrl {
        old: Option<String>,
        new: String,
    },
    WorkspaceRoot {
        old: PathBuf,
        new: PathBuf,
    },
    AutoApproveRule {
        old: Option<String>,
        new: String,
    },
    InlineProfileRun {
        profile: InlineProfile,
    },
    /// `session_grant` is set when the CLI's request carries a widening — a `grantRoot`, a
    /// permission profile, anything beyond the one operation. Such a request is refused
    /// before any dialog opens.
    CliCommand {
        cli_request_id: String,
        command: String,
        cwd: PathBuf,
        session_grant: bool,
    },
    BridgeForward {
        tool: String,
        arguments: String,
    },
}

/// The immutable value a token is bound to. Only the gate constructs one.
#[derive(Debug)]
pub struct Request {
    pub(crate) invocation: InvocationId,
    pub(crate) binding: Binding,
    pub(crate) workspace_root: PathBuf,
    pub(crate) class: Class,
}

impl Request {
    pub fn invocation(&self) -> InvocationId {
        self.invocation
    }

    pub fn binding(&self) -> Binding {
        self.binding
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn class(&self) -> &Class {
        &self.class
    }

    /// The CLI's own id for a delegated request, which its reply must carry.
    pub fn cli_request_id(&self) -> Option<&str> {
        match &self.class {
            Class::CliCommand { cli_request_id, .. } => Some(cli_request_id),
            _ => None,
        }
    }
}
