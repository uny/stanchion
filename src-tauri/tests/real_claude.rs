//! The shell's assembly against the real `claude`, off by default.
//!
//! Everything the shell resolves at startup (`assembly`) is exercised here on the machine
//! the application runs on: the binary it found, a config root, the socket directory, and
//! the helper — the application's own binary in `--prompt-helper` mode, which is what
//! `assembly::helper` resolves when the application is what is running. It cannot be
//! `assembly::helper()` here: that returns `current_exe`, which under `cargo test` is the
//! test binary, and a test binary has no helper mode — the CLI then reports the prompt
//! tool as not found and denies the call for a reason that is the harness's, not the
//! gate's.
//!
//! What the gate does with the call is the presenter's. The application ships the native
//! alert, which needs a running application's main thread and a person to press it; this
//! harness has neither, so it runs with [`FailClosed`], and pins the refusal path every
//! request too long for the alert also takes: a presenter of capacity zero refuses every
//! request *before* it is presented — the gate checks capacity ahead of the observer
//! (`Consent::ask_observed`), so no `ApprovalRequested` and no `ApprovalResolved` are
//! emitted at all. The refusal surfaces as a `Diagnostic` naming the call and the reason,
//! and as the CLI's own error `ToolResult`; the call does not run, and it is not reported
//! as `RanWithoutAsking`. That is what this asserts. The approved path is measured by hand
//! in the application itself.
//!
//! A second test runs the same assembly against a config root it creates, whose account
//! directory has never been signed in, and pins what a turn there ends as (#64). It needs
//! no sign-in and spends nothing: the CLI answers without reaching the network.
//!
//! Both are ignored, and gated on an environment variable besides: they spawn the real
//! binary, and the first needs a signed-in config directory and spends the user's
//! subscription. Run them deliberately:
//!
//! ```text
//! STANCHION_REAL_CLAUDE=1 \
//!   STANCHION_REAL_CLAUDE_CONFIG_ROOT=<a root whose <account> directory is signed in> \
//!   cargo test -p stanchion --test real_claude -- --ignored --nocapture
//! STANCHION_REAL_CLAUDE=1 \
//!   cargo test -p stanchion --test real_claude -- --ignored --nocapture never_signed_in
//! ```

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use stanchion_core::backend::claude_code::{ClaudeCode, ConfigRoot, Helper};
use stanchion_core::backend::{
    AccountId, ConversationId, Event, EventSink, RunBackend, Start, TurnEnd, UserInput,
};
use stanchion_core::consent::policy::AlwaysAsk;
use stanchion_core::consent::{Config, Consent};
use stanchion_lib::assembly;
use stanchion_lib::presenter::FailClosed;

#[derive(Default)]
struct Recorder(Mutex<Vec<Event>>);

impl EventSink for Recorder {
    fn event(&self, event: Event) {
        println!("event: {event:?}");
        self.0.lock().unwrap().push(event);
    }
}

impl Recorder {
    fn events(&self) -> Vec<Event> {
        self.0.lock().unwrap().clone()
    }

    fn wait_for(&self, what: &str, f: impl Fn(&Event) -> bool, within: Duration) -> Vec<Event> {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            let events = self.events();
            if events.iter().any(&f) {
                return events;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("no {what} within {within:?}: {:#?}", self.events());
    }
}

/// The backend as the shell assembles it, over `root`.
fn backend(root: &std::ffi::OsStr) -> ClaudeCode {
    let binary = assembly::resolve_claude(
        std::env::var_os("PATH").as_deref(),
        &assembly::known_dirs(
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .as_deref(),
        ),
        std::env::var_os("SHELL")
            .map(std::path::PathBuf::from)
            .as_deref(),
    )
    .expect("a claude binary");
    println!("binary: {}", binary.display());
    // The application's binary, not this test's: `--prompt-helper` is dispatched in the
    // application's `main`, which is the binary a user runs.
    let helper = Helper::new(env!("CARGO_BIN_EXE_stanchion")).arg(assembly::HELPER_MODE);
    println!("helper: {:?} {:?}", helper.program(), helper.args());
    let sockets = assembly::socket_dir().expect("a socket directory");
    println!("sockets: {}", sockets.path().display());
    ClaudeCode::new(
        binary,
        ConfigRoot::new(root).expect("config root"),
        helper,
        sockets,
    )
}

#[test]
#[ignore = "spawns the real claude binary against a signed-in config directory"]
fn a_delegated_call_reaches_the_gate_and_the_fail_closed_presenter_refuses_it() {
    if std::env::var_os("STANCHION_REAL_CLAUDE").is_none() {
        eprintln!("STANCHION_REAL_CLAUDE is not set; skipping");
        return;
    }
    let root = std::env::var_os("STANCHION_REAL_CLAUDE_CONFIG_ROOT")
        .expect("STANCHION_REAL_CLAUDE_CONFIG_ROOT names a root with a signed-in account dir");
    let account = std::env::var("STANCHION_REAL_CLAUDE_ACCOUNT").unwrap_or("default".into());
    let backend = backend(&root);

    let gate = Arc::new(Consent::new(
        Arc::new(FailClosed),
        Arc::new(AlwaysAsk),
        Config::default(),
    ));
    let events = Arc::new(Recorder::default());
    let workspace = std::env::temp_dir().join(format!("stanchion-real-{}", std::process::id()));
    std::fs::create_dir_all(&workspace).unwrap();
    let session = backend
        .start(Start {
            conversation: ConversationId(1),
            account: AccountId(account),
            workspace_root: workspace.clone(),
            gate,
            events: events.clone(),
        })
        .expect("start");
    session
        .send(UserInput {
            // Command substitution keeps the call off the CLI's built-in safe list, which
            // answers a bare `date +%s` on its own and reports it as `RanWithoutAsking`
            // (measured; the assertion below names that case).
            text: "Run the shell command `printf '%s\\n' \"$(date +%s)\"` and tell me what it printed."
                .into(),
        })
        .expect("send");

    let seen = events.wait_for(
        "turn end",
        |e| matches!(e, Event::TurnEnded { .. }),
        Duration::from_secs(120),
    );
    let call = seen
        .iter()
        .find_map(|e| match e {
            Event::ToolCall { call, name, .. } if name == "Bash" => Some(call.0.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the model made no Bash call: {seen:#?}"));
    // The CLI's own rules did not answer it: had they, this would be the event instead of
    // a trip to the gate (a bare `date +%s` is on its safe list; the command substitution
    // above keeps this one off it).
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, Event::RanWithoutAsking { call: c, .. } if c.0 == call)),
        "the CLI allowed the call on its own, so the gate was never asked: {seen:#?}"
    );
    // The gate refused it, before any presentation: capacity zero.
    assert!(
        seen.iter().any(|e| matches!(
            e,
            Event::Diagnostic { text } if text.contains(&call) && text.contains("refused")
        )),
        "no diagnostic says the gate refused the call: {seen:#?}"
    );
    assert!(
        seen.iter().any(|e| matches!(
            e,
            Event::ToolResult { call: c, is_error: true, .. } if c.0 == call
        )),
        "the refused call did not come back as an error result: {seen:#?}"
    );
    // And a presenter that shows nothing is asked for nothing.
    assert!(
        !seen.iter().any(|e| matches!(
            e,
            Event::ApprovalRequested { .. } | Event::ApprovalResolved { .. }
        )),
        "a presenter of capacity zero was asked to present: {seen:#?}"
    );
    session.terminate().expect("terminate");
    events.wait_for(
        "exit",
        |e| matches!(e, Event::Exited { .. }),
        Duration::from_secs(30),
    );
    let _ = std::fs::remove_dir_all(&workspace);
}

#[test]
#[ignore = "spawns the real claude binary"]
fn a_turn_on_a_never_signed_in_account_ends_not_signed_in() {
    if std::env::var_os("STANCHION_REAL_CLAUDE").is_none() {
        eprintln!("STANCHION_REAL_CLAUDE is not set; skipping");
        return;
    }
    // A space in the root, as the application's own has (`Application Support`).
    let base = std::env::temp_dir().join(format!("stanchion real {}", std::process::id()));
    let root = base.join("config root");
    let workspace = base.join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let backend = backend(root.as_os_str());
    let gate = Arc::new(Consent::new(
        Arc::new(FailClosed),
        Arc::new(AlwaysAsk),
        Config::default(),
    ));
    let events = Arc::new(Recorder::default());
    let session = backend
        .start(Start {
            conversation: ConversationId(1),
            account: AccountId("signed-out".into()),
            workspace_root: workspace.clone(),
            gate,
            events: events.clone(),
        })
        .expect("start");
    let turn = session.send(UserInput { text: "hi".into() }).expect("send");
    let seen = events.wait_for(
        "turn end",
        |e| matches!(e, Event::TurnEnded { .. }),
        Duration::from_secs(60),
    );
    let end = seen
        .iter()
        .find_map(|e| match e {
            Event::TurnEnded { turn: t, end } if *t == turn => Some(end.clone()),
            _ => None,
        })
        .unwrap();
    let config_dir = root.canonicalize().unwrap().join("signed-out");
    match end {
        TurnEnd::NotSignedIn { how } => assert!(
            how.contains(&format!("\"{}\"", config_dir.display())) && how.ends_with("/login"),
            "{how}"
        ),
        other => panic!("the turn ended {other:?}, not NotSignedIn: {seen:#?}"),
    }
    session.terminate().expect("terminate");
    events.wait_for(
        "exit",
        |e| matches!(e, Event::Exited { .. }),
        Duration::from_secs(30),
    );
    let _ = std::fs::remove_dir_all(&base);
}
