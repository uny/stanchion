// Keeps the console window from appearing on Windows release builds. macOS is the only
// supported target today; this costs nothing and avoids a surprise if that changes.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // The prompt helper runs as a mode of this executable — the one file a bundle is sure
    // to carry — so it is selected here, before anything of the application is touched.
    // A CLI spawned by the core names this executable with `--prompt-helper` and the
    // socket path, and gets a stdio MCP server and nothing else: no window, no Tauri.
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() == Some(stanchion_lib::assembly::HELPER_MODE.as_ref()) {
        let (Some(socket), None) = (args.next(), args.next()) else {
            eprintln!(
                "usage: stanchion {} <socket>",
                stanchion_lib::assembly::HELPER_MODE
            );
            std::process::exit(2);
        };
        stanchion_core::prompt_helper::serve(socket.into());
        return;
    }
    stanchion_lib::run()
}
