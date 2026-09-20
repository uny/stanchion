//! The runtime half of the consent rule, against a fake presenter that records what it was
//! shown. One test per clause of "How this is tested, and where" in `docs/decisions.md`
//! under "Consent is a native dialog the core owns". The compile-time half is the
//! `compile_fail` doctests in `src/lib.rs`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use stanchion_core::consent::policy::{AlwaysAsk, Policy, Refusal, Tier};
use stanchion_core::consent::presenter::{
    Answer, ConsentPresenter, Handle, PresenterError, Rendered, Responder,
};
use stanchion_core::consent::render::{escape, unescape};
use stanchion_core::consent::request::{
    Backend, Binding, ClassSpec, InlineProfile, Program, Request, RequestSpec, RunId,
};
use stanchion_core::consent::token::Origin;
use stanchion_core::consent::{Config, Consent, AFFIRMATIVE, NEGATIVE};
use stanchion_core::execute::{
    Bridge, BridgeSink, CliApproval, ExecutionSink, NativeExecutor, Reply, ReplyTransport, RunSink,
    RunStarter, SettingsChange, SettingsStore, SettingsWriter,
};

// ---------------------------------------------------------------------------------------
// Fakes

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Decline,
    Allow,
    /// `show` fails.
    CannotOpen,
    /// Answers through `fail`.
    Fails,
    /// Drops the responder without answering.
    DropsResponder,
    /// Never answers on its own; the test answers through `answer_held`.
    Hold,
}

struct FakePresenter {
    mode: Mutex<Mode>,
    capacity: usize,
    shown: Mutex<Vec<Rendered>>,
    held: Mutex<Vec<Responder>>,
    dismissed: Mutex<Vec<Handle>>,
    next_handle: AtomicUsize,
}

impl FakePresenter {
    fn new(mode: Mode) -> Arc<Self> {
        Arc::new(FakePresenter {
            mode: Mutex::new(mode),
            capacity: 1 << 16,
            shown: Mutex::new(Vec::new()),
            held: Mutex::new(Vec::new()),
            dismissed: Mutex::new(Vec::new()),
            next_handle: AtomicUsize::new(1),
        })
    }

    fn with_capacity(mode: Mode, capacity: usize) -> Arc<Self> {
        let p = Self::new(mode);
        Arc::new(FakePresenter {
            capacity,
            ..Arc::try_unwrap(p).ok().expect("fresh")
        })
    }

    fn shown(&self) -> Vec<Rendered> {
        self.shown.lock().unwrap().clone()
    }

    /// Answers the oldest held dialog, after waiting for one to be held.
    fn answer_held(&self, answer: Answer) {
        let responder = wait_for(|| {
            let mut held = self.held.lock().unwrap();
            (!held.is_empty()).then(|| held.remove(0))
        });
        responder.answer(answer);
    }
}

impl ConsentPresenter for FakePresenter {
    fn capacity(&self) -> usize {
        self.capacity
    }

    fn show(&self, rendered: &Rendered, responder: Responder) -> Result<Handle, PresenterError> {
        let mode = *self.mode.lock().unwrap();
        if mode == Mode::CannotOpen {
            return Err(PresenterError("cannot open".into()));
        }
        self.shown.lock().unwrap().push(rendered.clone());
        let handle = Handle(self.next_handle.fetch_add(1, Ordering::SeqCst) as u64);
        match mode {
            Mode::Decline => responder.answer(Answer::Decline),
            Mode::Allow => responder.answer(Answer::Allow),
            Mode::Fails => responder.fail(PresenterError("unknown response".into())),
            Mode::DropsResponder => drop(responder),
            Mode::Hold => self.held.lock().unwrap().push(responder),
            Mode::CannotOpen => unreachable!(),
        }
        Ok(handle)
    }

    fn dismiss(&self, handle: Handle) {
        self.dismissed.lock().unwrap().push(handle);
    }
}

struct SwitchablePolicy(AtomicU8);

impl SwitchablePolicy {
    fn new(tier: Tier) -> Arc<Self> {
        let p = Arc::new(SwitchablePolicy(AtomicU8::new(0)));
        p.set(tier);
        p
    }
    fn set(&self, tier: Tier) {
        self.0.store(
            match tier {
                Tier::Refused => 0,
                Tier::Ask => 1,
                Tier::AutoRun => 2,
            },
            Ordering::SeqCst,
        );
    }
}

impl Policy for SwitchablePolicy {
    fn classify(&self, _: &Request) -> Tier {
        match self.0.load(Ordering::SeqCst) {
            0 => Tier::Refused,
            1 => Tier::Ask,
            _ => Tier::AutoRun,
        }
    }
}

type Ran = (String, PathBuf, Vec<(String, String)>, Origin);

/// Every door's sink in one place, so a test can assert that nothing at all happened.
#[derive(Default)]
struct Effects {
    ran: Vec<Ran>,
    wrote: Vec<(PathBuf, Origin)>,
    settings: Vec<(String, Origin)>,
    started: Vec<(InlineProfile, Origin)>,
    replies: Vec<Reply>,
    forwarded: Vec<(String, String, Origin)>,
}

impl Effects {
    fn count(&self) -> usize {
        self.ran.len()
            + self.wrote.len()
            + self.settings.len()
            + self.started.len()
            + self
                .replies
                .iter()
                .filter(|r| matches!(r, Reply::Allow { .. }))
                .count()
            + self.forwarded.len()
    }
}

impl ExecutionSink for Effects {
    fn run(&mut self, command: &str, cwd: &Path, env: &[(String, String)], origin: Origin) {
        self.ran
            .push((command.to_string(), cwd.to_path_buf(), env.to_vec(), origin));
    }
    fn wrote(&mut self, path: &Path, origin: Origin) {
        self.wrote.push((path.to_path_buf(), origin));
    }
}

impl SettingsStore for Effects {
    fn apply(&mut self, change: SettingsChange<'_>, origin: Origin) {
        self.settings.push((format!("{change:?}"), origin));
    }
}

impl RunSink for Effects {
    fn start(&mut self, profile: &InlineProfile, origin: Origin) {
        self.started.push((profile.clone(), origin));
    }
}

impl ReplyTransport for Effects {
    fn send(&mut self, reply: Reply) {
        self.replies.push(reply);
    }
}

impl BridgeSink for Effects {
    fn forward(&mut self, tool: &str, arguments: &str, origin: Origin) {
        self.forwarded
            .push((tool.to_string(), arguments.to_string(), origin));
    }
}

// ---------------------------------------------------------------------------------------
// Helpers

fn wait_for<T>(mut f: impl FnMut() -> Option<T>) -> T {
    for _ in 0..500 {
        if let Some(v) = f() {
            return v;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("condition not met within 5s");
}

fn gate(presenter: &Arc<FakePresenter>) -> Consent {
    gate_with(presenter, Arc::new(AlwaysAsk))
}

fn gate_with(presenter: &Arc<FakePresenter>, policy: Arc<dyn Policy>) -> Consent {
    Consent::new(
        presenter.clone(),
        policy,
        Config {
            settle: Duration::ZERO,
            queue_limit: 8,
        },
    )
}

static DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A fresh directory under the system temp dir, removed on drop.
struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        let n = DIR_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("stanchion-core-consent-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Workspace(dir.canonicalize().unwrap())
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn program(p: &str) -> Program {
    Program {
        program: p.into(),
        args: vec!["-c".into(), "echo hi".into()],
        env: vec![("K".into(), "v".into())],
    }
}

fn profile() -> InlineProfile {
    InlineProfile {
        name: "inline".into(),
        base_url: "https://api.example.com@evil.example/v1".into(),
        model: "m".into(),
    }
}

/// One spec per class. `run` is `None` for the classes bound to the application.
fn specs(run: RunId, ws: &Path) -> Vec<(&'static str, RequestSpec)> {
    let at = |run: Option<RunId>, class: ClassSpec| RequestSpec {
        run,
        workspace_root: ws.to_path_buf(),
        class,
    };
    vec![
        (
            "shell",
            at(
                Some(run),
                ClassSpec::ShellCommand {
                    command: "rm -rf ./build && make".into(),
                    cwd: ws.to_path_buf(),
                    env: vec![("PATH".into(), "/usr/bin".into())],
                },
            ),
        ),
        (
            "write",
            at(
                Some(run),
                ClassSpec::FileWrite {
                    path: "out.txt".into(),
                    content: b"hello\n".to_vec(),
                },
            ),
        ),
        (
            "mcp",
            at(
                None,
                ClassSpec::McpServerEntry {
                    name: "fs".into(),
                    old: None,
                    new: program("/bin/sh"),
                },
            ),
        ),
        (
            "credential",
            at(
                None,
                ClassSpec::CredentialProviderCommand {
                    old: Some(program("/usr/bin/true")),
                    new: program("/bin/sh"),
                },
            ),
        ),
        (
            "gateway",
            at(
                None,
                ClassSpec::GatewayUrl {
                    old: Some("https://api.example.com/v1".into()),
                    new: "https://api.example.com@evil.example/v1".into(),
                },
            ),
        ),
        (
            "workspace-root",
            at(
                None,
                ClassSpec::WorkspaceRoot {
                    old: ws.to_path_buf(),
                    new: PathBuf::from("/"),
                },
            ),
        ),
        (
            "auto-approve",
            at(
                None,
                ClassSpec::AutoApproveRule {
                    old: None,
                    new: "allow: *".into(),
                },
            ),
        ),
        (
            "inline-profile",
            at(None, ClassSpec::InlineProfileRun { profile: profile() }),
        ),
        (
            "cli",
            at(
                Some(run),
                ClassSpec::CliCommand {
                    cli_request_id: "req-1".into(),
                    command: "git push --force".into(),
                    cwd: ws.to_path_buf(),
                    session_grant: false,
                },
            ),
        ),
        (
            "bridge",
            at(
                Some(run),
                ClassSpec::BridgeForward {
                    tool: "browser.click".into(),
                    arguments: "{\"selector\":\"#buy\"}".into(),
                },
            ),
        ),
    ]
}

/// Routes a token to the door for its class. Returns the door's result.
fn execute(gate: &Consent, spec: &RequestSpec, effects: &mut Effects) -> Result<(), Refusal> {
    let token = gate.ask(spec.clone())?;
    match &spec.class {
        ClassSpec::ShellCommand { .. } | ClassSpec::FileWrite { .. } => {
            NativeExecutor.execute(gate, token, effects)
        }
        ClassSpec::McpServerEntry { .. }
        | ClassSpec::CredentialProviderCommand { .. }
        | ClassSpec::GatewayUrl { .. }
        | ClassSpec::WorkspaceRoot { .. }
        | ClassSpec::AutoApproveRule { .. } => SettingsWriter.apply(gate, token, effects),
        ClassSpec::InlineProfileRun { .. } => RunStarter.start(gate, token, effects),
        ClassSpec::CliCommand { .. } => CliApproval.allow(gate, token, effects),
        ClassSpec::BridgeForward { .. } => Bridge.forward(gate, token, effects),
    }
}

fn shell_spec(run: RunId, ws: &Path, command: &str) -> RequestSpec {
    RequestSpec {
        run: Some(run),
        workspace_root: ws.to_path_buf(),
        class: ClassSpec::ShellCommand {
            command: command.into(),
            cwd: ws.to_path_buf(),
            env: vec![],
        },
    }
}

fn write_spec(run: RunId, ws: &Path, path: &str, content: &[u8]) -> RequestSpec {
    RequestSpec {
        run: Some(run),
        workspace_root: ws.to_path_buf(),
        class: ClassSpec::FileWrite {
            path: path.into(),
            content: content.to_vec(),
        },
    }
}

// ---------------------------------------------------------------------------------------
// Tests

#[test]
fn a_declining_presenter_executes_nothing_in_any_class() {
    let ws = Workspace::new();
    let presenter = FakePresenter::new(Mode::Decline);
    let gate = gate(&presenter);
    let run = gate.register_run(Backend::Native);
    let mut effects = Effects::default();
    for (name, spec) in specs(run, ws.path()) {
        assert_eq!(
            execute(&gate, &spec, &mut effects),
            Err(Refusal::Declined),
            "{name}"
        );
    }
    assert_eq!(effects.count(), 0);
    assert_eq!(
        presenter.shown().len(),
        10,
        "every class reached the presenter"
    );
}

#[test]
fn an_approving_presenter_executes_each_class_exactly_once_with_the_bytes_shown() {
    let ws = Workspace::new();
    let presenter = FakePresenter::new(Mode::Allow);
    let gate = gate(&presenter);
    let run = gate.register_run(Backend::Native);
    let all = specs(run, ws.path());
    let mut effects = Effects::default();
    for (name, spec) in &all {
        assert_eq!(execute(&gate, spec, &mut effects), Ok(()), "{name}");
    }
    assert_eq!(effects.count(), all.len());
    assert_eq!(effects.ran.len(), 1);
    assert_eq!(effects.wrote.len(), 1);
    assert_eq!(effects.settings.len(), 5);
    assert_eq!(effects.started.len(), 1);
    assert_eq!(effects.forwarded.len(), 1);
    assert_eq!(
        effects.replies,
        vec![Reply::Allow {
            cli_request_id: "req-1".into()
        }]
    );
    assert!(effects.ran.iter().all(|r| r.3 == Origin::Consent));

    // The shell command the executor received is byte-for-byte what the dialog showed.
    let shown = presenter.shown();
    assert_eq!(
        unescape(&shown[0].body).unwrap(),
        effects.ran[0].0.as_bytes()
    );
    // And the write landed with the bytes that were shown.
    assert_eq!(
        std::fs::read(ws.path().join("out.txt")).unwrap(),
        unescape(&shown[1].body).unwrap()
    );
}

#[test]
fn a_failing_presenter_mints_nothing_and_the_request_ends_refused() {
    let ws = Workspace::new();
    for mode in [Mode::CannotOpen, Mode::Fails, Mode::DropsResponder] {
        let presenter = FakePresenter::new(mode);
        let gate = gate(&presenter);
        let run = gate.register_run(Backend::Native);
        let result = gate.ask(shell_spec(run, ws.path(), "ls"));
        assert!(
            matches!(result, Err(Refusal::PresenterFailed(_))),
            "{mode:?}: {result:?}"
        );
    }
}

#[test]
fn a_presenter_that_never_answers_mints_nothing_however_long_it_is_waited_on() {
    let ws = Workspace::new();
    let presenter = FakePresenter::new(Mode::Hold);
    let gate = Arc::new(gate(&presenter));
    let run = gate.register_run(Backend::Native);
    let spec = shell_spec(run, ws.path(), "ls");
    let asking = {
        let gate = gate.clone();
        thread::spawn(move || gate.ask(spec))
    };
    wait_for(|| (!presenter.shown().is_empty()).then_some(()));
    thread::sleep(Duration::from_millis(300));
    assert!(!asking.is_finished(), "still pending, nothing minted");
    let invocation = presenter.shown()[0].invocation;
    gate.cancel(invocation);
    assert_eq!(asking.join().unwrap().map(|_| ()), Err(Refusal::Withdrawn));
    assert_eq!(
        presenter.dismissed.lock().unwrap().len(),
        1,
        "the dialog was dismissed"
    );
}

#[test]
fn a_token_is_bound_to_its_run_and_workspace_not_to_its_content() {
    let ws_a = Workspace::new();
    let ws_b = Workspace::new();
    let presenter = FakePresenter::new(Mode::Allow);
    let gate = gate(&presenter);
    let run_a = gate.register_run(Backend::Native);
    let run_b = gate.register_run(Backend::Native);

    // Identical content, two runs: ending one run voids only its token.
    let token_a = gate.ask(shell_spec(run_a, ws_a.path(), "make")).unwrap();
    let token_b = gate.ask(shell_spec(run_b, ws_a.path(), "make")).unwrap();
    assert_ne!(
        token_a.request().invocation(),
        token_b.request().invocation()
    );
    gate.end_run(run_a);
    let mut effects = Effects::default();
    assert_eq!(
        NativeExecutor.execute(&gate, token_a, &mut effects),
        Err(Refusal::Withdrawn)
    );
    assert_eq!(NativeExecutor.execute(&gate, token_b, &mut effects), Ok(()));
    assert_eq!(effects.ran.len(), 1);

    // Identical content, two workspace roots: each token writes only under its own.
    let token_a = gate.ask(write_spec(run_b, ws_a.path(), "f", b"x")).unwrap();
    let token_b = gate.ask(write_spec(run_b, ws_b.path(), "f", b"x")).unwrap();
    assert_eq!(token_a.request().workspace_root(), ws_a.path());
    assert_eq!(token_b.request().workspace_root(), ws_b.path());
    assert_eq!(NativeExecutor.execute(&gate, token_a, &mut effects), Ok(()));
    assert!(ws_a.path().join("f").exists());
    assert!(!ws_b.path().join("f").exists());
    assert_eq!(NativeExecutor.execute(&gate, token_b, &mut effects), Ok(()));
    assert!(ws_b.path().join("f").exists());
}

#[test]
fn a_token_overtaken_by_cancellation_or_run_end_is_refused() {
    let ws = Workspace::new();
    let presenter = FakePresenter::new(Mode::Allow);
    let gate = gate(&presenter);
    let run = gate.register_run(Backend::Native);
    let mut effects = Effects::default();

    let token = gate.ask(shell_spec(run, ws.path(), "ls")).unwrap();
    gate.cancel(token.request().invocation());
    assert_eq!(
        NativeExecutor.execute(&gate, token, &mut effects),
        Err(Refusal::Withdrawn)
    );

    let token = gate.ask(shell_spec(run, ws.path(), "ls")).unwrap();
    gate.end_run(run);
    assert_eq!(
        NativeExecutor.execute(&gate, token, &mut effects),
        Err(Refusal::Withdrawn)
    );
    assert_eq!(effects.count(), 0);
}

#[test]
fn a_write_token_is_void_once_its_target_changed_appeared_or_resolves_elsewhere() {
    let presenter = FakePresenter::new(Mode::Allow);
    let gate = gate(&presenter);
    let run = gate.register_run(Backend::Native);
    let mut effects = Effects::default();
    let changed = |r: Result<(), Refusal>| matches!(r, Err(Refusal::PreconditionChanged(_)));

    // Changed.
    let ws = Workspace::new();
    std::fs::write(ws.path().join("a"), b"one").unwrap();
    let token = gate.ask(write_spec(run, ws.path(), "a", b"new")).unwrap();
    std::fs::write(ws.path().join("a"), b"two").unwrap();
    assert!(changed(NativeExecutor.execute(&gate, token, &mut effects)));
    assert_eq!(std::fs::read(ws.path().join("a")).unwrap(), b"two");

    // Appeared.
    let token = gate.ask(write_spec(run, ws.path(), "b", b"new")).unwrap();
    std::fs::write(ws.path().join("b"), b"surprise").unwrap();
    assert!(changed(NativeExecutor.execute(&gate, token, &mut effects)));
    assert_eq!(std::fs::read(ws.path().join("b")).unwrap(), b"surprise");

    // Disappeared.
    std::fs::write(ws.path().join("c"), b"one").unwrap();
    let token = gate.ask(write_spec(run, ws.path(), "c", b"new")).unwrap();
    std::fs::remove_file(ws.path().join("c")).unwrap();
    assert!(changed(NativeExecutor.execute(&gate, token, &mut effects)));

    // A symlink where a file was.
    std::fs::write(ws.path().join("d"), b"one").unwrap();
    std::fs::write(ws.path().join("elsewhere"), b"victim").unwrap();
    let token = gate.ask(write_spec(run, ws.path(), "d", b"new")).unwrap();
    std::fs::remove_file(ws.path().join("d")).unwrap();
    std::os::unix::fs::symlink(ws.path().join("elsewhere"), ws.path().join("d")).unwrap();
    assert!(changed(NativeExecutor.execute(&gate, token, &mut effects)));
    assert_eq!(
        std::fs::read(ws.path().join("elsewhere")).unwrap(),
        b"victim"
    );

    // The path now resolves elsewhere: its directory was swapped for a symlink.
    std::fs::create_dir(ws.path().join("sub")).unwrap();
    std::fs::create_dir(ws.path().join("other")).unwrap();
    let token = gate
        .ask(write_spec(run, ws.path(), "sub/e", b"new"))
        .unwrap();
    std::fs::rename(ws.path().join("sub"), ws.path().join("sub-moved")).unwrap();
    std::os::unix::fs::symlink(ws.path().join("other"), ws.path().join("sub")).unwrap();
    assert!(changed(NativeExecutor.execute(&gate, token, &mut effects)));
    assert!(!ws.path().join("other/e").exists());

    assert_eq!(effects.count(), 0);
}

#[test]
fn concurrent_writes_to_one_target_are_serialised_and_the_second_is_refused() {
    let ws = Workspace::new();
    // Large enough that reading and hashing it takes measurable time, so without the lock
    // both threads snapshot before either writes and the test fails rather than passing by
    // scheduling luck.
    std::fs::write(ws.path().join("shared"), vec![b'b'; 8 << 20]).unwrap();
    std::fs::create_dir(ws.path().join("sub")).unwrap();
    let presenter = FakePresenter::new(Mode::Allow);
    let gate = Arc::new(gate(&presenter));
    let run = gate.register_run(Backend::Native);

    // Both tokens are minted against the same prior, under the root spelled two ways. A
    // check-then-write lets both land; verify-and-write under the lock lets exactly one.
    let token_1 = gate
        .ask(write_spec(run, ws.path(), "shared", b"from-1"))
        .unwrap();
    // On a case-insensitive filesystem the second name is the same file; on a
    // case-sensitive one it is a new file in the same directory, and both writes land.
    let case_insensitive = ws.path().join("SHARED").exists();
    let token_2 = gate
        .ask(write_spec(
            run,
            &ws.path().join("sub").join(".."),
            if case_insensitive { "SHARED" } else { "shared" },
            b"from-2",
        ))
        .unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let threads: Vec<_> = [token_1, token_2]
        .into_iter()
        .map(|token| {
            let gate = gate.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                let mut effects = Effects::default();
                barrier.wait();
                NativeExecutor.execute(&gate, token, &mut effects)
            })
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    let ok = results.iter().filter(|r| r.is_ok()).count();
    let changed = results
        .iter()
        .filter(|r| matches!(r, Err(Refusal::PreconditionChanged(_))))
        .count();
    assert_eq!((ok, changed), (1, 1), "{results:?}");
    let content = std::fs::read(ws.path().join("shared")).unwrap();
    assert!(content == b"from-1" || content == b"from-2");
}

#[test]
fn an_answer_arriving_after_cancellation_mints_nothing() {
    let ws = Workspace::new();
    let presenter = FakePresenter::new(Mode::Hold);
    let gate = Arc::new(gate(&presenter));
    let run = gate.register_run(Backend::Native);
    let spec = shell_spec(run, ws.path(), "ls");
    let asking = {
        let gate = gate.clone();
        thread::spawn(move || gate.ask(spec))
    };
    wait_for(|| (!presenter.shown().is_empty()).then_some(()));
    gate.cancel(presenter.shown()[0].invocation);
    // The dialog answers late, and affirmatively.
    presenter.answer_held(Answer::Allow);
    assert_eq!(asking.join().unwrap().map(|_| ()), Err(Refusal::Withdrawn));
}

#[test]
fn the_refused_tier_never_reaches_the_presenter() {
    let ws = Workspace::new();
    let presenter = FakePresenter::new(Mode::Allow);
    let gate = gate_with(&presenter, SwitchablePolicy::new(Tier::Refused));
    let run = gate.register_run(Backend::Native);
    assert_eq!(
        gate.ask(shell_spec(run, ws.path(), "ls")).map(|_| ()),
        Err(Refusal::RefusedTier)
    );
    assert!(presenter.shown().is_empty());
}

#[test]
fn a_request_reclassified_as_refused_while_pending_is_refused_whatever_the_answer() {
    let ws = Workspace::new();
    let presenter = FakePresenter::new(Mode::Hold);
    let policy = SwitchablePolicy::new(Tier::Ask);
    let gate = Arc::new(gate_with(&presenter, policy.clone()));
    let run = gate.register_run(Backend::Native);
    let spec = shell_spec(run, ws.path(), "ls");
    let asking = {
        let gate = gate.clone();
        thread::spawn(move || gate.ask(spec))
    };
    wait_for(|| (!presenter.shown().is_empty()).then_some(()));
    policy.set(Tier::Refused);
    presenter.answer_held(Answer::Allow);
    assert_eq!(
        asking.join().unwrap().map(|_| ()),
        Err(Refusal::RefusedTier)
    );
}

#[test]
fn a_session_wide_grant_from_a_cli_is_refused_before_any_dialog() {
    let ws = Workspace::new();
    let presenter = FakePresenter::new(Mode::Allow);
    let gate = gate(&presenter);
    let run = gate.register_run(Backend::Codex);
    let spec = RequestSpec {
        run: Some(run),
        workspace_root: ws.path().to_path_buf(),
        class: ClassSpec::CliCommand {
            cli_request_id: "req-9".into(),
            command: "ls".into(),
            cwd: ws.path().to_path_buf(),
            session_grant: true,
        },
    };
    let mut effects = Effects::default();
    assert_eq!(
        CliApproval.resolve(&gate, spec, &mut effects),
        Err(Refusal::SessionGrant)
    );
    assert!(presenter.shown().is_empty());
    assert!(
        matches!(&effects.replies[..], [Reply::Deny { cli_request_id, .. }] if cli_request_id == "req-9")
    );
}

#[test]
fn auto_run_takes_the_same_door_with_a_token_recorded_as_policy() {
    let ws = Workspace::new();
    let presenter = FakePresenter::new(Mode::Decline);
    let gate = gate_with(&presenter, SwitchablePolicy::new(Tier::AutoRun));
    let run = gate.register_run(Backend::Native);
    let token = gate.ask(shell_spec(run, ws.path(), "ls")).unwrap();
    assert_eq!(token.origin(), Origin::Policy);
    let mut effects = Effects::default();
    assert_eq!(NativeExecutor.execute(&gate, token, &mut effects), Ok(()));
    assert_eq!(effects.ran[0].3, Origin::Policy);
    assert!(presenter.shown().is_empty(), "no presenter was asked");
}

#[test]
fn a_request_over_the_presenters_capacity_is_refused_before_the_presenter_is_asked() {
    let ws = Workspace::new();
    // The capacity bounds everything shown — title and parsed fields, not only the body —
    // so a short command fits and the same command padded past the limit does not.
    let presenter = FakePresenter::with_capacity(Mode::Allow, 64);
    let gate = gate(&presenter);
    let run = gate.register_run(Backend::Native);
    let long = "x".repeat(64);
    assert!(matches!(
        gate.ask(shell_spec(run, ws.path(), &long)),
        Err(Refusal::OverCapacity {
            bytes,
            capacity: 64
        }) if bytes > 64
    ));
    // A write is refused on the same rule, not on its class.
    assert!(matches!(
        gate.ask(write_spec(run, ws.path(), "big", long.as_bytes())),
        Err(Refusal::OverCapacity { .. })
    ));
    assert!(presenter.shown().is_empty());
    assert!(gate.ask(shell_spec(run, ws.path(), "x")).is_ok());
}

#[test]
fn the_rendered_text_round_trips_and_makes_hidden_characters_visible() {
    let bytes: &[u8] = b"echo \xE2\x80\xAEgnp.exe\xE2\x80\x8D \\\\ \x00 \xFF tab\there\n";
    let text = escape(bytes);
    assert_eq!(unescape(&text).unwrap(), bytes);
    assert!(text.contains("\\u{202E}"), "{text}");
    assert!(text.contains("\\u{200D}"), "{text}");
    assert!(text.contains("\\u{0}"), "{text}");
    assert!(text.contains("\\x{FF}"), "{text}");
    assert!(text.contains("\\u{9}"), "{text}");
    assert!(text.contains("\\\\\\\\"), "{text}");
    assert!(text.ends_with('\n'), "a newline is shown as itself");
    for c in text.chars() {
        assert!(
            c == '\n' || (' '..='~').contains(&c),
            "{c:?} escaped the escape"
        );
    }
}

#[test]
fn the_render_names_the_run_or_the_application_and_the_backend() {
    let ws = Workspace::new();
    let presenter = FakePresenter::new(Mode::Decline);
    let gate = gate(&presenter);
    let run = gate.register_run(Backend::ClaudeCode);
    let _ = gate.ask(shell_spec(run, ws.path(), "ls"));
    let _ = gate.ask(RequestSpec {
        run: None,
        workspace_root: ws.path().to_path_buf(),
        class: ClassSpec::GatewayUrl {
            old: None,
            new: "https://api.example.com@evil.example/v1".into(),
        },
    });
    let shown = presenter.shown();
    assert_eq!(
        shown[0].title,
        format!("{run} (claude-code) \u{2014} run a shell command")
    );
    assert_eq!(
        shown[1].title,
        "application \u{2014} change the gateway URL"
    );
    assert_eq!(
        shown[1].parsed,
        vec![("new host".to_string(), "evil.example".to_string())]
    );
    assert!(shown[1]
        .body
        .contains("https://api.example.com@evil.example/v1"));
    assert_eq!(
        (shown[0].negative, shown[0].affirmative),
        (NEGATIVE, AFFIRMATIVE)
    );
    assert_eq!(NEGATIVE, "Deny");
}

#[test]
fn at_most_one_presentation_is_outstanding_and_the_request_past_the_limit_is_refused() {
    let ws = Workspace::new();
    let presenter = FakePresenter::new(Mode::Hold);
    let gate = Arc::new(Consent::new(
        presenter.clone(),
        Arc::new(AlwaysAsk),
        Config {
            settle: Duration::ZERO,
            queue_limit: 2,
        },
    ));
    let run = gate.register_run(Backend::Native);
    let spawn = |gate: &Arc<Consent>, cmd: &str| {
        let gate = gate.clone();
        let spec = shell_spec(run, ws.path(), cmd);
        thread::spawn(move || gate.ask(spec))
    };
    let first = spawn(&gate, "first");
    wait_for(|| (!presenter.shown().is_empty()).then_some(()));
    let second = spawn(&gate, "second");
    thread::sleep(Duration::from_millis(200));
    assert_eq!(
        presenter.shown().len(),
        1,
        "the second waits behind the first"
    );
    assert_eq!(
        gate.ask(shell_spec(run, ws.path(), "third")).map(|_| ()),
        Err(Refusal::QueueFull)
    );
    presenter.answer_held(Answer::Allow);
    assert!(first.join().unwrap().is_ok());
    wait_for(|| (presenter.shown().len() == 2).then_some(()));
    presenter.answer_held(Answer::Decline);
    assert_eq!(second.join().unwrap().map(|_| ()), Err(Refusal::Declined));
}

#[test]
fn an_affirmative_within_the_settle_interval_is_a_decline() {
    let ws = Workspace::new();
    let presenter = FakePresenter::new(Mode::Allow);
    let gate = Consent::new(
        presenter.clone(),
        Arc::new(AlwaysAsk),
        Config {
            settle: Duration::from_millis(200),
            queue_limit: 8,
        },
    );
    let run = gate.register_run(Backend::Native);
    assert_eq!(
        gate.ask(shell_spec(run, ws.path(), "ls")).map(|_| ()),
        Err(Refusal::TooSoon)
    );
}

#[test]
fn an_affirmative_after_the_settle_interval_mints() {
    let ws = Workspace::new();
    let presenter = FakePresenter::new(Mode::Hold);
    let gate = Arc::new(Consent::new(
        presenter.clone(),
        Arc::new(AlwaysAsk),
        Config {
            settle: Duration::from_millis(100),
            queue_limit: 8,
        },
    ));
    let run = gate.register_run(Backend::Native);
    let asking = {
        let gate = gate.clone();
        let spec = shell_spec(run, ws.path(), "ls");
        thread::spawn(move || gate.ask(spec))
    };
    wait_for(|| (!presenter.shown().is_empty()).then_some(()));
    thread::sleep(Duration::from_millis(150));
    presenter.answer_held(Answer::Allow);
    assert!(asking.join().unwrap().is_ok());
}

#[test]
fn a_request_withdrawn_while_queued_returns_without_waiting_for_the_dialog_on_screen() {
    let ws = Workspace::new();
    let presenter = FakePresenter::new(Mode::Hold);
    let gate = Arc::new(gate(&presenter));
    let run_a = gate.register_run(Backend::Native);
    let run_b = gate.register_run(Backend::Codex);
    let spawn = |gate: &Arc<Consent>, run: RunId, cmd: &str| {
        let gate = gate.clone();
        let spec = shell_spec(run, ws.path(), cmd);
        thread::spawn(move || gate.ask(spec))
    };
    let first = spawn(&gate, run_a, "first");
    wait_for(|| (!presenter.shown().is_empty()).then_some(()));
    let second = spawn(&gate, run_b, "second");
    thread::sleep(Duration::from_millis(100));
    // Ending the second run while its request waits behind the first dialog: its `ask`
    // returns now, not when the first dialog is answered, and the queue slot it held is
    // free for another request.
    gate.end_run(run_b);
    wait_for(|| second.is_finished().then_some(()));
    assert_eq!(second.join().unwrap().map(|_| ()), Err(Refusal::Withdrawn));
    assert_eq!(
        presenter.shown().len(),
        1,
        "no dialog opened for the ended run"
    );
    presenter.answer_held(Answer::Allow);
    assert!(first.join().unwrap().is_ok());
}

#[test]
fn a_labelled_value_cannot_forge_a_line_and_a_url_shows_the_host_a_client_would_use() {
    let ws = Workspace::new();
    let presenter = FakePresenter::new(Mode::Decline);
    let gate = gate(&presenter);
    let _ = gate.ask(RequestSpec {
        run: None,
        workspace_root: ws.path().to_path_buf(),
        class: ClassSpec::McpServerEntry {
            name: "x".into(),
            old: None,
            new: Program {
                program: "npx\nnew arg[0]: --safe".into(),
                args: vec!["--unsafe".into()],
                env: vec![],
            },
        },
    });
    for url in [
        "https://evil.example\\@api.example.com/",
        "https:\\\\evil.example\\@api.example.com/",
    ] {
        let _ = gate.ask(RequestSpec {
            run: None,
            workspace_root: ws.path().to_path_buf(),
            class: ClassSpec::GatewayUrl {
                old: None,
                new: url.into(),
            },
        });
    }
    let shown = presenter.shown();
    let lines: Vec<&str> = shown[0].body.lines().collect();
    assert_eq!(
        lines,
        vec![
            "name: x",
            "old: (none)",
            "new program: npx\\u{A}new arg[0]: --safe",
            "new arg[0]: --unsafe",
        ],
        "a newline in a labelled value must not start a line of its own"
    );
    for r in &shown[1..] {
        assert_eq!(
            r.parsed,
            vec![("new host".to_string(), "evil.example".to_string())],
            "a backslash ends the authority as WHATWG parsers read it: {}",
            r.body
        );
    }
}

#[test]
fn a_write_outside_the_workspace_or_through_a_second_name_is_refused_before_any_dialog() {
    let ws = Workspace::new();
    let outside = Workspace::new();
    let presenter = FakePresenter::new(Mode::Allow);
    let gate = gate(&presenter);
    let run = gate.register_run(Backend::Native);
    let refused = |r: Result<_, Refusal>| matches!(r, Err(Refusal::OutsideWorkspace(_)));

    // `..` out of the root, and an absolute path elsewhere.
    let up = format!(
        "../{}/escaped",
        outside.path().file_name().unwrap().to_str().unwrap()
    );
    assert!(refused(gate.ask(write_spec(run, ws.path(), &up, b"x"))));
    assert!(refused(gate.ask(write_spec(
        run,
        ws.path(),
        outside.path().join("escaped").to_str().unwrap(),
        b"x"
    ))));
    // A directory inside the root that is a symlink to one outside it.
    std::os::unix::fs::symlink(outside.path(), ws.path().join("link")).unwrap();
    assert!(refused(gate.ask(write_spec(
        run,
        ws.path(),
        "link/escaped",
        b"x"
    ))));
    // A file inside the root that is a second hard link to one outside it.
    std::fs::write(outside.path().join("victim"), b"victim").unwrap();
    std::fs::hard_link(outside.path().join("victim"), ws.path().join("twin")).unwrap();
    assert!(matches!(
        gate.ask(write_spec(run, ws.path(), "twin", b"x")),
        Err(Refusal::Unresolvable(_))
    ));

    // Refused before the target is looked at: an outside path that is not a regular file
    // is reported as outside, not as whatever it is.
    std::fs::create_dir(outside.path().join("dir")).unwrap();
    assert!(refused(gate.ask(write_spec(
        run,
        ws.path(),
        outside.path().join("dir").to_str().unwrap(),
        b"x"
    ))));

    assert!(presenter.shown().is_empty());
    assert!(!outside.path().join("escaped").exists());
    assert_eq!(
        std::fs::read(outside.path().join("victim")).unwrap(),
        b"victim"
    );

    // The dialog for a write inside the root names where the bytes land.
    let token = gate.ask(write_spec(run, ws.path(), "in", b"x")).unwrap();
    let shown = presenter.shown();
    assert_eq!(
        shown[0].parsed,
        vec![
            (
                "path".into(),
                escape(ws.path().join("in").as_os_str().as_encoded_bytes())
            ),
            (
                "resolves to".into(),
                escape(ws.path().join("in").as_os_str().as_encoded_bytes())
            ),
        ]
    );
    drop(token);
}

#[test]
fn a_token_opens_only_the_door_for_its_class() {
    let ws = Workspace::new();
    let presenter = FakePresenter::new(Mode::Allow);
    let gate = gate(&presenter);
    let run = gate.register_run(Backend::Native);
    let mut effects = Effects::default();
    let token = gate.ask(shell_spec(run, ws.path(), "ls")).unwrap();
    assert_eq!(
        CliApproval.allow(&gate, token, &mut effects),
        Err(Refusal::WrongDoor)
    );
    let token = gate.ask(shell_spec(run, ws.path(), "ls")).unwrap();
    assert_eq!(
        Bridge.forward(&gate, token, &mut effects),
        Err(Refusal::WrongDoor)
    );
    let token = gate.ask(shell_spec(run, ws.path(), "ls")).unwrap();
    assert_eq!(
        SettingsWriter.apply(&gate, token, &mut effects),
        Err(Refusal::WrongDoor)
    );
    // And the other way round: a settings token opens neither the executor nor the run
    // starter.
    let settings = RequestSpec {
        run: Some(run),
        workspace_root: ws.path().to_path_buf(),
        class: ClassSpec::GatewayUrl {
            old: None,
            new: "https://api.example.com/".into(),
        },
    };
    let token = gate.ask(settings.clone()).unwrap();
    assert_eq!(
        NativeExecutor.execute(&gate, token, &mut effects),
        Err(Refusal::WrongDoor)
    );
    let token = gate.ask(settings).unwrap();
    assert_eq!(
        RunStarter.start(&gate, token, &mut effects),
        Err(Refusal::WrongDoor)
    );
    assert_eq!(effects.count(), 0);
}

#[test]
fn a_cli_request_that_does_not_mint_gets_exactly_one_well_formed_deny() {
    let ws = Workspace::new();
    for (mode, expect) in [
        (Mode::Decline, "declined"),
        (Mode::CannotOpen, "presenter failed"),
        (Mode::DropsResponder, "presenter failed"),
    ] {
        let presenter = FakePresenter::new(mode);
        let gate = gate(&presenter);
        let run = gate.register_run(Backend::ClaudeCode);
        let spec = RequestSpec {
            run: Some(run),
            workspace_root: ws.path().to_path_buf(),
            class: ClassSpec::CliCommand {
                cli_request_id: "req-7".into(),
                command: "ls".into(),
                cwd: ws.path().to_path_buf(),
                session_grant: false,
            },
        };
        let mut effects = Effects::default();
        assert!(CliApproval.resolve(&gate, spec, &mut effects).is_err());
        match &effects.replies[..] {
            [Reply::Deny {
                cli_request_id,
                reason,
            }] => {
                assert_eq!(cli_request_id, "req-7");
                assert!(reason.contains(expect), "{mode:?}: {reason}");
            }
            other => panic!("{mode:?}: expected one deny, got {other:?}"),
        }
    }

    // Refused tier, too.
    let presenter = FakePresenter::new(Mode::Allow);
    let gate = gate_with(&presenter, SwitchablePolicy::new(Tier::Refused));
    let run = gate.register_run(Backend::Codex);
    let mut effects = Effects::default();
    let spec = RequestSpec {
        run: Some(run),
        workspace_root: ws.path().to_path_buf(),
        class: ClassSpec::CliCommand {
            cli_request_id: "req-8".into(),
            command: "ls".into(),
            cwd: ws.path().to_path_buf(),
            session_grant: false,
        },
    };
    assert_eq!(
        CliApproval.resolve(&gate, spec, &mut effects),
        Err(Refusal::RefusedTier)
    );
    assert_eq!(effects.replies.len(), 1);
    assert!(matches!(&effects.replies[0], Reply::Deny { .. }));
}

#[test]
fn a_request_for_a_run_the_gate_does_not_know_is_refused() {
    let ws = Workspace::new();
    let presenter = FakePresenter::new(Mode::Allow);
    let gate = gate(&presenter);
    let run = gate.register_run(Backend::Native);
    gate.end_run(run);
    assert_eq!(
        gate.ask(shell_spec(run, ws.path(), "ls")).map(|_| ()),
        Err(Refusal::UnknownRun)
    );
    assert!(presenter.shown().is_empty());
}

#[test]
fn a_run_bound_request_carries_its_backend_and_an_application_request_none() {
    let ws = Workspace::new();
    let presenter = FakePresenter::new(Mode::Allow);
    let gate = gate(&presenter);
    let run = gate.register_run(Backend::Codex);
    let token = gate.ask(shell_spec(run, ws.path(), "ls")).unwrap();
    assert_eq!(
        token.request().binding(),
        Binding::Run {
            id: run,
            backend: Backend::Codex
        }
    );
    let token = gate
        .ask(RequestSpec {
            run: None,
            workspace_root: ws.path().to_path_buf(),
            class: ClassSpec::AutoApproveRule {
                old: None,
                new: "x".into(),
            },
        })
        .unwrap();
    assert_eq!(token.request().binding(), Binding::Application);
}
