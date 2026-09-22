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
//! - A reply that took 90 seconds — a user looking at the dialog — was still acted on:
//!   the call ran; so did replies delayed 5, 15 and 30 minutes (three CLIs in parallel,
//!   each with its own helper and socket). A dialog queued behind another attachment's
//!   turn at the gate's one presentation slot therefore holds its CLI's request open for
//!   as long as the first takes, bounded by the user, not by the CLI, at least to 30
//!   minutes. Past that is not measured; if the CLI did give up, the call would be denied
//!   (fail-closed, as above) and the dialog's answer, when it came, would reach a call
//!   the CLI has closed.
//!
//! # What the request cannot say
//!
//! The rendered `cwd` is the workspace root the attachment was started in. The CLI's
//! `Bash` keeps a working directory across calls, so after a `cd` the command runs
//! elsewhere; the request carries no cwd, and the dialog says the one the core knows.
//! And [`Event::ToolCall`] comes from the reading thread while
//! [`Event::ApprovalRequested`] comes from a handler thread, so for one call the two may
//! arrive in either order; the contract promises none.
//!
//! # Threads and locks
//!
//! One thread accepts connections for the life of the attachment; each request is
//! handled on a thread of its own, since the CLI may ask about several calls at once and
//! one dialog must not hold the socket against the next. A handler takes `emit` and then
//! `state`, as every emitter does, and holds neither while it waits on the gate. Once the
//! attachment has ended nothing is emitted: `Exited` stays the last event, and a request
//! still pending at that moment is withdrawn by the lease's end, answered *deny*, and
//! reported nowhere but the CLI. A request still pending when its *turn* ends — an
//! interrupt cut the call, or the CLI closed it on its own — is cancelled by `on_result`,
//! and the deny it produces reaches a CLI that has already moved on. That cancel happens
//! under `emit`, and `TurnEnded` is delivered before the lock is released, so the
//! cancelled request's `ApprovalResolved` follows the `TurnEnded` of the turn it names.

use std::collections::HashSet;
use std::io::{self, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::json::Value;
use super::{create_private_dir, read_bounded_line, Helper, Shared};
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
/// How long an accepted connection has to send its request line.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// How much of a request that is not JSON the diagnostic quotes.
const UNPARSABLE_SHOWN: usize = 1024;

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
    /// Taken by the accepting thread, which owns the descriptor from then on and closes
    /// it when it exits — so a connection queued at the end is refused, not left hanging
    /// on a descriptor that lives as long as the attachment's last reference.
    listener: Mutex<Option<UnixListener>>,
    closed: AtomicBool,
    /// Requests being handled right now. The helper is the one expected client, and it
    /// asks once per call; the bound is against a process on the user's side of the
    /// socket directory that is not the helper.
    in_flight: AtomicUsize,
}

/// The most requests handled at once; past it a request is denied at the door.
const MAX_IN_FLIGHT: usize = 16;

impl Listener {
    pub(super) fn bind(dir: &SocketDir, attachment: AttachmentId) -> Result<Self, BackendError> {
        // Named by process and attachment: two processes of the core sharing a directory
        // — two instances of the application, or a test run beside one — must not unlink
        // each other's live socket. A file already under this name is a stale one that a
        // dead process with this pid left; nothing else creates files here.
        let path = dir
            .0
            .join(format!("{}-{}.sock", std::process::id(), attachment.raw()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)
            .map_err(|e| BackendError::CannotStart(format!("bind {}: {e}", path.display())))?;
        Ok(Listener {
            path,
            listener: Mutex::new(Some(listener)),
            closed: AtomicBool::new(false),
            in_flight: AtomicUsize::new(0),
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    /// Stops accepting and unlinks the socket. Idempotent. Wakes the accepting thread by
    /// connecting to it once, which is the one way to end a blocking `accept`; that
    /// thread then drains what else is queued, answers it *deny*, and closes the
    /// descriptor.
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
/// from the helper command the backend was constructed with and this attachment's socket,
/// which goes last on its command line — never from a settings file, which is what keeps
/// it outside the class rule (`docs/decisions.md`).
pub(super) fn mcp_config(helper: &Helper, socket: &Path) -> String {
    let args = helper
        .args()
        .iter()
        .map(|a| Value::String(a.clone()))
        .chain([Value::String(socket.to_string_lossy().into_owned())])
        .collect();
    Value::Object(vec![(
        "mcpServers".into(),
        Value::Object(vec![(
            SERVER_NAME.into(),
            Value::Object(vec![
                ("type".into(), Value::String("stdio".into())),
                (
                    "command".into(),
                    Value::String(helper.program().to_string_lossy().into_owned()),
                ),
                ("args".into(), Value::Array(args)),
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
    TooMany,
    NotApprovable(String),
}

impl Shared {
    /// Accepts connections until the listener is closed, then answers what is still
    /// queued and closes the descriptor. Runs on its own thread.
    pub(super) fn serve_approvals(self: Arc<Self>) {
        let Some(listener) = self.approval.listener.lock().unwrap().take() else {
            return;
        };
        loop {
            let stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    // From here every helper connection is refused, which the helper
                    // turns into a deny; the core says so once rather than not at all.
                    self.diagnostic_if_live(format!("approval socket stopped accepting: {e}"));
                    break;
                }
            };
            if self.approval.closed.load(Ordering::SeqCst) {
                // The wake-up, or a request that arrived beside it; either way the
                // attachment is over. What is queued behind it gets the same answer.
                drop(stream);
                if listener.set_nonblocking(true).is_ok() {
                    while let Ok((stream, _)) = listener.accept() {
                        let mut reply = SocketReply {
                            stream: &stream,
                            error: None,
                        };
                        self.deny_at_the_door(&mut reply, AtTheDoor::Ended);
                    }
                }
                break;
            }
            if self.approval.in_flight.fetch_add(1, Ordering::SeqCst) >= MAX_IN_FLIGHT {
                self.approval.in_flight.fetch_sub(1, Ordering::SeqCst);
                let mut reply = SocketReply {
                    stream: &stream,
                    error: None,
                };
                self.deny_at_the_door(&mut reply, AtTheDoor::TooMany);
                continue;
            }
            let shared = self.clone();
            thread::spawn(move || {
                shared.handle_approval(stream);
                shared.approval.in_flight.fetch_sub(1, Ordering::SeqCst);
            });
        }
    }

    /// One request: read it, answer it, report it.
    fn handle_approval(&self, stream: UnixStream) {
        // The helper writes its line as soon as it has connected; a connection that
        // sends nothing must not pin this thread, and the attachment with it, for good.
        let _ = stream.set_read_timeout(Some(REQUEST_TIMEOUT));
        let mut reply = SocketReply {
            stream: &stream,
            error: None,
        };
        let line = match read_bounded_line(&mut BufReader::new(&stream)) {
            Some(Ok(line)) => line,
            Some(Err(over)) => {
                self.deny_at_the_door(&mut reply, AtTheDoor::Malformed("line too long"));
                self.diagnostic_if_live(format!("approval request dropped: {over}"));
                return;
            }
            None => {
                self.diagnostic_if_live("approval connection closed before a request".into());
                return;
            }
        };
        let text = String::from_utf8_lossy(&line);
        let request = match Value::parse(&text) {
            Ok(v) => v,
            Err(e) => {
                self.deny_at_the_door(&mut reply, AtTheDoor::Malformed("not JSON"));
                // The line may be anything up to the bound; the log gets its head.
                let shown = line.len().min(UNPARSABLE_SHOWN);
                self.diagnostic_if_live(format!(
                    "approval request unparsable ({e}): {}{}",
                    escape_inline(&line[..shown]),
                    if shown < line.len() { "…" } else { "" }
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
            self.diagnostic_if_live(format!(
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
                    self.diagnostic_if_live(format!(
                        "approval request for {shown_name} ({}) outside a turn",
                        escape_inline(id.as_bytes())
                    ));
                    return;
                }
            }
        };
        if request.get("permission_suggestions").is_some() {
            self.diagnostic_if_live(format!(
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
            self.diagnostic_if_live(format!(
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
            self.diagnostic_if_live(format!(
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
                // Under `emit`, so this is atomic with the turn's end: `on_result` takes
                // the turn and delivers `TurnEnded` under the same lock. While the turn
                // is still open the invocation is recorded on it, so that its end cancels
                // the dialog; if it ended in the meantime — the CLI closed the call
                // before the gate got to it — the request is withdrawn here, and no
                // approval event names it: `invocation` stays `None`, so the refusal
                // below is logged, not resolved.
                let _emit = self.emit.lock().unwrap();
                let live = {
                    let mut state = self.state.lock().unwrap();
                    let ended = state.ended;
                    match state.turn.as_mut() {
                        Some(open) if !ended && open.id == turn => {
                            open.pending.push(rendered.invocation);
                            true
                        }
                        _ => false,
                    }
                };
                if !live {
                    self.lease.gate().cancel(rendered.invocation);
                    return;
                }
                invocation = Some(rendered.invocation);
                self.events.event(Event::ApprovalRequested {
                    turn,
                    call: call.clone(),
                    invocation: rendered.invocation,
                    rendered: rendered.clone(),
                });
            },
        );
        if let Some(invocation) = invocation {
            if let Some(open) = self.state.lock().unwrap().turn.as_mut() {
                open.pending.retain(|i| *i != invocation);
            }
        }
        // An allow the CLI never read is no execution: the helper gave up on the socket,
        // the CLI denied the call (fail-closed), and the token stays in the record alone.
        let delivered = match reply.error.take() {
            Some(e) => {
                self.diagnostic_if_live(format!(
                    "approval reply for {} could not be written: {e}",
                    escape_inline(id.as_bytes())
                ));
                false
            }
            None => true,
        };
        match invocation {
            Some(invocation) => self.emit_if_live(Event::ApprovalResolved {
                turn,
                call,
                invocation,
                allowed: outcome.is_ok() && delivered,
            }),
            // Refused before a dialog: the CLI has its deny; the log has why.
            None => {
                if let Err(refusal) = outcome {
                    self.diagnostic_if_live(format!(
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
            AtTheDoor::TooMany => "stanchion: too many requests at once".into(),
            AtTheDoor::NotApprovable(name) => {
                format!("stanchion: {name} cannot be approved through this backend yet")
            }
        };
        reply.write(&deny_value(&message));
    }

    fn diagnostic_if_live(&self, text: String) {
        self.emit_if_live(Event::Diagnostic { text });
    }

    /// Emits unless the attachment has ended, so `Exited` stays the last event. Every
    /// emission from a handler thread goes through here: the reader's `finish` has no
    /// handler to join, and a request the run's end withdrew resolves after `Exited`.
    fn emit_if_live(&self, event: Event) {
        let _emit = self.emit.lock().unwrap();
        if self.state.lock().unwrap().ended {
            return;
        }
        self.events.event(event);
    }
}
