//! Where the Claude Code backend's four arguments come from: the `claude` binary, the
//! config root, the helper command and the socket directory. Each is resolved here, at
//! startup, from the application's own environment — none from a settings file, which the
//! run backend decision rules out for the helper and this module extends to the binary
//! (`docs/decisions.md`, "The run backend contract"). What a settings file could add is
//! left open there.

use std::path::{Path, PathBuf};
use std::process::Command;

use stanchion_core::backend::claude_code::{ClaudeCode, ConfigRoot, Helper, SocketDir};

/// The argument that selects the prompt helper mode of this executable (`main.rs`).
pub const HELPER_MODE: &str = "--prompt-helper";

/// The `claude` binary's file name.
pub const CLAUDE: &str = "claude";

/// Where the CLI is installed when it is not on the `PATH` an application inherits. A
/// `.app` launched from the Finder gets launchd's `PATH` (`/usr/bin:/bin:/usr/sbin:/sbin`),
/// which is none of these; `tauri dev` from a terminal inherits the user's and hides that.
/// Relative entries are under the home directory.
pub const KNOWN_DIRS: [&str; 3] = [".local/bin", "/opt/homebrew/bin", "/usr/local/bin"];

/// Finds the `claude` to run. In order: the `PATH` this process has, the known install
/// directories, and the user's login shell's `PATH` — the last spawns `$SHELL -lc`, and
/// is asked once at startup. A login shell reads `.zprofile` and `.zshenv`, not `.zshrc`,
/// so a `PATH` addition made there is not seen; the known directories are what find the
/// usual installs. The result is a path, so the CLI the user was shown at startup is the
/// CLI every session runs. What the CLI inherits is this process's environment, which
/// from the Finder is launchd's — whether a `claude` that needs more than that runs is
/// the shell slice's measurement, not this function's.
pub fn resolve_claude(
    path: Option<&std::ffi::OsStr>,
    home: Option<&Path>,
    shell: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(found) = path.and_then(|p| find_in_path(p, CLAUDE)) {
        return Some(found);
    }
    if let Some(found) = known_dirs(home).into_iter().find_map(|d| executable(&d)) {
        return Some(found);
    }
    let shell = shell?;
    let output = Command::new(shell)
        .args(["-lc", &format!("command -v {CLAUDE}")])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout);
    let line = line.trim();
    (!line.is_empty()).then(|| PathBuf::from(line))
}

/// Every candidate the known directories name, in order.
pub fn known_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    KNOWN_DIRS
        .iter()
        .filter_map(|d| {
            let d = Path::new(d);
            if d.is_absolute() {
                Some(d.join(CLAUDE))
            } else {
                home.map(|h| h.join(d).join(CLAUDE))
            }
        })
        .collect()
}

fn find_in_path(path: &std::ffi::OsStr, name: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|d| !d.as_os_str().is_empty())
        .find_map(|d| executable(&d.join(name)))
}

fn executable(candidate: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(candidate).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if meta.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }
    meta.is_file().then(|| candidate.to_path_buf())
}

/// The helper command: this executable in helper mode. `current_exe` is the bundle's own
/// binary, or the target directory's under `tauri dev`; either way the helper the CLI
/// spawns is the build the application is.
pub fn helper() -> std::io::Result<Helper> {
    Ok(Helper::new(std::env::current_exe()?).arg(HELPER_MODE))
}

/// The directory the per-attachment sockets are bound in: under the per-user temporary
/// directory, which macOS creates 0700 and sets for applications launched from the Finder
/// as well as from a terminal. Chosen for length: a socket path is limited to 104 bytes
/// on macOS, and `$TMPDIR` is about 50, where the application data directory can run past
/// the limit on a long user name. `/tmp` would be shorter still, but its parent is
/// world-writable and a directory there can be created first by someone else.
pub fn socket_dir() -> std::io::Result<SocketDir> {
    SocketDir::new(std::env::temp_dir().join("stanchion"))
}

/// The bytes a socket path may have on this platform; a longer one is refused at bind.
#[cfg(target_os = "macos")]
pub const SOCKET_PATH_MAX: usize = 104;
#[cfg(not(target_os = "macos"))]
pub const SOCKET_PATH_MAX: usize = 108;

/// The longest file name the core gives a socket (`<pid>-<attachment>.sock`, both at
/// their widest), with its separator.
const LONGEST_SOCKET_NAME: &str = "/4294967295-18446744073709551615.sock";

/// The longest socket path `dir` can hold.
fn longest_socket_path(dir: &SocketDir) -> usize {
    dir.path().as_os_str().len() + LONGEST_SOCKET_NAME.len()
}

/// The config root: under the application's own data directory, which the shell resolves
/// through Tauri (the core takes the path and creates it 0700).
pub fn config_root(app_data_dir: &Path) -> std::io::Result<ConfigRoot> {
    ConfigRoot::new(app_data_dir.join("claude-config"))
}

/// The backend, or the first reason it cannot be built. Nothing here is read from a
/// settings file.
pub fn backend(app_data_dir: &Path) -> Result<ClaudeCode, String> {
    let binary = resolve_claude(
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("HOME").map(PathBuf::from).as_deref(),
        std::env::var_os("SHELL").map(PathBuf::from).as_deref(),
    )
    .ok_or_else(|| {
        format!(
            "no `{CLAUDE}` on PATH, in {}, or on the login shell's PATH",
            KNOWN_DIRS.join(", ")
        )
    })?;
    let root = config_root(app_data_dir).map_err(|e| format!("config root: {e}"))?;
    let helper = helper().map_err(|e| format!("helper: {e}"))?;
    let sockets = socket_dir().map_err(|e| format!("socket directory: {e}"))?;
    if longest_socket_path(&sockets) > SOCKET_PATH_MAX {
        return Err(format!(
            "socket directory {} leaves a socket path over {SOCKET_PATH_MAX} bytes",
            sockets.path().display()
        ));
    }
    Ok(ClaudeCode::new(binary, root, helper, sockets))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory removed when dropped.
    struct Dir(PathBuf);

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    impl std::ops::Deref for Dir {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    fn dir(name: &str) -> Dir {
        let d =
            std::env::temp_dir().join(format!("stanchion-assembly-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }

    fn place(dir: &Path, mode: u32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let p = dir.join(CLAUDE);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        p
    }

    #[test]
    fn path_wins_over_the_known_directories_and_the_shell() {
        let on_path = dir("path");
        let home = dir("home");
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        let wanted = place(&on_path, 0o755);
        place(&home.join(".local/bin"), 0o755);
        let found = resolve_claude(
            Some(on_path.as_os_str()),
            Some(&home),
            Some(Path::new("/nonexistent/shell")),
        );
        assert_eq!(found, Some(wanted));
    }

    #[test]
    fn a_known_directory_under_home_is_found_without_path() {
        let home = dir("home2");
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        let wanted = place(&home.join(".local/bin"), 0o755);
        let found = resolve_claude(None, Some(&home), Some(Path::new("/nonexistent/shell")));
        assert_eq!(found, Some(wanted));
    }

    #[test]
    fn a_file_without_the_execute_bit_is_not_the_binary() {
        let on_path = dir("noexec");
        place(&on_path, 0o644);
        let found = resolve_claude(Some(on_path.as_os_str()), None, None);
        assert_eq!(found, None);
    }

    #[test]
    fn the_login_shell_is_asked_last_and_its_answer_is_a_path() {
        let home = dir("shell");
        let shell = home.join("sh");
        std::fs::write(&shell, "#!/bin/sh\necho /somewhere/claude\n").unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        let found = resolve_claude(None, None, Some(&shell));
        assert_eq!(found, Some(PathBuf::from("/somewhere/claude")));
    }

    #[test]
    fn nothing_found_is_none_not_a_bare_name() {
        // A bare `claude` would let the CLI's `PATH` at spawn time decide, which is not
        // the `PATH` the user was shown at startup.
        assert_eq!(
            resolve_claude(None, None, Some(Path::new("/nonexistent/shell"))),
            None
        );
    }

    #[test]
    fn the_helper_is_this_executable_in_helper_mode() {
        let helper = helper().unwrap();
        assert_eq!(helper.program(), std::env::current_exe().unwrap());
        assert_eq!(helper.args(), [HELPER_MODE]);
    }

    #[test]
    fn the_socket_directory_leaves_room_for_a_socket_path() {
        let dir = socket_dir().unwrap();
        let longest = longest_socket_path(&dir);
        assert!(longest <= SOCKET_PATH_MAX, "{longest} bytes");
    }
}
