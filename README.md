# stanchion

A desktop agentic coding client for OpenAI-compatible LLM gateways.

A *stanchion* is the upright that holds a line in place — it supports the load and defines
the lane things pass through. This one stands between a workspace and a model gateway.

## Why this exists

Existing desktop LLM clients fall into two groups, and neither covers this ground:

- **Chat clients** (LibreChat, Cherry Studio, Witsy, 5ire, Open WebUI) point at a custom base
  URL and hold a conversation, but they are not agentic coding environments: no workspace, no
  tool-call loop over your files, no approval flow, no diff review.
- **Agentic coding clients** are tuned for one vendor's model family. Running a different
  model through a harness whose prompts, tool schemas and recovery behaviour were shaped for
  another vendor produces a degraded result, and their gateway support assumes a long-lived
  API key.

stanchion targets the intersection: an **agentic coding GUI** that treats any
OpenAI-compatible model as a first-class citizen, and that can authenticate to a gateway by
means other than a static key.

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

### 2. A model-agnostic agent loop

Tool schemas, system prompts, and the recovery behaviour when a model returns a malformed
tool call all differ by model family. stanchion keeps one loop and a per-model *profile*
that adapts it, rather than a loop written for one family and patched for the others.
See [docs/architecture.md](docs/architecture.md).

## Status

Early. Nothing is usable yet. The issue tracker holds the current milestone.

## Stack

Tauri 2 — a Rust core with a TypeScript/React frontend.

The frontend runs in a system WebView. The reason is text editing: a WebView inherits the
platform's own editing contract — modifier-aware deletion, dictionary lookup, spell check,
the emoji picker — which a native toolkit reimplements one key at a time. Markdown,
highlighting and diff rendering have native Rust answers and are not the reason. See
[docs/decisions.md](docs/decisions.md).

That choice means model-produced text is rendered inside a browser engine, so the frontend
is treated as untrusted. The Rust core owns the credential lifecycle, the agent loop,
filesystem access and MCP process supervision, and reaches the frontend only through named
IPC commands it can refuse — so no secret is reachable from rendered model output.

## Contributing

Read [AGENTS.md](AGENTS.md) first. It is short, and one of its rules is non-negotiable.

[docs/decisions.md](docs/decisions.md) records why the project is shaped as it is, and which
alternatives were rejected and why. Read it before proposing a different shape.

## License

Not yet chosen. Tracked as an open decision in the issue tracker.
