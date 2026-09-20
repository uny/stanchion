//! The "no branch above" half of the run backend contract (`docs/architecture.md`, "Run
//! backends"): one driver, written against `dyn RunBackend` and `dyn Session` and never
//! matching on `Backend`, takes a fake CLI backend and a fake native backend through
//! start, a turn, an approval the backend asks the gate for itself, the three inbox
//! states, interrupt, and terminate. The compile-time half — nothing on `Session` takes an
//! answer — is a `compile_fail` doctest in `src/lib.rs`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use stanchion_core::backend::{
    AccountId, ApprovalReach, AttachmentId, Backend, BackendError, Capabilities, ConversationId,
    CutTurn, Delivery, DeliveryId, Event, EventSink, Exit, InboxMessage, Message, Resume, Role,
    RunBackend, Session, SessionId, Start, TurnEnd, TurnId, Usage, UserInput,
};
use stanchion_core::consent::policy::AlwaysAsk;
use stanchion_core::consent::presenter::{
    Answer, ConsentPresenter, Handle, PresenterError, Rendered, Responder,
};
use stanchion_core::consent::request::{ClassSpec, RequestSpec, RunId};
use stanchion_core::consent::{Config, Consent};
use stanchion_core::execute::{CliApproval, Reply, ReplyTransport};

// ---------------------------------------------------------------------------------------
// Fakes shared by both backends

/// Declines everything after a zero settle, so a backend that asks the gate gets a real
/// answer and the driver sees the request go round.
#[derive(Default)]
struct Declines {
    shown: AtomicU64,
}

impl ConsentPresenter for Declines {
    fn capacity(&self) -> usize {
        1 << 16
    }
    fn show(&self, _: &Rendered, responder: Responder) -> Result<Handle, PresenterError> {
        self.shown.fetch_add(1, Ordering::SeqCst);
        responder.answer(Answer::Decline);
        Ok(Handle(1))
    }
    fn dismiss(&self, _: Handle) {}
}

fn gate() -> (Arc<Consent>, Arc<Declines>) {
    let presenter = Arc::new(Declines::default());
    let gate = Arc::new(Consent::new(
        presenter.clone(),
        Arc::new(AlwaysAsk),
        Config {
            settle: std::time::Duration::ZERO,
            ..Config::default()
        },
    ));
    (gate, presenter)
}

#[derive(Default)]
struct Recorder(Mutex<Vec<Event>>);

impl EventSink for Recorder {
    fn event(&self, event: Event) {
        self.0.lock().unwrap().push(event);
    }
}

impl Recorder {
    fn take(&self) -> Vec<Event> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

/// Everything a fake session needs, whichever backend it pretends to be.
struct FakeSession {
    kind: Backend,
    run: RunId,
    attachment: AttachmentId,
    gate: Arc<Consent>,
    events: Arc<dyn EventSink>,
    workspace_root: PathBuf,
    next_turn: AtomicU64,
    open_turn: Mutex<Option<TurnId>>,
    replies: Mutex<Vec<Reply>>,
}

impl FakeSession {
    fn new(kind: Backend, session: SessionId, start_like: &Start, attachment: u64) -> Self {
        let run = start_like.gate.register_run(kind);
        start_like.events.event(Event::SessionOpened {
            session: session.clone(),
        });
        FakeSession {
            kind,
            run,
            attachment: AttachmentId(attachment),
            gate: start_like.gate.clone(),
            events: start_like.events.clone(),
            workspace_root: start_like.workspace_root.clone(),
            next_turn: AtomicU64::new(1),
            open_turn: Mutex::new(None),
            replies: Mutex::new(Vec::new()),
        }
    }

    fn emit(&self, event: Event) {
        self.events.event(event);
    }
}

impl Session for FakeSession {
    fn run(&self) -> RunId {
        self.run
    }

    fn attachment(&self) -> AttachmentId {
        self.attachment
    }

    fn send(&self, input: UserInput) -> Result<TurnId, BackendError> {
        let turn = TurnId(self.next_turn.fetch_add(1, Ordering::SeqCst));
        *self.open_turn.lock().unwrap() = Some(turn);
        self.emit(Event::TurnStarted { turn });
        self.emit(Event::MessagePartial {
            turn,
            text: "ok".into(),
        });
        self.emit(Event::MessageComplete {
            turn,
            message: Message {
                role: Role::Assistant,
                text: format!("ok: {}", input.text),
            },
        });
        // The backend asks the gate itself. A CLI backend owes the CLI a reply either
        // way; the native one would run its executor on a token. Both go through the
        // gate, and the driver above never sees a token or an answer.
        let spec = RequestSpec {
            run: Some(self.run),
            workspace_root: self.workspace_root.clone(),
            class: ClassSpec::CliCommand {
                cli_request_id: "req-1".into(),
                command: "ls".into(),
                cwd: self.workspace_root.clone(),
                session_grant: false,
            },
        };
        // What the gate decided is the record's; the fake only reports that the turn went
        // on. A real backend emits `ApprovalRequested` / `ApprovalResolved` around this.
        let _allowed = match self.kind {
            Backend::Native => {
                // The native fake asks the same gate with the same spec shape; a real one
                // would build a `ShellCommand` and take the executor door.
                self.gate.ask(spec).is_ok()
            }
            _ => {
                let mut transport = SessionReplies(self);
                CliApproval
                    .resolve(&self.gate, spec, &mut transport)
                    .is_ok()
            }
        };
        self.emit(Event::Usage {
            turn,
            usage: if matches!(self.kind, Backend::Native) {
                Usage::NotReported
            } else {
                Usage::Reported {
                    input_tokens: 10,
                    output_tokens: 5,
                    cost: None,
                }
            },
        });
        Ok(turn)
    }

    fn deliver(&self, message: InboxMessage) -> Result<(), BackendError> {
        for state in [Delivery::Enqueued, Delivery::Accepted, Delivery::Injected] {
            self.emit(Event::Delivery {
                id: message.id,
                state,
            });
        }
        Ok(())
    }

    fn interrupt(&self) -> Result<(), BackendError> {
        if let Some(turn) = self.open_turn.lock().unwrap().take() {
            self.emit(Event::TurnEnded {
                turn,
                end: TurnEnd::Interrupted,
            });
        }
        Ok(())
    }

    fn terminate(self: Box<Self>) {
        self.gate.end_run(self.run);
        self.emit(Event::Exited {
            exit: Exit::Terminated,
        });
    }

    fn usage(&self) -> Usage {
        Usage::NotReported
    }
}

/// `ReplyTransport` over the session's reply log, so `resolve` can be called with `&self`.
struct SessionReplies<'a>(&'a FakeSession);

impl ReplyTransport for SessionReplies<'_> {
    fn send(&mut self, reply: Reply) {
        self.0.replies.lock().unwrap().push(reply);
    }
}

struct FakeBackend {
    kind: Backend,
    capabilities: Capabilities,
    attachments: AtomicU64,
}

impl RunBackend for FakeBackend {
    fn kind(&self) -> Backend {
        self.kind
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn start(&self, start: Start) -> Result<Box<dyn Session>, BackendError> {
        let session = SessionId::new(self.kind, start.account.clone(), "s-1");
        let n = self.attachments.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeSession::new(self.kind, session, &start, n)))
    }

    fn resume(&self, resume: Resume) -> Result<Box<dyn Session>, BackendError> {
        if !self.capabilities.resume {
            return Err(BackendError::Unsupported("resume"));
        }
        if resume.session.backend() != self.kind {
            return Err(BackendError::UnknownSession);
        }
        let start = Start {
            conversation: resume.conversation,
            account: resume.session.account().clone(),
            workspace_root: resume.workspace_root,
            gate: resume.gate,
            events: resume.events,
        };
        let n = self.attachments.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeSession::new(
            self.kind,
            resume.session,
            &start,
            n,
        )))
    }
}

fn cli() -> FakeBackend {
    FakeBackend {
        kind: Backend::ClaudeCode,
        capabilities: Capabilities {
            resume: true,
            mid_turn_input: true,
            approvals: ApprovalReach::Delegated,
            reports_usage: true,
            after_interrupt: CutTurn::AsksBeforeContinuing,
            after_crash: CutTurn::MayRerun,
        },
        attachments: AtomicU64::new(1),
    }
}

fn native() -> FakeBackend {
    FakeBackend {
        kind: Backend::Native,
        capabilities: Capabilities {
            resume: false,
            mid_turn_input: false,
            approvals: ApprovalReach::Every,
            reports_usage: false,
            after_interrupt: CutTurn::Unmeasured,
            after_crash: CutTurn::Unmeasured,
        },
        attachments: AtomicU64::new(1),
    }
}

// ---------------------------------------------------------------------------------------
// The driver: everything above a backend, with no branch on which one it is

/// Runs one conversation through a backend and returns what the sink saw. This is the
/// shape of the code above a backend: it holds trait objects and reads `Capabilities` only
/// to decide whether to offer something, never to change how an approval is handled.
fn drive(backend: &dyn RunBackend) -> (Vec<Event>, Capabilities, Arc<Declines>) {
    let events = Arc::new(Recorder::default());
    let (gate, presenter) = gate();
    let session = backend
        .start(Start {
            conversation: ConversationId(1),
            account: AccountId("acct".into()),
            workspace_root: PathBuf::from("/tmp/ws"),
            gate,
            events: events.clone(),
        })
        .expect("start");
    session
        .send(UserInput {
            text: "hello".into(),
        })
        .expect("send");
    session
        .deliver(InboxMessage {
            id: DeliveryId(7),
            from: ConversationId(2),
            text: "from another run".into(),
        })
        .expect("deliver");
    session.interrupt().expect("interrupt");
    session.terminate();
    (events.take(), backend.capabilities(), presenter)
}

fn kinds(events: &[Event]) -> Vec<&'static str> {
    events
        .iter()
        .map(|e| match e {
            Event::SessionOpened { .. } => "session_opened",
            Event::TurnStarted { .. } => "turn_started",
            Event::MessagePartial { .. } => "message_partial",
            Event::MessageComplete { .. } => "message_complete",
            Event::ToolCall { .. } => "tool_call",
            Event::ApprovalRequested { .. } => "approval_requested",
            Event::ApprovalResolved { .. } => "approval_resolved",
            Event::RanWithoutAsking { .. } => "ran_without_asking",
            Event::Delivery { .. } => "delivery",
            Event::Usage { .. } => "usage",
            Event::TurnEnded { .. } => "turn_ended",
            Event::Exited { .. } => "exited",
        })
        .collect()
}

#[test]
fn both_backends_take_the_same_driver() {
    for backend in [cli(), native()] {
        let (events, _, _) = drive(&backend);
        assert_eq!(
            kinds(&events),
            [
                "session_opened",
                "turn_started",
                "message_partial",
                "message_complete",
                "usage",
                "delivery",
                "delivery",
                "delivery",
                "turn_ended",
                "exited",
            ],
            "{}",
            backend.kind().label()
        );
    }
}

#[test]
fn the_backend_asks_the_gate_itself_and_the_driver_never_sees_an_answer() {
    for backend in [cli(), native()] {
        let (_, _, presenter) = drive(&backend);
        // The driver handed over the gate and called nothing approval-shaped, yet a
        // dialog was shown: the backend asked. What was answered is the gate's record.
        assert_eq!(
            presenter.shown.load(Ordering::SeqCst),
            1,
            "{}",
            backend.kind().label()
        );
    }
}

#[test]
fn inbox_delivery_has_three_states_in_order() {
    let (events, _, _) = drive(&cli());
    let states: Vec<Delivery> = events
        .iter()
        .filter_map(|e| match e {
            Event::Delivery { id, state } if *id == DeliveryId(7) => Some(*state),
            _ => None,
        })
        .collect();
    assert_eq!(
        states,
        [Delivery::Enqueued, Delivery::Accepted, Delivery::Injected]
    );
}

#[test]
fn usage_not_reported_is_not_zero() {
    let (events, caps, _) = drive(&native());
    assert!(!caps.reports_usage);
    assert!(events.iter().any(|e| matches!(
        e,
        Event::Usage {
            usage: Usage::NotReported,
            ..
        }
    )));
    assert!(!events.iter().any(|e| matches!(
        e,
        Event::Usage {
            usage: Usage::Reported {
                input_tokens: 0,
                output_tokens: 0,
                ..
            },
            ..
        }
    )));
}

#[test]
fn a_session_id_carries_its_account_and_resume_takes_nothing_else() {
    let backend = cli();
    let session = SessionId::new(Backend::ClaudeCode, AccountId("acct-a".into()), "s-9");
    let events = Arc::new(Recorder::default());
    let resumed = backend
        .resume(Resume {
            conversation: ConversationId(1),
            session: session.clone(),
            workspace_root: PathBuf::from("/tmp/ws"),
            gate: gate().0,
            events: events.clone(),
        })
        .expect("resume");
    assert!(matches!(
        events.take().as_slice(),
        [Event::SessionOpened { session: s }] if s.account() == &AccountId("acct-a".into()) && s.value() == "s-9"
    ));
    resumed.terminate();
}

#[test]
fn resume_is_refused_where_capabilities_say_so_and_for_another_backends_session() {
    let events = Arc::new(Recorder::default());
    let resume = |backend: &dyn RunBackend, session: SessionId| {
        backend
            .resume(Resume {
                conversation: ConversationId(1),
                session,
                workspace_root: PathBuf::from("/tmp/ws"),
                gate: gate().0,
                events: events.clone(),
            })
            .map(|_| ())
            .unwrap_err()
    };
    assert_eq!(
        resume(
            &native(),
            SessionId::new(Backend::Native, AccountId("a".into()), "x")
        ),
        BackendError::Unsupported("resume")
    );
    assert_eq!(
        resume(
            &cli(),
            SessionId::new(Backend::Codex, AccountId("a".into()), "x")
        ),
        BackendError::UnknownSession
    );
}

#[test]
fn terminate_ends_the_consent_run() {
    let backend = cli();
    let (gate, _) = gate();
    let events = Arc::new(Recorder::default());
    let session = backend
        .start(Start {
            conversation: ConversationId(1),
            account: AccountId("acct".into()),
            workspace_root: PathBuf::from("/tmp/ws"),
            gate: gate.clone(),
            events,
        })
        .expect("start");
    let run = session.run();
    session.terminate();
    // A request bound to an ended run is refused before any dialog.
    let refused = gate.ask(RequestSpec {
        run: Some(run),
        workspace_root: PathBuf::from("/tmp/ws"),
        class: ClassSpec::CliCommand {
            cli_request_id: "late".into(),
            command: "ls".into(),
            cwd: PathBuf::from("/tmp/ws"),
            session_grant: false,
        },
    });
    assert!(refused.is_err());
}
