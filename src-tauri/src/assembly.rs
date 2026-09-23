//! Where the Claude Code backend's four arguments come from: the `claude` binary, the
//! config root, the helper command and the socket directory. Each is resolved here, at
//! startup, from the application's own environment — none from a settings file, which the
//! run backend decision rules out for the helper and this module extends to the binary
//! (`docs/decisions.md`, "The run backend contract"). What a settings file could add is
//! left open there.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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

/// Finds the `claude` to run. In order: the `PATH` this process has, the candidates in
/// `known` (the known install directories, [`known_dirs`]), and the user's login shell's `PATH` — the last spawns `$SHELL -lc`, and
/// is asked once at startup. A login shell reads `.zprofile` and `.zshenv`, not `.zshrc`,
/// so a `PATH` addition made there is not seen; the known directories are what find the
/// usual installs. The result is a path to a file this process may execute — the
/// shell's answer is held to that too, since a profile that prints a banner or an alias
/// puts something other than a path on its stdout — so the CLI the user was shown at
/// startup is the CLI every session runs. What the CLI inherits is this process's
/// environment, which from the Finder is launchd's — whether a `claude` that needs more
/// than that runs is the shell slice's measurement, not this function's.
pub fn resolve_claude(
    path: Option<&std::ffi::OsStr>,
    known: &[PathBuf],
    shell: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(found) = path.and_then(|p| find_in_path(p, CLAUDE)) {
        return Some(found);
    }
    if let Some(found) = known.iter().find_map(|d| executable(d)) {
        return Some(found);
    }
    let output = ask_login_shell(shell?)?;
    // The last line: a profile may print before `command -v` does. And only a path that
    // is a file this process can execute; anything else is not the binary.
    output
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .filter(|l| Path::new(l).is_absolute())
        .and_then(|l| executable(Path::new(l)))
}

/// How long the login shell has to answer. A profile that waits on something never
/// gets to hold the window closed; the answer is then "not found".
const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(5);

/// Runs `command -v claude` in a login shell and returns its stdout, or `None` when the
/// shell fails, cannot be spawned, or does not answer in time — in which case it is
/// killed, so a child of the profile that kept the pipe open does not keep us here.
fn ask_login_shell(shell: &Path) -> Option<String> {
    let mut child = Command::new(shell)
        .args(["-lc", &format!("command -v {CLAUDE}")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // Read on a thread of its own: the pipe is what a profile's background child would
    // hold open, and the wait below is on the shell, not the pipe.
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        let _ = stdout.read_to_string(&mut out);
        out
    });
    let deadline = Instant::now() + LOGIN_SHELL_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }
    // The shell has exited; if something it left behind still holds the pipe, the
    // reader is left to it and the answer is "not found" rather than a wait.
    while !reader.is_finished() {
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    reader.join().ok()
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

/// The bytes of `sun_path` on this platform. A socket path takes one of them for its
/// terminating NUL, so one of this length or longer is refused at bind.
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
        &known_dirs(std::env::var_os("HOME").map(PathBuf::from).as_deref()),
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
    if longest_socket_path(&sockets) >= SOCKET_PATH_MAX {
        return Err(format!(
            "socket directory {} leaves a socket path of {SOCKET_PATH_MAX} bytes or more",
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

    /// The known candidates under `home` only: the absolute ones are this machine's own
    /// install directories, and a `claude` installed there would answer every test.
    fn under(home: &Path) -> Vec<PathBuf> {
        known_dirs(Some(home))
            .into_iter()
            .filter(|p| p.starts_with(home))
            .collect()
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
            &under(&home),
            Some(Path::new("/nonexistent/shell")),
        );
        assert_eq!(found, Some(wanted));
    }

    #[test]
    fn a_known_directory_under_home_is_found_without_path() {
        let home = dir("home2");
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        let wanted = place(&home.join(".local/bin"), 0o755);
        let found = resolve_claude(None, &under(&home), Some(Path::new("/nonexistent/shell")));
        assert_eq!(found, Some(wanted));
    }

    #[test]
    fn a_file_without_the_execute_bit_is_not_the_binary() {
        let on_path = dir("noexec");
        place(&on_path, 0o644);
        let found = resolve_claude(Some(on_path.as_os_str()), &[], None);
        assert_eq!(found, None);
    }

    fn fake_shell(dir: &Path, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let shell = dir.join("sh");
        std::fs::write(&shell, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        shell
    }

    #[test]
    fn the_login_shell_is_asked_last_and_its_last_line_is_the_path() {
        let d = dir("shell");
        let wanted = place(&d, 0o755);
        // A profile that prints a banner before the answer.
        let shell = fake_shell(
            &d,
            &format!("echo 'welcome back'\necho {}", wanted.display()),
        );
        let found = resolve_claude(None, &[], Some(&shell));
        assert_eq!(found, Some(wanted));
    }

    #[test]
    fn the_login_shell_answer_must_be_an_executable_file() {
        let d = dir("shell-alias");
        // What `command -v` prints for an alias, and a path that does not exist.
        for answer in ["alias claude='claude --foo'", "/nonexistent/claude"] {
            let shell = fake_shell(&d, &format!("echo \"{answer}\""));
            assert_eq!(resolve_claude(None, &[], Some(&shell)), None, "{answer}");
        }
    }

    #[test]
    fn a_login_shell_that_does_not_answer_in_time_is_not_found() {
        let d = dir("shell-hang");
        let shell = fake_shell(&d, "sleep 30");
        let started = Instant::now();
        assert_eq!(resolve_claude(None, &[], Some(&shell)), None);
        assert!(started.elapsed() < LOGIN_SHELL_TIMEOUT + Duration::from_secs(2));
    }

    #[test]
    fn nothing_found_is_none_not_a_bare_name() {
        // A bare `claude` would let the CLI's `PATH` at spawn time decide, which is not
        // the `PATH` the user was shown at startup.
        assert_eq!(
            resolve_claude(None, &[], Some(Path::new("/nonexistent/shell"))),
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
        assert!(longest < SOCKET_PATH_MAX, "{longest} bytes");
    }
}
