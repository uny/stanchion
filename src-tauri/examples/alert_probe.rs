//! Drives `NativeAlert` under the real Tauri event loop, with no backend: the half of the
//! presenter that needs a running application. Run by hand; some scenarios wait for a click.
//!
//!     cargo run -p stanchion --example alert_probe -- <scenario>
//!
//! Scenarios: `withdraw`, `early`, `queued`, `fit`, `human`.

use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use stanchion_core::consent::policy::AlwaysAsk;
use stanchion_core::consent::request::{Backend, ClassSpec, InvocationId, RequestSpec, RunId};
use stanchion_core::consent::{Config, Consent};
use stanchion_lib::presenter::NativeAlert;

fn spec(run: RunId, command: &str) -> RequestSpec {
    let ws = std::env::temp_dir();
    RequestSpec {
        run: Some(run),
        workspace_root: ws.clone(),
        class: ClassSpec::ShellCommand {
            command: command.into(),
            cwd: ws,
            env: vec![],
        },
    }
}

/// Asks on its own thread; reports the invocation as soon as it is known, then the result.
fn ask(
    gate: &Arc<Consent>,
    run: RunId,
    command: String,
    t0: Instant,
    name: &'static str,
) -> (mpsc::Receiver<InvocationId>, std::thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let gate = Arc::clone(gate);
    let join = std::thread::spawn(move || {
        let result = gate.ask_observed(spec(run, &command), &mut |r| {
            let _ = tx.send(r.invocation);
        });
        eprintln!(
            "[{:>5}ms] {name}: {:?}",
            t0.elapsed().as_millis(),
            result.map(|_| "token minted")
        );
    });
    (rx, join)
}

/// Presses the modal alert's *Allow* from the main thread, as a click would.
fn click_allow() {
    use objc2::{MainThreadMarker, Message as _};
    use objc2_app_kit::{NSApplication, NSButton, NSView};
    fn find(view: &NSView) -> Option<objc2::rc::Retained<NSButton>> {
        if let Some(button) = view.downcast_ref::<NSButton>() {
            if button.title().to_string() == "Allow" {
                return Some(button.retain());
            }
        }
        view.subviews().iter().find_map(|v| find(&v))
    }
    dispatch_main(|| {
        let mtm = MainThreadMarker::new().unwrap();
        let window = NSApplication::sharedApplication(mtm).modalWindow();
        match window.and_then(|w| w.contentView()).and_then(|v| find(&v)) {
            Some(button) => unsafe { button.performClick(None) },
            None => eprintln!("no modal Allow button"),
        }
    });
}

/// Runs `work` on the main thread inside whatever modal is running.
fn dispatch_main(work: impl Fn() + 'static) {
    use objc2_core_foundation::{kCFRunLoopCommonModes, CFRunLoop};
    let main = CFRunLoop::main().unwrap();
    let block = block2::RcBlock::new(work);
    unsafe { main.perform_block(kCFRunLoopCommonModes.map(|m| &**m), Some(&block)) };
    main.wake_up();
}

fn main() {
    let scenario = std::env::args().nth(1).unwrap_or_default();
    tauri::Builder::default()
        .setup(move |app| {
            let t0 = Instant::now();
            let config = Config::default();
            let gate = Arc::new(Consent::new(
                Arc::new(NativeAlert::new(config.settle)),
                Arc::new(AlwaysAsk),
                config,
            ));
            let run = gate.register_run(Backend::Native);
            // A tick the Tauri event loop delivers: does it arrive while an alert is up?
            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_millis(500));
                let _ = handle.run_on_main_thread(move || {
                    eprintln!("[{:>5}ms] tick", t0.elapsed().as_millis())
                });
            });
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let at = |ms: u64| std::thread::sleep(Duration::from_millis(ms));
                match scenario.as_str() {
                    "withdraw" => {
                        let (inv, j) = ask(&gate, run, "echo $(date +%s)".into(), t0, "A");
                        let inv = inv.recv().unwrap();
                        at(2000);
                        eprintln!("[{:>5}ms] cancel A", t0.elapsed().as_millis());
                        gate.cancel(inv);
                        j.join().unwrap();
                    }
                    "early" => {
                        let (inv, j) = ask(&gate, run, "echo early".into(), t0, "A");
                        gate.cancel(inv.recv().unwrap());
                        j.join().unwrap();
                        at(1000);
                    }
                    "queued" => {
                        let (a, ja) = ask(&gate, run, "echo A".into(), t0, "A");
                        let a = a.recv().unwrap();
                        at(300);
                        let (b, jb) = ask(&gate, run, "echo B".into(), t0, "B");
                        let b = b.recv().unwrap();
                        at(1700);
                        eprintln!("[{:>5}ms] cancel A", t0.elapsed().as_millis());
                        gate.cancel(a);
                        ja.join().unwrap();
                        at(2000);
                        eprintln!("[{:>5}ms] cancel B", t0.elapsed().as_millis());
                        gate.cancel(b);
                        jb.join().unwrap();
                    }
                    "fit" => {
                        for lines in [5usize, 60, 110, 400] {
                            let body: Vec<String> =
                                (0..lines).map(|i| format!("echo line {i}")).collect();
                            eprintln!("[{:>5}ms] {lines} lines", t0.elapsed().as_millis());
                            let (inv, j) = ask(&gate, run, body.join("\n"), t0, "fit");
                            // Refused before it was presented: no invocation to cancel.
                            if let Ok(inv) = inv.recv() {
                                at(1500);
                                gate.cancel(inv);
                            }
                            j.join().unwrap();
                        }
                    }
                    "early-click" => {
                        // Presses Allow 100 ms after the alert opens, then again at 1 s:
                        // the first is swallowed and the alert stays; the second answers.
                        let (_, j) = ask(&gate, run, "echo early-click".into(), t0, "A");
                        at(100);
                        eprintln!("[{:>5}ms] click Allow", t0.elapsed().as_millis());
                        click_allow();
                        at(900);
                        eprintln!("[{:>5}ms] click Allow", t0.elapsed().as_millis());
                        click_allow();
                        j.join().unwrap();
                    }
                    "human" => {
                        let steps = [
                            "1/5 click Deny",
                            "2/5 wait a second, then click Allow",
                            "3/5 press Return",
                            "4/5 click Allow the instant the alert appears",
                            "5/5 switch to another application now; when the Dock icon \
                             bounces, come back and click Allow",
                        ];
                        for step in steps {
                            eprintln!("[{:>5}ms] {step}", t0.elapsed().as_millis());
                            if step.starts_with("5/5") {
                                at(4000);
                            }
                            let (_, j) = ask(&gate, run, format!("echo '{step}'"), t0, "result");
                            j.join().unwrap();
                        }
                    }
                    other => eprintln!("unknown scenario {other:?}"),
                }
                handle.exit(0);
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the probe");
}
