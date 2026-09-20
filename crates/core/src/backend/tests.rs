//! The "no branch above" half of the contract (`docs/architecture.md`, "Run backends"):
//! one driver, written against `dyn RunBackend` and `dyn Session` and never matching on
//! `Backend`, takes two independent fakes — one shaped like a CLI, one like the native
//! loop, sharing no code — through start, a turn with a tool call the backend asks the
//! gate about itself, the three inbox states, interrupt, and terminate. The compile-time
//! half — nothing on `Session` takes an answer — is a `compile_fail` doctest in
//! `src/lib.rs`. In-crate because the traits are sealed and `SessionId` is
//! backend-constructed.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::sealed::Sealed;
use super::*;
use crate::consent::policy::AlwaysAsk;
use crate::consent::presenter::{
    Answer, ConsentPresenter, Handle, PresenterError, Rendered, Responder,
};
use crate::consent::request::{ClassSpec, RequestSpec};
use crate::consent::Config;
use crate::execute::{CliApproval, Reply, ReplyTransport};

// ---------------------------------------------------------------------------------------
// Shared by the driver, not by the fakes

/// Declines everything after a zero settle, counting what it was shown.
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
            settle: Duration::ZERO,
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

fn ws() -> PathBuf {
    PathBuf::from("/tmp/ws")
}

// ---------------------------------------------------------------------------------------
// A fake shaped like a CLI backend: a "process" that reports its session id late, asks the
// gate through the CLI door, and replies to itself.

struct FakeCli;

impl Sealed for FakeCli {}

struct CliSession {
    lease: Attachment,
    account: AccountId,
    workspace_root: PathBuf,
    events: Arc<dyn EventSink>,
    open_turn: Mutex<Option<TurnId>>,
    session_known: Mutex<bool>,
    replies: Mutex<Vec<Reply>>,
    ended: Mutex<bool>,
}

impl Sealed for CliSession {}

struct Replies<'a>(&'a CliSession);

impl ReplyTransport for Replies<'_> {
    fn send(&mut self, reply: Reply) {
        self.0.replies.lock().unwrap().push(reply);
    }
}

impl RunBackend for FakeCli {
    fn kind(&self) -> Backend {
        Backend::ClaudeCode
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            resume: true,
            mid_turn_input: true,
            approvals: ApprovalReach::Delegated,
            after_interrupt: CutTurn::AsksBeforeContinuing,
            after_crash: CutTurn::MayRerun,
            measured_on: "fake 0.0",
        }
    }

    fn start(&self, start: Start) -> Result<Box<dyn Session>, BackendError> {
        Ok(Box::new(CliSession {
            lease: Attachment::open(start.gate, Backend::ClaudeCode),
            account: start.account,
            workspace_root: start.workspace_root,
            events: start.events,
            open_turn: Mutex::new(None),
            session_known: Mutex::new(false),
            replies: Mutex::new(Vec::new()),
            ended: Mutex::new(false),
        }))
    }

    fn resume(&self, resume: Resume) -> Result<Box<dyn Session>, BackendError> {
        if resume.session.backend() != Backend::ClaudeCode {
            return Err(BackendError::UnknownSession);
        }
        let s = CliSession {
            lease: Attachment::open(resume.gate, Backend::ClaudeCode),
            account: resume.session.account().clone(),
            workspace_root: resume.session.workspace_root().to_path_buf(),
            events: resume.events,
            open_turn: Mutex::new(None),
            session_known: Mutex::new(true),
            replies: Mutex::new(Vec::new()),
            ended: Mutex::new(false),
        };
        s.events.event(Event::SessionOpened {
            session: resume.session,
        });
        Ok(Box::new(s))
    }
}

impl Session for CliSession {
    fn attachment(&self) -> AttachmentId {
        self.lease.id()
    }

    fn send(&self, input: UserInput) -> Result<TurnId, BackendError> {
        if *self.ended.lock().unwrap() {
            return Err(BackendError::Ended);
        }
        // A CLI reports its session id in its init record, after the first input.
        if !std::mem::replace(&mut *self.session_known.lock().unwrap(), true) {
            self.events.event(Event::SessionOpened {
                session: SessionId::new(
                    Backend::ClaudeCode,
                    self.account.clone(),
                    self.workspace_root.clone(),
                    "cli-session-1",
                ),
            });
        }
        let turn = self.lease.next_turn();
        *self.open_turn.lock().unwrap() = Some(turn);
        self.events.event(Event::TurnStarted { turn });
        self.events.event(Event::MessagePartial {
            turn,
            text: "ok".into(),
        });
        self.events.event(Event::MessageComplete {
            turn,
            message: Message {
                role: Role::Assistant,
                text: format!("ok: {}", input.text),
            },
        });
        let call = ToolCallId("toolu_1".into());
        self.events.event(Event::ToolCall {
            turn,
            call: call.clone(),
            name: "Bash".into(),
            arguments: "ls".into(),
        });
        // The CLI delegated the call: ask the gate through the CLI door, which owes the
        // CLI one reply either way.
        let spec = RequestSpec {
            run: Some(self.lease.run()),
            workspace_root: self.workspace_root.clone(),
            class: ClassSpec::CliCommand {
                cli_request_id: call.0.clone(),
                command: "ls".into(),
                cwd: self.workspace_root.clone(),
                session_grant: false,
            },
        };
        let _ = CliApproval.resolve(self.lease.gate(), spec, &mut Replies(self));
        self.events.event(Event::ToolResult {
            turn,
            call,
            output: "denied".into(),
            is_error: true,
        });
        self.events.event(Event::Usage {
            turn,
            usage: Usage::Reported {
                input_tokens: 10,
                output_tokens: 5,
                cost: Some(EstimatedUsd { micros: 1200 }),
            },
        });
        Ok(turn)
    }

    fn deliver(&self, message: InboxMessage) -> Result<(), BackendError> {
        for state in [Delivery::Enqueued, Delivery::Accepted, Delivery::Injected] {
            self.events.event(Event::Delivery {
                id: message.id,
                state,
            });
        }
        Ok(())
    }

    fn interrupt(&self) -> Result<(), BackendError> {
        if let Some(turn) = self.open_turn.lock().unwrap().take() {
            self.events.event(Event::TurnEnded {
                turn,
                end: TurnEnd::Interrupted,
            });
        }
        Ok(())
    }

    fn terminate(&self) -> Result<(), BackendError> {
        if !std::mem::replace(&mut *self.ended.lock().unwrap(), true) {
            let ended = self.lease.end();
            self.events.event(Event::Exited {
                exit: Exit::Terminated,
                ended,
            });
        }
        Ok(())
    }

    fn usage(&self) -> Usage {
        Usage::Reported {
            input_tokens: 10,
            output_tokens: 5,
            cost: Some(EstimatedUsd { micros: 1200 }),
        }
    }
}

// ---------------------------------------------------------------------------------------
// A fake shaped like the native loop: the session id is the core's own, known at once;
// the loop asks the gate directly and would take the executor door on a token.

struct FakeNative;

impl Sealed for FakeNative {}

struct NativeSession {
    lease: Attachment,
    workspace_root: PathBuf,
    events: Arc<dyn EventSink>,
    turn: Mutex<Option<TurnId>>,
    exited: Mutex<bool>,
}

impl Sealed for NativeSession {}

impl RunBackend for FakeNative {
    fn kind(&self) -> Backend {
        Backend::Native
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            resume: false,
            mid_turn_input: false,
            approvals: ApprovalReach::Every,
            after_interrupt: CutTurn::Unmeasured,
            after_crash: CutTurn::Unmeasured,
            measured_on: "none",
        }
    }

    fn start(&self, start: Start) -> Result<Box<dyn Session>, BackendError> {
        let lease = Attachment::open(start.gate, Backend::Native);
        start.events.event(Event::SessionOpened {
            session: SessionId::new(
                Backend::Native,
                start.account,
                start.workspace_root.clone(),
                format!("native-{}", lease.id().0),
            ),
        });
        Ok(Box::new(NativeSession {
            lease,
            workspace_root: start.workspace_root,
            events: start.events,
            turn: Mutex::new(None),
            exited: Mutex::new(false),
        }))
    }

    fn resume(&self, _: Resume) -> Result<Box<dyn Session>, BackendError> {
        Err(BackendError::Unsupported("resume"))
    }
}

impl Session for NativeSession {
    fn attachment(&self) -> AttachmentId {
        self.lease.id()
    }

    fn send(&self, input: UserInput) -> Result<TurnId, BackendError> {
        if self.turn.lock().unwrap().is_some() {
            return Err(BackendError::Busy);
        }
        let turn = self.lease.next_turn();
        *self.turn.lock().unwrap() = Some(turn);
        self.events.event(Event::TurnStarted { turn });
        self.events.event(Event::MessagePartial {
            turn,
            text: "ok".into(),
        });
        self.events.event(Event::MessageComplete {
            turn,
            message: Message {
                role: Role::Assistant,
                text: format!("ok: {}", input.text),
            },
        });
        let call = ToolCallId("call-1".into());
        self.events.event(Event::ToolCall {
            turn,
            call: call.clone(),
            name: "shell".into(),
            arguments: "ls".into(),
        });
        let spec = RequestSpec {
            run: Some(self.lease.run()),
            workspace_root: self.workspace_root.clone(),
            class: ClassSpec::ShellCommand {
                command: "ls".into(),
                cwd: self.workspace_root.clone(),
                env: Vec::new(),
            },
        };
        // Declined: no token, nothing to hand the executor.
        let outcome = self.lease.gate().ask(spec);
        self.events.event(Event::ToolResult {
            turn,
            call,
            output: outcome.err().map(|e| e.to_string()).unwrap_or_default(),
            is_error: true,
        });
        self.events.event(Event::Usage {
            turn,
            usage: Usage::NotReported,
        });
        Ok(turn)
    }

    fn deliver(&self, message: InboxMessage) -> Result<(), BackendError> {
        for state in [Delivery::Enqueued, Delivery::Accepted, Delivery::Injected] {
            self.events.event(Event::Delivery {
                id: message.id,
                state,
            });
        }
        Ok(())
    }

    fn interrupt(&self) -> Result<(), BackendError> {
        if let Some(turn) = self.turn.lock().unwrap().take() {
            self.events.event(Event::TurnEnded {
                turn,
                end: TurnEnd::Interrupted,
            });
        }
        Ok(())
    }

    fn terminate(&self) -> Result<(), BackendError> {
        if !std::mem::replace(&mut *self.exited.lock().unwrap(), true) {
            let ended = self.lease.end();
            self.events.event(Event::Exited {
                exit: Exit::Terminated,
                ended,
            });
        }
        Ok(())
    }

    fn usage(&self) -> Usage {
        Usage::NotReported
    }
}

// ---------------------------------------------------------------------------------------
// The driver: everything above a backend, with no branch on which one it is

/// What the driver is left holding: the gate it handed over and the attachment id, and
/// nothing that could redeem a token or bind a request to the run.
struct Driven {
    events: Vec<Event>,
    presenter: Arc<Declines>,
    attachment: AttachmentId,
}

/// Runs one conversation through a backend. This is the shape of the code above a
/// backend: it holds trait objects and reads `Capabilities` only to decide whether to
/// offer something, never to change how an approval is handled.
fn drive(backend: &dyn RunBackend) -> Driven {
    let events = Arc::new(Recorder::default());
    let (gate, presenter) = gate();
    let session = backend
        .start(Start {
            conversation: ConversationId(1),
            account: AccountId("acct".into()),
            workspace_root: ws(),
            gate: gate.clone(),
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
    session.terminate().expect("terminate");
    session.terminate().expect("terminate is idempotent");
    let attachment = session.attachment();
    drop(session);
    drop(gate);
    Driven {
        events: events.take(),
        presenter,
        attachment,
    }
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
            Event::ToolResult { .. } => "tool_result",
            Event::ApprovalRequested { .. } => "approval_requested",
            Event::ApprovalResolved { .. } => "approval_resolved",
            Event::RanWithoutAsking { .. } => "ran_without_asking",
            Event::Delivery { .. } => "delivery",
            Event::Usage { .. } => "usage",
            Event::Diagnostic { .. } => "diagnostic",
            Event::TurnEnded { .. } => "turn_ended",
            Event::Exited { .. } => "exited",
        })
        .collect()
}

fn backends() -> [Box<dyn RunBackend>; 2] {
    [Box::new(FakeCli), Box::new(FakeNative)]
}

#[test]
fn both_backends_take_the_same_driver_and_report_the_same_shape() {
    for backend in backends() {
        let driven = drive(backend.as_ref());
        // `session_opened` may come before or after the first turn starts, per the
        // contract; everything else is in one order.
        let without_open: Vec<&str> = kinds(&driven.events)
            .into_iter()
            .filter(|k| *k != "session_opened")
            .collect();
        assert_eq!(
            without_open,
            [
                "turn_started",
                "message_partial",
                "message_complete",
                "tool_call",
                "tool_result",
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
        assert_eq!(
            driven
                .events
                .iter()
                .filter(|e| matches!(e, Event::SessionOpened { .. }))
                .count(),
            1,
            "{}: session id reported exactly once per attachment",
            backend.kind().label()
        );
    }
}

#[test]
fn the_backend_asks_the_gate_itself_and_the_driver_never_sees_an_answer() {
    for backend in backends() {
        let driven = drive(backend.as_ref());
        // The driver handed over the gate and called nothing approval-shaped, yet a
        // dialog was shown: the backend asked. What was answered is the gate's record.
        assert_eq!(
            driven.presenter.shown.load(Ordering::SeqCst),
            1,
            "{}",
            backend.kind().label()
        );
    }
}

#[test]
fn a_session_id_binds_backend_account_and_workspace() {
    let driven = drive(&FakeCli);
    let opened = driven
        .events
        .iter()
        .find_map(|e| match e {
            Event::SessionOpened { session } => Some(session.clone()),
            _ => None,
        })
        .expect("session opened");
    assert_eq!(opened.backend(), Backend::ClaudeCode);
    assert_eq!(opened.account(), &AccountId("acct".into()));
    assert_eq!(opened.workspace_root(), ws());
    assert_eq!(opened.value(), "cli-session-1");
}

#[test]
fn resume_takes_the_stored_session_and_nothing_else_that_names_an_account() {
    let stored = SessionId::new(Backend::ClaudeCode, AccountId("acct-a".into()), ws(), "s-9");
    let events = Arc::new(Recorder::default());
    let resumed = FakeCli
        .resume(Resume {
            conversation: ConversationId(1),
            session: stored.clone(),
            gate: gate().0,
            events: events.clone(),
        })
        .expect("resume");
    assert_eq!(
        events.take(),
        [Event::SessionOpened { session: stored }],
        "the resumed attachment reports the identifier it resumed, account and all"
    );
    resumed.terminate().unwrap();
}

#[test]
fn resume_is_refused_where_capabilities_say_so_and_for_another_backends_session() {
    let events: Arc<dyn EventSink> = Arc::new(Recorder::default());
    let resume = |backend: &dyn RunBackend, session: SessionId| {
        backend
            .resume(Resume {
                conversation: ConversationId(1),
                session,
                gate: gate().0,
                events: events.clone(),
            })
            .map(|_| ())
            .unwrap_err()
    };
    assert!(!FakeNative.capabilities().resume);
    assert_eq!(
        resume(
            &FakeNative,
            SessionId::new(Backend::Native, AccountId("a".into()), ws(), "x")
        ),
        BackendError::Unsupported("resume")
    );
    assert_eq!(
        resume(
            &FakeCli,
            SessionId::new(Backend::Codex, AccountId("a".into()), ws(), "x")
        ),
        BackendError::UnknownSession
    );
}

#[test]
fn inbox_delivery_has_three_states_in_order_on_both() {
    for backend in backends() {
        let driven = drive(backend.as_ref());
        let states: Vec<Delivery> = driven
            .events
            .iter()
            .filter_map(|e| match e {
                Event::Delivery { id, state } if *id == DeliveryId(7) => Some(*state),
                _ => None,
            })
            .collect();
        assert_eq!(
            states,
            [Delivery::Enqueued, Delivery::Accepted, Delivery::Injected],
            "{}",
            backend.kind().label()
        );
    }
}

#[test]
fn usage_not_reported_is_a_variant_not_a_zero() {
    let driven = drive(&FakeNative);
    assert!(driven.events.iter().any(|e| matches!(
        e,
        Event::Usage {
            usage: Usage::NotReported,
            ..
        }
    )));
    let driven = drive(&FakeCli);
    assert!(driven.events.iter().any(|e| matches!(
        e,
        Event::Usage {
            usage: Usage::Reported {
                cost: Some(EstimatedUsd { micros: 1200 }),
                ..
            },
            ..
        }
    )));
}

#[test]
fn turn_ids_are_unique_across_attachments() {
    let a = drive(&FakeCli);
    let b = drive(&FakeCli);
    let turn = |d: &Driven| {
        d.events.iter().find_map(|e| match e {
            Event::TurnStarted { turn } => Some(*turn),
            _ => None,
        })
    };
    assert_ne!(a.attachment, b.attachment);
    assert_ne!(turn(&a), turn(&b));
}

#[test]
fn the_lease_ends_the_consent_run_on_terminate_and_on_drop() {
    for terminate in [true, false] {
        let (gate, _) = gate();
        let session = FakeCli
            .start(Start {
                conversation: ConversationId(1),
                account: AccountId("acct".into()),
                workspace_root: ws(),
                gate: gate.clone(),
                events: Arc::new(Recorder::default()),
            })
            .unwrap();
        // The session exposes no `RunId`; in-crate, the gate is fresh and `start` registered
        // its one run first, so this is the lease's run. Read the lease's effect through it:
        // a request under the run is refused only once the run has ended.
        let run = RunId(1);
        let ask = || {
            gate.ask(RequestSpec {
                run: Some(run),
                workspace_root: ws(),
                class: ClassSpec::CliCommand {
                    cli_request_id: "p".into(),
                    command: "ls".into(),
                    cwd: ws(),
                    session_grant: false,
                },
            })
            .err()
        };
        assert!(!matches!(
            ask(),
            Some(crate::consent::policy::Refusal::UnknownRun)
        ));
        if terminate {
            // Ended by `terminate` alone, while the session value is still held.
            session.terminate().unwrap();
            assert_eq!(ask(), Some(crate::consent::policy::Refusal::UnknownRun));
        }
        drop(session);
        assert_eq!(ask(), Some(crate::consent::policy::Refusal::UnknownRun));
    }
}

#[test]
fn send_is_refused_mid_turn_without_the_capability_and_after_the_end() {
    let (gate, _) = gate();
    let start = |gate: &Arc<Consent>| Start {
        conversation: ConversationId(1),
        account: AccountId("acct".into()),
        workspace_root: ws(),
        gate: gate.clone(),
        events: Arc::new(Recorder::default()),
    };
    let input = || UserInput {
        text: "hello".into(),
    };

    assert!(!FakeNative.capabilities().mid_turn_input);
    let native = FakeNative.start(start(&gate)).unwrap();
    native.send(input()).unwrap();
    assert_eq!(native.send(input()).unwrap_err(), BackendError::Busy);

    assert!(FakeCli.capabilities().mid_turn_input);
    let cli = FakeCli.start(start(&gate)).unwrap();
    cli.send(input()).unwrap();
    cli.send(input()).expect("a second input joins the turn");
    cli.terminate().unwrap();
    assert_eq!(cli.send(input()).unwrap_err(), BackendError::Ended);
}
