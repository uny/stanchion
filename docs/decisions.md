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

**Amended 2026-09-18: the vendor CLIs become first-class, and the positioning is a hypothesis
again.** The entry above located the gap in a credential lifecycle against a generic gateway.
The project now also drives the user's own Claude Code and Codex sign-ins, through those
binaries, as run backends (see "Run backends" below). That puts it in the same shape as Orca,
Conductor, Maestro and Vibe Kanban — GUIs over vendor CLI processes, some multi-account —
and Orca already runs Claude and Codex workers in one orchestration with an inbox between
them, so a client that messages between runs of different vendors is not new either. What
this project has that those do not is the native backend's credential lifecycle against a
generic endpoint, and a stated, per-backend account of what the core enforces. Whether either
is a reason to prefer it is untested. The first entry in this file is therefore a hypothesis
with a user attached, not a claim about the field, and it stays that way until something is
measured.

**Rules out, additionally:** describing inter-run messaging as a differentiator.

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
  half. It did not when this was written, because with no application permission manifest the
  ACL was skipped for application commands altogether, so an emptied capability scoped only
  the Tauri-provided ones and a locally-served window could still invoke every command the
  core registers (AGENTS.md section 5; a preview served from a non-local origin is still
  rejected). The ACL has since been switched on, and it still does not close this:
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
issues a *static* key Orca is already a substitute, reached through a custom provider on the
wrapped CLI — that last step is asserted here, not tested. What Orca's own surface has no
notion of is a credential with a lifecycle: its provider-account panel accepts vendor
subscription logins only, and first-class custom endpoints are an open request
(`stablyai/orca` #9239, filed 2026-07-17) whose stated workaround is a base URL plus a static
token pasted into a wrapper command — the same shape as the sidecar turned down above under
*"Rejected: shipping only a static-key provider"*.

**What that issue does and does not show.** It shows the static-token workaround is what
people reach for in practice. It does **not** show anyone feeling a credential-*lifecycle*
gap: the filer reports the workaround works, asks for in-panel switching and usage tracking,
and reaches Anthropic-compatible endpoints through `ANTHROPIC_BASE_URL` — a wire protocol
`architecture.md` puts outside this core. One issue carrying one comment is not evidence of
scale either; the tool's popularity is not the workaround's. So the gap this project aims at
is *adjacent to* a real complaint rather than demonstrated by it, and it is a gap in
credential lifecycle rather than in orchestration.

Both major agent CLIs already accept a credential *command* re-run on an interval: Claude
Code's `apiKeyHelper`, and Codex's `[model_providers.<id>.auth]` carrying `command` and
`refresh_interval_ms` (`codex-rs/model-provider/src/auth.rs`). This project's own `command`
provider consumes that shape; the entry below produces it.

## The credential lifecycle gets a command-line surface, but not before it exists

`crates/core` does not depend on the frontend, so a command that prints a live token for a
configured profile is a third consumer of the same core rather than a second product. Because
both agent CLIs above accept exactly that shape, the surface reaches tools this project
otherwise has no contact with — the orchestrator above included, which makes it a host rather
than a competitor. How much of the distinguishing capability actually travels that far is the
subject of the next three paragraphs, and the answer is: less than the shape suggests.

**It ships behind a provider that actually refreshes; the milestone is the consequence, not
the rule.** Until such a provider exists the command could only print a stored static key,
which is the sidecar arrangement this file rejects — except shipped by us rather than
suggested to the user. That places it in the authentication milestone, but "is it in M2?" is
not the check, because the `static` provider is M1 and a command exercised only against it
would satisfy the label while being exactly the thing forbidden. The condition is that a
refreshing provider exists *and the command is exercised against it*; issue #27 carries that
as its blocker (#7, #9) and in its done-when, and this entry is the looser statement of the
two.

**What the shape does not carry.** The consuming interface is a bare token on stdout, so the
expiry does not cross the boundary. The consumer re-runs the command on a fixed interval it
was configured with, not when the token is near expiry: Codex defaults `refresh_interval_ms`
to 300000 and documents it as the maximum age of the cached token, while this project's rule
is a 60-second buffer — so a token handed over with 61 seconds left satisfies `auth.md` and
is still being sent four minutes after it died, which `auth.md` classes as a bug rather than
an expected path. Three more of the rules that apply to every provider are in-process rules
and do not survive the boundary either: `invalidate()` and the single retry happen inside the
consumer, where this core cannot see them; "concurrent refresh collapses" collapses nothing
when N agent CLIs each spawn their own process, and against an IdP that rotates refresh
tokens two such processes can get the whole token family revoked; and an interactive flow
cannot prompt from a helper the caller runs on a timer and reads verbatim, so
`oidc_device_code` and `oidc_auth_code_pkce` cannot be served this way at all without first
settling `auth.md`'s open question on persisting a refresh token. **The surface exports the
token, not the lifecycle** — whether it must therefore emit a TTL the consumer can be
configured from, share a cache with the running application, or simply refuse the interactive
providers is open, and #27 does not close it.

**It is also a standing token oracle, and that is a security change.** Printing a live bearer
token to stdout is one sanctioned unwrap of the secret type, against `auth.md`'s rule that
redaction lives at the boundary rather than at the call site — the exception is here so that
the next such unwrap is still a defect. The larger cost is who may call it. This project's
own shell tool would reach it: model output proposes `stanchion token --profile <name>`, the
result lands in the message history, is persisted with the conversation and rendered in the
untrusted WebView, which retires "rendered model output runs with no path to the credential
store" (`AGENTS.md` section 5) in a single step. The same holds one level out — once the
command is a host agent's credential helper, that agent's shell tool can invoke it too, and
the only remaining control is the *host's* approval configuration, which this project neither
sets nor observes.

**Rules out:** a token command reachable from this project's own tool set; a `command`
provider allowed to resolve to this command, which would spawn itself until the process runs
out of descriptors; and shipping the surface without stating, where users will read it, that
it hands the credential's blast radius to the calling agent.

**"A third consumer of the same core" is a structural claim and nothing checks it.** The
neighbouring boundary claim is mechanised — `.github/workflows/build.yml` fails the build if
`cargo tree` finds a `tauri` edge under `stanchion-core` — and the WebView-widening rule has a
line in the pull-request template. This one has neither, so a command bolted onto the
`src-tauri` binary, or placed behind a crate that depends on `tauri`, ships green. Whoever
builds #27 owes it the same kind of check the core's own rule already has.

**The guard, because drift is a risk alongside the two above.** The command is useful, people
wire it into their agent CLI, and the desktop client never gets finished — at which point
"shipping only a static-key provider and telling users to run a sidecar" has come true by
accident, with us maintaining the sidecar. The walking-skeleton milestone therefore keeps the
completion condition it has: the GUI walks. **That is a weaker guard than it looks**, and the
gap should be seen rather than papered over: M1 is already behind the command when it ships in
M2, so keeping M1's condition prevents the command being *substituted* for the skeleton and
does nothing about the failure actually named, which is a GUI abandoned somewhere in M2 or M3.
Nothing here ties the command's continued shipping to progress on the loop. The command is a
surface over the core, never the product — and if that stops being true, this paragraph is
where it was predicted, not where it was prevented.

## One agent loop, with model differences pushed into profiles

If supporting a model requires a branch inside the loop, the profile abstraction is wrong
and gets fixed rather than worked around.

**Scoped 2026-09-18 to the `native` backend.** A CLI backend owns its own loop, tools, context
management, retries and subagents, and this principle says nothing about it. What the two
backends share is everything above the loop — the run, the account, the event stream, the
inbox, the approval record, process supervision — and that is the layer #39 defines.

**Rules out:** a loop that is correct for one vendor and patched for the others.

## A run is a value, not the application's mode

The loop must be instantiable many times over, concurrently, inside one process. Everything
that describes a run — the workspace root, the model profile, the credential handle, the
message history, the limits — is carried in a value, never in a global or in a singleton the
application configures once at startup.

This is recorded before the loop is written because it is free now and a rewrite afterwards.
Nothing in `crates/core` assumes a single run yet; the moment the loop lands, something will.

**Why it is unusually cheap here.** Fanning one task across several models and comparing the
results is the most-praised capability of Orca, evaluated above, and the complaint filed most
often against it is that fanning out across vendor agents multiplies the subscription each one
burns. Neither superlative is sourced — they are recorded impressions, carried also by issue
#26, and should be re-checked before anything heavier is rested on them. This project's shape
inverts that cost: several profiles against one gateway credential is the same loop run N
times, not N products paid for separately. The two decisions already made — one loop with the
differences pushed into profiles, and a credential that is a provider rather than a string —
are precisely what make concurrency a consequence of the architecture rather than a feature
bolted on later.

**Rules out:** a loop that reads its configuration from process-wide state; a credential
provider that can serve only one consumer; a tool implementation that assumes the process has
exactly one workspace root.

**Carried in a value is not the same as supplied by the caller, and the difference is a
security boundary.** A run names the model profile — which carries the gateway base URL — the
credential handle, and the limits. `architecture.md` already puts "change where a credential
is sent" in the class needing unforgeable consent, but it reaches that class through *settings
writes*, and a run started from a value writes no setting. Starting a run therefore belongs to
that same class under the definition's own logic: the frontend may select a stored profile and
never assemble one, or a forged IPC call pairs the real credential handle with an
attacker-chosen base URL and the core sends the token there on the first request. Limits ride
along for the same reason — a run whose limits its caller chooses is a run that need never
stop, against the one credential every concurrent run shares.

**Each of the three bullets above needs a done-when, and today none of them has one.** Issue
#13 asks for two runs *against different profiles* progressing without observing each other,
and a profile is prompt, tool-schema dialect, recovery and context management (#14) — so a
conforming test can pass while a process-global workspace root serves both runs, and #15's
confinement tests pass too, since a single global root is exactly what a single-root escape
test proves correct. The same test can pass with two mock credential providers, never
exercising one provider serving two runs. And asserting interleaved *progress* does not assert
that each request carried its own run's configuration. Concretely: a per-run assertion on what
each outbound request carried, a two-root test, and a shared-provider test — until those
exist, this entry is a preference rather than a constraint.

**What the concurrency makes stale elsewhere, recorded so it is not discovered in code.**
`auth.md`'s `invalidate()` takes no token identity, so with N runs behind one provider a 401
in one run drops the token the others are using, and "a second 401 is surfaced, not retried"
then fails a healthy run for an unrelated one; "concurrent refresh collapses" covers the
refresh, not the invalidation. And `architecture.md`'s approval is per-invocation over a diff
with no snapshot of what was diffed, so two runs sharing one workspace root can make the
content approved and the content written differ — while an approval keyed only on a
model-supplied call id is ambiguous across runs in the first place. Neither is settled here;
both are now known, and both bind #21 and #16.

Not decided here: whether concurrent runs get isolated git worktrees, and what comparing
their results looks like. Those are product questions, and this entry only keeps them
reachable.

## Run backends: the vendor CLIs are first-class, and the terms are why

A run is driven either by the core's own loop (`native`) or by an unmodified vendor binary the
core supervises as a subprocess (`cli`: Claude Code over `stream-json`, Codex over its
app-server protocol). Both are built in and static; this is not a plugin system. The contract
between the core and a backend is #39.

**The terms decide the shape, not preference.** Anthropic's Claude Code legal page
(`code.claude.com/docs/en/legal-and-compliance`, read 2026-09-18) permits an end user signing
in to the unmodified Claude Code binary with their own subscription, including inside a
product that runs it, and forbids three things: developers — Agent SDK users included —
routing requests through Free, Pro or Max credentials; offering Claude.ai login inside one's
own application; and collecting, storing or intermediating Claude.ai credentials or session
tokens. Third-party harnesses that used subscription OAuth against the API directly were cut
off on 2026-04-04. So a Claude subscription reaches this project in exactly one way: the
`cli` backend, with the binary unmodified, the sign-in completed through Claude Code's own
flow, and the core creating the per-account directory and never reading from it (#41).
**Rejected:** a Claude-subscription credential provider for the native backend, in any form.
The boundary is the substance — unmodified binary, the user's own sign-in, no credential
intermediation — not the name of the package used to reach it.

OpenAI tolerates ChatGPT-subscription sign-in in third-party harnesses today and says so
publicly; no term this project can cite guarantees it. Codex therefore owns that credential
too (#47), and the backend must work unchanged with an API key when the lane closes. A
native ChatGPT-OAuth provider is not built.

**What `cli` does not carry.** The approval promise in `architecture.md` — every write
diffed, every command waited on — is the native backend's. A CLI executes whatever its own
rules auto-allow and, per its hooks reference, whatever runs while a hook fails to start or
times out. The core states per backend what it enforces and what it delegates (#40) and does
not describe the delegated set as carrying the native guarantee. Likewise `auth.md`'s rules
on where tokens may live are the native backend's; a CLI writes its credential where it
writes it, and the core neither reads it nor promises anything about it (#41). Tools the
core insists on policing — the browser first — reach a CLI run through one bridge MCP server
the core exposes, never through a server the CLI starts itself (#44).

**Initial scope, deliberately narrow.** The native backend speaks OpenAI-compatible endpoints
only; an `anthropic_messages` transport was considered and deferred, since nothing it enables
is needed while Claude arrives through its own binary. `architecture.md`'s "no non-OpenAI
dialects in the core" stands.

**Order, and the guard.** #21 first, because both backends land their approvals in it. Then
one CLI backend end to end (#46), then the second and the inbox (#47, #43), then the native
loop (M3). The native loop is what carries the credential lifecycle this file calls the reason
the project exists, and the token-command entry above already predicted the failure mode
where a surface ships and the rest never does. It applies here with more force: the CLI
backends will be useful before the native one exists. The guard is the milestone order and
nothing stronger; if M3 is still open when M4 closes, that is the signal, and this is where
it was predicted.

**Rules out:** a backend abstraction that lets the code above it branch on which backend a
run is on; an account model that forces a CLI's authentication into `CredentialProvider`
(#45); starting a run on a different account than the one its stored session was created
under; any code path in this project that reads, copies or moves a credential another
program stored.

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

## Browser: the user's own Chrome is a tool, reached through MCP first

The loop gets a browser tool, and the browser is the user's own running Chrome with its
logged-in tabs — not a fresh automation profile. That is the useful case and the dangerous
one: every action runs with the user's sessions, and every page returned is a document a
third party wrote.

**Phase A reuses Playwright MCP in `--extension` mode** through the MCP client (#18). The
server attaches to a running Chrome or Edge via an extension the user installs once; its tools
carry a `readOnly` annotation. No first-party browser code exists in this phase. The
annotation is treated as an untrusted hint: it may place a tool in the *observe* tier, never
take one out of *act*, and the tier the core refuses regardless of approval is matched on the
call itself (#33). This is not an amendment to "no plugin system beyond MCP"; it is that rule
being used.

**Phase B — a first-party extension and native messaging host — is gated on limits Phase A
actually hits**, recorded in #37 rather than assumed. If it is built, the extension is a
core-owned tool source that holds no policy: it executes what the core sends and returns
data. It is a second untrusted surface, never a source of consent, and the host process Chrome
spawns is a third consumer of `crates/core` that the `cargo tree` gate has to cover.

**What this decides elsewhere.** A browser-originated action must be approved by the same
mechanism as a WebView-originated one, which narrows #21 toward a core-owned native dialog.
Tool results that carry the user's own session data now reach persistence (#34), and runs as
values meet one shared browser (#35); both are open and both block Phase A.

**Rules out:** an approval prompt rendered inside the browser; a browser tool whose risk tier
is decided by the tool's own description; shipping Phase B without a written limit of Phase A
that it removes.

## Consent is a native dialog the core owns, and a token only the gate can mint

The WebView renders model output and is untrusted; the approval UI was placed inside it. #21
recorded the contradiction and three candidate mechanisms. This entry picks one and states
what it does and does not guarantee, before #15, #16, #17 and #46 build on it.

**The mechanism.** The core asks for consent through a presenter it defines and the shell
implements; on macOS the shell opens a native modal (`NSAlert`, via `rfd` or equivalent) from
Rust, and consumes the answer in Rust. The answer never transits IPC. There is no
`approve`-shaped application command, and the WebView is not granted any dialog plugin
permission — `tauri-plugin-dialog`'s JavaScript API hands the result back to the WebView,
which is the path this entry closes. The WebView's part in an approval is display and focus:
it may show the pending request and the diff, and it may not answer it.

**Rejected: a core-issued nonce.** The nonce has to be unreachable from the context that
renders model output, which means the approval UI and the rendered output live in separate
contexts. In `tauri 2.11.5` a second window is not that separation: `plugin:__TAURI_CHANNEL__|fetch`
is exempt from the ACL and drains an application-wide map by a guessable id (#25), so
isolation is not something this project can currently rely on. The nonce also proves only
that the WebView answered — it says nothing about a request from a CLI subprocess or a
browser extension unless the answer is routed through the WebView anyway, at which point the
native dialog is the same shape with one fewer trusted piece. Context isolation and a
restrictive CSP stay required for other reasons (rendered HTML fetching remote resources,
the preview window); they are not the consent mechanism.

**The gate, and what the token is.** One `Consent` gate in `crates/core`. Everything in the
core-owned-state class of `architecture.md` — a tool call, a settings write that names a
program or a credential destination, a workspace-root move, an auto-approve rule, and
starting a run from anything other than a stored configuration selected by reference — asks
the gate. The gate holds the request as an immutable value it built itself: a core-issued
invocation id, the run, the workspace root, every path already resolved, and for a write the
hash of the content it will write and of the file it will replace. What the gate returns is
a single-use, in-memory token bound to that value, and every execution entry point demands
the token: the native executor, the reply the core sends to a CLI's approval request (Claude
Code's permission-prompt tool result, Codex's `requestApproval` decision — on a CLI backend
that reply *is* the execution), and a bridge forward to a tool the core polices (#44). There
is no way to execute without one, and the token does not survive the process.
A token is spent on first use, is void once its run ends or the request is cancelled, and is
void if a precondition it was minted under no longer holds — the file to be replaced has
changed, the path resolves elsewhere. This is what answers the shared-workspace race that the
concurrency entry left open for #21 and #16: the approval snapshots what was diffed, and a
write that no longer matches the snapshot is a new request. The check is only as good as its
distance from the write, so verifying the precondition and performing the write are one
operation — a per-workspace write lock held across both, or a verify-then-rename — not a
check followed by a write. A late answer to a dialog whose request was cancelled mints
nothing.

**Consent is not authorization.** The tier the core refuses regardless of approval (#33) is
refused before any dialog is shown; an answer to a dialog that should not have opened is
ignored. Auto-run decisions are recorded as policy, never as consent. If policy changes while
a request is pending, the request is re-classified before its answer is honoured.

**What the dialog attests is the whole of what runs.** The dialog carries the exact request:
a shell command in full, the same string the executor receives (#16); a settings write as the
full new value and the old one. A request the presenter cannot show in full is not approvable
through it — it is refused with that reason, not summarised. Which means the write path is
not shippable on `NSAlert`: `informativeText` does not scroll, `rfd` exposes no accessory
view, and a diff reduced to a path and a content hash is a checksum the user cannot check
against the WebView's rendering, so a compromised WebView could show one diff while the
request carries another. A core-owned presenter that renders a diff — a second window whose
content is core-generated escaped text, or a native text view — is #50, a blocker for #17
rather than a follow-up, and it inherits #25 before it can be a window. It gates the CLI
backend's write cells too: a write Claude Code delegates through its permission tool, or a
Codex `requestApproval` on a write, lands in the same presenter and is refused until #50
exists, so #46's done-when either excludes writes or waits on it. Shell commands, MCP server
entries, credential provider commands and gateway URLs are short enough for the modal.

**Presenter rules.** The affirmative is never the default button: the WebView decides *when*
a request fires and can render "press Return" bait timed to it. On macOS `NSAlert` makes the
first button added the default and `rfd` 0.17.2 adds them in the order given with no API to
change that, so the negative is passed first — read from the source, and pinned by a
shell-side test on the button order, since it is a fact about an implementation rather than
its contract. Presentation is serialised, one dialog at a time, per process; the core is not.
A run waiting on a dialog blocks only itself, a run that ends withdraws its pending requests,
a queue that grows past a limit refuses rather than stacks, and nothing is approved by
timeout. Every dialog names the run and the backend it is on — this is where #40's "the UI
shows which backend" is satisfied, because this is where the user reads approvals.
Model-produced text is shown byte-exact with control and bidi characters made visible, and is
never truncated from the tail. If the presenter fails — cannot open, returns nothing,
returns something unexpected — nothing executes.

**Scope, stated so it is not overread.** This decides *who approved*, for requests that reach
the core. It is a guarantee about the IPC boundary: consent cannot be forged by script in the
WebView. It is not a guarantee against a process that can synthesise OS input — a CLI
subprocess with accessibility access could press the button; that is outside this threat
model and outside what any in-process mechanism could address. And on a CLI backend the
dialog sees only what the CLI delegates: #42 measured that Codex under `never` raises no
approval request in any cell, that Claude Code's `PreToolUse` hook is fail-open and silent in
every failure mode, and that both re-issue the last cut command on resume after a crash or
an interrupt. A
re-issue is a fresh request and gets a fresh dialog — on the native backend always, on a CLI
backend only in the cells that delegate. Which cells those are is #40's table, and nothing in
this entry moves a row of it.

**How this is tested, and where.** In `crates/core`, against a fake presenter: a presenter
that always declines executes nothing, for one request per class — shell command, MCP server
entry, credential provider command, gateway URL, workspace root, auto-approve rule, run
started from an inline profile; a token minted for one request does not execute another with
identical content; a token is spent on first use; a token for a write is void once the target
file has changed; an answer arriving after cancellation mints nothing; a request in the refused
tier never reaches the presenter; a CLI approval reply and a bridge forward are refused
without a token exactly as the native executor is. In `src-tauri`: the capability grants no command that could
carry an answer and no `dialog:` permission, the application manifest is non-empty (#24), and
the negative button is first. The capability assertions are auxiliary — the load-bearing test
is that every execution entry point demands a token, which the type makes a compile error
and the fake-presenter tests make a runtime one.

**Rules out:** any IPC command whose argument is an approval decision; a presenter opened
from the WebView side of the boundary; execution on a request the presenter could not show in
full; a summary, hash or line count standing in for the content being approved; approving on
timeout; a token that survives a restart or is minted by anything but the gate.
