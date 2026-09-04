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
