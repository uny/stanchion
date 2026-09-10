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
