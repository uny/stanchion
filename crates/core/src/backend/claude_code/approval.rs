//! The core's end of the approval path: the socket one attachment listens on, and what
//! is done with each request the helper relays over it.
//!
//! The CLI is started with `--permission-prompt-tool mcp__stanchion__approve` and an MCP
//! configuration naming the helper binary (`src/bin/stanchion-prompt-helper.rs`) with
//! this attachment's socket as its one argument. A call the CLI's own rules do not
//! already allow reaches the helper as a tool call, the helper connects here and writes
//! the call's arguments as one line, and this module answers with one line. The reply is
//! the CLI's to act on: *allow* is the execution, *deny* is a synthetic error result to
//! the model and an entry in the `result` line's `permission_denials`.
//!
//! # What was measured (`claude` 2.1.266, this module's own runs, signed in)
//!
//! - The arguments are `{tool_name, input, tool_use_id}`, and the id is the `tool_use`
//!   block's id from the `assistant` line — the same id `permission_denials` names — so
//!   the request is matched to its call exactly, not by comparing inputs.
//!   `permission_suggestions` did not appear on a `Bash` request.
//! - `{"behavior":"deny","message":…}` denies: the message is the model's tool result,
//!   `is_error: true`, and the call is in `permission_denials`. `{"behavior":"allow"}`
//!   runs the call as sent; `updatedInput` is not required.
//! - The CLI fails closed: a result that is not one of the two shapes, and a server that
//!   could not be spawned at all, both deny the call ("The permission prompt tool
//!   returned an invalid permission result") and list it in `permission_denials` — while
//!   `init` still reports the server as `connected`.
//! - A call the CLI's own rules allow — a user `allow` rule such as `Bash(echo:*)`, or
//!   its built-in safe list (`printf x` ran; `printf $(date)` asked) — never reaches the
//!   prompt tool. That is the call `Event::RanWithoutAsking` reports.
//! - The prompt tool is called under the user's `defaultMode: auto` too, but the backend
//!   passes `--permission-mode manual` rather than rely on it: which calls ask is not a
//!   setting the model can reach, and not one this backend inherits either.
//! - `--mcp-config` accepts the configuration as a JSON string on the command line, and
//!   `--strict-mcp-config` makes it the only MCP server the CLI loads.
//!
//! # Threads and locks
//!
//! One thread accepts connections for the life of the attachment; each request is
//! handled on a thread of its own, since the CLI may ask about several calls at once and
//! one dialog must not hold the socket against the next. A handler takes `emit` and then
//! `state`, as every emitter does, and holds neither while it waits on the gate. Once the
//! attachment has ended nothing is emitted: `Exited` stays the last event, and a request
//! still pending at that moment is withdrawn by the lease's end, answered *deny*, and
//! reported nowhere but the CLI.

use std::collections::HashSet;
use std::io::{self, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use super::json::Value;
use super::{create_private_dir, read_bounded_line, Shared};
use crate::backend::{AttachmentId, BackendError, Event, ToolCallId};
use crate::consent::presenter::Rendered;
use crate::consent::render::escape_inline;
use crate::consent::request::{ClassSpec, RequestSpec};
use crate::execute::{CliApproval, Reply, ReplyTransport};

/// The name the MCP configuration gives the helper, and so the prefix of the tool the
/// CLI is told to call.
pub(super) const SERVER_NAME: &str = "stanchion";
/// The tool as `--permission-prompt-tool` names it: `mcp__<server>__<tool>`.
pub(super) const PROMPT_TOOL: &str = "mcp__stanchion__approve";

/// The directory the per-attachment sockets live in. Created by the core with mode 0700
/// and resolved, like [`super::ConfigRoot`]. A socket path is short on every platform
/// (104 bytes on macOS), so this is the application's to place somewhere short, and
/// `start` fails rather than truncates when the path does not fit.
#[derive(Clone, Debug)]
pub struct SocketDir(PathBuf);

impl SocketDir {
    pub fn new(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        create_private_dir(path)?;
        Ok(SocketDir(path.canonicalize()?))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

/// One attachment's socket. Bound before the CLI is spawned, so the helper it names can
/// connect from the CLI's first request; closed and unlinked when the attachment ends.
pub(super) struct Listener {
    path: PathBuf,
    listener: UnixListener,
    closed: AtomicBool,
}

impl Listener {
    pub(super) fn bind(dir: &SocketDir, attachment: AttachmentId) -> Result<Self, BackendError> {
        let path = dir.0.join(format!("{}.sock", attachment.raw()));
        // A stale socket under this name is one a previous process of the core left; the
        // ids restart with the process, and nothing else creates files here.
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)
            .map_err(|e| BackendError::CannotStart(format!("bind {}: {e}", path.display())))?;
        Ok(Listener {
            path,
            listener,
            closed: AtomicBool::new(false),
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    /// Stops accepting and unlinks the socket. Idempotent. Wakes the accepting thread by
    /// connecting to it once, which is the one way to end a blocking `accept`.
    pub(super) fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = UnixStream::connect(&self.path);
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.close();
    }
}

/// The MCP configuration the CLI is given, as one JSON string for `--mcp-config`. Built
/// from the helper path the backend was constructed with and this attachment's socket —
/// never from a settings file, which is what keeps it outside the class rule
/// (`docs/decisions.md`).
pub(super) fn mcp_config(helper: &Path, socket: &Path) -> String {
    Value::Object(vec![(
        "mcpServers".into(),
        Value::Object(vec![(
            SERVER_NAME.into(),
            Value::Object(vec![
                ("type".into(), Value::String("stdio".into())),
                (
                    "command".into(),
                    Value::String(helper.to_string_lossy().into_owned()),
                ),
                (
                    "args".into(),
                    Value::Array(vec![Value::String(socket.to_string_lossy().into_owned())]),
                ),
            ]),
        )]),
    )])
    .to_json()
}

/// The calls a turn's approval requests named, by the CLI's id. Reconciled in
/// `on_result`: a call neither here nor in `permission_denials` ran without asking.
pub(super) type Asked = HashSet<String>;

/// Writes the reply as the one line the helper waits for.
struct SocketReply<'a> {
    stream: &'a UnixStream,
    error: Option<io::Error>,
}

impl ReplyTransport for SocketReply<'_> {
    fn send(&mut self, reply: Reply) {
        let value = match reply {
            Reply::Allow { .. } => {
                Value::Object(vec![("behavior".into(), Value::String("allow".into()))])
            }
            Reply::Deny { reason, .. } => deny_value(&format!("stanchion: {reason}")),
        };
        self.write(&value);
    }
}

impl SocketReply<'_> {
    fn write(&mut self, value: &Value) {
        let mut line = value.to_json();
        line.push('\n');
        let mut stream = self.stream;
        if let Err(e) = stream
            .write_all(line.as_bytes())
            .and_then(|()| stream.flush())
        {
            self.error = Some(e);
        }
    }
}

fn deny_value(message: &str) -> Value {
    Value::Object(vec![
        ("behavior".into(), Value::String("deny".into())),
        ("message".into(), Value::String(message.into())),
    ])
}

/// Why a request was denied without the gate being asked.
enum AtTheDoor {
    Malformed(&'static str),
    OutsideATurn,
    Ended,
    NotApprovable(String),
}

impl Shared {
    /// Accepts connections until the listener is closed. Runs on its own thread.
    pub(super) fn serve_approvals(self: Arc<Self>) {
        loop {
            let stream = match self.approval.listener.accept() {
                Ok((stream, _)) => stream,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            if self.approval.closed.load(Ordering::SeqCst) {
                break;
            }
            let shared = self.clone();
            thread::spawn(move || shared.handle_approval(stream));
        }
    }

    /// One request: read it, answer it, report it.
    fn handle_approval(&self, stream: UnixStream) {
        let mut reply = SocketReply {
            stream: &stream,
            error: None,
        };
        let line = match read_bounded_line(&mut BufReader::new(&stream)) {
            Some(Ok(line)) => line,
            Some(Err(over)) => {
                self.deny_at_the_door(&mut reply, AtTheDoor::Malformed("line too long"));
                self.diagnostic(format!("approval request dropped: {over}"));
                return;
            }
            None => {
                self.diagnostic("approval connection closed before a request".into());
                return;
            }
        };
        let text = String::from_utf8_lossy(&line);
        let request = match Value::parse(&text) {
            Ok(v) => v,
            Err(e) => {
                self.deny_at_the_door(&mut reply, AtTheDoor::Malformed("not JSON"));
                self.diagnostic(format!(
                    "approval request unparsable ({e}): {}",
                    escape_inline(&line)
                ));
                return;
            }
        };
        let name = request.get("tool_name").and_then(Value::as_str);
        let id = request.get("tool_use_id").and_then(Value::as_str);
        let (Some(name), Some(id)) = (name, id) else {
            self.deny_at_the_door(
                &mut reply,
                AtTheDoor::Malformed("tool_name or tool_use_id missing"),
            );
            self.diagnostic(format!(
                "approval request without tool_name or tool_use_id: {}",
                escape_inline(&line)
            ));
            return;
        };
        let call = ToolCallId(id.to_string());
        let shown_name = escape_inline(name.as_bytes());

        // The turn the call belongs to, and the record that it asked — taken before the
        // gate is involved, so `on_result` sees it whichever way the answer goes.
        let turn = {
            let mut state = self.state.lock().unwrap();
            if state.ended {
                self.deny_at_the_door(&mut reply, AtTheDoor::Ended);
                return;
            }
            match state.turn.as_mut() {
                Some(turn) => {
                    turn.asked.insert(id.to_string());
                    turn.id
                }
                None => {
                    drop(state);
                    self.deny_at_the_door(&mut reply, AtTheDoor::OutsideATurn);
                    self.diagnostic(format!(
                        "approval request for {shown_name} ({}) outside a turn",
                        escape_inline(id.as_bytes())
                    ));
                    return;
                }
            }
        };
        if request.get("permission_suggestions").is_some() {
            self.diagnostic(format!(
                "approval request for {shown_name} ({}) carried permission_suggestions; \
                 ignored — the reply never widens",
                escape_inline(id.as_bytes())
            ));
        }
        // Only a shell command has a door on the gate today (#50 decides the file tools);
        // any other tool is denied before the gate and said so, and it did not run.
        let command = if name == "Bash" {
            request
                .get("input")
                .and_then(|i| i.get("command"))
                .and_then(Value::as_str)
        } else {
            self.deny_at_the_door(&mut reply, AtTheDoor::NotApprovable(name.to_string()));
            self.diagnostic(format!(
                "approval request for {shown_name} ({}) denied at the door: only Bash \
                 reaches the gate in this slice",
                escape_inline(id.as_bytes())
            ));
            return;
        };
        let Some(command) = command else {
            self.deny_at_the_door(
                &mut reply,
                AtTheDoor::Malformed("Bash input without a command"),
            );
            self.diagnostic(format!(
                "approval request for Bash ({}) without a command",
                escape_inline(id.as_bytes())
            ));
            return;
        };
        let spec = RequestSpec {
            run: Some(self.lease.run()),
            workspace_root: self.workspace_root.clone(),
            class: ClassSpec::CliCommand {
                cli_request_id: id.to_string(),
                command: command.to_string(),
                cwd: self.workspace_root.clone(),
                // The reply never carries `updatedPermissions` or `updatedInput`, so
                // whatever the request offers, nothing beyond this one call is granted.
                session_grant: false,
            },
        };
        let mut invocation = None;
        let outcome = CliApproval.resolve_observed(
            self.lease.gate(),
            spec,
            &mut reply,
            &mut |rendered: &Rendered| {
                invocation = Some(rendered.invocation);
                self.emit_if_live(Event::ApprovalRequested {
                    turn,
                    call: call.clone(),
                    invocation: rendered.invocation,
                    rendered: rendered.clone(),
                });
            },
        );
        if let Some(e) = reply.error.take() {
            self.diagnostic(format!(
                "approval reply for {} could not be written: {e}",
                escape_inline(id.as_bytes())
            ));
        }
        match invocation {
            Some(invocation) => self.emit_if_live(Event::ApprovalResolved {
                turn,
                call,
                invocation,
                allowed: outcome.is_ok(),
            }),
            // Refused before a dialog: the CLI has its deny; the log has why.
            None => {
                if let Err(refusal) = outcome {
                    self.diagnostic(format!(
                        "approval request for {shown_name} ({}) refused: {refusal}",
                        escape_inline(id.as_bytes())
                    ));
                }
            }
        }
    }

    fn deny_at_the_door(&self, reply: &mut SocketReply<'_>, why: AtTheDoor) {
        let message = match why {
            AtTheDoor::Malformed(what) => format!("stanchion: malformed request: {what}"),
            AtTheDoor::OutsideATurn => "stanchion: no turn is open".into(),
            AtTheDoor::Ended => "stanchion: the attachment has ended".into(),
            AtTheDoor::NotApprovable(name) => {
                format!("stanchion: {name} cannot be approved through this backend yet")
            }
        };
        reply.write(&deny_value(&message));
    }

    /// Emits unless the attachment has ended, so `Exited` stays the last event.
    fn emit_if_live(&self, event: Event) {
        let _emit = self.emit.lock().unwrap();
        if self.state.lock().unwrap().ended {
            return;
        }
        self.events.event(event);
    }
}
