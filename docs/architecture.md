# Architecture

## Shape

```
┌─────────────────────────────────────────────┐
│  WebView (TypeScript / React)               │
│  conversation · diff review · approvals     │
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

The split is a security boundary, not only a layering preference. The WebView renders text
that a model produced, so it is treated as untrusted. It never holds a credential, never
opens a socket to the gateway, and never touches the filesystem directly. Every capability
it has is a named IPC command the core can refuse.

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
