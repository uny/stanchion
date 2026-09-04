# Decisions

Why the project is shaped the way it is. Each entry records what was chosen, what it rules
out, and what was rejected — so a later session, or a later machine, does not silently
re-litigate a settled question or retry a known dead end.

Add to this file when a decision would otherwise survive only in someone's memory.

## Build an agentic coding client, not a chat client

Chat clients for custom gateways are a solved and crowded space: LibreChat, Cherry Studio,
Witsy, 5ire and Open WebUI are all free, mature, and point at a custom base URL. There is no
defensible reason to add another.

The gap is one layer up. Agentic coding clients are tuned for a single vendor's model
family, and a harness whose prompts, tool schemas and malformed-output recovery were shaped
for one family degrades when pointed at another.

**Rules out:** competing on chat UX, conversation features, or breadth of provider support.

## Tauri 2 rather than a JVM-based desktop toolkit

The hardest rendering work in this application — Markdown, syntax highlighting, diff review,
sandboxed HTML preview — is solved by the web ecosystem and unsolved elsewhere.

Compose Multiplatform was evaluated and rejected: the JVM has no built-in web engine, the
de-facto embedding library's CEF backend has had maintenance discontinued, and an official
WebView component remains an open feature request. Long-form CJK text input and selection
across a long transcript is also a known weak area there, whereas a system WebView inherits
the platform's own behaviour.

**Rules out:** sharing UI code with a mobile target.

## The WebView boundary is a security boundary

The frontend renders text a model produced, so it is untrusted. It holds no credential,
opens no socket to the gateway, and touches no files. Every capability it has is a named IPC
command the core can refuse.

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

## One agent loop, with model differences pushed into profiles

If supporting a model requires a branch inside the loop, the profile abstraction is wrong
and gets fixed rather than worked around.

**Rules out:** a loop that is correct for one vendor and patched for the others.

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
