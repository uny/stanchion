//! The Rust core.
//!
//! Everything the WebView can reach is a named command registered here. The WebView is
//! treated as untrusted: it renders model output, so it gets no filesystem, network or
//! shell capability of its own. See `docs/architecture.md`.

/// Reports the core's version. The skeleton's only command; it exists to prove the IPC
/// boundary is wired end to end.
#[tauri::command]
fn core_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![core_version])
        .run(tauri::generate_context!())
        .expect("error while running stanchion");
}
