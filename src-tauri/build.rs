fn main() {
    // Passing an application manifest is what switches the application command ACL from
    // fail-open to fail-closed. Without it Tauri skips the check for every
    // `#[tauri::command]` this crate registers; with it, a command reaches the WebView only
    // when `capabilities/default.json` grants the matching `allow-` permission. Every
    // command in `invoke_handler!` belongs in this list, and adding one here is a widening
    // that AGENTS.md section 5 requires the pull request to state.
    let attributes = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(&["core_version"]));
    tauri_build::try_build(attributes).expect("failed to run tauri-build");
}
