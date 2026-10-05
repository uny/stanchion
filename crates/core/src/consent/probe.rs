//! The gate's doors, opened for a hand-run probe of a presenter,
//! `src-tauri/examples/alert_probe.rs`, which drives the native alert with no backend and
//! so has none of its own to ask through. Compiled only with the `probe` feature, which the
//! application never enables: in the shipped build these doors are crate-private (#54).

use super::policy::Refusal;
use super::presenter::Rendered;
use super::request::{Backend, InvocationId, RequestSpec, RunId};
use super::token::ConsentToken;
use super::Consent;

/// The gate's `register_run`, for the probe.
pub fn register_run(gate: &Consent, backend: Backend) -> RunId {
    gate.register_run(backend)
}

/// The gate's `ask_observed`, for the probe.
pub fn ask_observed(
    gate: &Consent,
    spec: RequestSpec,
    observer: &mut dyn FnMut(&Rendered),
) -> Result<ConsentToken, Refusal> {
    gate.ask_observed(spec, observer)
}

/// The gate's `cancel`, for the probe.
pub fn cancel(gate: &Consent, invocation: InvocationId) {
    gate.cancel(invocation)
}
