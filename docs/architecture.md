# Architecture

## Shape

```
┌─────────────────────────────────────────────┐
│  WebView (TypeScript / React)               │
│  conversation · diff review · pending asks  │
│  markdown · syntax highlighting             │
│  no credentials, no filesystem, no network  │
└───────────────────┬─────────────────────────┘
                    │  Tauri IPC (typed commands + event stream)
┌───────────────────┴─────────────────────────┐
│  Core (Rust)                                │
│                                             │
│  agent loop ── model profile                │
│      │                                      │
│      ├── tools ── workspace fs              │
│      │         ── shell (gated)             │
│      │         ── MCP client (stdio/HTTP)   │
│      │                                      │
│      └── transport ── credential provider   │
│                    ── OS keychain           │
└─────────────────────────────────────────────┘
```

The split is a security boundary, not only a layering preference — but the WebView is the
risk it contains, not a component that supplies safety. It renders text that a model
produced, so it is treated as untrusted. It never holds a credential, never opens a socket to
the gateway, and never touches the filesystem directly. Every capability it has is a named
IPC command the core can refuse.

That last sentence is the design, and it has two halves. It says nothing about *who* caused
a call — a separate problem, and the approval rule below is what answers it. *Which*
commands are reachable, on the other hand, is now enforced: the application command ACL is
switched on, so a command the capability does not grant is rejected at the IPC boundary
rather than skipped past. The rule that keeps it that way, and the mechanism
that made the unenforced state possible, are in AGENTS.md, section 5.

## Run backends

A run is driven by one of two backends, chosen per run and carried in the run value:

- `native` — the loop below, in this core, against an OpenAI-compatible endpoint.
- `cli` — an unmodified vendor binary (Claude Code, Codex) supervised as a subprocess, owning
  its own loop, tools and credential.

Everything above the backend is shared: the run list, the account (#45), the event stream,
the inbox (#43), the approval record, process supervision. The approval and credential rules
in this file and in `auth.md` are stated for the native backend; what a CLI backend
enforces, delegates or leaves open is tabulated under #40.

**The contract** is `crates/core/src/backend`, decided under "The run backend contract" in
`decisions.md` (#39). The code above a backend holds a `RunBackend` and a `Session` as trait
objects and never branches on which backend it has; the module's tests drive two independent
fakes — one shaped like a CLI, one like the native loop, sharing no code — through one
function to keep that true. The traits are sealed: the backends are the ones this crate
ships, not a plugin surface. Seven items, each a type:

1. **Capabilities** — `resume`, `mid_turn_input`, which approvals reach the gate (`Every`
   on native, `Delegated` on a CLI, where "which" is #40's table), and what a resumed
   session was *observed* to do with a cut turn, separately after `interrupt` and after a
   crash, on the backend version the capability names (#42). The code above reads
   capabilities to offer or withhold an affordance — a resume button, a mid-turn input
   box, a "delegated" badge — and never to alter the approval path, which is the same for
   every backend. `kind()` is there for the same reason: the dialog title and the run list
   name the backend, which #40 requires; it is shown, not branched on.
2. **`start` / `resume`** — `resume` takes a `SessionId`, which carries the backend's own
   identifier as the core stored it *and* the account and workspace it was created under,
   and takes nothing else that names either. A stored session cannot be reattached under a
   different account or root because there is nowhere at the call site to say so; only a
   backend constructs a `SessionId`, from what the backend reported. Never "the latest".
3. **Events** — one sink the caller supplies. `MessagePartial` and `MessageComplete` are
   distinct kinds: a UI that renders a partial as the message shows text the model may
   still retract. A tool call, its approval and its result share the backend's call id, so
   they correlate when several interleave. `ApprovalRequested` carries what the dialog
   shows so the WebView can display the same bytes (no backend emits it yet: the gate
   surfaces the request to its asker only on the answer, which the decision entry lists as
   open); `RanWithoutAsking` reports a call the
   backend executed that never reached the gate, recorded under #40 rather than silently
   accepted; `Diagnostic` carries what the backend said outside the conversation — an init
   record, stderr — for a log. `SessionOpened` arrives once per attachment, as soon as the
   backend knows its id, which on a CLI may be after the first input.
4. **Approval** — not a method, and not reachable. A backend receives the consent gate at
   `start`, wraps it in an `Attachment` lease that registers the consent run, and asks the
   gate itself (on a CLI backend through `CliApproval::resolve`, which owes the CLI exactly
   one reply). Nothing on `Session` takes an answer or exposes the run id, so the code above
   has nothing on a session by which to answer, or to ask under the backend's run;
   `src/lib.rs` shows the missing method's shape in a `compile_fail` doctest beside the
   token ones, and the lease's constructor is crate-private. Not closed by this: the gate's
   `register_run` and `ask` are `pub` for the integration tests, so a holder of the gate
   can open a run of its own and ask under it — narrowing them to the crate is #54.
   The resolution is the gate's, as "An IPC message is not consent" already
   requires — a `resolve(decision)` on the backend trait would be `approve(tool_call_id)`
   under another name.
5. **`deliver`** — an inbox message (#43) enters the backend, and *enqueued*, *accepted*
   (written to the backend's input) and *injected* (confirmed in context) are three
   reported states, not one. A backend that cannot observe the third never reports it.
6. **`interrupt` / `terminate`** — with the cut-turn behaviour stated in capabilities per
   cause, since Claude Code asks before continuing after its own interrupt and may re-run
   the cut call after a crash (#42). `terminate` is idempotent and takes `&self`; the end
   is observed as `Exited`, and the consent run ends with the lease — on terminate, on a
   crash, or when the session is dropped.
7. **Usage** — `NotReported` is a variant, distinct from zero; cost is an `EstimatedUsd`
   in integer millionths, the backend's estimate, and the UI labels it as such and never as
   what a subscription will bill. Per turn in the event, cumulative from the session.

**Four lifetimes, four id types, never conflated:**

| lifetime | id | owner | ends when |
|:--|:--|:--|:--|
| the GUI conversation | `ConversationId` | core | the user closes it; outlives everything below |
| the backend session | `SessionId` | backend's value, stored with its account and workspace root by the core | the backend forgets it; a resume reattaches to it |
| one turn | `TurnId` | core | the model stops, is interrupted, or the attachment under it ends (`Cut`) |
| one attachment | `AttachmentId` | core | the process exits or the loop instance ends; each resume is a new one |

The consent gate's `RunId` is the **attachment**: the `Attachment` lease registers one when
opened and ends it when dropped, so a token minted under a process that crashed is void
before the resumed process exists, and a pending dialog from it is withdrawn rather than
answered into the wrong process. A conversation therefore sees several consent runs over its
life, one per attachment, and the approval record keys on that. Turn ids come from one
counter for the process, so a turn is unique across attachments, not merely within one.

## The agent loop

One loop, parameterised by a per-model profile.

```
send(messages, tools)
  → model returns text and/or tool calls
  → for each tool call:
        classify → auto-run | request approval | refuse
        execute, capture result
  → append results to messages
  → repeat until the model returns no tool calls, or a limit trips
```

The parts that differ by model family live in the profile, not in the loop:

- **Tool schema dialect.** Strictness of JSON Schema support, whether parallel tool calls are
  emitted, how arguments are encoded.
- **System prompt.** Not a shared prompt with per-model patches — a per-profile prompt, since
  the instructions that make one family behave make another worse.
- **Malformed-output recovery.** What to do when the model emits a tool call that does not
  parse: re-prompt, repair, or fail. Model families differ here more than anywhere else.
- **Context management.** Window size, and what gets dropped or summarised first.

A profile is data plus a small amount of behaviour, and adding support for a new model
family should mean adding a profile, not editing the loop. If a change to support one model
requires a branch inside the loop, the profile abstraction is wrong and should be fixed
rather than worked around.

## Tools

The first-class set is deliberately small: read a file, write a file, apply an edit, list a
directory, search, run a shell command. Everything else arrives through MCP.

Each tool declares a risk class. Reads inside the workspace run without asking. Writes show a
diff and wait. Shell commands wait, and the approval carries the exact command. Approvals are
per-invocation by default, with opt-in rules the user writes, never rules the model proposes.

**An IPC message is not consent.** The pending request is displayed in the WebView, which is
the untrusted surface. Model output that achieves script execution there can invoke any command
the frontend is allowed to invoke, and the core cannot tell a scripted call from a click — so
a bare `approve(tool_call_id)` command would let a model approve its own shell command. The
rule, decided under "Consent is a native dialog the core owns" in `decisions.md` (#21):

- **The core executes an approved request only against a consent token, and only the
  consent gate mints one.** The gate asks a presenter the core defines and the shell
  implements — on macOS a native modal opened from Rust and answered in Rust. The answer
  never transits IPC; no application command takes an approval decision as an argument, and
  the WebView holds no dialog permission. The WebView displays a pending request; it cannot
  answer it. Every affirmative execution entry point demands the token — the native
  executor, the *allow* reply to a CLI's approval request, a bridge forward to a core-policed
  tool (#44). A *deny* reply needs none and is always sent. Auto-run takes the same door,
  with a token the gate mints on policy and records as policy.
- **The token is bound to the request the core built**, not to a call id the model supplied:
  a core-issued invocation id, the run, the workspace root, resolved paths, a command's
  directory and environment (bound, and shown only where the model supplied them, as in an
  MCP entry's `env`), and for a write the hash of the content to be written and of
  the file to be replaced, or its absence. Single use, memory only, void when the run ends,
  the request is cancelled, or a precondition changes. On the native backend verifying the
  precondition and performing the write are one operation under a write lock, so the core's
  write lands on what was verified — a mismatch is a new request; a shell command writes
  outside that lock and carries no such guarantee. On a CLI backend the CLI writes after the
  reply, so that cell carries none either — a row in #40's table. A CLI's request is shown
  as the CLI supplied it; the reply is the plain per-request answer, and a request shaped
  as a session-wide grant is refused.
- **The dialog shows the whole of what will run**, byte-exact through a lossless escape,
  never summarised; a request over the presenter's capacity is refused, not approved on a
  hash. Most shell commands and settings writes fit a modal; a diff does not, and neither
  does a long command, so a core-owned presenter that renders more (#50) blocks #17 and the
  write cells of a CLI backend (#46).
- **Consent is not authorization.** The refused tier (#33) is rejected before any dialog
  opens; the affirmative is never the default button and is not accepted in the instant a
  dialog opens; nothing is approved by timeout; if the presenter fails, nothing executes.
- **Scope.** This guarantees that consent cannot be forged from the WebView. A process that
  can synthesise OS input is outside it. On a CLI backend the dialog answers only the
  requests the CLI delegates to the core; which those are is the table under #40.

**The rule covers anything that decides an approval was unnecessary — and enumerating those
is how one gets missed.** Gating tool execution alone is not enough: a forged message that
widens an auto-run rule, or that moves the workspace root, reaches the same privileged effect
with no approval ever requested. But so does settings state that never enters the loop at
all. Two such paths are already in this design:

- **Anything that names a program the core will run.** An MCP stdio server entry is an
  executable plus an argument list, and the `command` credential provider (`docs/auth.md`) is
  a shell command re-run on token acquisition, TTL lapse and 401 invalidation. A forged
  settings write supplies `/bin/sh -c ...` and the core runs it at startup or on the next
  refresh — before tool discovery, before any classification.
- **Anything that names where a credential is sent.** A profile update that keeps the
  existing keychain reference but changes the gateway base URL exfiltrates the token on the
  next request, without touching one approval-related field.

So the requirement is a class, not a list: **core-owned state is any state whose change can
cause execution, relocate the workspace boundary, alter what is auto-approved, or change
where a credential is sent — and every write to it needs the same unforgeable consent as
running a shell command.** Otherwise "never rules the model proposes" is vacuous, since model
output is exactly what the WebView renders.

The workspace root is the boundary for filesystem tools. Paths that escape it are refused by
the core, not by the prompt.

## Transport

One OpenAI-compatible client speaking `/v1/chat/completions` with streaming, tools, and
whatever extensions a profile declares support for. Server-sent events are streamed through
to the UI without buffering; a response that arrives all at once instead of incrementally is
a defect.

The transport asks the credential provider for a token per request — the provider decides
whether that means a cache hit or a refresh. On a 401 it invalidates once and retries once.

## Persistence

Conversations, workspace association and settings go in SQLite in the platform application
data directory. Long-lived secrets go in the OS keychain and are referenced from settings by
name. Short-lived tokens are held in memory only.

## What is deliberately not here

- No cloud sync, no account, no telemetry back to anyone.
- No plugin system beyond MCP. MCP is the extension point.
- No support for non-OpenAI-shaped APIs in the core. A gateway that speaks another dialect is
  the gateway's problem to translate, not stanchion's.
