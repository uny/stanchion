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

/// The core's version, as compiled.
///
/// The Tauri shell reports this over IPC so the skeleton can prove the boundary is wired end
/// to end. It is deliberately the *core's* version rather than the shell's: the shell's
/// version would say nothing about which core it is talking to.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
