#!/usr/bin/env bash
# Crude structural check for content AGENTS.md forbids. It catches shapes, not meaning:
# passing this is not evidence of compliance.
set -uo pipefail

status=0

# scan <label> <pattern> [allowed-substring ...]
scan() {
  local label="$1" pattern="$2"; shift 2
  local hits
  hits=$(git grep -nIE "$pattern" -- ':!.github/scripts/hygiene.sh' ':!AGENTS.md' 2>/dev/null) || true
  local allowed
  for allowed in "$@"; do
    hits=$(printf '%s\n' "$hits" | grep -vF "$allowed") || true
  done
  hits=$(printf '%s' "$hits" | sed '/^$/d')
  if [ -n "$hits" ]; then
    echo "::error::$label"
    printf '%s\n' "$hits"
    status=1
  fi
}

scan "UUID-shaped string (use 00000000-0000-0000-0000-000000000000)" \
  '[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}' \
  '00000000-0000-0000-0000-000000000000'

scan "JWT-shaped string" 'eyJ[A-Za-z0-9_-]{20,}'

scan "API-key-shaped string (use sk-example)" \
  'sk-[A-Za-z0-9_-]{16,}' 'sk-example'

scan "Tenant-shaped hostname (use example.com)" \
  '[A-Za-z0-9-]+\.onmicrosoft\.com'

scan "Private-range IP address" \
  '(10\.[0-9]{1,3}|192\.168|172\.(1[6-9]|2[0-9]|3[01]))\.[0-9]{1,3}\.[0-9]{1,3}'

if [ $status -ne 0 ]; then
  echo
  echo "See AGENTS.md section 1. Replace the value with a placeholder."
fi
exit $status
