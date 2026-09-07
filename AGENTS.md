# Working in this repository

Applies to humans and to AI agents. Read it before writing code, an issue, or a comment.

## 1. Context hygiene — non-negotiable

This repository is worked on from more than one machine, and some of those machines sit
inside organisations whose internal details must not end up here. **The repository, its
issues, its pull requests and its commit messages contain no organisational context.**

Never commit, and never write into an issue or a comment:

- The name of an employer, client, tenant, team, or internal project
- Internal hostnames, endpoint URLs, or IP addresses
- Tenant IDs, directory IDs, application/client IDs, object IDs, group names
- Internal model deployment names or fleet composition
- Access tokens, refresh tokens, API keys, or headers containing them
- Screenshots, HAR files, or logs that have not been scrubbed of the above

Use placeholders instead:

| Instead of | Write |
|:--|:--|
| a real host | `gateway.example.com` |
| a real ID | `00000000-0000-0000-0000-000000000000` |
| a real key | `sk-example` |
| a real tenant | `example-tenant` |

**Naming a public product is fine; naming who uses it is not.** "Support the OIDC
device-code flow against Microsoft Entra ID" is a correct requirement. "Our company's Entra
tenant uses ..." is a leak. The distinction is between describing a *technology* stanchion
must support and describing a *deployment* someone operates.

When a bug reproduces only against a specific deployment, reduce it to a generic
reproduction before filing. If it cannot be reduced, file the generic symptom and keep the
specifics out of the tracker entirely.

CI enforces a crude version of this rule (see `.github/workflows/hygiene.yml`). Passing CI
is not evidence that you complied; the check catches shapes, not meaning.

## 2. Language

All repository content is in English: code, comments, commit messages, README, issues, pull
requests. This holds regardless of the language used in the conversation that produced the
change.

## 3. Issues are the coordination channel

Work moves between machines through issues, not through chat history or local notes. An
issue should be actionable by someone with no memory of the discussion that created it:
state the goal, the constraint, and how the result will be judged.

Keep one issue per decision or per shippable unit. If a comment thread produces a new
decision, edit the issue body to reflect it rather than leaving the conclusion buried in
comments.

## 4. Commits

Conventional-style prefix and an imperative description: `feat: add device-code auth
provider`. One logical change per commit.

## 5. Secrets at runtime

Long-lived secrets live in the OS keychain, never in a config file in the repository or in
the user's home directory in plaintext. Short-lived tokens live in memory only and are never
written to disk, never logged, and never included in crash reports or telemetry.

Rendered model output runs with no path to the credential store. Any change that widens what
the WebView can reach is a security change and must say so in its pull request.

**The application command ACL is fail-open until it is switched on.** With no application
permission manifest, Tauri does not merely grant nothing — it skips the check entirely, and
*every* `#[tauri::command]` the application registers is callable from a local-origin
frontend. The enforcement site is `tauri/src/webview/mod.rs`, which runs the ACL rejection
only `if plugin_command.is_some() || has_app_acl_manifest || !is_local` — and for an app
command invoked from our own WebView all three are false. `tauri-build/src/acl.rs` sets that
middle flag only once the app manifest actually yields permissions, so with no manifest the
condition never holds. (`tauri-macros/src/command/handler.rs` carries a similar-looking "All
application commands are allowed if we don't have an application ACL" early return. That one
is compile-time dead-code removal, it is inert unless `build > removeUnusedCommands` is set,
and this project does not set it — so it is not the mechanism and must not be cited as it.)

This is the live state, not a future hazard: `src-tauri/build.rs` calls bare
`tauri_build::build()`, and `core_version` already reaches the WebView through it. The
description in `src-tauri/capabilities/default.json` records the same fact — *while no app
manifest exists*, that file's empty `permissions` list is the set of Tauri-provided commands
the frontend may call and says nothing about the application's own. That stops being true
the moment the ACL is switched on, which is the next rule.

**Switching the ACL on is a two-part change, and doing half of it breaks the app.** Passing
`AppManifest::commands(...)` autogenerates `allow-`/`deny-` permissions, and that alone flips
enforcement on for *every* application command at once — including `core_version`, which the
capability file does not grant. So a pull request that adds a command must do both: pass
`AppManifest::commands(...)` — through `tauri_build::try_build(Attributes::new()
.app_manifest(...))`, since bare `build()` takes no attributes — *and* grant every
application command the frontend still needs in `src-tauri/capabilities/default.json`. Do
only the first and the window renders "core unreachable" while CI stays green, because
nothing in CI launches the app. A pull request that does neither must say why.
`WindowsAttributes::app_manifest` is an unrelated Windows XML manifest and is not this.
