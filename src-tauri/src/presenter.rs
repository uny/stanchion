//! The shell's half of consent: the presenter the core asks.
//!
//! The mechanism is decided (`docs/decisions.md`, "Consent is a native dialog the core
//! owns"): a native `NSAlert` driven from Rust through `objc2`, with the negative button
//! added first so the affirmative is never the default, and the alert held so the core can
//! abort the modal when it withdraws a request. [`NativeAlert`] is that presenter. The pure
//! half — the button list, the mapping from the first slot to *decline*, the mapping from
//! the alert's response code to a slot, the settle rule — is tested on its values here;
//! the half that needs a running application is measured by hand with
//! `examples/alert_probe.rs` (`docs/decisions.md`, "The native alert, as built and
//! measured").
//! [`FailClosed`] stays as the presenter that shows nothing.
//!
//! What must never appear here: an application command that takes an approval decision,
//! or a `dialog:` permission in `capabilities/default.json`. `tests` checks both.

use std::collections::HashSet;
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertSecondButtonReturn, NSApplication, NSModalResponse,
    NSRequestUserAttentionType, NSScreen, NSWindowDidBecomeKeyNotification,
};
use objc2_core_foundation::{kCFRunLoopCommonModes, kCFRunLoopDefaultMode, CFRunLoop};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSString};
use stanchion_core::consent::presenter::{
    Answer, ConsentPresenter, Handle, PresenterError, Rendered, Responder,
};
use stanchion_core::consent::{AFFIRMATIVE, NEGATIVE};

/// The captions in the order a native alert receives them. `NSAlert` makes the first button
/// added the default (Return), so the negative goes first.
pub fn buttons() -> [&'static str; 2] {
    [NEGATIVE, AFFIRMATIVE]
}

/// Maps the index of the button the user pressed to an answer. Getting the order in
/// [`buttons`] right without this mapping would execute on the Deny click. Any other slot —
/// a response code the alert was not built with — is no answer at all.
pub fn answer_for_slot(slot: usize) -> Option<Answer> {
    match slot {
        0 => Some(Answer::Decline),
        1 => Some(Answer::Allow),
        _ => None,
    }
}

/// Maps the code `runModal` returned to the index of the button pressed. The alert is built
/// with two buttons, so only the first two codes are a slot; an aborted modal
/// (`NSModalResponseAbort`) or anything else is not a press.
pub fn slot_for_response(response: NSModalResponse) -> Option<usize> {
    if response == NSAlertFirstButtonReturn {
        Some(0)
    } else if response == NSAlertSecondButtonReturn {
        Some(1)
    } else {
        None
    }
}

/// The largest request [`NativeAlert`] lays out at all. It is a bound on work, not a promise
/// that anything under it fits: whether a request fits is decided on the laid-out alert,
/// against the screen, and a request under this that does not is refused as not fitting.
/// Laying out is done on the main thread and grows faster than the text — measured at
/// about 120 ms for 4 KiB with no break to wrap at, and 2.4 s for 5.4 KiB over 400 lines —
/// while a laid-out alert grows about 16 points per line of about 60 characters, so 4 KiB
/// is already taller than the visible area of a 900-point screen.
pub const LAYOUT_BOUND: usize = 4096;

/// The height of one line of an alert's informative text, in points. Measured; a body
/// with more lines than the screen has room for at this height is refused without laying
/// it out.
const LINE_HEIGHT: f64 = 16.0;

/// `NSAlert`, driven from the thread the gate asks on.
///
/// `show` hands the alert to the main run loop and returns. The alert is run from a
/// `CFRunLoopPerformBlock` block in the default mode, not from a block on the GCD main
/// queue: the main queue is serial, so a modal run inside one of its blocks holds back
/// every later block — a dismissal among them — until the modal ends. Measured, as is the
/// rest of this paragraph. A dismissal is a block in the common modes, which the modal run
/// loop drains, and it runs on the main thread, where the alert on screen is known without
/// a race. An alert queued behind one still on screen waits in the default mode, which the
/// modal run loop does not run, so two alerts are never nested.
///
/// Before the alert is shown it is laid out; if it is taller than the shortest screen it
/// could appear on, the request is refused as not fitting rather than shown clipped. The
/// alert does not take focus: an application that is not frontmost asks for attention,
/// and the settle interval starts again each time the alert becomes the key window. An
/// *Allow* inside it — or before the alert has ever been key — is not an answer: the
/// alert is run again, unchanged, rather than the request refused, since the click that
/// brings the application forward is the one most likely to land on it.
pub struct NativeAlert {
    state: Arc<Mutex<State>>,
    settle: Duration,
}

#[derive(Default)]
struct State {
    /// The last handle issued.
    issued: u64,
    /// The last handle whose block has finished on the main thread. Blocks run in the order
    /// their handles are issued, one at a time.
    finished: u64,
    /// Handles dismissed and not yet finished. Checked on the main thread before the modal
    /// starts; a dismissal that comes after that finds the handle live instead.
    cancelled: HashSet<u64>,
    /// The handle whose alert is running its modal, if any. Written on the main thread only.
    live: Option<u64>,
}

impl NativeAlert {
    /// `settle` is the gate's own interval (`Config::settle`), which the gate still checks
    /// after this presenter answers.
    pub fn new(settle: Duration) -> Self {
        NativeAlert {
            state: Arc::new(Mutex::new(State::default())),
            settle,
        }
    }
}

/// Whether an *Allow* pressed at `now` counts: the alert has been the key window, and has
/// been for `settle` since it last became key.
pub fn settled(key_at: Option<Instant>, now: Instant, settle: Duration) -> bool {
    key_at.is_some_and(|at| now.saturating_duration_since(at) >= settle)
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

/// Runs `work` on the main thread's run loop in the default or the common modes.
fn perform_on_main(common: bool, work: impl Fn() + 'static) {
    let Some(main) = CFRunLoop::main() else {
        return;
    };
    let block = RcBlock::new(work);
    // SAFETY: both modes are CFString constants CoreFoundation defines; the block is copied
    // by `CFRunLoopPerformBlock` and run once on the main thread.
    unsafe {
        let mode = if common {
            kCFRunLoopCommonModes
        } else {
            kCFRunLoopDefaultMode
        };
        main.perform_block(mode.map(|m| &**m), Some(&block));
    }
    main.wake_up();
}

impl ConsentPresenter for NativeAlert {
    fn capacity(&self) -> usize {
        LAYOUT_BOUND
    }

    fn show(&self, rendered: &Rendered, responder: Responder) -> Result<Handle, PresenterError> {
        let id = {
            let mut s = lock(&self.state);
            s.issued += 1;
            s.issued
        };
        let state = Arc::clone(&self.state);
        let settle = self.settle;
        let rendered = rendered.clone();
        // The block type is `Fn`; it runs once, so the responder is taken out of a cell.
        let responder = Mutex::new(Some(responder));
        perform_on_main(false, move || {
            let Some(responder) = responder.lock().ok().and_then(|mut r| r.take()) else {
                return;
            };
            let cancelled = lock(&state).cancelled.contains(&id);
            if cancelled {
                finish(&state, id);
                // The gate has already withdrawn it and ignores this.
                responder.fail(PresenterError("dismissed before it was shown".into()));
                return;
            }
            let Some(mtm) = MainThreadMarker::new() else {
                responder.fail(PresenterError("not on the main thread".into()));
                finish(&state, id);
                return;
            };
            // An unwind out of a CoreFoundation callout is not recoverable; a panic here
            // drops the responder, which the gate reads as presenter failure.
            let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
                run_alert(mtm, &state, id, settle, &rendered, responder)
            }));
            finish(&state, id);
        });
        Ok(Handle(id))
    }

    fn dismiss(&self, handle: Handle) {
        let id = handle.0;
        {
            let mut s = lock(&self.state);
            if id <= s.finished {
                return;
            }
            s.cancelled.insert(id);
        }
        // Recorded first, so a block that has not reached its modal yet will not start it;
        // this closes one that already has.
        let state = Arc::clone(&self.state);
        perform_on_main(true, move || {
            if lock(&state).live != Some(id) {
                return;
            }
            if let Some(mtm) = MainThreadMarker::new() {
                NSApplication::sharedApplication(mtm).abortModal();
            }
        });
    }
}

/// Marks `id` done: no longer live, no longer cancellable.
fn finish(state: &Mutex<State>, id: u64) {
    let mut s = lock(state);
    s.live = None;
    s.finished = s.finished.max(id);
    s.cancelled.remove(&id);
}

/// Builds, checks, lays out and runs one alert, and answers for it. Main thread only.
fn run_alert(
    mtm: MainThreadMarker,
    state: &Mutex<State>,
    id: u64,
    settle: Duration,
    rendered: &Rendered,
    responder: Responder,
) {
    let alert = NSAlert::new(mtm);
    // The parsed fields go with the title, the raw body alone below it: the body keeps its
    // newlines, so a body sharing a field with labels could forge one.
    let mut message = rendered.title.clone();
    for (label, value) in &rendered.parsed {
        message.push('\n');
        message.push_str(label);
        message.push_str(": ");
        message.push_str(value);
    }
    alert.setMessageText(&NSString::from_str(&message));
    alert.setInformativeText(&NSString::from_str(&rendered.body));
    for caption in buttons() {
        alert.addButtonWithTitle(&NSString::from_str(caption));
    }

    // Return must press the negative and nothing may press the affirmative from the
    // keyboard. `NSAlert` assigns both from the order and the captions; check what it did
    // rather than trust it.
    let keys: Vec<String> = alert
        .buttons()
        .iter()
        .map(|b| b.keyEquivalent().to_string())
        .collect();
    if keys != ["\r", ""] {
        responder.fail(PresenterError(format!(
            "unexpected key equivalents: {keys:?}"
        )));
        return;
    }

    // No screen, or a height that is not a number, fits nowhere.
    let Some(room) = NSScreen::screens(mtm)
        .iter()
        .map(|screen| screen.visibleFrame().size.height)
        .reduce(f64::min)
    else {
        responder.does_not_fit();
        return;
    };
    // Every line of the body is at least one line of the alert; laying out a body that
    // cannot fit would only hold the main thread.
    let lines = rendered.body.matches('\n').count() + 1;
    if lines as f64 * LINE_HEIGHT > room {
        responder.does_not_fit();
        return;
    }
    alert.layout();
    let height = alert.window().frame().size.height;
    let fits = matches!(
        height.partial_cmp(&room),
        Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
    );
    if !fits {
        responder.does_not_fit();
        return;
    }

    // Each time the alert becomes key the settle interval starts again, so a click aimed at
    // another window cannot land on it the moment it comes forward.
    let responder = Arc::new(Mutex::new(Some(responder)));
    let key = Arc::new(Mutex::new(None::<Instant>));
    let observer = {
        let responder = Arc::clone(&responder);
        let key = Arc::clone(&key);
        let block = RcBlock::new(move |_: NonNull<NSNotification>| {
            if let Ok(mut r) = responder.lock() {
                if let Some(r) = r.as_mut() {
                    r.shown();
                }
            }
            if let Ok(mut k) = key.lock() {
                *k = Some(Instant::now());
            }
        });
        let window = alert.window();
        // SAFETY: the notification is posted on the main thread, which is this one; the
        // observer is removed below, before anything it captures is dropped.
        unsafe {
            NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
                Some(NSWindowDidBecomeKeyNotification),
                Some(&window),
                None,
                &block,
            )
        }
    };

    {
        let mut s = lock(state);
        if s.cancelled.contains(&id) {
            drop(s);
            // SAFETY: the observer was returned by `addObserverForName:` above.
            unsafe { NSNotificationCenter::defaultCenter().removeObserver(observer.as_ref()) };
            if let Some(r) = responder.lock().ok().and_then(|mut r| r.take()) {
                r.fail(PresenterError("dismissed before it was shown".into()));
            }
            return;
        }
        s.live = Some(id);
    }
    let app = NSApplication::sharedApplication(mtm);
    // A critical request bounces until the application is activated or it is cancelled, so
    // an alert withdrawn while the application stays behind must cancel its own.
    let attention = (!app.isActive())
        .then(|| app.requestUserAttention(NSRequestUserAttentionType::CriticalRequest));
    let response = loop {
        let response = alert.runModal();
        let key_at = key.lock().map(|k| *k).unwrap_or(None);
        if slot_for_response(response).and_then(answer_for_slot) == Some(Answer::Allow)
            && !settled(key_at, Instant::now(), settle)
        {
            continue;
        }
        break response;
    };
    lock(state).live = None;
    if let Some(request) = attention {
        app.cancelUserAttentionRequest(request);
    }
    // SAFETY: the observer was returned by `addObserverForName:` above.
    unsafe { NSNotificationCenter::defaultCenter().removeObserver(observer.as_ref()) };

    let Some(responder) = responder.lock().ok().and_then(|mut r| r.take()) else {
        return;
    };
    match slot_for_response(response).and_then(answer_for_slot) {
        Some(answer) => responder.answer(answer),
        None => responder.fail(PresenterError(format!(
            "the alert closed with response {response}"
        ))),
    }
}

/// The presenter that shows nothing, so nothing is approved: for a harness with no main
/// thread to run an alert on (`tests/real_claude.rs`). Its capacity is zero, so the gate
/// refuses every request before it would be shown.
pub struct FailClosed;

impl ConsentPresenter for FailClosed {
    fn capacity(&self) -> usize {
        0
    }

    fn show(&self, _: &Rendered, _: Responder) -> Result<Handle, PresenterError> {
        Err(PresenterError("this presenter shows nothing".into()))
    }

    fn dismiss(&self, _: Handle) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every capability file, not only the default one: `tauri-build` loads
    /// `capabilities/**/*`, so a permission added in a second file, at any depth, widens
    /// the WebView exactly as one here would.
    fn capabilities() -> Vec<(String, String)> {
        fn walk(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
            for entry in std::fs::read_dir(dir).expect("capabilities/ exists") {
                let path = entry.expect("readable entry").path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|x| x == "json" || x == "toml") {
                    out.push((
                        path.display().to_string(),
                        std::fs::read_to_string(&path).unwrap(),
                    ));
                }
            }
        }
        let mut files = Vec::new();
        walk(
            std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/capabilities")),
            &mut files,
        );
        files.sort();
        assert!(!files.is_empty(), "no capability files");
        files
    }

    #[test]
    fn the_button_list_is_negative_first() {
        assert_eq!(buttons(), ["Deny", "Allow"]);
    }

    #[test]
    fn only_the_two_button_codes_are_a_slot() {
        assert_eq!(slot_for_response(NSAlertFirstButtonReturn), Some(0));
        assert_eq!(slot_for_response(NSAlertSecondButtonReturn), Some(1));
        // `NSModalResponseAbort`, which a dismissal returns, and a third button's code.
        assert_eq!(slot_for_response(-1001), None);
        assert_eq!(slot_for_response(NSAlertSecondButtonReturn + 1), None);
        assert_eq!(slot_for_response(0), None);
    }

    #[test]
    fn an_allow_counts_only_once_the_alert_has_been_key_for_the_settle_interval() {
        let settle = Duration::from_millis(500);
        let at = Instant::now();
        assert!(!settled(None, at + settle * 10, settle), "never key");
        assert!(!settled(Some(at), at, settle));
        assert!(!settled(
            Some(at),
            at + settle - Duration::from_millis(1),
            settle
        ));
        assert!(settled(Some(at), at + settle, settle));
        // Became key again after the press was timestamped: not settled.
        assert!(!settled(Some(at + settle), at, settle));
    }

    #[test]
    fn the_first_slot_maps_to_decline_and_nothing_else_maps_to_allow() {
        assert_eq!(answer_for_slot(0), Some(Answer::Decline));
        assert_eq!(answer_for_slot(1), Some(Answer::Allow));
        assert_eq!(answer_for_slot(2), None);
        assert_eq!(answer_for_slot(usize::MAX), None);
    }

    #[test]
    fn the_capability_grants_no_dialog_permission() {
        // `tauri-plugin-dialog` hands its result back to the WebView, which is the path the
        // decision closes. Its identifiers are `dialog:...`.
        for (file, text) in capabilities() {
            assert!(
                !text.contains("\"dialog:"),
                "{file} grants a dialog permission"
            );
        }
    }

    #[test]
    fn the_capability_grants_no_approval_shaped_command() {
        // Tauri names a command's permission `allow-<kebab-command>`.
        for (file, text) in capabilities() {
            let text = text.to_ascii_lowercase();
            for word in ["approve", "consent", "answer", "decision"] {
                assert!(
                    !text.contains(&format!("allow-{word}")),
                    "{file} grants an approval-shaped command: {word}"
                );
            }
        }
    }

    #[test]
    fn the_application_manifest_is_not_empty() {
        // #24: with no application permission manifest Tauri skips the ACL check entirely.
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/permissions/autogenerated");
        let entries: Vec<_> = std::fs::read_dir(dir)
            .expect("permissions/autogenerated exists")
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "toml"))
            .collect();
        assert!(!entries.is_empty(), "no autogenerated permission files");
    }

    #[test]
    fn the_fail_closed_presenter_shows_nothing() {
        assert_eq!(FailClosed.capacity(), 0);
    }
}
