//! The events the WebView receives: a mirror of [`stanchion_core::backend::Event`] that
//! serialises, delivered on the Tauri channel the WebView passed when it started the
//! conversation. The mapping is total and mechanical — every variant, every field — so
//! nothing a backend reports is dropped on the way; the only thing added is the
//! conversation the event belongs to. The core links no serialisation library, which is
//! why the mirror lives on this side of the boundary.
//!
//! A channel needs no permission in `capabilities/default.json`: `__TAURI_CHANNEL__|fetch`
//! is exempt from the ACL unconditionally (`docs/decisions.md`). And nothing here can
//! carry an answer back: the channel is one way, from the core to the WebView.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use stanchion_core::backend::{
    ConversationId, Delivery, Event, EventSink, Exit, Message, Role, SessionId, TurnEnd,
    TurnOrigin, Usage,
};
use stanchion_core::consent::presenter::Rendered;
use tauri::ipc::Channel;

#[derive(Clone, Debug, Serialize)]
pub struct SessionRef {
    pub backend: String,
    pub account: String,
    pub workspace_root: String,
    pub value: String,
}

impl From<&SessionId> for SessionRef {
    fn from(s: &SessionId) -> Self {
        SessionRef {
            backend: format!("{:?}", s.backend()),
            account: s.account().0.clone(),
            workspace_root: s.workspace_root().display().to_string(),
            value: s.value().to_string(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct MessageRef {
    pub role: &'static str,
    pub text: String,
}

impl From<Message> for MessageRef {
    fn from(m: Message) -> Self {
        MessageRef {
            role: match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            },
            text: m.text,
        }
    }
}

/// What the dialog shows, verbatim, so the WebView can display the same bytes; it cannot
/// answer them.
#[derive(Clone, Debug, Serialize)]
pub struct RenderedRef {
    pub title: String,
    pub body: String,
    pub parsed: Vec<(String, String)>,
    pub negative: &'static str,
    pub affirmative: &'static str,
}

impl From<Rendered> for RenderedRef {
    fn from(r: Rendered) -> Self {
        RenderedRef {
            title: r.title,
            body: r.body,
            parsed: r.parsed,
            negative: r.negative,
            affirmative: r.affirmative,
        }
    }
}

/// Estimates only; the UI labels the cost as such (`docs/architecture.md`).
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UsageRef {
    NotReported,
    Reported {
        input_tokens: u64,
        output_tokens: u64,
        estimated_cost_micros: Option<u64>,
    },
}

impl From<Usage> for UsageRef {
    fn from(u: Usage) -> Self {
        match u {
            Usage::NotReported => UsageRef::NotReported,
            Usage::Reported {
                input_tokens,
                output_tokens,
                cost,
            } => UsageRef::Reported {
                input_tokens,
                output_tokens,
                estimated_cost_micros: cost.map(|c| c.micros),
            },
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TurnEndRef {
    Completed,
    Interrupted,
    Failed { detail: String },
    NotSignedIn { how: String },
    Cut,
}

impl From<TurnEnd> for TurnEndRef {
    fn from(e: TurnEnd) -> Self {
        match e {
            TurnEnd::Completed => TurnEndRef::Completed,
            TurnEnd::Interrupted => TurnEndRef::Interrupted,
            TurnEnd::Failed { detail } => TurnEndRef::Failed { detail },
            TurnEnd::NotSignedIn { how } => TurnEndRef::NotSignedIn { how },
            TurnEnd::Cut => TurnEndRef::Cut,
        }
    }
}

/// Who started a turn: the caller's own input or inbox delivery, or the backend on its own.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOriginRef {
    Caller,
    Backend,
}

impl From<TurnOrigin> for TurnOriginRef {
    fn from(o: TurnOrigin) -> Self {
        match o {
            TurnOrigin::Caller => TurnOriginRef::Caller,
            TurnOrigin::Backend => TurnOriginRef::Backend,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExitRef {
    Terminated,
    Exited { status: Option<i32> },
    Crashed { detail: String },
}

impl From<Exit> for ExitRef {
    fn from(e: Exit) -> Self {
        match e {
            Exit::Terminated => ExitRef::Terminated,
            Exit::Exited { status } => ExitRef::Exited { status },
            Exit::Crashed { detail } => ExitRef::Crashed { detail },
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventRef {
    SessionOpened {
        session: SessionRef,
    },
    TurnStarted {
        turn: u64,
        origin: TurnOriginRef,
    },
    MessagePartial {
        turn: u64,
        text: String,
    },
    MessageComplete {
        turn: u64,
        message: MessageRef,
    },
    ToolCall {
        turn: u64,
        call: String,
        name: String,
        arguments: String,
    },
    ToolResult {
        turn: u64,
        call: String,
        output: String,
        is_error: bool,
    },
    ApprovalRequested {
        turn: u64,
        call: String,
        invocation: u64,
        rendered: RenderedRef,
    },
    ApprovalResolved {
        turn: u64,
        call: String,
        invocation: u64,
        allowed: bool,
    },
    RanWithoutAsking {
        turn: u64,
        call: String,
        name: String,
        arguments: String,
    },
    Delivery {
        id: u64,
        state: &'static str,
    },
    Usage {
        turn: u64,
        usage: UsageRef,
    },
    Diagnostic {
        text: String,
    },
    TurnEnded {
        turn: u64,
        end: TurnEndRef,
    },
    Exited {
        exit: ExitRef,
        attachment: u64,
    },
}

impl From<Event> for EventRef {
    fn from(e: Event) -> Self {
        match e {
            Event::SessionOpened { session } => EventRef::SessionOpened {
                session: SessionRef::from(&session),
            },
            Event::TurnStarted { turn, origin } => EventRef::TurnStarted {
                turn: turn.raw(),
                origin: origin.into(),
            },
            Event::MessagePartial { turn, text } => EventRef::MessagePartial {
                turn: turn.raw(),
                text,
            },
            Event::MessageComplete { turn, message } => EventRef::MessageComplete {
                turn: turn.raw(),
                message: message.into(),
            },
            Event::ToolCall {
                turn,
                call,
                name,
                arguments,
            } => EventRef::ToolCall {
                turn: turn.raw(),
                call: call.0,
                name,
                arguments,
            },
            Event::ToolResult {
                turn,
                call,
                output,
                is_error,
            } => EventRef::ToolResult {
                turn: turn.raw(),
                call: call.0,
                output,
                is_error,
            },
            Event::ApprovalRequested {
                turn,
                call,
                invocation,
                rendered,
            } => EventRef::ApprovalRequested {
                turn: turn.raw(),
                call: call.0,
                invocation: invocation.raw(),
                rendered: rendered.into(),
            },
            Event::ApprovalResolved {
                turn,
                call,
                invocation,
                allowed,
            } => EventRef::ApprovalResolved {
                turn: turn.raw(),
                call: call.0,
                invocation: invocation.raw(),
                allowed,
            },
            Event::RanWithoutAsking {
                turn,
                call,
                name,
                arguments,
            } => EventRef::RanWithoutAsking {
                turn: turn.raw(),
                call: call.0,
                name,
                arguments,
            },
            Event::Delivery { id, state } => EventRef::Delivery {
                id: id.0,
                state: match state {
                    Delivery::Enqueued => "enqueued",
                    Delivery::Accepted => "accepted",
                    Delivery::Injected => "injected",
                },
            },
            Event::Usage { turn, usage } => EventRef::Usage {
                turn: turn.raw(),
                usage: usage.into(),
            },
            Event::Diagnostic { text } => EventRef::Diagnostic { text },
            Event::TurnEnded { turn, end } => EventRef::TurnEnded {
                turn: turn.raw(),
                end: end.into(),
            },
            Event::Exited { exit, ended } => EventRef::Exited {
                exit: exit.into(),
                attachment: ended.attachment().raw(),
            },
        }
    }
}

/// One event as the WebView receives it: the conversation it belongs to and the event.
#[derive(Clone, Debug, Serialize)]
pub struct ConversationEvent {
    pub conversation: u64,
    pub event: EventRef,
}

/// The sink a conversation's events go to. A send that fails — the WebView reloaded, the
/// window closed — is dropped: the session outlives its channel, and the core's contract
/// lets a sink drop what it cannot deliver. Two events are read on the way past and kept
/// here, on the sink itself, so there is no window in which they can be missed: the
/// `SessionOpened` a resume needs — which on a fresh start may arrive before the shell
/// has stored the session at all (`init` before the first input is a measured shape) —
/// and the `Exited` that says this attachment is over. The invocations requested and not
/// yet resolved are kept too, so Cmd-. on an alert can find the conversation it belongs to.
pub struct ChannelSink {
    conversation: ConversationId,
    channel: Channel<ConversationEvent>,
    session: Mutex<Option<SessionId>>,
    exited: AtomicBool,
    pending: Mutex<HashSet<u64>>,
}

impl ChannelSink {
    pub fn new(conversation: ConversationId, channel: Channel<ConversationEvent>) -> Arc<Self> {
        Arc::new(ChannelSink {
            conversation,
            channel,
            session: Mutex::new(None),
            exited: AtomicBool::new(false),
            pending: Mutex::new(HashSet::new()),
        })
    }

    pub fn channel(&self) -> &Channel<ConversationEvent> {
        &self.channel
    }

    /// The session id this attachment reported, if it has.
    pub fn session(&self) -> Option<SessionId> {
        self.session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Whether this attachment asked the gate about `invocation` and has not heard back.
    pub fn is_pending(&self, invocation: u64) -> bool {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&invocation)
    }

    /// Whether this attachment reported its end. Nothing follows `Exited` on a sink.
    pub fn exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }
}

impl EventSink for ChannelSink {
    fn event(&self, event: Event) {
        match &event {
            Event::SessionOpened { session } => {
                *self.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(session.clone());
            }
            Event::ApprovalRequested { invocation, .. } => {
                self.pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(invocation.raw());
            }
            Event::ApprovalResolved { invocation, .. } => {
                self.pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&invocation.raw());
            }
            Event::Exited { .. } => {
                // A request still pending when the attachment ends is withdrawn by the
                // lease's end and never resolved here.
                self.pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clear();
                self.exited.store(true, Ordering::SeqCst);
            }
            _ => {}
        }
        let _ = self.channel.send(ConversationEvent {
            conversation: self.conversation.0,
            event: event.into(),
        });
    }
}
