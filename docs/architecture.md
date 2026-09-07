# Architecture

## Shape

```
┌─────────────────────────────────────────────┐
│  WebView (TypeScript / React)               │
│  conversation · diff review · approval UI   │
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

That last sentence is the design, and both halves of it need qualifying. It says nothing
about *who* caused a call — a separate problem, and the approval rule below is what answers
it. And *which* commands are reachable is not enforced yet either: with no application
permission manifest, Tauri skips the ACL check for application commands entirely, so every
command the core registers is reachable from the WebView today. The rule that closes that
is in AGENTS.md, section 5.

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

**An IPC message is not consent.** The approval UI is rendered in the WebView, which is the
untrusted surface. Model output that achieves script execution there can invoke any command
the frontend is allowed to invoke, and the core cannot tell a scripted call from a click — so
a bare `approve(tool_call_id)` command would let a model approve its own shell command. The
core must require something the WebView cannot forge before it executes an approved call.
Which mechanism supplies that is open (issue #21); no design that takes the frontend's word
for it is acceptable. Because every signal that originates in the WebView is forgeable by
the same script, the unforgeable factor has to come from outside it — a core-owned native
dialog, or a secret the rendering context cannot read — never a token the frontend also
holds.

**The rule covers anything that decides an approval was unnecessary — and enumerating those
is how one gets missed.** Gating the `approve` call alone is not enough: a forged message that
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
