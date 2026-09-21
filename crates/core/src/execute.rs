//! The doors: every affirmative effect on core-owned state, and each one takes a
//! [`ConsentToken`] by value.
//!
//! There are five, one per kind of effect, and each opens only for the classes that are
//! its business: the native executor (shell commands and file writes), the settings writer
//! (anything that names a program, a credential destination, the workspace root or an
//! auto-approve rule), the run starter (an inline profile), the *allow* reply to a CLI's
//! approval request, and the bridge forward. A token presented at the wrong door is
//! refused. A *deny* reply is not an effect and takes no token; on a CLI backend one is
//! always sent, because a CLI left without an answer either hangs or runs the call (#42).
//!
//! The effects themselves are sinks the caller supplies, since the shell tool (#16), the
//! settings store (#6), the CLI transports (#46, #47) and the bridge (#44) do not exist
//! yet. What exists is the shape: no path to any of them without a token.

use std::path::Path;

use crate::consent::policy::Refusal;
use crate::consent::presenter::Rendered;
use crate::consent::request::{Class, ClassSpec, InlineProfile, Program, RequestSpec};
use crate::consent::token::{ConsentToken, Origin};
use crate::consent::Consent;

/// Where the native executor delivers an approved shell command.
pub trait ExecutionSink {
    /// `command` is byte-for-byte what the dialog showed, unescaped.
    fn run(&mut self, command: &str, cwd: &Path, env: &[(String, String)], origin: Origin);
    /// A write the gate has already performed under the workspace write lock.
    fn wrote(&mut self, path: &Path, origin: Origin);
}

/// The native backend's executor.
pub struct NativeExecutor;

impl NativeExecutor {
    /// Executes one approved request. Consumes the token.
    pub fn execute(
        &self,
        gate: &Consent,
        token: ConsentToken,
        sink: &mut dyn ExecutionSink,
    ) -> Result<(), Refusal> {
        let approved = gate.redeem(token)?;
        match approved.request().class() {
            Class::ShellCommand { command, cwd, env } => {
                sink.run(command, cwd, env, approved.origin());
                Ok(())
            }
            Class::FileWrite { path, .. } => {
                gate.write(&approved)?;
                sink.wrote(path, approved.origin());
                Ok(())
            }
            _ => Err(Refusal::WrongDoor),
        }
    }
}

/// A change to settings, as the store receives it. Old and new, both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsChange<'a> {
    McpServerEntry {
        name: &'a str,
        old: Option<&'a Program>,
        new: &'a Program,
    },
    CredentialProviderCommand {
        old: Option<&'a Program>,
        new: &'a Program,
    },
    GatewayUrl {
        old: Option<&'a str>,
        new: &'a str,
    },
    WorkspaceRoot {
        old: &'a Path,
        new: &'a Path,
    },
    AutoApproveRule {
        old: Option<&'a str>,
        new: &'a str,
    },
}

/// Where an approved settings change is stored.
pub trait SettingsStore {
    fn apply(&mut self, change: SettingsChange<'_>, origin: Origin);
}

/// The door for core-owned settings.
pub struct SettingsWriter;

impl SettingsWriter {
    pub fn apply(
        &self,
        gate: &Consent,
        token: ConsentToken,
        store: &mut dyn SettingsStore,
    ) -> Result<(), Refusal> {
        let approved = gate.redeem(token)?;
        let origin = approved.origin();
        let change = match approved.request().class() {
            Class::McpServerEntry { name, old, new } => SettingsChange::McpServerEntry {
                name,
                old: old.as_ref(),
                new,
            },
            Class::CredentialProviderCommand { old, new } => {
                SettingsChange::CredentialProviderCommand {
                    old: old.as_ref(),
                    new,
                }
            }
            Class::GatewayUrl { old, new } => SettingsChange::GatewayUrl {
                old: old.as_deref(),
                new,
            },
            Class::WorkspaceRoot { old, new } => SettingsChange::WorkspaceRoot { old, new },
            Class::AutoApproveRule { old, new } => SettingsChange::AutoApproveRule {
                old: old.as_deref(),
                new,
            },
            _ => return Err(Refusal::WrongDoor),
        };
        store.apply(change, origin);
        Ok(())
    }
}

/// Where an approved inline-profile run is started.
pub trait RunSink {
    fn start(&mut self, profile: &InlineProfile, origin: Origin);
}

/// The door for starting a run from anything other than a stored configuration selected
/// by reference.
pub struct RunStarter;

impl RunStarter {
    pub fn start(
        &self,
        gate: &Consent,
        token: ConsentToken,
        sink: &mut dyn RunSink,
    ) -> Result<(), Refusal> {
        let approved = gate.redeem(token)?;
        match approved.request().class() {
            Class::InlineProfileRun { profile } => {
                sink.start(profile, approved.origin());
                Ok(())
            }
            _ => Err(Refusal::WrongDoor),
        }
    }
}

/// The plain per-request reply to a CLI's approval request. There are no widening
/// variants here on purpose: the core never sends `acceptForSession` or a policy amendment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Allow {
        cli_request_id: String,
    },
    Deny {
        cli_request_id: String,
        reason: String,
    },
}

/// The channel back to the CLI.
pub trait ReplyTransport {
    fn send(&mut self, reply: Reply);
}

/// The door for a CLI backend's approval requests. On a CLI backend the *allow* reply is
/// the execution.
pub struct CliApproval;

impl CliApproval {
    /// Asks the gate and sends exactly one reply: *allow* if a token was minted, *deny*
    /// otherwise — on decline, on refusal, on presenter failure, on withdrawal. This is
    /// the entry a CLI transport calls; [`CliApproval::allow`] and [`CliApproval::deny`]
    /// are its halves.
    pub fn resolve(
        &self,
        gate: &Consent,
        spec: RequestSpec,
        transport: &mut dyn ReplyTransport,
    ) -> Result<(), Refusal> {
        self.resolve_observed(gate, spec, transport, &mut |_| {})
    }

    /// [`CliApproval::resolve`] with [`Consent::ask_observed`]'s observer: a backend that
    /// reports the request as pending before it is answered passes one here.
    pub fn resolve_observed(
        &self,
        gate: &Consent,
        spec: RequestSpec,
        transport: &mut dyn ReplyTransport,
        observer: &mut dyn FnMut(&Rendered),
    ) -> Result<(), Refusal> {
        let cli_request_id = match &spec.class {
            ClassSpec::CliCommand { cli_request_id, .. } => cli_request_id.clone(),
            _ => return Err(Refusal::WrongDoor),
        };
        // A token minted and then overtaken — its run ended, its request cancelled — is
        // refused at the door, and that refusal owes the CLI a deny as much as any other.
        let refusal = match gate.ask_observed(spec, observer) {
            Ok(token) => match self.allow(gate, token, transport) {
                Ok(()) => return Ok(()),
                Err(refusal) => refusal,
            },
            Err(refusal) => refusal,
        };
        self.deny(&cli_request_id, &refusal.to_string(), transport);
        Err(refusal)
    }

    /// Sends the *allow* reply. Consumes the token.
    pub fn allow(
        &self,
        gate: &Consent,
        token: ConsentToken,
        transport: &mut dyn ReplyTransport,
    ) -> Result<(), Refusal> {
        let approved = gate.redeem(token)?;
        let Some(id) = approved.request().cli_request_id() else {
            return Err(Refusal::WrongDoor);
        };
        transport.send(Reply::Allow {
            cli_request_id: id.to_string(),
        });
        Ok(())
    }

    /// Sends the *deny* reply. Needs no token; a denial is not an effect.
    pub fn deny(&self, cli_request_id: &str, reason: &str, transport: &mut dyn ReplyTransport) {
        transport.send(Reply::Deny {
            cli_request_id: cli_request_id.to_string(),
            reason: reason.to_string(),
        });
    }
}

/// Where a forwarded tool call goes (#44).
pub trait BridgeSink {
    fn forward(&mut self, tool: &str, arguments: &str, origin: Origin);
}

/// The door for the MCP bridge.
pub struct Bridge;

impl Bridge {
    pub fn forward(
        &self,
        gate: &Consent,
        token: ConsentToken,
        sink: &mut dyn BridgeSink,
    ) -> Result<(), Refusal> {
        let approved = gate.redeem(token)?;
        match approved.request().class() {
            Class::BridgeForward { tool, arguments } => {
                sink.forward(tool, arguments, approved.origin());
                Ok(())
            }
            _ => Err(Refusal::WrongDoor),
        }
    }
}
