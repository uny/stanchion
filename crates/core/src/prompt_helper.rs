//! The helper that carries Claude Code's approval requests to the core (#46).
//!
//! Claude Code delivers the calls it delegates by calling a tool on an MCP server named in
//! `--permission-prompt-tool`. This is that server: a stdio process the CLI spawns like
//! any other, with one tool, `approve`, that relays its arguments over a Unix domain
//! socket to the attachment that spawned the CLI and returns whatever the core answers.
//! It holds no policy and decides nothing — the request is the core's to render and the
//! user's to answer — so the thing the CLI talks to and the thing the dialog answers are
//! one gate (`docs/decisions.md`, "The run backend contract, and the helper that carries
//! a CLI's approval requests").
//!
//! One argument: the socket path. The core passes it on the command line of the MCP
//! configuration it builds for the CLI; nothing here reads a settings file. The body is
//! here in the library so that two entry points can share it: `src/bin` builds it as its
//! own binary, which the tests drive, and the application runs it as a mode of its own
//! executable — the one file a bundle is sure to carry — so that nothing is copied next
//! to the application at build time and no build step can leave a stale helper behind
//! (`docs/decisions.md`, "The run backend contract").
//!
//! The wire on the socket is one line each way: the tool's arguments as the CLI sent them,
//! then the core's reply — `{"behavior":"allow"}` or `{"behavior":"deny","message":…}` —
//! which is returned to the CLI verbatim as the tool's text result. If the socket cannot
//! be reached, or the reply never comes, the tool returns a deny of its own. The CLI
//! denies on a malformed result or an unreachable server anyway (measured on 2.1.266 in
//! `crates/core/src/backend/claude_code`); the deny here only says why.
//!
//! JSON is read and written through the core's own parser, since the helper is part of
//! the core crate and the crate links no serialisation library (`crates/core/src/lib.rs`).

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::backend::claude_code::json::Value;

/// The MCP protocol revision answered when the client names none.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// How long one request may wait for the core's reply. The reply arrives when the user
/// answers the dialog, or when the request is withdrawn — the attachment's end withdraws
/// every pending request, so a reply always comes while the core is up. This bounds the
/// case where it is not.
const REPLY_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// The most of one stdin line the helper reads; past this the line is dropped and the
/// request answered with an error rather than buffered without bound.
const MAX_LINE: usize = 16 * 1024 * 1024;

/// Serves the CLI on stdin/stdout until stdin closes, relaying every `approve` call to
/// the core at `socket`. Returns when the CLI is done with the server; exits the process
/// on its own only when stdout is gone, since nothing can be reported then.
pub fn serve(socket: std::path::PathBuf) {
    let socket = Arc::new(socket);
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    // One writer for every thread: a call is answered on a thread of its own, so that a
    // dialog the user is looking at does not stall the CLI's next message — a `ping`,
    // another call — behind it. Lines are whole, so interleaving is at line level.
    let out = Arc::new(Mutex::new(io::stdout()));
    // Calls still being answered when stdin closes are answered before exit: the CLI
    // closes stdin when it is done with the server, and a client that pipes its
    // messages in one go (the test fixture) closes it before the reply is out.
    let mut calls: Vec<thread::JoinHandle<()>> = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = (&mut reader)
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', &mut line);
        match read {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Ok(0) | Err(_) => {
                for call in calls {
                    let _ = call.join();
                }
                return;
            }
            Ok(_) => {}
        }
        if line.len() > MAX_LINE {
            // Past the cap. If the cap cut the line short its tail is still unread and
            // is discarded up to the newline, a buffer at a time, so the next line is
            // read from its start; nothing of it is kept.
            if line.last() != Some(&b'\n') && !discard_line(&mut reader) {
                return;
            }
            respond(&out, error(Value::Null, -32600, "line too long"));
            continue;
        }
        let text = String::from_utf8_lossy(&line);
        let message = match Value::parse(text.trim_end()) {
            Ok(v) => v,
            Err(_) => {
                respond(&out, error(Value::Null, -32700, "parse error"));
                continue;
            }
        };
        let Some(id) = message.get("id").cloned() else {
            // A notification. `notifications/initialized` is the one the CLI sends; none
            // needs an answer.
            continue;
        };
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let params = message.get("params");
        match method {
            "initialize" => respond(&out, result(id, initialize_result(params))),
            "ping" => respond(&out, result(id, Value::Object(Vec::new()))),
            "tools/list" => respond(&out, result(id, tools_list())),
            "tools/call" => match params.and_then(|p| p.get("name")).and_then(Value::as_str) {
                Some("approve") => {
                    let arguments = params
                        .and_then(|p| p.get("arguments"))
                        .cloned()
                        .unwrap_or(Value::Object(Vec::new()));
                    let (socket, out) = (socket.clone(), out.clone());
                    calls.retain(|c| !c.is_finished());
                    calls.push(thread::spawn(move || {
                        let reply = relay(&socket, &arguments);
                        respond(&out, result(id, text_result(&reply)));
                    }));
                }
                _ => respond(&out, error(id, -32602, "unknown tool")),
            },
            _ => respond(&out, error(id, -32601, "method not found")),
        }
    }
}

/// Reads and drops the rest of the current line. `false` at end of input.
fn discard_line(reader: &mut impl BufRead) -> bool {
    loop {
        let available = match reader.fill_buf() {
            Ok(buf) => buf,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        };
        if available.is_empty() {
            return false;
        }
        let (used, done) = match available.iter().position(|&b| b == b'\n') {
            Some(at) => (at + 1, true),
            None => (available.len(), false),
        };
        reader.consume(used);
        if done {
            return true;
        }
    }
}

/// Sends the arguments to the core and returns its reply line, or a deny that says why
/// there is none.
fn relay(socket: &std::path::Path, arguments: &Value) -> String {
    match relay_inner(socket, arguments) {
        Ok(reply) => reply,
        Err(why) => deny(&format!("stanchion: {why}")),
    }
}

fn relay_inner(socket: &std::path::Path, arguments: &Value) -> Result<String, String> {
    let mut stream = UnixStream::connect(socket).map_err(|e| format!("core unreachable: {e}"))?;
    stream
        .set_read_timeout(Some(REPLY_TIMEOUT))
        .map_err(|e| format!("socket: {e}"))?;
    let mut request = arguments.to_json();
    request.push('\n');
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("core unreachable: {e}"))?;
    let mut reply = String::new();
    BufReader::new(stream)
        .read_line(&mut reply)
        .map_err(|e| format!("no reply: {e}"))?;
    let reply = reply.trim_end().to_string();
    if reply.is_empty() {
        return Err("no reply: the core closed the socket".into());
    }
    Ok(reply)
}

fn deny(message: &str) -> String {
    Value::Object(vec![
        ("behavior".into(), Value::String("deny".into())),
        ("message".into(), Value::String(message.into())),
    ])
    .to_json()
}

fn initialize_result(params: Option<&Value>) -> Value {
    let version = params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str)
        .unwrap_or(PROTOCOL_VERSION);
    Value::Object(vec![
        ("protocolVersion".into(), Value::String(version.into())),
        (
            "capabilities".into(),
            Value::Object(vec![("tools".into(), Value::Object(Vec::new()))]),
        ),
        (
            "serverInfo".into(),
            Value::Object(vec![
                ("name".into(), Value::String("stanchion".into())),
                (
                    "version".into(),
                    Value::String(env!("CARGO_PKG_VERSION").into()),
                ),
            ]),
        ),
    ])
}

fn tools_list() -> Value {
    let string = |what: &str| {
        Value::Object(vec![
            ("type".into(), Value::String("string".into())),
            ("description".into(), Value::String(what.into())),
        ])
    };
    Value::Object(vec![(
        "tools".into(),
        Value::Array(vec![Value::Object(vec![
            ("name".into(), Value::String("approve".into())),
            (
                "description".into(),
                Value::String("Asks the stanchion consent gate about one tool call.".into()),
            ),
            (
                "inputSchema".into(),
                Value::Object(vec![
                    ("type".into(), Value::String("object".into())),
                    (
                        "properties".into(),
                        Value::Object(vec![
                            ("tool_name".into(), string("The tool the call is for.")),
                            (
                                "input".into(),
                                Value::Object(vec![(
                                    "type".into(),
                                    Value::String("object".into()),
                                )]),
                            ),
                            ("tool_use_id".into(), string("The call's id.")),
                        ]),
                    ),
                    (
                        "required".into(),
                        Value::Array(vec![
                            Value::String("tool_name".into()),
                            Value::String("input".into()),
                            Value::String("tool_use_id".into()),
                        ]),
                    ),
                ]),
            ),
        ])]),
    )])
}

fn text_result(text: &str) -> Value {
    Value::Object(vec![(
        "content".into(),
        Value::Array(vec![Value::Object(vec![
            ("type".into(), Value::String("text".into())),
            ("text".into(), Value::String(text.into())),
        ])]),
    )])
}

fn result(id: Value, result: Value) -> Value {
    Value::Object(vec![
        ("jsonrpc".into(), Value::String("2.0".into())),
        ("id".into(), id),
        ("result".into(), result),
    ])
}

fn error(id: Value, code: i64, message: &str) -> Value {
    Value::Object(vec![
        ("jsonrpc".into(), Value::String("2.0".into())),
        ("id".into(), id),
        (
            "error".into(),
            Value::Object(vec![
                ("code".into(), Value::Number(code as f64)),
                ("message".into(), Value::String(message.into())),
            ]),
        ),
    ])
}

fn respond(out: &Mutex<io::Stdout>, response: Value) {
    let mut line = response.to_json();
    line.push('\n');
    let mut out = out.lock().unwrap_or_else(|e| e.into_inner());
    if out
        .write_all(line.as_bytes())
        .and_then(|()| out.flush())
        .is_err()
    {
        // The CLI is gone; so is any reason to be here.
        std::process::exit(0);
    }
}
