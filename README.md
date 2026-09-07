# stanchion

A desktop agentic coding client for OpenAI-compatible LLM gateways.

A *stanchion* is the upright that holds a line in place — it supports the load and defines
the lane things pass through. This one stands between a workspace and a model gateway.

## Why this exists

Existing desktop LLM clients fall into two groups, and neither covers this ground:

- **Chat clients** (LibreChat, Cherry Studio, Witsy, 5ire, Open WebUI) point at a custom base
  URL and hold a conversation, but they are not agentic coding environments: no workspace, no
  tool-call loop over your files, no approval flow, no diff review.
- **Agentic coding clients** are split. Some are tuned for one vendor's model family, and
  running a different model through a harness whose prompts, tool schemas and recovery
  behaviour were shaped for another produces a degraded result. But the provider-agnostic
  ones are neither few nor immature: Goose advertises working with any LLM across 15+
  providers, OpenCode with 75+, and Continue is provider-agnostic as well. Nor do all of
  them assume a static key: Goose authenticates GitHub Copilot by device code and ChatGPT
  Codex by browser OAuth. But that lifecycle exists only for the providers they implement
  first-hand. Point any of them at a *generic* OpenAI-compatible endpoint and the
  credential is a long-lived API key again.

stanchion targets what that leaves: an **agentic coding GUI** for OpenAI-compatible gateways
that can authenticate by means other than a static key.

**Being model-agnostic is not the claim.** As of 2026 it is table stakes, and any positioning
that rests on it is describing the field rather than a difference from it. The credential
lifecycle below is the difference. The agent loop is how this one is built, not a reason to
prefer it.

## The two things that are actually hard

Everything else here is ordinary application work. These two are not.

### 1. Pluggable authentication

A gateway in front of a model fleet is frequently protected by an identity provider rather
than by a shared secret. Almost every existing desktop client assumes
`Authorization: Bearer <static key>` and stops there. stanchion treats the credential as a
provider with a lifecycle. See [docs/auth.md](docs/auth.md).

| Provider | Credential source | Refresh |
|:--|:--|:--|
| `static` | OS keychain | none |
| `command` | stdout of a shell command | re-run on TTL |
| `oauth2_client_credentials` | IdP token endpoint | before expiry |
| `oidc_device_code` | interactive device-code flow | refresh token |
| `oidc_auth_code_pkce` | browser redirect, PKCE | refresh token |

### 2. One agent loop that holds up across model families

Reaching many providers is plumbing, and it is solved. Staying *correct* across families that
disagree is not: tool schemas, system prompts, and the recovery behaviour when a model returns
a malformed tool call all differ. stanchion keeps one loop and a per-model *profile* that
adapts it, rather than a loop written for one family and patched for the others. Whether that
holds up better than the alternatives is untested — nothing here runs yet.
See [docs/architecture.md](docs/architecture.md).

## Status

Early. Nothing is usable yet. The issue tracker holds the current milestone.

## Stack

Tauri 2 — a Rust core with a TypeScript/React frontend.

The frontend runs in a system WebView. The reason is text editing: a WebView inherits the
platform's own editing contract — modifier-aware deletion, dictionary lookup, spell check,
the emoji picker — which a toolkit that draws its own text widgets reimplements one key at a
time. Markdown, highlighting and diff rendering have native Rust answers and are not the
reason. See [docs/decisions.md](docs/decisions.md).

That choice means model-produced text is rendered inside a browser engine, so the frontend
is treated as untrusted. The Rust core owns the credential lifecycle, the agent loop,
filesystem access and MCP process supervision, and the frontend reaches it only through
named IPC commands the core can refuse — so no secret is reachable from rendered model
output.

## Development

macOS, with a Rust toolchain, Node 24 and pnpm.

```sh
pnpm install
pnpm tauri dev            # opens the window against the Vite dev server
pnpm tauri build --no-bundle
```

The core lives in `crates/core` and must not depend on Tauri; `src-tauri` is the IPC
boundary that wraps it. `cargo fmt`, `cargo clippy` and `cargo test` run from the repository
root, which is the workspace root.

## Contributing

Read [AGENTS.md](AGENTS.md) first. It is short, and one of its rules is non-negotiable.

[docs/decisions.md](docs/decisions.md) records why the project is shaped as it is, and which
alternatives were rejected and why. Read it before proposing a different shape.

## License

Not yet chosen. Tracked as an open decision in the issue tracker.
