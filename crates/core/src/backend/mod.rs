//! The run backend contract: what the core asks of the thing that drives a run, stated so
//! that a run on the core's own loop and a run on a vendor CLI are the same kind of value
//! to everything above them.
//!
//! `docs/architecture.md`, "Run backends", carries the contract and the four lifetimes; this
//! module is it in types. Two backends implement it — `native` (the loop in this crate,
//! M3) and `cli` (Claude Code in #46, Codex in #47) — and the code above them holds a
//! [`RunBackend`] and a [`Session`] and never branches on [`Backend`]: `tests/backend.rs`
//! drives two fakes through one function to keep that so.
//!
//! # What the trait does not carry
//!
//! **An approval decision.** A backend receives the consent gate at [`Start`] and asks it
//! itself — on a CLI backend through [`crate::execute::CliApproval::resolve`], which owes the
//! CLI exactly one reply. Upward it emits [`Event::ApprovalRequested`] for display, and
//! nothing on [`Session`] takes an answer, so the code above — the shell, the WebView behind
//! it — has no handle by which to approve. `crates/core/src/lib.rs` pins that with a
//! `compile_fail` doctest beside the token ones.
//!
//! **A runtime.** Events reach the caller through an [`EventSink`] it supplies, on whatever
//! thread the backend delivers from, as the presenter does for consent. Whether a backend
//! drives its subprocess from a thread or an executor is that backend's business, decided
//! when it is written.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use crate::consent::presenter::Rendered;
use crate::consent::request::{InvocationId, RunId};
use crate::consent::Consent;

pub use crate::consent::request::Backend;

// ---------------------------------------------------------------------------------------
// The four lifetimes

/// The conversation as the GUI holds it. Core-issued, and the longest-lived of the four: it
/// outlives every session, turn and process under it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConversationId(pub u64);

/// The account a session was created under (#45). Opaque here: what it authenticates as,
/// where requests go and who owns the credential are the account model's, not the
/// backend's. On a CLI backend it names a per-account config directory the core created
/// and never reads (#41).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AccountId(pub String);

/// A backend's own identifier for a session, as the core stored it, with the account it was
/// created under. The two travel together on purpose: [`RunBackend::resume`] takes a
/// `SessionId` and nothing else that names an account, so a stored session cannot be
/// reattached under a different one — there is nowhere at the call site to say so.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SessionId {
    backend: Backend,
    account: AccountId,
    value: String,
}

impl SessionId {
    /// Only a backend calls this, with the identifier the backend reported.
    pub fn new(backend: Backend, account: AccountId, value: impl Into<String>) -> Self {
        SessionId {
            backend,
            account,
            value: value.into(),
        }
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    pub fn account(&self) -> &AccountId {
        &self.account
    }

    /// The backend's identifier, verbatim. Never "the latest": a resume names this.
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} session {}", self.backend.label(), self.value)
    }
}

/// One turn: from an input the backend accepted to the point the backend reports the model
/// has stopped, however it stopped. Core-issued.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TurnId(pub u64);

/// One attachment of the core to a session: on a CLI backend one supervised process, on the
/// native backend one instance of the loop. Core-issued. A session may see several — each
/// resume is a new one — and a turn a process cut is a turn the next process did not
/// finish.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AttachmentId(pub u64);

// ---------------------------------------------------------------------------------------
// Capabilities

/// Which approvals reach the core's gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalReach {
    /// Every call: the core classifies and executes each one (native).
    Every,
    /// Only what the CLI delegates. Which calls those are is the table under #40; the rest
    /// are reported as [`Event::RanWithoutAsking`] where the backend can see them and are
    /// otherwise open.
    Delegated,
}

/// What a resumed session does with a turn that was cut. Stated per backend and per cause,
/// from measurement, never from documentation (#42).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutTurn {
    /// The cut call is reported to the model as rejected, and the model asks before it
    /// continues. Claude Code after `interrupt` (#42).
    AsksBeforeContinuing,
    /// The cut call is reported as an error and the model may re-issue it as a new call,
    /// which passes through approval again but is not the user's decision to re-run.
    /// Claude Code after a crash (#42).
    MayRerun,
    /// Not measured for this backend and cause. The UI says so rather than promising either.
    Unmeasured,
}

/// What a backend can promise. The code above may read this to enable or disable an
/// affordance — a resume button, a mid-turn input box — and may not read it to alter the
/// approval path, which is the same for every backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// [`RunBackend::resume`] works, by a stored [`SessionId`].
    pub resume: bool,
    /// [`Session::send`] is accepted while a turn is in progress.
    pub mid_turn_input: bool,
    pub approvals: ApprovalReach,
    /// Whether [`Usage`] is ever anything but [`Usage::NotReported`].
    pub reports_usage: bool,
    /// After [`Session::interrupt`].
    pub after_interrupt: CutTurn,
    /// After the process died without being asked to.
    pub after_crash: CutTurn,
}

// ---------------------------------------------------------------------------------------
// Usage

/// The backend's estimate of cost, in US dollars. An estimate: the UI labels it as such and
/// never as what a subscription will bill, which the core has no way to know.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EstimatedUsd(pub f64);

/// Token counts and cost as the backend reports them.
#[derive(Clone, Copy, Debug, PartialEq)]
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

/// Where an inbox message is. Three states, reported through [`Event::Delivery`] as each is
/// reached; a message is at exactly one at a time, and a backend that cannot tell the last
/// two apart says so by never reporting [`Delivery::Injected`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// The core holds it; the backend has not taken it.
    Enqueued,
    /// The backend took it and will present it to the model.
    Accepted,
    /// It is in the model's context.
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

/// How a turn ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnEnd {
    /// The model stopped on its own.
    Completed,
    /// [`Session::interrupt`] cut it.
    Interrupted,
    /// The attachment ended under it. What a resume does with it is
    /// [`Capabilities::after_crash`].
    Cut,
}

/// How an attachment ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Exit {
    /// [`Session::terminate`] was called.
    Terminated,
    /// The process died without being asked to. `detail` is core-generated, never the
    /// process's own last words unescaped.
    Crashed { detail: String },
}

/// What a backend reports upward. Rendered by the UI; nothing here is an instruction to the
/// core. Partial and complete messages are distinct kinds on purpose: a UI that renders
/// a partial as if it were the message shows text the model may still retract.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The backend reported its session identifier. Once per attachment, before any turn;
    /// on a resume it repeats the identifier that was resumed.
    SessionOpened {
        session: SessionId,
    },
    TurnStarted {
        turn: TurnId,
    },
    /// A fragment of a message still being produced. `text` is a delta, appended to what
    /// came before in the same turn.
    MessagePartial {
        turn: TurnId,
        text: String,
    },
    MessageComplete {
        turn: TurnId,
        message: Message,
    },
    /// A call the backend made or is making, for display. Whether it asked first is the
    /// next two events' business.
    ToolCall {
        turn: TurnId,
        name: String,
        /// Escaped, core-generated text of the arguments, never markup the model wrote.
        arguments: String,
    },
    /// The gate was asked. `rendered` is what the dialog shows, so the WebView can display
    /// the same bytes; it cannot answer them.
    ApprovalRequested {
        turn: TurnId,
        invocation: InvocationId,
        rendered: Rendered,
    },
    /// The gate answered. Whether it minted a token is not reported here — the record has
    /// that — only that the request is no longer pending and which way it went.
    ApprovalResolved {
        turn: TurnId,
        invocation: InvocationId,
        allowed: bool,
    },
    /// A call the backend executed that never reached the gate: auto-allowed by its own
    /// rules, or made while the path to the gate was down. Recorded under #40, never
    /// silently accepted. A backend with [`ApprovalReach::Every`] never emits this.
    RanWithoutAsking {
        turn: TurnId,
        name: String,
        arguments: String,
    },
    /// An inbox message reached the state given.
    Delivery {
        id: DeliveryId,
        state: Delivery,
    },
    Usage {
        turn: TurnId,
        usage: Usage,
    },
    TurnEnded {
        turn: TurnId,
        end: TurnEnd,
    },
    /// The attachment ended. Nothing follows it on this sink.
    Exited {
        exit: Exit,
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
    /// [`Capabilities`] said no.
    Unsupported(&'static str),
    /// The attachment has ended; see the [`Event::Exited`] that said so.
    Ended,
    /// The session id is another backend's or the backend does not know it.
    UnknownSession,
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackendError::CannotStart(why) => write!(f, "cannot start: {why}"),
            BackendError::Unsupported(what) => write!(f, "unsupported: {what}"),
            BackendError::Ended => f.write_str("the attachment has ended"),
            BackendError::UnknownSession => f.write_str("unknown session"),
        }
    }
}

impl std::error::Error for BackendError {}

/// What a backend needs to open a new session. The gate is here because approvals are the
/// backend's to ask for and nobody else's; the caller hands it over and keeps no way to
/// answer on the backend's behalf.
pub struct Start {
    pub conversation: ConversationId,
    pub account: AccountId,
    pub workspace_root: PathBuf,
    pub gate: Arc<Consent>,
    pub events: Arc<dyn EventSink>,
}

/// What a backend needs to reattach to a stored session. The account is the session's;
/// there is no field for another.
pub struct Resume {
    pub conversation: ConversationId,
    pub session: SessionId,
    pub workspace_root: PathBuf,
    pub gate: Arc<Consent>,
    pub events: Arc<dyn EventSink>,
}

/// A backend: the thing that opens sessions. One value per backend kind, built in and
/// static; this is not a plugin registry.
pub trait RunBackend: Send + Sync {
    /// For labels. The code above may show it and may not branch on it.
    fn kind(&self) -> Backend;

    fn capabilities(&self) -> Capabilities;

    /// Opens a new session under `start.account`. The first event on the sink is
    /// [`Event::SessionOpened`].
    fn start(&self, start: Start) -> Result<Box<dyn Session>, BackendError>;

    /// Reattaches to `resume.session`, under the account it carries. Refused with
    /// [`BackendError::Unsupported`] when [`Capabilities::resume`] is false, and with
    /// [`BackendError::UnknownSession`] when the id is not this backend's.
    fn resume(&self, resume: Resume) -> Result<Box<dyn Session>, BackendError>;
}

/// One attachment to one session. Dropping it without [`Session::terminate`] is a bug the
/// backend may treat as terminate.
pub trait Session: Send + Sync {
    /// The consent run this attachment registered. Tokens minted under it die with the
    /// attachment: a crashed process's pending approvals do not carry into the resumed one.
    fn run(&self) -> RunId;

    fn attachment(&self) -> AttachmentId;

    /// The user's own input. Starts a turn, or — when [`Capabilities::mid_turn_input`] —
    /// joins the one in progress.
    fn send(&self, input: UserInput) -> Result<TurnId, BackendError>;

    /// An inbox message (#43). Returns once the message is [`Delivery::Enqueued`]; the
    /// later states arrive as [`Event::Delivery`].
    fn deliver(&self, message: InboxMessage) -> Result<(), BackendError>;

    /// Cuts the turn in progress. What a later resume does with the cut turn is
    /// [`Capabilities::after_interrupt`]. A no-op when no turn is in progress.
    fn interrupt(&self) -> Result<(), BackendError>;

    /// Ends the attachment. The last event on the sink is [`Event::Exited`] with
    /// [`Exit::Terminated`]; the consent run is ended and its tokens are void.
    fn terminate(self: Box<Self>);

    /// Usage so far, as the backend reports it.
    fn usage(&self) -> Usage;
}
