//! The conversations the shell holds, and the commands the WebView drives them with.
//!
//! One `Arc<dyn Session>` per open conversation, under one lock, and the gate they all
//! share. The commands are thin: each one names a conversation, hands the call to the
//! core on a blocking thread — a spawn and a write to a process's stdin both block, and a
//! synchronous command would block the main thread with them — and returns what the core
//! said. Nothing here takes or relays an approval decision; the presenter is the gate's
//! (`presenter`), and until the native alert is wired it is [`crate::presenter::FailClosed`],
//! whose capacity is zero: every request the CLI delegates is refused before it is
//! presented, with no `ApprovalRequested` or `ApprovalResolved`, and the WebView sees the
//! refusal as a `Diagnostic` and the CLI's own error `ToolResult`.
//!
//! Resume: a `SessionId` reaches the WebView in `SessionOpened`, and the WebView asks for
//! a resume by the conversation it was opened in; the shell resumes the session that
//! conversation's sink recorded, with the account and workspace root it was created
//! under, never ones the WebView names. Only an attachment that has reported `Exited`
//! is resumed, and one resume at a time per conversation, so the events of two
//! attachments never interleave on one channel. Sessions are held for the process's
//! life only; a store that survives it is later work (#46).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use stanchion_core::backend::claude_code::ClaudeCode;
use stanchion_core::backend::{
    AccountId, ConversationId, Resume, RunBackend, Session, Start, UserInput,
};
use stanchion_core::consent::Consent;
use tauri::ipc::Channel;

use crate::events::{ChannelSink, ConversationEvent};

/// Why a command could not do what was asked, as the WebView receives it: core-generated
/// text, never a process's own output unescaped.
pub type CommandError = String;

pub struct Conversations {
    backend: Result<ClaudeCode, String>,
    gate: Arc<Consent>,
    next: AtomicU64,
    open: Mutex<HashMap<u64, Open>>,
}

struct Open {
    session: Arc<dyn Session>,
    /// The sink of the current attachment: it holds the session id and whether the
    /// attachment has ended.
    sink: Arc<ChannelSink>,
    /// Held for the length of a resume, so two cannot race to replace the attachment.
    resuming: Arc<Mutex<()>>,
}

impl Conversations {
    pub fn new(backend: Result<ClaudeCode, String>, gate: Arc<Consent>) -> Self {
        Conversations {
            backend,
            gate,
            next: AtomicU64::new(1),
            open: Mutex::new(HashMap::new()),
        }
    }

    fn backend(&self) -> Result<&ClaudeCode, CommandError> {
        self.backend
            .as_ref()
            .map_err(|why| format!("the Claude Code backend is not available: {why}"))
    }

    fn open(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Open>> {
        self.open.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn session(&self, conversation: u64) -> Result<Arc<dyn Session>, CommandError> {
        self.open()
            .get(&conversation)
            .map(|o| o.session.clone())
            .ok_or_else(|| format!("no conversation {conversation}"))
    }
}

/// The reason the backend could not be built, or nothing. Shown by the WebView at
/// startup, since a missing `claude` is the first thing a user needs to know.
#[tauri::command]
pub fn backend_status(state: tauri::State<'_, Arc<Conversations>>) -> Option<String> {
    state.backend.as_ref().err().cloned()
}

/// Opens a conversation under `account` in `workspace_root`. Returns its id once the
/// process is up; the session id follows on the channel.
#[tauri::command]
pub async fn start_conversation(
    state: tauri::State<'_, Arc<Conversations>>,
    account: String,
    workspace_root: String,
    channel: Channel<ConversationEvent>,
) -> Result<u64, CommandError> {
    if account.is_empty() {
        return Err("an account name is required".into());
    }
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let backend = state.backend()?;
        let conversation = state.next.fetch_add(1, Ordering::SeqCst);
        let sink = ChannelSink::new(ConversationId(conversation), channel);
        let session = backend
            .start(Start {
                conversation: ConversationId(conversation),
                account: AccountId(account),
                workspace_root: PathBuf::from(workspace_root),
                gate: state.gate.clone(),
                events: sink.clone(),
            })
            .map_err(|e| e.to_string())?;
        state.open().insert(
            conversation,
            Open {
                session: Arc::from(session),
                sink,
                resuming: Arc::new(Mutex::new(())),
            },
        );
        Ok(conversation)
    })
    .await
    .map_err(|e| format!("command thread: {e}"))?
}

#[tauri::command]
pub async fn send_input(
    state: tauri::State<'_, Arc<Conversations>>,
    conversation: u64,
    text: String,
) -> Result<u64, CommandError> {
    let session = state.session(conversation)?;
    tauri::async_runtime::spawn_blocking(move || {
        session
            .send(UserInput { text })
            .map(|turn| turn.raw())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("command thread: {e}"))?
}

#[tauri::command]
pub async fn interrupt_conversation(
    state: tauri::State<'_, Arc<Conversations>>,
    conversation: u64,
) -> Result<(), CommandError> {
    let session = state.session(conversation)?;
    tauri::async_runtime::spawn_blocking(move || session.interrupt().map_err(|e| e.to_string()))
        .await
        .map_err(|e| format!("command thread: {e}"))?
}

/// Ends the attachment. The conversation stays open with its session id, so it can be
/// resumed; `Exited` arrives on its channel.
#[tauri::command]
pub async fn terminate_conversation(
    state: tauri::State<'_, Arc<Conversations>>,
    conversation: u64,
) -> Result<(), CommandError> {
    let session = state.session(conversation)?;
    tauri::async_runtime::spawn_blocking(move || session.terminate().map_err(|e| e.to_string()))
        .await
        .map_err(|e| format!("command thread: {e}"))?
}

/// Reattaches to the session this conversation reported, on the same channel, once the
/// attachment before it has reported `Exited`. The account and workspace root are the
/// stored session's; nothing from the WebView names either.
#[tauri::command]
pub async fn resume_conversation(
    state: tauri::State<'_, Arc<Conversations>>,
    conversation: u64,
) -> Result<(), CommandError> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let backend = state.backend()?;
        let (resuming, previous) = {
            let open = state.open();
            let open = open
                .get(&conversation)
                .ok_or_else(|| format!("no conversation {conversation}"))?;
            (open.resuming.clone(), open.sink.clone())
        };
        // One resume at a time: the checks below are against the attachment that is
        // current while this lock is held, and the replacement happens under it.
        let _resuming = resuming.lock().unwrap_or_else(|e| e.into_inner());
        let current = state
            .open()
            .get(&conversation)
            .map(|o| o.sink.clone())
            .ok_or_else(|| format!("no conversation {conversation}"))?;
        if !Arc::ptr_eq(&current, &previous) {
            return Err("this conversation was just resumed".into());
        }
        if !current.exited() {
            return Err("this conversation is still attached; terminate it first".into());
        }
        let id = current
            .session()
            .ok_or_else(|| "this conversation has no session id to resume".to_string())?;
        let sink = ChannelSink::new(ConversationId(conversation), current.channel().clone());
        let session = backend
            .resume(Resume {
                conversation: ConversationId(conversation),
                session: id,
                gate: state.gate.clone(),
                events: sink.clone(),
            })
            .map_err(|e| e.to_string())?;
        if let Some(open) = state.open().get_mut(&conversation) {
            open.session = Arc::from(session);
            open.sink = sink;
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("command thread: {e}"))?
}
