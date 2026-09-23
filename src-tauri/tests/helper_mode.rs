//! `--prompt-helper` is dispatched in the application's `main`, before anything of Tauri
//! is touched. Asked with no socket, the helper mode answers with its usage and exit
//! status 2; had the dispatch missed, the application would have started instead.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use stanchion_lib::assembly::HELPER_MODE;

#[test]
fn the_application_binary_has_a_helper_mode() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_stanchion"))
        .arg(HELPER_MODE)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("no exit within 10s: `{HELPER_MODE}` did not select the helper mode");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains(&format!("{HELPER_MODE} <socket>")),
        "{stderr}"
    );
}
