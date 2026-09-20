//! The Tauri shell, which is the IPC boundary and nothing else.
//!
//! Everything the WebView can reach is a named command registered here. The WebView is
//! treated as untrusted: it renders model output, so it gets no filesystem, network or
//! shell capability of its own. See `docs/architecture.md`.
//!
//! Commands belong here; the work they delegate to belongs in `stanchion_core`, which does
//! not depend on Tauri. A command that grows logic of its own has put interface code on the
//! trusted side of the boundary.
//!
//! One thing the WebView cannot reach at all is an approval: `presenter` is the shell's
//! half of the consent gate, and its answer never transits IPC.

pub mod presenter;

/// Reports the core's version. The skeleton's only command; it exists to prove the IPC
/// boundary is wired end to end.
#[tauri::command]
fn core_version() -> &'static str {
    stanchion_core::version()
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![core_version])
        .run(tauri::generate_context!())
        .expect("error while running stanchion");
}
