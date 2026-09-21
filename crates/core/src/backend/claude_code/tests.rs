//! The supervisor against the fake `claude` in `crates/core/tests/fixtures/fake-claude.sh`:
//! a shell script that speaks the measured stream-json shapes and nothing else. No test
//! here runs the real binary, signs in, or reaches the network. In-crate because the
//! traits are sealed and `SessionId` is backend-constructed.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::*;
use crate::backend::{ConversationId, DeliveryId, RunEnded};
use crate::consent::policy::AlwaysAsk;
use crate::consent::presenter::{
    Answer, ConsentPresenter, Handle, PresenterError, Rendered, Responder,
};
use crate::consent::{Config, Consent};

const FAKE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake-claude.sh");
const SESSION: &str = "00000000-0000-0000-0000-000000000000";
const WAIT: Duration = Duration::from_secs(10);

#[derive(Default)]
struct Declines;

impl ConsentPresenter for Declines {
    fn capacity(&self) -> usize {
        1 << 16
    }
    fn show(&self, _: &Rendered, responder: Responder) -> Result<Handle, PresenterError> {
        responder.answer(Answer::Decline);
        Ok(Handle(1))
    }
    fn dismiss(&self, _: Handle) {}
}

fn gate() -> Arc<Consent> {
    Arc::new(Consent::new(
        Arc::new(Declines),
        Arc::new(AlwaysAsk),
        Config {
            settle: Duration::ZERO,
            ..Config::default()
        },
    ))
}

/// Records events and lets a test wait for one.
#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<Event>>,
    changed: Condvar,
}

impl EventSink for Recorder {
    fn event(&self, event: Event) {
        self.events.lock().unwrap().push(event);
        self.changed.notify_all();
    }
}

impl Recorder {
    /// Waits until an event matching `pred` has arrived, then returns everything so far.
    fn wait_for(&self, what: &str, pred: impl Fn(&Event) -> bool) -> Vec<Event> {
        let deadline = Instant::now() + WAIT;
        let mut events = self.events.lock().unwrap();
        while !events.iter().any(&pred) {
            let now = Instant::now();
            assert!(now < deadline, "no {what} within {WAIT:?}; got {events:#?}");
            events = self.changed.wait_timeout(events, deadline - now).unwrap().0;
        }
        events.clone()
    }

    fn all(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }
}

static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

/// A fresh workspace and config root, side by side, under a temporary directory.
struct Dirs {
    base: PathBuf,
    workspace: PathBuf,
    root: PathBuf,
    state: PathBuf,
}

impl Dirs {
    fn new(name: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "stanchion-claude-code-{name}-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::SeqCst)
        ));
        let workspace = base.join("ws");
        let root = base.join("cfg");
        std::fs::create_dir_all(&workspace).unwrap();
        // Resolved, as a `SessionId` reports it (`temp_dir` is a symlink on macOS).
        let workspace = workspace.canonicalize().unwrap();
        Dirs {
            state: base.join("state.log"),
            base,
            workspace,
            root,
        }
    }

    fn state(&self) -> String {
        std::fs::read_to_string(&self.state).unwrap_or_default()
    }
}

impl Drop for Dirs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn backend(dirs: &Dirs) -> ClaudeCode {
    ClaudeCode::new(FAKE, ConfigRoot::new(&dirs.root).unwrap())
        .env("FAKE_CLAUDE_STATE", dirs.state.to_str().unwrap())
}

fn account() -> AccountId {
    AccountId("alice@example.com".into())
}

fn start(backend: &ClaudeCode, dirs: &Dirs, events: &Arc<Recorder>) -> Box<dyn Session> {
    backend
        .start(Start {
            conversation: ConversationId(1),
            account: account(),
            workspace_root: dirs.workspace.clone(),
            gate: gate(),
            events: events.clone(),
        })
        .unwrap()
}

fn kinds(events: &[Event]) -> Vec<&'static str> {
    events
        .iter()
        .map(|e| match e {
            Event::SessionOpened { .. } => "SessionOpened",
            Event::TurnStarted { .. } => "TurnStarted",
            Event::MessagePartial { .. } => "MessagePartial",
            Event::MessageComplete { .. } => "MessageComplete",
            Event::ToolCall { .. } => "ToolCall",
            Event::ToolResult { .. } => "ToolResult",
            Event::ApprovalRequested { .. } => "ApprovalRequested",
            Event::ApprovalResolved { .. } => "ApprovalResolved",
            Event::RanWithoutAsking { .. } => "RanWithoutAsking",
            Event::Delivery { .. } => "Delivery",
            Event::Usage { .. } => "Usage",
            Event::Diagnostic { .. } => "Diagnostic",
            Event::TurnEnded { .. } => "TurnEnded",
            Event::Exited { .. } => "Exited",
        })
        .collect()
}

fn exited(events: &[Event]) -> Option<(&Exit, &RunEnded)> {
    events.iter().find_map(|e| match e {
        Event::Exited { exit, ended } => Some((exit, ended)),
        _ => None,
    })
}

fn is_exited(e: &Event) -> bool {
    matches!(e, Event::Exited { .. })
}

fn is_turn_ended(e: &Event) -> bool {
    matches!(e, Event::TurnEnded { .. })
}

// ---------------------------------------------------------------------------------------

#[test]
fn one_turn_over_the_wire() {
    let dirs = Dirs::new("turn");
    let events = Arc::new(Recorder::default());
    let backend = backend(&dirs);
    let session = start(&backend, &dirs, &events);
    assert_eq!(session.usage(), Usage::NotReported);

    // The process is up but has said nothing: the session id is not known before the
    // first input, as measured.
    let turn = session.send(UserInput { text: "hi".into() }).unwrap();
    let got = events.wait_for("TurnEnded", is_turn_ended);
    assert_eq!(
        kinds(&got),
        [
            "TurnStarted",
            "Diagnostic", // init
            "SessionOpened",
            "MessagePartial",
            "MessagePartial",
            "ToolCall",
            "ToolCall",
            "MessageComplete",
            "ToolResult",
            "ToolResult",
            "Usage",
            "RanWithoutAsking",
            "TurnEnded",
        ]
    );
    assert_eq!(got[0], Event::TurnStarted { turn });
    let Event::SessionOpened { session: id } = &got[2] else {
        unreachable!()
    };
    assert_eq!(id.backend(), Backend::ClaudeCode);
    assert_eq!(id.account(), &account());
    assert_eq!(id.workspace_root(), dirs.workspace);
    assert_eq!(id.value(), SESSION);
    assert_eq!(
        got[3],
        Event::MessagePartial {
            turn,
            text: "pon".into()
        }
    );
    assert_eq!(
        got[7],
        Event::MessageComplete {
            turn,
            message: Message {
                role: Role::Assistant,
                text: "pong".into()
            }
        }
    );
    assert_eq!(
        got[5],
        Event::ToolCall {
            turn,
            call: ToolCallId("toolu_ran".into()),
            name: "Read".into(),
            arguments: r#"{"file_path":"a.txt"}"#.into(),
        }
    );
    assert_eq!(
        got[8],
        Event::ToolResult {
            turn,
            call: ToolCallId("toolu_ran".into()),
            output: "hello\n<b>".into(),
            is_error: false,
        }
    );
    assert_eq!(
        got[9],
        Event::ToolResult {
            turn,
            call: ToolCallId("toolu_denied".into()),
            output: "Permission to use Bash has been denied.".into(),
            is_error: true,
        }
    );
    let usage = Usage::Reported {
        input_tokens: 10,
        output_tokens: 5,
        cost: Some(EstimatedUsd { micros: 12_500 }),
    };
    assert_eq!(got[10], Event::Usage { turn, usage });
    // The CLI's own record says which call it refused; the other ran, and nothing asked.
    assert_eq!(
        got[11],
        Event::RanWithoutAsking {
            turn,
            call: ToolCallId("toolu_ran".into()),
            name: "Read".into(),
            arguments: r#"{"file_path":"a.txt"}"#.into(),
        }
    );
    assert_eq!(
        got[12],
        Event::TurnEnded {
            turn,
            end: TurnEnd::Completed
        }
    );
    assert_eq!(session.usage(), usage);

    // A second turn accumulates.
    let turn2 = session
        .send(UserInput {
            text: "again".into(),
        })
        .unwrap();
    assert!(turn2 > turn);
    let got = events.wait_for(
        "second TurnEnded",
        |e| matches!(e, Event::TurnEnded { turn, .. } if *turn == turn2),
    );
    assert_eq!(
        got.iter()
            .filter(|e| matches!(e, Event::SessionOpened { .. }))
            .count(),
        1
    );
    assert_eq!(
        session.usage(),
        Usage::Reported {
            input_tokens: 20,
            output_tokens: 10,
            cost: Some(EstimatedUsd { micros: 25_000 }),
        }
    );

    session.terminate().unwrap();
    session.terminate().unwrap();
    let got = events.wait_for("Exited", is_exited);
    assert_eq!(got.last().map(kinds_one), Some("Exited"));
    let (exit, ended) = exited(&got).unwrap();
    assert_eq!(*exit, Exit::Terminated);
    assert_eq!(ended.attachment(), session.attachment());
    assert_eq!(
        session.send(UserInput { text: "x".into() }),
        Err(BackendError::Ended)
    );

    // What the fake was given: the per-account directory under the root, 0700, and the
    // flags the measurements were made with.
    let state = dirs.state();
    let config_dir = dirs
        .root
        .canonicalize()
        .unwrap()
        .join("alice%40example%2Ecom");
    assert!(
        state.contains(&format!("config_dir={}", config_dir.display())),
        "{state}"
    );
    assert!(config_dir.is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&config_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&dirs.root).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    assert!(state.contains(
        "args=-p --output-format stream-json --input-format stream-json --verbose --include-partial-messages --setting-sources user\n"
    ), "{state}");
    assert!(!state.contains("--resume"));
}

fn kinds_one(e: &Event) -> &'static str {
    kinds(std::slice::from_ref(e))[0]
}

#[test]
fn init_may_arrive_before_the_first_input() {
    let dirs = Dirs::new("init-first");
    let events = Arc::new(Recorder::default());
    let backend = backend(&dirs).env("FAKE_CLAUDE_INIT_FIRST", "1");
    let session = start(&backend, &dirs, &events);
    let got = events.wait_for("SessionOpened", |e| {
        matches!(e, Event::SessionOpened { .. })
    });
    assert_eq!(kinds(&got), ["Diagnostic", "SessionOpened"]);
    drop(session);
    let got = events.wait_for("Exited", is_exited);
    assert_eq!(exited(&got).unwrap().0, &Exit::Terminated);
}

#[test]
fn busy_while_a_turn_is_open_and_the_inbox_waits_for_it() {
    let dirs = Dirs::new("busy");
    let events = Arc::new(Recorder::default());
    let backend = backend(&dirs);
    let session = start(&backend, &dirs, &events);

    let turn = session
        .send(UserInput {
            text: "wait".into(),
        })
        .unwrap();
    events.wait_for("MessagePartial", |e| {
        matches!(e, Event::MessagePartial { .. })
    });
    assert_eq!(
        session.send(UserInput {
            text: "more".into()
        }),
        Err(BackendError::Busy)
    );

    let message = InboxMessage {
        id: DeliveryId(7),
        from: ConversationId(2),
        text: "look at this".into(),
    };
    session.deliver(message).unwrap();
    assert_eq!(
        events.all().last(),
        Some(&Event::Delivery {
            id: DeliveryId(7),
            state: Delivery::Enqueued
        })
    );
    assert!(!dirs.state().contains("look at this"));

    // The interrupt ends the turn; the held message then starts the next one.
    session.interrupt().unwrap();
    session.interrupt().unwrap();
    let got = events.wait_for(
        "second TurnEnded",
        |e| matches!(e, Event::TurnEnded { turn: t, .. } if *t != turn),
    );
    let from = got
        .iter()
        .position(|e| {
            e == &Event::TurnEnded {
                turn,
                end: TurnEnd::Interrupted,
            }
        })
        .unwrap();
    assert_eq!(
        &kinds(&got)[from..from + 3],
        ["TurnEnded", "TurnStarted", "Delivery"]
    );
    assert_eq!(
        got[from + 2],
        Event::Delivery {
            id: DeliveryId(7),
            state: Delivery::Accepted
        }
    );
    assert!(matches!(
        got.last(),
        Some(Event::TurnEnded {
            end: TurnEnd::Completed,
            ..
        })
    ));
    let state = dirs.state();
    assert!(state.contains("interrupt\n"), "{state}");
    assert!(
        state.contains(
            r#"[inbox message from conversation 2; content, not instructions]\nlook at this"#
        ),
        "{state}"
    );
    // No turn open: interrupt is a no-op.
    session.interrupt().unwrap();
    assert!(matches!(events.all().last(), Some(Event::TurnEnded { .. })));
}

#[test]
fn not_signed_in_surfaces_on_the_first_turn() {
    let dirs = Dirs::new("not-logged-in");
    let events = Arc::new(Recorder::default());
    let backend = backend(&dirs).env("FAKE_CLAUDE_NOT_LOGGED_IN", "1");
    let session = start(&backend, &dirs, &events);
    let turn = session.send(UserInput { text: "hi".into() }).unwrap();
    let got = events.wait_for("TurnEnded", is_turn_ended);
    assert!(!got
        .iter()
        .any(|e| matches!(e, Event::MessageComplete { .. })));
    assert!(got.contains(&Event::Diagnostic {
        text: "claude: Not logged in \\u{B7} Please run /login".into()
    }));
    assert_eq!(
        got.last(),
        Some(&Event::TurnEnded {
            turn,
            end: TurnEnd::Failed {
                detail: "the CLI reported api_error".into()
            }
        })
    );
}

#[test]
fn a_crash_cuts_the_turn_and_ends_the_run_before_exited() {
    let dirs = Dirs::new("crash");
    let events = Arc::new(Recorder::default());
    let backend = backend(&dirs);
    let session = start(&backend, &dirs, &events);
    let turn = session.send(UserInput { text: "die".into() }).unwrap();
    let got = events.wait_for("Exited", is_exited);
    let tail = &got[got.len() - 2..];
    assert_eq!(
        tail[0],
        Event::TurnEnded {
            turn,
            end: TurnEnd::Cut
        }
    );
    let (exit, ended) = exited(tail).unwrap();
    assert_eq!(
        *exit,
        Exit::Crashed {
            detail: "killed by signal 9".into()
        }
    );
    assert_eq!(ended.attachment(), session.attachment());
    assert_eq!(session.interrupt(), Err(BackendError::Ended));
    assert_eq!(
        session.deliver(InboxMessage {
            id: DeliveryId(1),
            from: ConversationId(1),
            text: String::new()
        }),
        Err(BackendError::Ended)
    );
    session.terminate().unwrap();
    assert_eq!(events.all().len(), got.len(), "nothing follows Exited");
}

#[test]
fn a_clean_exit_reports_the_status() {
    let dirs = Dirs::new("exit");
    let events = Arc::new(Recorder::default());
    let backend = backend(&dirs).env("FAKE_CLAUDE_EXIT_AFTER", "1");
    let session = start(&backend, &dirs, &events);
    session.send(UserInput { text: "hi".into() }).unwrap();
    let got = events.wait_for("Exited", is_exited);
    assert_eq!(exited(&got).unwrap().0, &Exit::Exited { status: Some(3) });
    assert!(matches!(
        got[got.len() - 2],
        Event::TurnEnded {
            end: TurnEnd::Completed,
            ..
        }
    ));
}

#[test]
fn stderr_is_a_diagnostic() {
    let dirs = Dirs::new("stderr");
    let events = Arc::new(Recorder::default());
    let backend = backend(&dirs).env("FAKE_CLAUDE_STDERR", "warn: é\u{202E}x");
    let _session = start(&backend, &dirs, &events);
    let got = events.wait_for("Diagnostic", |e| matches!(e, Event::Diagnostic { .. }));
    assert_eq!(
        got[0],
        Event::Diagnostic {
            text: "stderr: warn: \\u{E9}\\u{202E}x".into()
        }
    );
}

#[test]
fn resume_reattaches_under_the_stored_account_and_workspace() {
    let dirs = Dirs::new("resume");
    let events = Arc::new(Recorder::default());
    let backend = backend(&dirs);
    let stored = SessionId::new(
        Backend::ClaudeCode,
        account(),
        dirs.workspace.clone(),
        SESSION,
    );
    let session = backend
        .resume(Resume {
            conversation: ConversationId(1),
            session: stored.clone(),
            gate: gate(),
            events: events.clone(),
        })
        .unwrap();
    // Reported at once, from what was stored.
    assert_eq!(events.all(), [Event::SessionOpened { session: stored }]);
    session.send(UserInput { text: "hi".into() }).unwrap();
    let got = events.wait_for("TurnEnded", is_turn_ended);
    // The init record repeats the id: not a second SessionOpened.
    assert_eq!(
        got.iter()
            .filter(|e| matches!(e, Event::SessionOpened { .. }))
            .count(),
        1
    );
    assert!(dirs
        .state()
        .contains(&format!("--setting-sources user --resume {SESSION}\n")));

    // Another backend's id is refused before anything is spawned.
    let codex = SessionId::new(Backend::Codex, account(), dirs.workspace.clone(), "x");
    assert!(matches!(
        backend
            .resume(Resume {
                conversation: ConversationId(1),
                session: codex,
                gate: gate(),
                events: events.clone(),
            })
            .map(|_| ()),
        Err(BackendError::UnknownSession)
    ));
}

#[test]
fn an_init_that_names_another_session_is_reported_not_adopted() {
    let dirs = Dirs::new("resume-mismatch");
    let events = Arc::new(Recorder::default());
    let backend = backend(&dirs);
    let stored = SessionId::new(
        Backend::ClaudeCode,
        account(),
        dirs.workspace.clone(),
        "other",
    );
    let session = backend
        .resume(Resume {
            conversation: ConversationId(1),
            session: stored,
            gate: gate(),
            events: events.clone(),
        })
        .unwrap();
    session.send(UserInput { text: "hi".into() }).unwrap();
    let got = events.wait_for("TurnEnded", is_turn_ended);
    assert_eq!(
        got.iter()
            .filter(|e| matches!(e, Event::SessionOpened { .. }))
            .count(),
        1
    );
    assert!(got.contains(&Event::Diagnostic {
        text: format!("init reported session {SESSION} but this attachment is other")
    }));
}

#[test]
fn a_config_root_inside_the_workspace_or_around_it_is_refused() {
    let dirs = Dirs::new("overlap");
    let events: Arc<Recorder> = Arc::new(Recorder::default());
    let inside = ClaudeCode::new(
        FAKE,
        ConfigRoot::new(dirs.workspace.join(".claude-accounts")).unwrap(),
    );
    let err = inside
        .start(Start {
            conversation: ConversationId(1),
            account: account(),
            workspace_root: dirs.workspace.clone(),
            gate: gate(),
            events: events.clone(),
        })
        .map(|_| ())
        .unwrap_err();
    assert!(
        matches!(err, BackendError::CannotStart(ref why) if why.contains("overlap")),
        "{err}"
    );

    let around = ClaudeCode::new(FAKE, ConfigRoot::new(&dirs.base).unwrap());
    let err = around
        .start(Start {
            conversation: ConversationId(1),
            account: account(),
            workspace_root: dirs.workspace.clone(),
            gate: gate(),
            events: events.clone(),
        })
        .map(|_| ())
        .unwrap_err();
    assert!(
        matches!(err, BackendError::CannotStart(ref why) if why.contains("overlap")),
        "{err}"
    );
    assert!(events.all().is_empty());

    // A workspace root that is a symlink into the config root is still caught: a
    // workspace beside the account directories reaches them by `..`.
    #[cfg(unix)]
    {
        let link = dirs.base.join("link");
        std::fs::create_dir_all(dirs.root.join("ws-alias")).unwrap();
        std::os::unix::fs::symlink(dirs.root.join("ws-alias"), &link).unwrap();
        let err = backend(&dirs)
            .start(Start {
                conversation: ConversationId(1),
                account: account(),
                workspace_root: link,
                gate: gate(),
                events: events.clone(),
            })
            .map(|_| ())
            .unwrap_err();
        assert!(
            matches!(err, BackendError::CannotStart(ref why) if why.contains("overlap")),
            "{err}"
        );
    }
}

#[cfg(unix)]
#[test]
fn an_existing_root_is_tightened_to_0700() {
    use std::os::unix::fs::PermissionsExt as _;
    let dirs = Dirs::new("tighten");
    std::fs::create_dir_all(&dirs.root).unwrap();
    std::fs::set_permissions(&dirs.root, std::fs::Permissions::from_mode(0o755)).unwrap();
    let root = ConfigRoot::new(&dirs.root).unwrap();
    assert_eq!(
        std::fs::metadata(root.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_account_directory_is_refused() {
    let dirs = Dirs::new("symlink-account");
    let root = ConfigRoot::new(&dirs.root).unwrap();
    std::os::unix::fs::symlink(&dirs.workspace, dirs.root.join("alice%40example%2Ecom")).unwrap();
    let err = ClaudeCode::new(FAKE, root)
        .start(Start {
            conversation: ConversationId(1),
            account: account(),
            workspace_root: dirs.workspace.clone(),
            gate: gate(),
            events: Arc::new(Recorder::default()),
        })
        .map(|_| ())
        .unwrap_err();
    assert!(
        matches!(err, BackendError::CannotStart(ref why) if why.contains("symlink")),
        "{err}"
    );
}

/// A sink that reads the session from inside `event`: allowed for `usage` and
/// `terminate`, which take only the state lock.
struct Reentrant {
    inner: Recorder,
    session: Mutex<Option<Arc<dyn Session>>>,
    usages: Mutex<Vec<Usage>>,
}

impl EventSink for Reentrant {
    fn event(&self, event: Event) {
        let session = self.session.lock().unwrap().clone();
        if let Some(session) = session {
            self.usages.lock().unwrap().push(session.usage());
            if matches!(event, Event::TurnEnded { .. }) {
                session.terminate().unwrap();
            }
        }
        self.inner.event(event);
    }
}

#[test]
fn a_sink_may_read_usage_and_terminate_from_inside_event() {
    let dirs = Dirs::new("reentrant");
    let events = Arc::new(Reentrant {
        inner: Recorder::default(),
        session: Mutex::new(None),
        usages: Mutex::new(Vec::new()),
    });
    let backend = backend(&dirs);
    let session: Arc<dyn Session> = Arc::from(
        backend
            .start(Start {
                conversation: ConversationId(1),
                account: account(),
                workspace_root: dirs.workspace.clone(),
                gate: gate(),
                events: events.clone(),
            })
            .unwrap(),
    );
    *events.session.lock().unwrap() = Some(session.clone());
    session.send(UserInput { text: "hi".into() }).unwrap();
    let got = events.inner.wait_for("Exited", is_exited);
    assert_eq!(exited(&got).unwrap().0, &Exit::Terminated);
    let usages = events.usages.lock().unwrap();
    assert_eq!(usages.first(), Some(&Usage::NotReported));
    assert!(matches!(
        usages.last(),
        Some(Usage::Reported {
            input_tokens: 10,
            ..
        })
    ));
    drop(usages);
    // The sink holds the last reference; release it so the session drops here, off the
    // reader thread.
    events.session.lock().unwrap().take();
    drop(session);
}

#[test]
fn a_missing_binary_cannot_start() {
    let dirs = Dirs::new("missing");
    let backend = ClaudeCode::new(
        dirs.base.join("no-such-claude"),
        ConfigRoot::new(&dirs.root).unwrap(),
    );
    let err = backend
        .start(Start {
            conversation: ConversationId(1),
            account: account(),
            workspace_root: dirs.workspace.clone(),
            gate: gate(),
            events: Arc::new(Recorder::default()),
        })
        .map(|_| ())
        .unwrap_err();
    assert!(
        matches!(err, BackendError::CannotStart(ref why) if why.starts_with("spawn ")),
        "{err}"
    );
}

#[test]
fn directory_names_are_injective_and_have_no_separators() {
    assert_eq!(dir_name("alice"), "alice");
    assert_eq!(dir_name("Alice"), "%41lice");
    assert_ne!(dir_name("Alice").to_lowercase(), dir_name("alice"));
    assert_eq!(dir_name("a b/../c"), "a%20b%2F%2E%2E%2Fc");
    assert_eq!(dir_name("é"), "%C3%A9");
    assert_ne!(dir_name("a%2F"), dir_name("a/"));
}

#[test]
fn the_user_line_is_the_shape_the_cli_reads() {
    assert_eq!(
        user_line("hi \"there\"\n"),
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"hi \"there\"\n"}]}}"#
    );
    assert_eq!(
        interrupt_line(4),
        r#"{"type":"control_request","request_id":"interrupt-4","request":{"subtype":"interrupt"}}"#
    );
}
