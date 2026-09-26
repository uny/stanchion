//! The run backend contract: what the core asks of the thing that drives a run, stated so
//! that a run on the core's own loop and a run on a vendor CLI are the same kind of value
//! to everything above them.
//!
//! `docs/architecture.md`, "Run backends", carries the contract and the four lifetimes; this
//! module is it in types. Two backends implement it — `native` (the loop in this crate,
//! M3) and `cli` (Claude Code in #46, Codex in #47) — and the code above them holds a
//! [`RunBackend`] and a [`Session`] and never branches on [`Backend`]: `tests` drives two
//! independent fakes through one function to keep that so. The traits are sealed: the
//! backends are built in and static, not a plugin surface.
//!
//! # What the trait does not carry
//!
//! **An approval decision, or a way to reach one.** A backend receives the consent gate at
//! [`Start`] and asks it itself — on a CLI backend through
//! [`crate::execute::CliApproval::resolve`], which owes the CLI exactly one reply. Upward
//! it emits [`Event::ApprovalRequested`] for display, and nothing on [`Session`] takes an
//! answer or hands out the consent run id, so the code above — the shell, the WebView
//! behind it — has no handle on a session by which to approve, or by which to ask the
//! gate under the backend's run. `crates/core/src/lib.rs` shows the shape of the method
//! that does not exist in a `compile_fail` doctest beside the token ones (a doctest can
//! pin one name, not the absence of a capability; the trait is the pin); the run id is
//! held by the [`Attachment`] lease, which only this crate constructs. What this does
//! *not* close: [`Consent::register_run`] and [`Consent::ask`] are `pub` for the
//! integration tests, so a holder of the gate can still open a run of its own and ask
//! under it. Narrowing them to the crate is #54, which moves those tests in-crate.
//!
//! **A runtime.** Events reach the caller through an [`EventSink`] it supplies, on whatever
//! thread the backend delivers from, as the presenter does for consent. Whether a backend
//! drives its subprocess from a thread or an executor is that backend's business, decided
//! when it is written.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::consent::presenter::Rendered;
use crate::consent::request::{InvocationId, RunId};
use crate::consent::Consent;

pub use crate::consent::request::Backend;

pub mod claude_code;
#[cfg(test)]
mod tests;

mod sealed {
    pub trait Sealed {}
}

// ---------------------------------------------------------------------------------------
// The four lifetimes

/// The conversation as the GUI holds it. Core-issued, and the longest-lived of the four: it
/// outlives every session, turn and attachment under it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConversationId(pub u64);

/// The account a session was created under (#45). Opaque here: what it authenticates as,
/// where requests go and who owns the credential are the account model's, not the
/// backend's. On a CLI backend it names a per-account config directory the core created
/// and never reads (#41).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AccountId(pub String);

/// A backend's own identifier for a session, as the core stored it, with the account and
/// the workspace it was created under. The three travel together on purpose:
/// [`RunBackend::resume`] takes a `SessionId` and nothing else that names an account or a
/// workspace, so a stored session cannot be reattached under a different one — there is
/// nowhere at the call site to say so. Only a backend constructs one, from what the
/// backend reported; the code above stores and returns it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SessionId {
    backend: Backend,
    account: AccountId,
    workspace_root: PathBuf,
    value: String,
}

impl SessionId {
    pub(crate) fn new(
        backend: Backend,
        account: AccountId,
        workspace_root: PathBuf,
        value: impl Into<String>,
    ) -> Self {
        SessionId {
            backend,
            account,
            workspace_root,
            value: value.into(),
        }
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    pub fn account(&self) -> &AccountId {
        &self.account
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// The backend's identifier, verbatim. Never "the latest": a resume names this.
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for SessionId {
    /// The value is the backend's, verbatim, so it is escaped here as every other
    /// backend-supplied string on a labelled line is: it cannot end the line.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} session {}",
            self.backend.label(),
            crate::consent::render::escape_inline(self.value.as_bytes())
        )
    }
}

/// One turn: from an input the backend accepted — or, on a backend that starts turns of its
/// own, the point it started one ([`TurnOrigin::Backend`]) — to the point the backend
/// reports the model has stopped, however it stopped. Core-issued from one counter for the process, so a turn
/// id is unique across every attachment and conversation, not merely within one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TurnId(u64);

impl TurnId {
    /// The number, for a UI that keys its rendering on it.
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// One attachment of the core to a session: on a CLI backend one supervised process, on the
/// native backend one instance of the loop. Core-issued. A session may see several — each
/// resume is a new one — and a turn an attachment cut is a turn the next one did not
/// finish.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AttachmentId(u64);

impl AttachmentId {
    /// The number, for a backend that names a per-attachment resource after it, or a UI
    /// that keys its rendering on it.
    pub fn raw(self) -> u64 {
        self.0
    }
}

// The lease lives in its own module so that `RunEnded` has exactly one constructor:
// `Attachment::end`. A backend in a sibling module cannot build one.
mod lease {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Once};

    use super::{AttachmentId, Backend, Consent, RunId, TurnId};

    // Process-wide counters for identity only — never configuration, which the
    // run-is-a-value entry keeps out of process-wide state. The consent gate numbers its
    // runs per gate; these number turns and attachments per process, so the scopes differ
    // and neither reads the other.
    static NEXT_TURN: AtomicU64 = AtomicU64::new(1);
    static NEXT_ATTACHMENT: AtomicU64 = AtomicU64::new(1);

    /// The lease a backend holds on the consent gate for one attachment. Registers the
    /// consent run when opened and ends it when dropped, so the run cannot outlive the
    /// session value that holds it. Before that, the run ends when the attachment does:
    /// [`Event::Exited`](super::Event::Exited) carries a [`RunEnded`] that only
    /// `Attachment::end` produces, so a backend cannot report the attachment over — on
    /// `terminate`, on a crash, on the process finishing — without having ended the run
    /// first. The run id is not exposed: a backend asks the gate through
    /// `Attachment::run` from inside this crate.
    pub struct Attachment {
        id: AttachmentId,
        run: RunId,
        gate: Arc<Consent>,
        ended: Once,
    }

    impl Attachment {
        pub(crate) fn open(gate: Arc<Consent>, backend: Backend) -> Self {
            Attachment {
                id: AttachmentId(NEXT_ATTACHMENT.fetch_add(1, Ordering::SeqCst)),
                run: gate.register_run(backend),
                gate,
                ended: Once::new(),
            }
        }

        pub fn id(&self) -> AttachmentId {
            self.id
        }

        /// The consent run to bind requests to. Crate-private on purpose: a backend asks
        /// under it; nothing above a backend learns it.
        pub(crate) fn run(&self) -> RunId {
            self.run
        }

        pub(crate) fn gate(&self) -> &Consent {
            &self.gate
        }

        /// Allocates the next turn id.
        pub(crate) fn next_turn(&self) -> TurnId {
            TurnId(NEXT_TURN.fetch_add(1, Ordering::SeqCst))
        }

        /// Ends the consent run now — pending requests withdrawn, tokens void — and returns
        /// the proof [`Event::Exited`](super::Event::Exited) requires. Idempotent; `Drop`
        /// calls it too.
        pub(crate) fn end(&self) -> RunEnded {
            // `Once`, not a flag: a second caller blocks until the first has ended the run,
            // so no proof is returned while the run is still live — at the cost that it
            // waits across the presenter's `dismiss`, which `end_run` calls last. `_force`
            // so that a panic in that `dismiss` does not turn every later `Drop` into
            // another; the run is already out of the gate's state by then, so the poisoned
            // path does nothing rather than dismissing the same handle twice.
            self.ended.call_once_force(|state| {
                if !state.is_poisoned() {
                    self.gate.end_run(self.run);
                }
            });
            RunEnded {
                attachment: self.id,
            }
        }
    }

    /// Proof that an attachment's consent run has ended. Only `Attachment::end` produces
    /// one, and [`Event::Exited`](super::Event::Exited) cannot be built without it. It
    /// names the attachment, so a proof from one cannot certify another's exit unnoticed.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct RunEnded {
        attachment: AttachmentId,
    }

    impl RunEnded {
        pub fn attachment(&self) -> AttachmentId {
            self.attachment
        }
    }

    impl Drop for Attachment {
        fn drop(&mut self) {
            self.end();
        }
    }
}

pub use lease::{Attachment, RunEnded};

// ---------------------------------------------------------------------------------------
// Capabilities

/// Which approvals reach the core's gate. Shown to the user where they read approvals
/// (#40); never read to decide how one is handled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalReach {
    /// Every call: the core classifies and executes each one (native).
    Every,
    /// Only what the CLI delegates. Which calls those are is the table under #40; the rest
    /// are reported as [`Event::RanWithoutAsking`] where the backend can see them and are
    /// otherwise open.
    Delegated,
}

/// What a resumed session was *observed* to do with a turn that was cut, on the backend
/// version named in [`Capabilities::measured_on`]. Per cause, from measurement, never from
/// documentation (#42). A promise about a later version is not one this type makes.
///
/// None of these says whether the cut call ran. The result the backend reports for it
/// — rejected, or an error — is the backend's, not an outcome: Claude Code 2.1.280 reported
/// "rejected" for a command that had run in part, and the model repeated that. And a
/// re-issued call passes through approval only where the backend delegates it; one the
/// backend's own rules allow is re-run with no dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutTurn {
    /// The cut call is reported to the model as rejected, and the model asks before it
    /// continues. Claude Code after a SIGINT (#42, 2.1.266) and after the stream-json
    /// interrupt its backend sends (2.1.280).
    AsksBeforeContinuing,
    /// The cut call is reported as an error and the model may re-issue it as a new call,
    /// which passes through approval again but is not the user's decision to re-run.
    /// Claude Code after a crash: it re-issued on 2.1.266 (#42); on 2.1.280, once
    /// observed, it looked first instead — a model's choice, so "may" stands.
    MayRerun,
    /// Not measured for this backend and cause. The UI says so rather than promising either.
    Unmeasured,
}

/// What a backend can promise. The code above may read this to enable or disable an
/// affordance — a resume button, a mid-turn input box, a "delegated" badge — and may not
/// read it to alter the approval path, which is the same for every backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// [`RunBackend::resume`] works, by a stored [`SessionId`].
    pub resume: bool,
    /// [`Session::send`] is accepted while a turn is in progress.
    pub mid_turn_input: bool,
    pub approvals: ApprovalReach,
    /// After [`Session::interrupt`].
    pub after_interrupt: CutTurn,
    /// After the attachment ended without being asked to.
    pub after_crash: CutTurn,
    /// The backend version the two `CutTurn`s were last measured on, as the backend
    /// reports its version — `"claude 2.1.280"` — or `"none"` when unmeasured. Where an
    /// earlier version behaved otherwise, the variant's own doc says so.
    pub measured_on: &'static str,
}

// ---------------------------------------------------------------------------------------
// Usage

/// The backend's estimate of cost, in millionths of a US dollar. An estimate: the UI labels
/// it as such and never as what a subscription will bill, which the core has no way to
/// know. Integer so that it is never negative, infinite or NaN.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EstimatedUsd {
    pub micros: u64,
}

/// Token counts and cost as the backend reports them. In [`Event::Usage`] the figures are
/// for that turn; from [`Session::usage`] they are the attachment's total so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Usage {
    /// The backend has not said. Distinct from zero, and shown as "not reported".
    NotReported,
    Reported {
        input_tokens: u64,
        output_tokens: u64,
        /// `None` when the backend reports tokens but no cost.
        cost: Option<EstimatedUsd>,
    },
}

// ---------------------------------------------------------------------------------------
// Input and the inbox

/// What the user typed for their own turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserInput {
    pub text: String,
}

/// Core-issued identity of one inbox delivery (#43).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeliveryId(pub u64);

/// An inbox message as the backend receives it. The sender is what the core recorded, never
/// what the message claims; the text is data a model wrote, and delivering it widens
/// nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InboxMessage {
    pub id: DeliveryId,
    pub from: ConversationId,
    pub text: String,
}

/// Where an inbox message is, as observable facts. Three states, reported through
/// [`Event::Delivery`] as each is reached; a message is at exactly one at a time, and a
/// backend that cannot observe the last never reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// The core holds it; the backend has not taken it.
    Enqueued,
    /// The backend has written it to its input — the CLI's stdin, the app-server
    /// connection, the native loop's queue. Whether the model sees it is not yet known.
    Accepted,
    /// The backend has confirmed it is in the model's context.
    Injected,
}

// ---------------------------------------------------------------------------------------
// Events

/// One model message, complete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub role: Role,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// The backend's own identifier for one tool call, verbatim, so a call, its approval and
/// its result can be correlated in a turn where several interleave.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ToolCallId(pub String);

/// What started a turn. For display: a turn's calls take the same approval path whichever
/// started it, and nothing here widens or narrows what the gate is asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnOrigin {
    /// [`Session::send`] or an inbox delivery [`Session::deliver`] accepted.
    Caller,
    /// The backend started it with no input from the caller: Claude Code does when a
    /// background task it ran finishes (#63).
    Backend,
}

/// How a turn ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnEnd {
    /// The model stopped on its own.
    Completed,
    /// [`Session::interrupt`] cut it.
    Interrupted,
    /// The backend reported the turn failed. `detail` is core-generated.
    Failed { detail: String },
    /// The backend reported the turn failed because the account has no credential.
    /// `how` is core-generated: where the user signs in, through the backend's own flow
    /// (#41).
    NotSignedIn { how: String },
    /// The attachment ended under it. What a resume does with it is
    /// [`Capabilities::after_crash`].
    Cut,
}

/// How an attachment ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Exit {
    /// [`Session::terminate`] was called.
    Terminated,
    /// The process exited on its own, cleanly, without being asked — end of input, a
    /// non-interactive backend finishing. `status` is the exit status when there is one.
    Exited { status: Option<i32> },
    /// The process died without being asked to. `detail` is core-generated, never the
    /// process's own last words unescaped.
    Crashed { detail: String },
}

/// What a backend reports upward. Rendered by the UI; nothing here is an instruction to the
/// core. Partial and complete messages are distinct kinds on purpose: a UI that renders
/// a partial as if it were the message shows text the model may still retract.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The backend learned its session identifier: at most once per attachment, as soon
    /// as the backend knows it, which on a CLI may be after the first input rather than
    /// before. On a resume it repeats the identifier that was resumed.
    SessionOpened {
        session: SessionId,
    },
    TurnStarted {
        turn: TurnId,
        origin: TurnOrigin,
    },
    /// A fragment of a message still being produced. `text` is a delta, appended to what
    /// came before it since the last [`Event::MessageComplete`] in the same turn.
    MessagePartial {
        turn: TurnId,
        text: String,
    },
    MessageComplete {
        turn: TurnId,
        message: Message,
    },
    /// A call the backend made or is making, for display. Whether it asked first is the
    /// next events' business.
    ToolCall {
        turn: TurnId,
        call: ToolCallId,
        name: String,
        /// Escaped, core-generated text of the arguments, never markup the model wrote.
        arguments: String,
    },
    /// What the call returned, escaped, for display.
    ToolResult {
        turn: TurnId,
        call: ToolCallId,
        output: String,
        is_error: bool,
    },
    /// The gate was asked. `rendered` is what the dialog shows, so the WebView can display
    /// the same bytes; it cannot answer them.
    ApprovalRequested {
        turn: TurnId,
        call: ToolCallId,
        invocation: InvocationId,
        rendered: Rendered,
    },
    /// The gate answered. Whether it minted a token is not reported here — the record has
    /// that — only that the request is no longer pending and which way it went. `allowed`
    /// is true when the gate allowed *and* the answer reached the backend's process; an
    /// allow that could not be delivered resolves as not allowed, with a `Diagnostic`
    /// saying why, since the call did not run.
    ApprovalResolved {
        turn: TurnId,
        call: ToolCallId,
        invocation: InvocationId,
        allowed: bool,
    },
    /// A call the backend executed that never asked: auto-allowed by its own rules, or
    /// made while the path to the gate was down. A call that asked and was denied before
    /// the gate — a tool with no door yet — is not this; its deny is the backend's answer. Recorded under #40, never
    /// silently accepted. A backend with [`ApprovalReach::Every`] never emits this.
    RanWithoutAsking {
        turn: TurnId,
        call: ToolCallId,
        name: String,
        arguments: String,
    },
    /// An inbox message reached the state given.
    Delivery {
        id: DeliveryId,
        state: Delivery,
    },
    /// Usage for the turn. May arrive more than once as the backend refines it.
    Usage {
        turn: TurnId,
        usage: Usage,
    },
    /// Something the backend said that is not part of the conversation — a system or init
    /// record, a line of stderr — escaped, for a log the user can open.
    Diagnostic {
        text: String,
    },
    TurnEnded {
        turn: TurnId,
        end: TurnEnd,
    },
    /// The attachment ended. Nothing follows it on this sink, and the consent run is over:
    /// `ended` is the proof, only the lease issues it, and it names this attachment.
    Exited {
        exit: Exit,
        ended: RunEnded,
    },
}

/// Where a backend delivers events. Supplied by the caller at [`Start`] / [`Resume`];
/// called from whatever thread the backend delivers on.
pub trait EventSink: Send + Sync {
    fn event(&self, event: Event);
}

// ---------------------------------------------------------------------------------------
// The contract

/// Why a backend could not do what was asked. Core-generated text; a process's own output
/// is not repeated here unescaped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendError {
    /// The binary, the loop or the endpoint could not be brought up.
    CannotStart(String),
    /// The backend is not signed in for this account. The core does not sign in on its
    /// behalf; the user does, through the backend's own flow (#41).
    NotSignedIn,
    /// [`Capabilities`] said no.
    Unsupported(&'static str),
    /// A turn is in progress and [`Capabilities::mid_turn_input`] is false.
    Busy,
    /// The attachment has ended; see the [`Event::Exited`] that said so.
    Ended,
    /// The session id is another backend's or the backend does not know it.
    UnknownSession,
    /// The channel to the process broke or carried something the backend could not parse.
    Transport(String),
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackendError::CannotStart(why) => write!(f, "cannot start: {why}"),
            BackendError::NotSignedIn => f.write_str("not signed in"),
            BackendError::Unsupported(what) => write!(f, "unsupported: {what}"),
            BackendError::Busy => f.write_str("a turn is in progress"),
            BackendError::Ended => f.write_str("the attachment has ended"),
            BackendError::UnknownSession => f.write_str("unknown session"),
            BackendError::Transport(why) => write!(f, "transport: {why}"),
        }
    }
}

impl std::error::Error for BackendError {}

/// What a backend needs to open a new session. The gate is here because approvals are the
/// backend's to ask for and nobody else's; the backend wraps it in an [`Attachment`] and
/// the caller keeps no run id to ask under.
pub struct Start {
    pub conversation: ConversationId,
    pub account: AccountId,
    pub workspace_root: PathBuf,
    pub gate: Arc<Consent>,
    pub events: Arc<dyn EventSink>,
}

/// What a backend needs to reattach to a stored session. The account and the workspace are
/// the session's; there is no field for another.
pub struct Resume {
    pub conversation: ConversationId,
    pub session: SessionId,
    pub gate: Arc<Consent>,
    pub events: Arc<dyn EventSink>,
}

/// A backend: the thing that opens sessions. One value per backend kind, built in and
/// static; sealed, so this is not a plugin registry.
pub trait RunBackend: sealed::Sealed + Send + Sync {
    /// For labels: the dialog title and the run list name the backend (#40 requires the
    /// UI to show it). The code above may show it and may not branch on it.
    fn kind(&self) -> Backend;

    fn capabilities(&self) -> Capabilities;

    /// Opens a new session under `start.account`. Returns once the attachment exists —
    /// the process is up, the loop is built — which may be before the backend knows its
    /// session id.
    fn start(&self, start: Start) -> Result<Box<dyn Session>, BackendError>;

    /// Reattaches to `resume.session`, under the account and workspace it carries. Refused
    /// with [`BackendError::Unsupported`] when [`Capabilities::resume`] is false, and with
    /// [`BackendError::UnknownSession`] when the id is not this backend's.
    fn resume(&self, resume: Resume) -> Result<Box<dyn Session>, BackendError>;
}

/// One attachment to one session. Shared between the thread that drives the process and
/// the caller, so every method takes `&self`; the backend holds the [`Attachment`] lease,
/// ends it before it reports [`Event::Exited`], and dropping the session ends it whether
/// or not anything was reported.
pub trait Session: sealed::Sealed + Send + Sync {
    fn attachment(&self) -> AttachmentId;

    /// The user's own input. Starts a turn, or — when [`Capabilities::mid_turn_input`] —
    /// joins the one in progress; otherwise [`BackendError::Busy`]. The turn id is issued
    /// here and [`Event::TurnStarted`] follows on the sink.
    fn send(&self, input: UserInput) -> Result<TurnId, BackendError>;

    /// An inbox message (#43). Returns once the message is [`Delivery::Enqueued`]; the
    /// later states arrive as [`Event::Delivery`].
    fn deliver(&self, message: InboxMessage) -> Result<(), BackendError>;

    /// Cuts the turn in progress. What a later resume does with the cut turn is
    /// [`Capabilities::after_interrupt`]. A no-op when no turn is in progress.
    fn interrupt(&self) -> Result<(), BackendError>;

    /// Ends the attachment. Idempotent, and `Ok` once the request to end has been made;
    /// the end itself is observed as [`Event::Exited`] with [`Exit::Terminated`], which
    /// the backend can only emit once the consent run is over. A backend whose process
    /// starts processes of its own — a tool call's shell — ends those it can still find
    /// under it too, before it returns: an approved command must not run on after its
    /// run was ended. What one already did is not undone, and one that left the tree
    /// first is not found.
    fn terminate(&self) -> Result<(), BackendError>;

    /// Usage for the attachment so far, as the backend reports it.
    fn usage(&self) -> Usage;
}
