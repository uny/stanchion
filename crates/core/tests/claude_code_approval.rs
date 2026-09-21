//! The approval path end to end: the fake `claude` spawns the built prompt helper as the
//! real CLI would, the helper relays over the attachment's socket, the gate answers, and
//! the reply decides what the fake reports. The in-crate tests drive the socket directly;
//! this is the one place the helper binary itself runs, since `CARGO_BIN_EXE_*` exists
//! only for integration tests.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use stanchion_core::backend::claude_code::{ClaudeCode, ConfigRoot, SocketDir};
use stanchion_core::backend::{
    AccountId, ConversationId, Event, EventSink, RunBackend, Session, Start, UserInput,
};
use stanchion_core::consent::policy::AlwaysAsk;
use stanchion_core::consent::presenter::{
    Answer, ConsentPresenter, Handle, PresenterError, Rendered, Responder,
};
use stanchion_core::consent::{Config, Consent};

const FAKE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake-claude.sh");
const HELPER: &str = env!("CARGO_BIN_EXE_stanchion-prompt-helper");
const WAIT: Duration = Duration::from_secs(20);

struct Answers(Answer);

impl ConsentPresenter for Answers {
    fn capacity(&self) -> usize {
        1 << 16
    }
    fn show(&self, _: &Rendered, responder: Responder) -> Result<Handle, PresenterError> {
        responder.answer(self.0);
        Ok(Handle(1))
    }
    fn dismiss(&self, _: Handle) {}
}

fn gate(answer: Answer) -> Arc<Consent> {
    Arc::new(Consent::new(
        Arc::new(Answers(answer)),
        Arc::new(AlwaysAsk),
        Config {
            settle: Duration::ZERO,
            ..Config::default()
        },
    ))
}

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
}

static NEXT: AtomicU64 = AtomicU64::new(1);

struct Dirs {
    base: PathBuf,
    workspace: PathBuf,
    sockets: PathBuf,
}

impl Dirs {
    fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let base =
            std::env::temp_dir().join(format!("stanchion-approval-{}-{n}", std::process::id()));
        let sockets = std::env::temp_dir().join(format!("ska{}-{n}", std::process::id()));
        let workspace = base.join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        Dirs {
            workspace: workspace.canonicalize().unwrap(),
            base,
            sockets,
        }
    }
}

impl Drop for Dirs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
        let _ = std::fs::remove_dir_all(&self.sockets);
    }
}

/// One held turn of the fake with the helper in the loop; returns every event up to the
/// turn's end.
fn one_turn(answer: Answer) -> Vec<Event> {
    // Read by the fake; the same for every test in this binary.
    std::env::set_var("FAKE_CLAUDE_ASK", "1");
    let dirs = Dirs::new();
    let backend = ClaudeCode::new(
        FAKE,
        ConfigRoot::new(dirs.base.join("cfg")).unwrap(),
        HELPER,
        SocketDir::new(&dirs.sockets).unwrap(),
    );
    let events = Arc::new(Recorder::default());
    let session: Box<dyn Session> = backend
        .start(Start {
            conversation: ConversationId(1),
            account: AccountId("alice@example.com".into()),
            workspace_root: dirs.workspace.clone(),
            gate: gate(answer),
            events: events.clone(),
        })
        .unwrap();
    session.send(UserInput { text: "hi".into() }).unwrap();
    let all = events.wait_for("TurnEnded", |e| matches!(e, Event::TurnEnded { .. }));
    drop(session);
    all
}

fn ran_without_asking(events: &[Event]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::RanWithoutAsking { call, .. } => Some(call.0.as_str()),
            _ => None,
        })
        .collect()
}

fn result_of<'a>(events: &'a [Event], id: &str) -> (&'a str, bool) {
    events
        .iter()
        .find_map(|e| match e {
            Event::ToolResult {
                call,
                output,
                is_error,
                ..
            } if call.0 == id => Some((output.as_str(), *is_error)),
            _ => None,
        })
        .unwrap()
}

#[test]
fn a_decline_through_the_helper_is_the_clis_denial() {
    let all = one_turn(Answer::Decline);
    let requested = all
        .iter()
        .find_map(|e| match e {
            Event::ApprovalRequested { call, rendered, .. } => Some((call, rendered)),
            _ => None,
        })
        .expect("ApprovalRequested");
    assert_eq!(requested.0 .0, "toolu_denied");
    assert!(requested.1.body.contains("rm -rf /"));
    assert!(all
        .iter()
        .any(|e| matches!(e, Event::ApprovalResolved { allowed: false, .. })));
    assert_eq!(
        result_of(&all, "toolu_denied"),
        ("stanchion: declined", true)
    );
    assert_eq!(ran_without_asking(&all), ["toolu_ran"]);
}

#[test]
fn an_allow_through_the_helper_is_the_execution() {
    let all = one_turn(Answer::Allow);
    assert!(all
        .iter()
        .any(|e| matches!(e, Event::ApprovalResolved { allowed: true, .. })));
    assert_eq!(result_of(&all, "toolu_denied"), ("(ran)", false));
    // Allowed through the gate, so not "without asking"; the auto-allowed `Read` still is.
    assert_eq!(ran_without_asking(&all), ["toolu_ran"]);
}

#[test]
fn the_helper_denies_on_its_own_when_the_core_is_unreachable() {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let mut helper = Command::new(HELPER)
        .arg("/nonexistent/stanchion.sock")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    helper
        .stdin
        .take()
        .unwrap()
        .write_all(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list"}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"approve","arguments":{"tool_name":"Bash","input":{"command":"ls"},"tool_use_id":"toolu_1"}}}
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"other","arguments":{}}}
"#,
        )
        .unwrap();
    let out = helper.wait_with_output().unwrap();
    assert!(out.status.success());
    let lines: Vec<&str> = std::str::from_utf8(&out.stdout).unwrap().lines().collect();
    assert_eq!(lines.len(), 4, "{lines:#?}");
    assert!(
        lines[0].contains(r#""protocolVersion":"2025-11-25""#),
        "{}",
        lines[0]
    );
    assert!(lines[1].contains(r#""name":"approve""#), "{}", lines[1]);
    assert!(
        lines[2].contains(r#"{\"behavior\":\"deny\",\"message\":\"stanchion: core unreachable"#),
        "{}",
        lines[2]
    );
    assert!(lines[3].contains(r#""code":-32602"#), "{}", lines[3]);
}
