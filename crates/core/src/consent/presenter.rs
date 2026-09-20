//! The presenter: defined by the core, implemented by the shell.
//!
//! The gate hands a presenter a fully rendered request and a [`Responder`], and the
//! presenter answers through the responder from whatever thread its dialog lives on. The
//! answer never transits IPC, and the presenter never sees a token: it reports a click, and
//! the gate decides — after re-checking cancellation, the run, the policy and the settle
//! interval — whether that click mints anything.

use std::sync::mpsc::Sender;
use std::time::Instant;

use super::request::InvocationId;

/// What the user clicked. There are two buttons and no third answer; a presenter that
/// cannot map a native response onto one of these reports [`Responder::fail`] instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Decline,
    Allow,
}

/// Why a presenter could not present, or could not answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresenterError(pub String);

/// A presenter-issued handle for a dialog it opened, so the gate can ask for it to be
/// dismissed when its request is withdrawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Handle(pub u64);

/// A request as the dialog shows it. Every string is core-generated or has passed through
/// [`super::render::escape`]; the two are never mixed in one field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rendered {
    pub invocation: InvocationId,
    /// Names the run (or the application) and the backend, and the class of request.
    pub title: String,
    /// The whole of what will run, escaped. Nothing is truncated from the tail.
    pub body: String,
    /// Fields the core parsed, shown *alongside* the raw bytes — the host of a URL, the
    /// path a write targets — never instead of them.
    pub parsed: Vec<(String, String)>,
    /// Button captions, negative first. A presenter adds them in this order, since the
    /// affirmative is never the default.
    pub negative: &'static str,
    pub affirmative: &'static str,
}

/// Where a presenter's answer goes. Owned by the presenter until it answers; dropping it
/// unanswered is a presenter failure, not a pending request.
pub struct Responder {
    pub(super) invocation: InvocationId,
    pub(super) opened: Instant,
    pub(super) tx: Sender<Outcome>,
    pub(super) answered: bool,
}

impl Responder {
    pub fn invocation(&self) -> InvocationId {
        self.invocation
    }

    /// Reports the user's answer. Timestamped here, at the moment the presenter saw the
    /// click, which is what the settle interval is measured against.
    pub fn answer(mut self, answer: Answer) {
        self.answered = true;
        let _ = self.tx.send(Outcome::Answered {
            answer,
            at: Instant::now(),
            opened: self.opened,
        });
    }

    /// Reports that the dialog produced no usable answer.
    pub fn fail(mut self, error: PresenterError) {
        self.answered = true;
        let _ = self.tx.send(Outcome::Failed(error));
    }
}

impl Drop for Responder {
    /// Dropped unanswered — the dialog closed with no result — is a presenter failure. The
    /// gate keeps its own sender for withdrawal, so a closed channel cannot signal this.
    fn drop(&mut self) {
        if !self.answered {
            let _ = self.tx.send(Outcome::Failed(PresenterError(
                "presenter returned nothing".into(),
            )));
        }
    }
}

/// What the gate receives on the channel behind a [`Responder`].
#[derive(Debug)]
pub(super) enum Outcome {
    Answered {
        answer: Answer,
        at: Instant,
        opened: Instant,
    },
    Failed(PresenterError),
    /// Sent by the gate itself when it withdraws the request, so a wait on the channel ends
    /// whether or not the presenter ever answers.
    Withdrawn,
}

/// The core's view of a native modal. The shell implements it; the core never opens a
/// window of its own.
pub trait ConsentPresenter: Send + Sync {
    /// The largest `body` this presenter can show in full, in bytes. A request over it is
    /// refused before `show` is called; a hash or a summary is not a substitute.
    fn capacity(&self) -> usize;

    /// Opens the dialog and returns at once. The answer arrives through `responder`.
    /// `Err` means nothing was shown.
    fn show(&self, rendered: &Rendered, responder: Responder) -> Result<Handle, PresenterError>;

    /// Closes a dialog whose request was withdrawn, so it does not stall the queue behind
    /// it. The presenter may still answer through the responder afterwards; the gate
    /// ignores that.
    fn dismiss(&self, handle: Handle);
}
