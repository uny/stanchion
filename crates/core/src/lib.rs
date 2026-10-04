//! The core: everything that is not the user interface.
//!
//! The credential lifecycle, the OpenAI-compatible transport and the agent loop live here.
//! `docs/architecture.md` calls the core the trusted side of an IPC boundary, and this
//! crate is where that claim is made checkable: **it must not depend on `tauri`, on a
//! windowing library, or on anything else that implies a WebView.** A dependency edge in
//! that direction is a design error, not a convenience, because it would let interface
//! concerns reach into the side that holds credentials.
//!
//! Keeping the edge absent also means the core can be linked by something that is not the
//! desktop application — an integration test, or a headless proxy — without dragging a
//! window along.
//!
//! # What the type system pins
//!
//! The consent rule (`docs/decisions.md`, "Consent is a native dialog the core owns")
//! has a compile-time half, and these doctests are it. Each one is written to fail on one
//! diagnostic, the one its fence names, and calls nothing else that could fail; stable
//! rustdoc does not compare the code, so the fixture's shape, not the fence, is what keeps
//! one restriction from passing for another. The runtime half is `consent::tests`.
//!
//! A token cannot be constructed outside the gate — its constructor is private:
//!
//! ```compile_fail,E0624
//! use stanchion_core::consent::token::ConsentToken;
//! fn forge(request: std::sync::Arc<stanchion_core::consent::request::Request>) {
//!     let _ = ConsentToken::mint(todo!(), request, stanchion_core::consent::token::Origin::Consent);
//! }
//! ```
//!
//! Nor by literal — its fields are private:
//!
//! ```compile_fail,E0451
//! use stanchion_core::consent::token::{ConsentToken, Origin};
//! fn forge(request: std::sync::Arc<stanchion_core::consent::request::Request>) {
//!     let _ = ConsentToken { id: todo!(), request, origin: Origin::Consent };
//! }
//! ```
//!
//! The native executor demands a token:
//!
//! ```compile_fail,E0061
//! use stanchion_core::{consent::Consent, execute::{ExecutionSink, NativeExecutor}};
//! fn go(gate: &Consent, sink: &mut dyn ExecutionSink) {
//!     let _ = NativeExecutor.execute(gate, sink);
//! }
//! ```
//!
//! So does the settings writer:
//!
//! ```compile_fail,E0061
//! use stanchion_core::{consent::Consent, execute::{SettingsStore, SettingsWriter}};
//! fn go(gate: &Consent, store: &mut dyn SettingsStore) {
//!     let _ = SettingsWriter.apply(gate, store);
//! }
//! ```
//!
//! And the run starter:
//!
//! ```compile_fail,E0061
//! use stanchion_core::{consent::Consent, execute::{RunSink, RunStarter}};
//! fn go(gate: &Consent, sink: &mut dyn RunSink) {
//!     let _ = RunStarter.start(gate, sink);
//! }
//! ```
//!
//! And the *allow* reply to a CLI:
//!
//! ```compile_fail,E0061
//! use stanchion_core::{consent::Consent, execute::{CliApproval, ReplyTransport}};
//! fn go(gate: &Consent, transport: &mut dyn ReplyTransport) {
//!     let _ = CliApproval.allow(gate, transport);
//! }
//! ```
//!
//! And the bridge forward:
//!
//! ```compile_fail,E0061
//! use stanchion_core::{consent::Consent, execute::{Bridge, BridgeSink}};
//! fn go(gate: &Consent, sink: &mut dyn BridgeSink) {
//!     let _ = Bridge.forward(gate, sink);
//! }
//! ```
//!
//! A token is spent on first use — the doors take it by value:
//!
//! ```compile_fail,E0382
//! use stanchion_core::{consent::{Consent, token::ConsentToken}, execute::{ExecutionSink, NativeExecutor}};
//! fn twice(gate: &Consent, token: ConsentToken, sink: &mut dyn ExecutionSink) {
//!     let _ = NativeExecutor.execute(gate, token, sink);
//!     let _ = NativeExecutor.execute(gate, token, sink);
//! }
//! ```
//!
//! And it cannot be cloned to get around that:
//!
//! ```compile_fail,E0277
//! use stanchion_core::consent::token::ConsentToken;
//! fn dup<T: Clone>(_: &T) {}
//! fn go(token: &ConsentToken) { dup(token); }
//! ```
//!
//! It is not serialisable either, which no fixture can show directly: this crate depends on
//! no serialisation library, and `cargo tree` in CI is what keeps that so.
//!
//! The run backend contract (`backend`) keeps the same rule from the other side: the code
//! above a backend holds a session and has nothing on it by which to answer an approval —
//! the backend asks the gate itself, and an answer has no method to arrive through:
//!
//! ```compile_fail,E0599
//! use stanchion_core::{backend::Session, consent::{presenter::Answer, request::InvocationId}};
//! fn go(session: &dyn Session, invocation: InvocationId) {
//!     session.resolve(invocation, Answer::Allow);
//! }
//! ```
//!
//! And the backends are the ones this crate ships: the traits are sealed, so nothing
//! outside can implement one and hand the code above a backend of its own:
//!
//! ```compile_fail,E0277
//! use stanchion_core::backend::{Backend, BackendError, Capabilities, Resume, RunBackend, Session, Start};
//! struct Mine;
//! impl RunBackend for Mine {
//!     fn kind(&self) -> Backend { Backend::Native }
//!     fn capabilities(&self) -> Capabilities { todo!() }
//!     fn start(&self, _: Start) -> Result<Box<dyn Session>, BackendError> { todo!() }
//!     fn resume(&self, _: Resume) -> Result<Box<dyn Session>, BackendError> { todo!() }
//! }
//! ```
//!
//! Nor a session of its own, which is what would let it skip the lease:
//!
//! ```compile_fail,E0277
//! use stanchion_core::backend::{AttachmentId, BackendError, InboxMessage, Session, TurnId, Usage, UserInput};
//! struct Mine;
//! impl Session for Mine {
//!     fn attachment(&self) -> AttachmentId { todo!() }
//!     fn send(&self, _: UserInput) -> Result<TurnId, BackendError> { todo!() }
//!     fn deliver(&self, _: InboxMessage) -> Result<(), BackendError> { todo!() }
//!     fn interrupt(&self) -> Result<(), BackendError> { todo!() }
//!     fn terminate(&self) -> Result<(), BackendError> { todo!() }
//!     fn usage(&self) -> Usage { todo!() }
//! }
//! ```
//!
//! And a holder of the gate — the shell holds one, to build a backend's `Start` — cannot
//! use it to ask under a run of its own (#54). It cannot open a run:
//!
//! ```compile_fail,E0624
//! use stanchion_core::consent::{request::Backend, Consent};
//! fn go(gate: &Consent) {
//!     let _ = gate.register_run(Backend::ClaudeCode);
//! }
//! ```
//!
//! Nor ask, under any run or none:
//!
//! ```compile_fail,E0624
//! use stanchion_core::consent::{request::RequestSpec, Consent};
//! fn go(gate: &Consent, spec: RequestSpec) {
//!     let _ = gate.ask_observed(spec, &mut |_| {});
//! }
//! ```
//!
//! ```compile_fail,E0599
//! use stanchion_core::consent::{request::RequestSpec, Consent};
//! fn go(gate: &Consent, spec: RequestSpec) {
//!     let _ = gate.ask(spec);
//! }
//! ```
//!
//! Nor through the door a CLI backend asks through:
//!
//! ```compile_fail,E0624
//! use stanchion_core::{consent::{request::RequestSpec, Consent}, execute::{CliApproval, ReplyTransport}};
//! fn go(gate: &Consent, spec: RequestSpec, transport: &mut dyn ReplyTransport) {
//!     let _ = CliApproval.resolve_observed(gate, spec, transport, &mut |_| {});
//! }
//! ```
//!
//! ```compile_fail,E0599
//! use stanchion_core::{consent::{request::RequestSpec, Consent}, execute::{CliApproval, ReplyTransport}};
//! fn go(gate: &Consent, spec: RequestSpec, transport: &mut dyn ReplyTransport) {
//!     let _ = CliApproval.resolve(gate, spec, transport);
//! }
//! ```
//!
//! Nor end a run, or withdraw a request a backend made:
//!
//! ```compile_fail,E0624
//! use stanchion_core::consent::{request::RunId, Consent};
//! fn go(gate: &Consent, run: RunId) {
//!     gate.end_run(run);
//! }
//! ```
//!
//! ```compile_fail,E0624
//! use stanchion_core::consent::{request::InvocationId, Consent};
//! fn go(gate: &Consent, invocation: InvocationId) {
//!     gate.cancel(invocation);
//! }
//! ```
//!
//! The `probe` feature opens these doors to the hand-run presenter probe, through
//! `consent::probe`; the application never enables it.

pub mod backend;
pub mod consent;
pub mod execute;
pub mod prompt_helper;

/// The core's version, as compiled.
///
/// The Tauri shell reports this over IPC so the skeleton can prove the boundary is wired end
/// to end. It is deliberately the *core's* version rather than the shell's: the shell's
/// version would say nothing about which core it is talking to.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
