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

pub mod assembly;
pub mod conversations;
pub mod events;
pub mod presenter;

use std::sync::Arc;

use stanchion_core::consent::policy::AlwaysAsk;
use stanchion_core::consent::{Config, Consent};
use tauri::Manager as _;

/// Reports the core's version. The skeleton's first command; it exists to prove the IPC
/// boundary is wired end to end.
#[tauri::command]
fn core_version() -> &'static str {
    stanchion_core::version()
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // The gate and the backend are built once, here, from the application's own
            // environment (`assembly`). A backend that cannot be built is kept as its
            // reason, reported by `backend_status`, so the window still opens and says
            // why rather than failing to start.
            let config = Config::default();
            let alert = Arc::new(presenter::NativeAlert::new(config.settle));
            let gate = Arc::new(Consent::new(alert.clone(), Arc::new(AlwaysAsk), config));
            let backend = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("application data directory: {e}"))
                .and_then(|dir| assembly::backend(&dir));
            let conversations = Arc::new(conversations::Conversations::new(backend, gate));
            app.manage(conversations.clone());
            // Cmd-. and Cmd-Q while an alert is up. The hook runs inside the modal on the
            // main thread, so the work goes to a thread of its own: terminating walks the
            // CLI's process tree. Either way the run ends, the gate withdraws the request,
            // and the withdrawal aborts the modal; the quit is asked for once every
            // conversation has been told to end, and `RunEvent::Exit` below repeats that
            // for any that registered meanwhile.
            let handle = app.handle().clone();
            alert.on_abandon(Arc::new(move |what, invocation| {
                let conversations = conversations.clone();
                let handle = handle.clone();
                std::thread::spawn(move || match what {
                    presenter::Abandon::Terminate => {
                        conversations.terminate_waiting_on(invocation.raw())
                    }
                    presenter::Abandon::Quit => {
                        conversations.shut_down();
                        handle.exit(0);
                    }
                });
            }));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            core_version,
            conversations::backend_status,
            conversations::start_conversation,
            conversations::send_input,
            conversations::interrupt_conversation,
            conversations::terminate_conversation,
            conversations::resume_conversation,
        ])
        .build(tauri::generate_context!())
        .expect("error while building stanchion")
        .run(|app, event| {
            // Before the process exits, not on drop: nothing drops the managed state on
            // the way out. The Quit menu item cannot be reached while an alert is up —
            // the alert is app-modal — so a quit then comes from Cmd-Q, through the hook
            // above.
            if let tauri::RunEvent::Exit = event {
                app.state::<Arc<conversations::Conversations>>().shut_down();
            }
        });
}
