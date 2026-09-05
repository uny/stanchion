// Keeps the console window from appearing on Windows release builds. macOS is the only
// supported target today; this costs nothing and avoids a surprise if that changes.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    stanchion_lib::run()
}
