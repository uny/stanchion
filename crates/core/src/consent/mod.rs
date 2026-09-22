//! The consent gate: the one door between a request and its execution.
//!
//! `docs/architecture.md`, "An IPC message is not consent", states the rule; this module is
//! it. The gate builds the [`Request`], asks [`Policy`], renders the request through the
//! lossless escape, asks the [`ConsentPresenter`] the shell implements, and — only if the
//! answer survives every re-check — mints a [`ConsentToken`]. The entry points in
//! [`crate::execute`] demand that token and hand it back to [`Consent::redeem`], which is
//! where it is spent.
//!
//! Presentation is serialised, one dialog at a time per process; the gate is not. A run
//! waiting on a dialog blocks only its own thread.

pub mod policy;
pub mod presenter;
pub mod render;
pub mod request;
pub mod token;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use policy::{Policy, Refusal, Tier};
use presenter::{Answer, ConsentPresenter, Handle, Outcome, Rendered, Responder};
use request::{
    Backend, Binding, Class, ClassSpec, InvocationId, Prior, Request, RequestSpec, RunId,
    Sha256Digest,
};
use token::{Approved, ConsentToken, Origin, TokenId};

/// The captions on the two buttons. Negative first, everywhere.
pub const NEGATIVE: &str = "Deny";
pub const AFFIRMATIVE: &str = "Allow";

/// Tunables. The defaults are what the application runs with; tests narrow them.
#[derive(Clone, Debug)]
pub struct Config {
    /// An affirmative answered sooner than this after the dialog opened is treated as a
    /// decline: the WebView chooses when a request fires and can bait a click timed to it.
    pub settle: Duration,
    /// Requests waiting to be presented, including the one on screen, beyond which a new
    /// request is refused rather than stacked.
    pub queue_limit: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            settle: Duration::from_millis(500),
            queue_limit: 8,
        }
    }
}

struct Pending {
    binding: Binding,
    tx: Sender<Outcome>,
    handle: Option<Handle>,
    withdrawn: bool,
}

#[derive(Default)]
struct State {
    next_id: u64,
    live_runs: HashMap<RunId, Backend>,
    pending: HashMap<InvocationId, Pending>,
    /// Invocations cancelled after a token was minted for them.
    withdrawn: HashSet<InvocationId>,
    spent: HashSet<TokenId>,
    queued: usize,
}

/// The gate. One per process; `Sync`, so any run's thread may ask it.
pub struct Consent {
    presenter: Arc<dyn ConsentPresenter>,
    policy: Arc<dyn Policy>,
    config: Config,
    state: Mutex<State>,
    /// Whether a presentation is outstanding; at most one is. A request waits here for
    /// its turn, and is woken when the slot frees or when it is withdrawn.
    slot: Mutex<bool>,
    slot_changed: Condvar,
    /// One lock per canonical target directory, held across verifying a write's
    /// precondition and performing the write. Serialises the core's own writers, and only
    /// those.
    write_locks: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
}

impl Consent {
    pub fn new(
        presenter: Arc<dyn ConsentPresenter>,
        policy: Arc<dyn Policy>,
        config: Config,
    ) -> Self {
        Consent {
            presenter,
            policy,
            config,
            state: Mutex::new(State::default()),
            slot: Mutex::new(false),
            slot_changed: Condvar::new(),
            write_locks: Mutex::new(HashMap::new()),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // A panic while holding the lock is a bug in this module; the state is still
        // consistent enough to fail closed, which every path below does.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Registers a run, so requests can be bound to it and tokens die with it.
    pub fn register_run(&self, backend: Backend) -> RunId {
        let mut s = self.state();
        s.next_id += 1;
        let id = RunId(s.next_id);
        s.live_runs.insert(id, backend);
        id
    }

    /// Ends a run: its pending requests are withdrawn and its tokens are void.
    pub fn end_run(&self, run: RunId) {
        let handles = {
            let mut s = self.state();
            s.live_runs.remove(&run);
            let pending: Vec<InvocationId> = s
                .pending
                .iter()
                .filter(|(_, p)| matches!(p.binding, Binding::Run { id, .. } if id == run))
                .map(|(inv, _)| *inv)
                .collect();
            pending
                .into_iter()
                .filter_map(|inv| withdraw_locked(&mut s, inv))
                .collect::<Vec<_>>()
        };
        self.dismiss_all(handles);
    }

    /// Cancels one request. A dialog it has open is dismissed; a token minted for it is
    /// void; an answer that arrives afterwards mints nothing.
    pub fn cancel(&self, invocation: InvocationId) {
        let handle = withdraw_locked(&mut self.state(), invocation);
        self.dismiss_all(handle);
    }

    /// Dismisses dialogs *after* the gate's lock is released: `dismiss` is the shell's code
    /// and may hop threads or call back into the gate, and neither may happen under it.
    /// Also wakes every request waiting for the slot, so a withdrawn one stops waiting.
    fn dismiss_all(&self, handles: impl IntoIterator<Item = Handle>) {
        for h in handles {
            self.presenter.dismiss(h);
        }
        // Under the slot lock, so a waiter between its check and its wait is not missed.
        let _slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        self.slot_changed.notify_all();
    }

    fn is_withdrawn(&self, invocation: InvocationId) -> bool {
        self.state()
            .pending
            .get(&invocation)
            .is_none_or(|p| p.withdrawn)
    }

    /// Asks for consent. Blocks the calling thread until there is an answer or the request
    /// is withdrawn. `Ok` is the only way a [`ConsentToken`] comes into existence.
    ///
    /// Never call this from the thread the presenter needs — a native modal runs on the
    /// main thread, and a main thread parked here would never open it.
    pub fn ask(&self, spec: RequestSpec) -> Result<ConsentToken, Refusal> {
        self.ask_observed(spec, &mut |_| {})
    }

    /// [`Consent::ask`], with `observer` called once, on the asking thread, at the moment
    /// the request is pending and about to be presented — with the same [`Rendered`] the
    /// dialog is given, its invocation id included. That is the only way an asker learns
    /// what is being asked before it is answered: the return value names the invocation
    /// only when a token was minted. A request the policy refuses, or that is over the
    /// presenter's capacity or past the queue limit, is refused before the observer is
    /// called, so it is never told of a request that will not be shown. No gate lock is
    /// held across the call, so the observer may call back into the gate — [`cancel`]
    /// included, which withdraws the request before it is presented — but not ask.
    ///
    /// [`cancel`]: Consent::cancel
    pub fn ask_observed(
        &self,
        spec: RequestSpec,
        observer: &mut dyn FnMut(&Rendered),
    ) -> Result<ConsentToken, Refusal> {
        let request = Arc::new(self.build(spec)?);

        // Consent is not authorization: the refused tier never reaches a dialog, and
        // auto-run takes the same door with a token recorded as policy.
        match self.policy.classify(&request) {
            Tier::Refused => return Err(Refusal::RefusedTier),
            Tier::AutoRun => return Ok(self.mint(request, Origin::Policy)),
            Tier::Ask => {}
        }

        let rendered = render(&request);
        let capacity = self.presenter.capacity();
        let bytes = rendered.shown_len();
        if bytes > capacity {
            return Err(Refusal::OverCapacity { bytes, capacity });
        }

        let (tx, rx) = mpsc::channel();
        let invocation = request.invocation;
        {
            let mut s = self.state();
            if s.queued >= self.config.queue_limit {
                return Err(Refusal::QueueFull);
            }
            // The run was live when the request was built; it may have ended since, and
            // a request registered now would be one `end_run` has already looked for.
            if let Binding::Run { id, .. } = request.binding {
                if !s.live_runs.contains_key(&id) {
                    return Err(Refusal::Withdrawn);
                }
            }
            s.queued += 1;
            s.pending.insert(
                invocation,
                Pending {
                    binding: request.binding,
                    tx: tx.clone(),
                    handle: None,
                    withdrawn: false,
                },
            );
        }
        observer(&rendered);
        let outcome = self.present(&rendered, invocation, tx, &rx);
        let re_tier = self.policy.classify(&request);
        let withdrawn = {
            let mut s = self.state();
            s.queued -= 1;
            s.pending
                .remove(&invocation)
                .map(|p| p.withdrawn)
                .unwrap_or(true)
        };

        if withdrawn {
            return Err(Refusal::Withdrawn);
        }
        let (answer, at, opened) = match outcome {
            Ok(Outcome::Answered { answer, at, opened }) => (answer, at, opened),
            Ok(Outcome::Failed(e)) => return Err(Refusal::PresenterFailed(e.0)),
            Ok(Outcome::Withdrawn) => return Err(Refusal::Withdrawn),
            Err(r) => return Err(r),
        };
        // Re-classified while pending: the new tier wins over the answer.
        if re_tier == Tier::Refused {
            return Err(Refusal::RefusedTier);
        }
        match answer {
            Answer::Decline => Err(Refusal::Declined),
            Answer::Allow if at.duration_since(opened) < self.config.settle => {
                Err(Refusal::TooSoon)
            }
            Answer::Allow => Ok(self.mint(request, Origin::Consent)),
        }
    }

    /// Waits for the presentation slot, shows the dialog, and waits for the outcome. Holds
    /// no gate lock while waiting.
    fn present(
        &self,
        rendered: &Rendered,
        invocation: InvocationId,
        tx: Sender<Outcome>,
        rx: &mpsc::Receiver<Outcome>,
    ) -> Result<Outcome, Refusal> {
        let _slot = {
            let mut busy = self.slot.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                // Withdrawn while queued: do not open a dialog for it, and do not keep it
                // waiting behind the one on screen.
                if self.is_withdrawn(invocation) {
                    return Ok(Outcome::Withdrawn);
                }
                if !*busy {
                    *busy = true;
                    break Slot(self);
                }
                busy = self
                    .slot_changed
                    .wait(busy)
                    .unwrap_or_else(|e| e.into_inner());
            }
        };
        let responder = Responder {
            invocation,
            opened: Instant::now(),
            tx,
            answered: false,
        };
        let handle = self
            .presenter
            .show(rendered, responder)
            .map_err(|e| Refusal::PresenterFailed(e.0))?;
        // Withdrawn between `show` and here: the withdrawal saw no handle to dismiss, so
        // this is where the dialog it left open is closed.
        let withdrawn_meanwhile = {
            let mut s = self.state();
            match s.pending.get_mut(&invocation) {
                Some(p) => {
                    p.handle = Some(handle);
                    p.withdrawn
                }
                None => true,
            }
        };
        if withdrawn_meanwhile {
            self.presenter.dismiss(handle);
        }
        // The channel cannot close while the gate holds a sender for withdrawal; a
        // responder dropped unanswered reports itself as a failure instead.
        rx.recv()
            .map_err(|_| Refusal::PresenterFailed("presenter returned nothing".into()))
    }

    fn mint(&self, request: Arc<Request>, origin: Origin) -> ConsentToken {
        let mut s = self.state();
        s.next_id += 1;
        ConsentToken::mint(TokenId(s.next_id), request, origin)
    }

    /// Spends a token. Called by the entry points in [`crate::execute`] and nowhere else;
    /// they are the doors, and this is the check behind every one of them.
    pub(crate) fn redeem(&self, token: ConsentToken) -> Result<Approved, Refusal> {
        let ConsentToken {
            id,
            request,
            origin,
        } = token;
        let mut s = self.state();
        if !s.spent.insert(id) {
            // Unreachable through the public API — the token was moved — but a door is
            // checked, not trusted.
            return Err(Refusal::Withdrawn);
        }
        if s.withdrawn.contains(&request.invocation) {
            return Err(Refusal::Withdrawn);
        }
        if let Binding::Run { id, .. } = request.binding {
            if !s.live_runs.contains_key(&id) {
                return Err(Refusal::Withdrawn);
            }
        }
        Ok(Approved { request, origin })
    }

    /// Performs an approved write: verifies the precondition and writes, as one operation
    /// under the target directory's write lock. What this guarantees is that the core's write
    /// lands on what was verified unless a process outside the core changed it inside the
    /// window; a check followed by a write would not guarantee even that.
    pub(crate) fn write(&self, approved: &Approved) -> Result<(), Refusal> {
        let Class::FileWrite {
            path,
            parent,
            content,
            prior,
            ..
        } = &approved.request.class
        else {
            return Err(Refusal::WrongDoor);
        };
        // Keyed by the canonical directory, so two requests naming one file — under two
        // roots, one root spelled two ways, or a name in two cases on a case-insensitive
        // filesystem — contend for the same lock. The directory exists, so its canonical
        // form is one spelling; a name that does not yet exist has none.
        let lock = {
            let mut locks = self.write_locks.lock().unwrap_or_else(|e| e.into_inner());
            locks.entry(parent.clone()).or_default().clone()
        };
        let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
        // A target that can no longer be snapshotted — a symlink or a directory where a
        // file was — is a changed precondition, not a resolution failure.
        let changed = |e| match e {
            Refusal::Unresolvable(what) => Refusal::PreconditionChanged(what),
            other => other,
        };
        let now_parent = canonical_parent(path).map_err(changed)?;
        let now_prior = snapshot(path).map_err(changed)?;
        if &now_parent != parent {
            return Err(Refusal::PreconditionChanged(
                "path now resolves elsewhere".into(),
            ));
        }
        if now_prior != *prior {
            return Err(Refusal::PreconditionChanged(match (prior, now_prior) {
                (Prior::Absent, _) => "target has appeared".into(),
                (_, Prior::Absent) => "target has disappeared".into(),
                _ => "target has changed".into(),
            }));
        }
        std::fs::write(path, content).map_err(|e| Refusal::Unresolvable(e.to_string()))
    }

    /// Turns what the caller supplied into the value the token binds: core-issued id, the
    /// run's backend, resolved paths, snapshotted target.
    fn build(&self, spec: RequestSpec) -> Result<Request, Refusal> {
        let binding = match spec.run {
            Some(id) => {
                let backend = *self.state().live_runs.get(&id).ok_or(Refusal::UnknownRun)?;
                Binding::Run { id, backend }
            }
            None => Binding::Application,
        };
        let class = match spec.class {
            ClassSpec::ShellCommand { command, cwd, env } => Class::ShellCommand {
                command,
                cwd: resolve_against(&spec.workspace_root, cwd),
                env,
            },
            ClassSpec::FileWrite { path, content } => {
                let path = resolve_against(&spec.workspace_root, path);
                // The workspace root is the boundary for filesystem tools
                // (`docs/architecture.md`); compared on canonical paths, so neither `..`
                // nor a symlink out of the tree crosses it. Checked before the target is
                // read, so a path outside the root is refused, not inspected.
                let root = spec.workspace_root.canonicalize().map_err(|e| {
                    Refusal::Unresolvable(format!("{}: {e}", spec.workspace_root.display()))
                })?;
                // One resolution serves both the check and the binding: a directory that
                // is checked and then resolved again could be swapped in between.
                let parent = canonical_parent(&path)?;
                if !parent.starts_with(&root) {
                    return Err(Refusal::OutsideWorkspace(path));
                }
                let prior = snapshot(&path)?;
                let content: Arc<[u8]> = content.into();
                let content_hash = Sha256Digest::of(&content);
                Class::FileWrite {
                    path,
                    parent,
                    content,
                    content_hash,
                    prior,
                }
            }
            ClassSpec::McpServerEntry { name, old, new } => {
                Class::McpServerEntry { name, old, new }
            }
            ClassSpec::CredentialProviderCommand { old, new } => {
                Class::CredentialProviderCommand { old, new }
            }
            ClassSpec::GatewayUrl { old, new } => Class::GatewayUrl { old, new },
            ClassSpec::WorkspaceRoot { old, new } => Class::WorkspaceRoot { old, new },
            ClassSpec::AutoApproveRule { old, new } => Class::AutoApproveRule { old, new },
            ClassSpec::InlineProfileRun { profile } => Class::InlineProfileRun { profile },
            ClassSpec::CliCommand {
                session_grant: true,
                ..
            } => {
                // An auto-approve rule the model proposed, wearing an approval's clothes.
                return Err(Refusal::SessionGrant);
            }
            ClassSpec::CliCommand {
                cli_request_id,
                command,
                cwd,
                session_grant: false,
            } => Class::CliCommand {
                cli_request_id,
                command,
                cwd: resolve_against(&spec.workspace_root, cwd),
            },
            ClassSpec::BridgeForward { tool, arguments } => {
                Class::BridgeForward { tool, arguments }
            }
        };
        let mut s = self.state();
        s.next_id += 1;
        Ok(Request {
            invocation: InvocationId(s.next_id),
            binding,
            workspace_root: spec.workspace_root,
            class,
        })
    }
}

/// The presentation slot, held for the life of one presentation. Frees itself however the
/// presentation ends — answered, withdrawn, failed, or unwound — and wakes the next waiter.
struct Slot<'a>(&'a Consent);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        let mut busy = self.0.slot.lock().unwrap_or_else(|e| e.into_inner());
        *busy = false;
        self.0.slot_changed.notify_all();
    }
}

/// Marks a request withdrawn and wakes its `ask`. Returns the handle of a dialog it has
/// open, for the caller to dismiss once the lock is released.
fn withdraw_locked(s: &mut State, invocation: InvocationId) -> Option<Handle> {
    s.withdrawn.insert(invocation);
    let p = s.pending.get_mut(&invocation)?;
    p.withdrawn = true;
    // Wakes the waiting `ask` whether or not the presenter ever answers.
    let _ = p.tx.send(Outcome::Withdrawn);
    p.handle
}

/// The state of the target itself. `symlink_metadata`, so a symlink where a file was is a
/// change, not a file; and a file with a second hard link is not a regular file either,
/// since writing it writes the other name too.
fn snapshot(path: &Path) -> Result<Prior, Refusal> {
    let prior = match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Prior::Absent,
        Err(e) => return Err(Refusal::Unresolvable(format!("{}: {e}", path.display()))),
        Ok(meta) if meta.file_type().is_file() && !hard_linked(&meta) => {
            let bytes = std::fs::read(path)
                .map_err(|e| Refusal::Unresolvable(format!("{}: {e}", path.display())))?;
            Prior::File(Sha256Digest::of(&bytes))
        }
        Ok(_) => {
            return Err(Refusal::Unresolvable(format!(
                "{}: not a regular file",
                path.display()
            )))
        }
    };
    Ok(prior)
}

/// The canonical form of the directory a path names, which is what a write is bound to.
fn canonical_parent(path: &Path) -> Result<PathBuf, Refusal> {
    let parent = path
        .parent()
        .ok_or_else(|| Refusal::Unresolvable("path has no parent".into()))?;
    parent
        .canonicalize()
        .map_err(|e| Refusal::Unresolvable(format!("{}: {e}", parent.display())))
}

/// A relative path is relative to the workspace root, never to the process's directory.
fn resolve_against(root: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

#[cfg(unix)]
fn hard_linked(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    meta.nlink() > 1
}

#[cfg(not(unix))]
fn hard_linked(_: &std::fs::Metadata) -> bool {
    false
}

/// Renders a request for the dialog. Labels are core-generated; every model-supplied
/// value passes through the escape on its own, so the two never share a field. A body
/// that is one value (a command, a file's content) keeps its newlines; a value on a
/// labelled line goes through [`render::escape_inline`], so it cannot end its line and
/// forge the next.
pub fn render(request: &Request) -> Rendered {
    let esc = |s: &str| render::escape_inline(s.as_bytes());
    let path = |p: &Path| render::escape_inline(p.as_os_str().as_encoded_bytes());
    let program = |label: &str, p: &request::Program, out: &mut Vec<String>| {
        out.push(format!("{label} program: {}", esc(&p.program)));
        for (i, a) in p.args.iter().enumerate() {
            out.push(format!("{label} arg[{i}]: {}", esc(a)));
        }
        for (k, v) in &p.env {
            out.push(format!("{label} env {}: {}", esc(k), esc(v)));
        }
    };
    let opt_program = |label: &str, p: &Option<request::Program>, out: &mut Vec<String>| match p {
        Some(p) => program(label, p, out),
        None => out.push(format!("{label}: (none)")),
    };
    let opt = |label: &str, v: &Option<String>| match v {
        Some(v) => format!("{label}: {}", esc(v)),
        None => format!("{label}: (none)"),
    };

    let who = match request.binding {
        Binding::Run { id, backend } => format!("{id} ({})", backend.label()),
        Binding::Application => "application".to_string(),
    };
    let title = format!("{who} \u{2014} {}", request.class.label());

    let mut lines: Vec<String> = Vec::new();
    let mut parsed: Vec<(String, String)> = Vec::new();
    match &request.class {
        Class::ShellCommand { command, .. } => lines.push(render::escape(command.as_bytes())),
        Class::FileWrite {
            path: p,
            parent,
            content,
            ..
        } => {
            parsed.push(("path".into(), path(p)));
            // Where the bytes land, with every symlink in the directory resolved; the
            // request was bound to this at build time.
            if let Some(name) = p.file_name() {
                parsed.push(("resolves to".into(), path(&parent.join(name))));
            }
            lines.push(render::escape(content));
        }
        Class::McpServerEntry { name, old, new } => {
            lines.push(format!("name: {}", esc(name)));
            opt_program("old", old, &mut lines);
            program("new", new, &mut lines);
        }
        Class::CredentialProviderCommand { old, new } => {
            opt_program("old", old, &mut lines);
            program("new", new, &mut lines);
        }
        Class::GatewayUrl { old, new } => {
            lines.push(opt("old", old));
            lines.push(format!("new: {}", esc(new)));
            parsed.push(("new host".into(), esc(&host_of(new))));
        }
        Class::WorkspaceRoot { old, new } => {
            lines.push(format!("old: {}", path(old)));
            lines.push(format!("new: {}", path(new)));
        }
        Class::AutoApproveRule { old, new } => {
            lines.push(opt("old", old));
            lines.push(format!("new: {}", esc(new)));
        }
        Class::InlineProfileRun { profile } => {
            lines.push(format!("name: {}", esc(&profile.name)));
            lines.push(format!("base URL: {}", esc(&profile.base_url)));
            lines.push(format!("model: {}", esc(&profile.model)));
            parsed.push(("host".into(), esc(&host_of(&profile.base_url))));
        }
        Class::CliCommand { command, cwd, .. } => {
            lines.push(render::escape(command.as_bytes()));
            parsed.push(("cwd".into(), path(cwd)));
        }
        Class::BridgeForward { tool, arguments } => {
            lines.push(format!("tool: {}", esc(tool)));
            lines.push(format!("arguments: {}", esc(arguments)));
        }
    }

    Rendered {
        invocation: request.invocation,
        title,
        body: lines.join("\n"),
        parsed,
        negative: NEGATIVE,
        affirmative: AFFIRMATIVE,
    }
}

/// The host a URL actually names, so `https://api.example.com@evil.example/` reads as what
/// it is. Shown beside the raw bytes, never instead of them. A backslash ends the authority
/// as a slash does, which is how WHATWG clients read `https://evil.example\@api.example.com/`.
fn host_of(url: &str) -> String {
    // WHATWG: a special scheme (http, https, ws, wss, ftp) skips any run of slashes in
    // either direction before its authority, so `https:\\host` names `host`; any other
    // scheme has an authority only after exactly `//`; no scheme, no host.
    let after_scheme = match url.split_once(':') {
        Some((scheme, rest))
            if scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
                && scheme.starts_with(|c: char| c.is_ascii_alphabetic()) =>
        {
            let special = matches!(
                scheme.to_ascii_lowercase().as_str(),
                "http" | "https" | "ws" | "wss" | "ftp"
            );
            if special {
                rest.trim_start_matches(['/', '\\'])
            } else if let Some(authority) = rest.strip_prefix("//") {
                authority
            } else {
                return String::new();
            }
        }
        _ => return String::new(),
    };
    let authority = after_scheme
        .split(['/', '\\', '?', '#'])
        .next()
        .unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or("");
    let host = if host_port.starts_with('[') {
        host_port
            .split(']')
            .next()
            .map(|h| format!("{h}]"))
            .unwrap_or_default()
    } else {
        host_port.split(':').next().unwrap_or("").to_string()
    };
    host
}
