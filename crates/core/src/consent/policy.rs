//! Policy is consulted by the gate; it is not consent.
//!
//! The refused tier is rejected before any dialog opens, auto-run mints a token on policy
//! and records it as such, and everything else asks. Policy is consulted again when an
//! answer comes back, so a request re-classified into the refused tier while pending is
//! refused whatever the answer was; a re-classification the other way changes nothing,
//! since the answer given was to a dialog.

use std::fmt;
use std::path::PathBuf;

use super::request::Request;

/// How policy classifies a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// Refused regardless of any approval (#33). Never reaches the presenter.
    Refused,
    /// Needs the user's answer.
    Ask,
    /// Runs without a dialog. The token this mints is recorded as policy, not consent.
    AutoRun,
}

pub trait Policy: Send + Sync {
    fn classify(&self, request: &Request) -> Tier;
}

/// A policy that asks about everything. The default, and what the tests use unless a case
/// is about policy.
pub struct AlwaysAsk;

impl Policy for AlwaysAsk {
    fn classify(&self, _: &Request) -> Tier {
        Tier::Ask
    }
}

/// Why no token was minted, or why one was not honoured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Policy puts the request in the refused tier.
    RefusedTier,
    /// The CLI's request was shaped as a session-wide grant, not one operation.
    SessionGrant,
    /// The run the request names is not live.
    UnknownRun,
    /// The presenter cannot show the request in full.
    OverCapacity { bytes: usize, capacity: usize },
    /// The presenter laid the request out and it cannot be shown in full.
    DoesNotFit,
    /// The presentation queue is at its limit.
    QueueFull,
    /// The user declined.
    Declined,
    /// The affirmative arrived within the settle interval after the dialog opened.
    TooSoon,
    /// The presenter could not present, returned nothing, or returned something unexpected.
    PresenterFailed(String),
    /// The request was cancelled or its run ended before an answer was honoured.
    Withdrawn,
    /// The token was presented to a door for a class it does not open.
    WrongDoor,
    /// A precondition the token was minted under no longer holds.
    PreconditionChanged(String),
    /// Building the request failed — the path could not be resolved, the target could
    /// not be read.
    Unresolvable(String),
    /// A write whose target, resolved, lies outside the workspace root.
    OutsideWorkspace(PathBuf),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::RefusedTier => write!(f, "refused by policy"),
            Refusal::SessionGrant => write!(f, "refused: request is a session-wide grant"),
            Refusal::UnknownRun => write!(f, "refused: run is not live"),
            Refusal::OverCapacity { bytes, capacity } => write!(
                f,
                "refused: {bytes} bytes cannot be shown in full (capacity {capacity})"
            ),
            Refusal::DoesNotFit => write!(f, "refused: the request does not fit on screen"),
            Refusal::QueueFull => write!(f, "refused: too many requests pending"),
            Refusal::Declined => write!(f, "declined"),
            Refusal::TooSoon => write!(f, "declined: answered before the dialog settled"),
            Refusal::PresenterFailed(e) => write!(f, "refused: presenter failed: {e}"),
            Refusal::Withdrawn => write!(f, "refused: request withdrawn"),
            Refusal::WrongDoor => write!(f, "refused: token presented to the wrong door"),
            Refusal::PreconditionChanged(what) => {
                write!(f, "refused: precondition changed: {what}")
            }
            Refusal::Unresolvable(what) => write!(f, "refused: {what}"),
            Refusal::OutsideWorkspace(path) => {
                write!(f, "refused: {} is outside the workspace", path.display())
            }
        }
    }
}
