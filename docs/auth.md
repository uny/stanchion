# Authentication

The design premise: **a gateway credential has a lifecycle, and the client must own it.**

Most desktop LLM clients model the credential as a string the user pastes into a settings
field. That works for a personal API key and fails for everything else. A gateway fronted by
an identity provider issues credentials that expire in minutes or hours, are obtained
interactively, and must never touch disk.

## The provider interface

A credential provider answers one question — *what is the current bearer token?* — and owns
whatever work that requires.

```rust
#[async_trait]
pub trait CredentialProvider: Send + Sync {
    /// Returns a currently-valid bearer token, refreshing if needed.
    async fn token(&self) -> Result<Token, AuthError>;

    /// Called when the gateway rejects a token that this provider vended,
    /// so the provider can invalidate its cache and re-acquire once.
    async fn invalidate(&self);
}

pub struct Token {
    pub value: SecretString,
    pub expires_at: Option<Instant>,
}
```

Everything above the transport layer sees only `token()`. Whether that string came from the
keychain, a subprocess, or a browser round-trip is not the caller's business.

## Providers

### `static`

A long-lived key. Stored in the OS keychain, never in a config file. Present because it is
still the common case and must not be made awkward by the existence of the others.

### `command`

Runs a shell command and reads the credential from stdout. This covers every credential
source that already has a CLI — secret managers, cloud CLIs, corporate helper scripts —
without stanchion needing to integrate with any of them.

The output is cached for a TTL (default 5 minutes) and the command is re-run when the cache
lapses or when `invalidate()` is called. The command's stdout is treated as the credential
verbatim; a helper that prints a banner alongside the token is a broken helper.

### `oauth2_client_credentials`

The machine-to-machine flow: client ID and secret exchanged at the IdP's token endpoint for
an access token. The secret lives in the keychain. Tokens are cached and refreshed on a
buffer before expiry rather than after a 401.

### `oidc_device_code`

The interactive flow for a human identity where no browser redirect is available or wanted:
stanchion displays a code, the user authorises in a browser, stanchion polls for the token.
Yields a refresh token, so the interactive step happens once per session lifetime rather than
once per hour.

### `oidc_auth_code_pkce`

The interactive flow with a browser redirect to a loopback listener, using PKCE. Preferred
over the device-code flow when a browser is available on the same machine, because it is a
single click rather than a code transcription.

## Rules that apply to every provider

- **Refresh before expiry, not after failure.** A 60-second buffer. A request that fails
  with 401 because the token expired mid-flight is a bug, not an expected path.
- **A single retry on rejection.** On 401 the transport calls `invalidate()` and retries
  exactly once. A second 401 is surfaced to the user, not retried.
- **Short-lived tokens never touch disk.** Not in a cache file, not in a log, not in a crash
  report. Only long-lived secrets go to the keychain.
- **Redaction is at the boundary, not at the call site.** The token type does not implement
  `Display` or `Debug` in a way that reveals the value, so no logging statement can leak it
  by accident.
- **Concurrent refresh collapses.** Several in-flight requests discovering an expired token
  at once must result in one refresh, not several.

## Open questions

Tracked as issues rather than settled here:

- Whether the refresh token in the interactive flows may be persisted to the keychain, or
  must be re-acquired on every application start.
- How a gateway that returns a non-standard error body for an expired token is detected.
- Whether per-workspace credential selection is needed, or one credential per gateway
  profile is enough.
