# Decisions

Why the project is shaped the way it is. Each entry records what was chosen, what it rules
out, and what was rejected — so a later session, or a later machine, does not silently
re-litigate a settled question or retry a known dead end.

Add to this file when a decision would otherwise survive only in someone's memory.

## Build an agentic coding client, not a chat client

Chat clients for custom gateways are a solved and crowded space: LibreChat, Cherry Studio,
Witsy, 5ire and Open WebUI are all free, mature, and point at a custom base URL. There is no
defensible reason to add another.

The gap is one layer up — though not where this entry originally put it. "Agentic coding
clients are tuned for a single vendor's model family" was re-checked and is false as a
description of the field: Goose, OpenCode and Continue are all provider-agnostic, and some
already authenticate without a static key for the providers they implement first-hand. What
none of them offers is that credential lifecycle against a *generic* OpenAI-compatible
endpoint, and that is the actual gap. Prompts, tool schemas and malformed-output recovery do
still differ by family and one harness still has to absorb that — but that is how this client
is built, not why it exists. See `README.md`.

**Rules out:** competing on chat UX, conversation features, or breadth of provider support —
and equally, resting the case for this project on model-agnosticism, which is table stakes.

## Tauri 2 rather than a custom-drawn, native or JVM desktop toolkit

The reason is the platform's text-editing contract, not rendering.

This application is mostly a long editable transcript, and on macOS an editable field is
expected to honour a long tail of behaviour: Cmd+Delete, Option+Delete, Ctrl+A/E/K,
dictionary lookup, spell check, the emoji picker, Services. No single feature request covers
that tail, but users feel every gap in it. A system WebView inherits the whole contract. A
toolkit that draws its own text widgets reimplements it one key at a time.

That was read out of the source rather than assumed. In `iced_widget` 0.14.0-0.14.2
(`widget/src/text_editor.rs` upstream; `iced_widget/src/text_editor.rs` in the crates.io
layout), line 1211 maps `Key::Named(Backspace)` without consulting modifiers, so Cmd+Delete
and Option+Delete both collapse into deleting a single character. Seventeen lines below, at
1228-1242, the arrow keys *do* branch on `macos_command()` and `jump()` — so the gap is
missing work rather than a design stance, and the extent of what else is missing is not
knowable without auditing the whole widget. The line numbers are identical in all three
published 0.14.x releases; 0.14.2 is the latest.

**What that evidence does and does not cover.** It covers toolkits that draw their own text
widgets: `iced` renders through `wgpu`/`tiny-skia` and never touches `NSTextView`. It says
nothing about a native-widget toolkit — AppKit and SwiftUI inherit the same editing contract
a WebView does, and for the same reason. The measurement is also macOS-only; no equivalent
was taken on Windows or Linux.

**The constraint that closes that gap: stanchion is cross-platform by default.** macOS may
lead where leading costs nothing, but a frontend that cannot follow to Windows and Linux is
out. That, not the measurement above, is what rules out an AppKit/SwiftUI frontend: it
inherits the macOS editing contract as well as a WebView does, and inherits nothing anywhere
else. The macOS-only measurement is still enough, but because of the constraint rather than
anything measured about WebViews elsewhere: a frontend that fails the editing contract on
any one required platform is disqualified, so one platform suffices to disqualify. What a
system WebView inherits on Windows and Linux was not measured here.

Secondary, and untested: selection that runs in one pass across heterogeneous content —
prose, code and diff hunks in the same transcript. Recorded as a hypothesis, not a reason.

Compose Multiplatform was evaluated and rejected: the JVM has no built-in web engine, the
de-facto embedding library's CEF backend has had maintenance discontinued, and an official
WebView component remains an open feature request. Those three grounds are all about
embedding a web engine, which this entry no longer treats as the deciding factor, and
they are unpinned — no library named, no dates, no link — and were not re-checked here.
**So Compose's rejection currently rests on nothing this entry still uses.** What does
apply is the cross-platform constraint above, which Compose satisfies; the editing-contract
argument does *not* transfer to it. Compose Desktop draws its own text widgets through
Skia, but its macOS key mapping handles the case `iced` misses:
`compose-multiplatform-core`, `KeyMapping.skiko.kt:62-73`, maps `Key.Backspace` with `Meta`
to `DELETE_FROM_LINE_START` and with `Alt` to `DELETE_PREV_WORD`. **Anyone reopening Compose
should start there: the recorded grounds are stale, not the toolkit.**

**Rejected as reasons — these were believed, then re-examined, and do not support the
decision:**

- *Markdown, syntax highlighting and diff review are solved by the web and unsolved
  elsewhere.* All three are wrong about the underlying work: `pulldown-cmark`, `syntect`,
  `tree-sitter` and `similar` cover parsing, highlighting and diffing natively. They do not
  supply the review *interface* built on top of them — that cost is real, but it is ordinary
  UI work rather than something only a web stack can do.
- *Sandboxed HTML preview requires a WebView frontend.* It requires one window that can reach
  neither the core nor the network. An emptied Tauri capability does **not** supply the first
  half: while the application declares no permission manifest the ACL is skipped for
  application commands altogether, so an emptied capability scopes only the Tauri-provided
  ones, and a locally-served window could still invoke every command the core registers
  (AGENTS.md section 5; a preview served from a non-local origin is still rejected). Switching the ACL on would not fully close it either:
  `plugin:__TAURI_CHANNEL__|fetch` is exempt from the check unconditionally, and it drains an
  application-wide map keyed by a global counter without checking which window is asking, so a
  second window can steal a payload queued for the first by guessing a sequential id.
  Capabilities also do not stop model-generated HTML fetching remote resources, which takes a
  restrictive CSP; `src-tauri/tauri.conf.json` does set one, but it is global rather than a
  policy for an isolated preview window. Both halves are needed and neither is in place for
  such a window. A fully native macOS application could host that one window in a
  `WKWebView`; the Windows and Linux equivalents were not investigated.
- *Long-form CJK input is a weak area outside a system WebView.* Tested against `iced` only,
  and false there: it composes inline, puts the candidate window under the caret, allows
  clause movement and resizing, and holds state over long input. The claim being retired
  was originally made about Compose Multiplatform, which was **not** re-tested — nor were
  GTK or Qt, nor any OS or IME other than the one used. So this is not a reason to choose a
  WebView, and equally not evidence that another toolkit is fine. The decision does not
  rest on it either way; a reproduction against a specific toolkit is still worth filing.

**Rules out:** sharing UI code with a mobile target, and any frontend toolkit that exists on
only one desktop platform.

## Not evaluated: Electron

Electron is the obvious alternative to Tauri, and this record has never mentioned it. That is
an omission rather than a rejection: **nothing written here rules Electron out.**

The reason the entry above gives does not distinguish it. Electron bundles Chromium, which
inherits the platform's text-editing contract for the same reason a system WebView does, and
it satisfies the cross-platform constraint as well. Neither of the two arguments this project
actually relies on separates Tauri from Electron.

Whoever closes this should measure rather than argue. The candidate discriminators, none of
them tested here:

- **Where the privileged side lives.** Tauri's is Rust, reachable from the WebView only
  through named IPC commands. Electron's is Node in the main process. Whether that is a
  material difference for a design in which credentials never leave the core, or only a
  difference of language, has not been examined.
- **Who patches the engine.** Electron ships a Chromium this project would then have to keep
  current; a system WebView is patched by the OS vendor, and in exchange varies by OS version.
- **Distribution size and memory.** The usual grounds, and the least interesting.

## The WebView is the risk; the IPC boundary is what contains it

Embedding a browser engine to get the editing contract means rendering model-produced text
inside a full browser engine. That is the cost of choosing Tauri 2 for the text-editing
contract, not a benefit of it, and the boundary is what pays it down.

The frontend is untrusted. It holds no credential, opens no socket to the gateway, and
touches no files. Every capability it has is a named IPC command the core can refuse.

**Rules out:** convenience shortcuts that let the frontend call the gateway or the
filesystem directly. Any change that widens this must say so in its pull request.

## Credentials are providers with a lifecycle, not strings in a settings field

See `auth.md`. This is the single feature most responsible for the project existing.

**Rejected: reusing an existing gateway SDK's OAuth2/JWT auto-refresh.** At least one
OpenAI-compatible gateway ships exactly this, but as an SDK feature that injects headers
into its own calls. It does not act as a local proxy in front of an upstream protected
gateway, so it cannot serve a desktop client. The providers are implemented here.

**Rejected: shipping only a static-key provider and telling users to run a local
token-refreshing sidecar.** That is a real workaround and it works, but it makes the
distinguishing capability someone else's problem and leaves the product indistinguishable
from what already exists.

**Not evaluated: contributing this to an existing agent instead of building a client.** If the
credential lifecycle is the one differentiator, the fair question is why it is not a pull
request against an agent that already exists. Goose is the obvious candidate — Rust, open
source, and advertising work with any LLM across 15+ providers — so a credential provider
could plausibly live there rather than here.

Nothing was filed upstream, no maintainer was asked, and no attempt was made to size what a
provider-with-a-lifecycle would touch in someone else's codebase. One part is checkable from
outside without asking anyone, though, and it narrows the question: Goose already
authenticates some providers without a static key — a device-code flow for GitHub Copilot,
browser OAuth for ChatGPT Codex — so the upstream does model a credential as more than a
string read once at startup, at least for the providers it implements first-hand.

That does not settle it. What separates an additive change from an architectural one is
whether Goose's *generic* OpenAI-compatible provider can be pointed at the same lifecycle or
reads a static key by construction, and whether the maintainers want one that is not tied to
a first-party provider. Neither has been checked. **Still unknown — but the first half is a
morning's work: read that one provider, then ask.**

**Evaluated 2026-09-07: Orca, and the shape of the gap.** Orca is the largest adjacent tool —
an orchestrator that runs any CLI agent, each in its own git worktree. It does not call model
APIs itself; the wrapped CLI does, and each brings its own credential. For a gateway that
issues a *static* key Orca is already a substitute, reached today through a custom provider on
the Codex CLI. What it has no notion of is a credential with a lifecycle: its provider-account
panel accepts vendor subscription logins only, and first-class custom endpoints are an open
request (`stablyai/orca` #9239, filed 2026-07-17) whose stated workaround is a base URL plus a
static token pasted into a wrapper command. **That is the arrangement rejected two paragraphs
above, running at scale** — so the gap this project aims at is real and someone is already
feeling it, but it is a gap in credential lifecycle, not in orchestration.

Both major agent CLIs already accept a credential *command* re-run on an interval: Claude
Code's `apiKeyHelper`, and Codex's `[model_providers.<id>.auth]` carrying `command` and
`refresh_interval_ms` (`codex-rs/model-provider/src/auth.rs`). This project's own `command`
provider consumes that shape; the entry below produces it.

## The credential lifecycle gets a command-line surface, but not before it exists

`crates/core` does not depend on the frontend, so a command that prints a live token for a
configured profile is a third consumer of the same core rather than a second product. Because
both agent CLIs above accept exactly that shape, the surface puts this project's
distinguishing capability inside tools it otherwise has no contact with — the orchestrator
above included, which makes it a host rather than a competitor.

**It ships with the authentication milestone, not earlier.** Until a provider exists that
actually refreshes, such a command could only print a stored static key, which is the sidecar
arrangement this file rejects — except shipped by us rather than suggested to the user. The
value of the surface is exactly co-extensive with the lifecycle behind it.

**The guard, because the risk here is drift rather than logic.** The command is useful, people
wire it into their agent CLI, and the desktop client never gets finished — at which point
"shipping only a static-key provider and telling users to run a sidecar" has come true by
accident, with us maintaining the sidecar. So the walking-skeleton milestone keeps the
completion condition it has: the GUI walks. The command is a surface over the core, never the
product.

## One agent loop, with model differences pushed into profiles

If supporting a model requires a branch inside the loop, the profile abstraction is wrong
and gets fixed rather than worked around.

**Rules out:** a loop that is correct for one vendor and patched for the others.

## A run is a value, not the application's mode

The loop must be instantiable many times over, concurrently, inside one process. Everything
that describes a run — the workspace root, the model profile, the credential handle, the
message history, the limits — is carried in a value, never in a global or in a singleton the
application configures once at startup.

This is recorded before the loop is written because it is free now and a rewrite afterwards.
Nothing in `crates/core` assumes a single run yet; the moment the loop lands, something will.

**Why it is unusually cheap here.** Fanning one task across several models and comparing the
results is the most-praised capability of the closest adjacent tool, and the complaint filed
most often against that tool is that fanning out across vendor agents multiplies the
subscription each one burns. This project's shape inverts that cost: several profiles against
one gateway credential is the same loop run N times, not N products paid for separately. The
two decisions already made — one loop with the differences pushed into profiles, and a
credential that is a provider rather than a string — are precisely what make concurrency a
consequence of the architecture rather than a feature bolted on later.

**Rules out:** a loop that reads its configuration from process-wide state; a credential
provider that can serve only one consumer; a tool implementation that assumes the process has
exactly one workspace root.

Not decided here: whether concurrent runs get isolated git worktrees, and what comparing
their results looks like. Those are product questions, and this entry only keeps them
reachable.

## Rejected: rendering an agent-driven UI description format natively

Considered as a way to avoid embedding a browser engine and to exercise a separate renderer
project. Rejected on a structural fact: generic models behind a generic gateway do not emit
such a format. Its producers live on the agent side, so a coding client would have to coerce
the output through prompt instructions — which exercises nothing real.

Model-generated HTML, by contrast, arrives whether or not anyone asked for it. That is the
format worth rendering.

## Naming

`stanchion`. Two earlier candidates were discarded, and the reasons generalise:

- A machine-part name was discarded after it turned out to read as an insult in American
  slang. **Check slang connotation before adopting an English name.**
- Several mechanism metaphors — escapement, trunnion, detent — are already taken by active
  projects in the agent-tooling space specifically. **Check for collisions inside this
  domain, not just for global uniqueness.**
