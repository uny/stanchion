#!/usr/bin/env bash
# A stand-in for the `claude` binary, for `crates/core/src/backend/claude_code/tests.rs`.
# Speaks enough of the stream-json protocol (as `claude` 2.1.266 emits it, measured in
# #42 and in the backend's module docs) for the supervisor to be tested without the real
# binary, a sign-in, or the network. Every line is canned; nothing here is a model.
#
# Behaviour, per environment variable (unset = the measured default):
#   FAKE_CLAUDE_INIT_FIRST=1   emit `system/init` at startup, before any input (the
#                              contract allows either; the real CLI waits for input and
#                              then repeats init for every turn, which this does too)
#   FAKE_CLAUDE_NOT_LOGGED_IN=1 answer the first input as an unauthenticated CLI does
#   FAKE_CLAUDE_CRASH_AFTER=N  die by SIGKILL after the N-th result line
#   FAKE_CLAUDE_EXIT_AFTER=N   exit 3 after the N-th result line
#   FAKE_CLAUDE_NO_DENIALS=1   omit `permission_denials` from the result line
#   FAKE_CLAUDE_STDERR=text    write `text` to stderr at startup
#   FAKE_CLAUDE_STATE=path     append one line per event to `path` (what it received)
#
# The session id is the placeholder AGENTS.md prescribes; `.github/scripts/hygiene.sh`
# rejects a UUID-shaped one.
set -u

SESSION="00000000-0000-0000-0000-000000000000"
turns=0
init_done=0

note() {
  if [ -n "${FAKE_CLAUDE_STATE:-}" ]; then
    printf '%s\n' "$1" >> "$FAKE_CLAUDE_STATE"
  fi
}

emit_init() {
  if [ "$init_done" -eq 0 ]; then
    init_done=1
    printf '%s\n' "{\"type\":\"system\",\"subtype\":\"init\",\"cwd\":\"$PWD\",\"session_id\":\"$SESSION\",\"model\":\"fake-model\",\"permissionMode\":\"default\",\"apiKeySource\":\"none\",\"claude_code_version\":\"0.0.0-fake\"}"
  fi
}

emit_result() {
  turns=$((turns + 1))
  printf '%s\n' "$1"
  if [ "${FAKE_CLAUDE_CRASH_AFTER:-0}" -eq "$turns" ]; then
    kill -9 $$
  fi
  if [ "${FAKE_CLAUDE_EXIT_AFTER:-0}" -eq "$turns" ]; then
    exit 3
  fi
}

if [ -n "${FAKE_CLAUDE_STDERR:-}" ]; then
  printf '%s\n' "$FAKE_CLAUDE_STDERR" >&2
fi
note "config_dir=${CLAUDE_CONFIG_DIR:-}"
note "args=$*"
if [ "${FAKE_CLAUDE_INIT_FIRST:-0}" = "1" ]; then
  emit_init
fi

while IFS= read -r line; do
  case "$line" in
    *'"subtype":"interrupt"'*)
      note "interrupt"
      printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"1"}}'
      emit_result '{"type":"result","subtype":"success","is_error":false,"session_id":"'"$SESSION"'","total_cost_usd":0.001,"usage":{"input_tokens":3,"output_tokens":1},"permission_denials":[]}'
      ;;
    *'"type":"user"'*)
      note "user:$line"
      init_done=0
      emit_init
      if [ "${FAKE_CLAUDE_NOT_LOGGED_IN:-0}" = "1" ]; then
        printf '%s\n' '{"type":"assistant","message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"Not logged in · Please run /login"}]}}'
        emit_result '{"type":"result","subtype":"success","is_error":true,"terminal_reason":"api_error","session_id":"'"$SESSION"'","total_cost_usd":0,"usage":{"input_tokens":0,"output_tokens":0},"permission_denials":[],"result":"Not logged in · Please run /login"}'
        continue
      fi
      case "$line" in
        *'"text":"wait"'*)
          # A turn that stays open until interrupted or the process is killed, with a
          # call in flight that the interrupt cuts.
          printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"working"}}}'
          printf '%s\n' '{"type":"assistant","message":{"model":"fake-model","role":"assistant","content":[{"type":"tool_use","id":"toolu_cut","name":"Bash","input":{"command":"sleep 600"}}]}}'
          continue
          ;;
        *'"text":"die"'*)
          # A crash mid-turn.
          printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"working"}}}'
          kill -9 $$
          ;;
      esac
      # The delta types the real CLI interleaves: only the text one is the message.
      printf '%s\n' '{"type":"system","subtype":"status","status":"requesting"}'
      printf '%s\n' '{"type":"system","subtype":"thinking_tokens","count":1}'
      printf '%s\n' '{"type":"system","subtype":"commands_changed"}'
      printf '%s\n' '{"type":"system","subtype":"post_turn_summary"}'
      printf '%s\n' '{"type":"system","subtype":"task_summary"}'
      printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}}'
      printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":""}}}'
      printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"AAAA"}}}'
      printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"pon"}}}'
      printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"g"}}}'
      printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"file"}}}'
      printf '%s\n' '{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}'
      printf '%s\n' '{"type":"assistant","message":{"model":"fake-model","role":"assistant","content":[{"type":"thinking","thinking":"hmm","signature":"AAAA"},{"type":"text","text":"pong"},{"type":"tool_use","id":"toolu_ran","name":"Read","input":{"file_path":"a.txt"}},{"type":"tool_use","id":"toolu_denied","name":"Bash","input":{"command":"rm -rf /"}}]}}'
      printf '%s\n' '{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_ran","content":"hello\n<b>","is_error":false},{"type":"tool_result","tool_use_id":"toolu_denied","content":[{"type":"text","text":"Permission to use Bash has been denied."}],"is_error":true}]}}'
      denials=',"permission_denials":[{"tool_name":"Bash","tool_use_id":"toolu_denied","tool_input":{"command":"rm -rf /"}}]'
      if [ "${FAKE_CLAUDE_NO_DENIALS:-0}" = "1" ]; then
        denials=''
      fi
      emit_result '{"type":"result","subtype":"success","is_error":false,"session_id":"'"$SESSION"'","total_cost_usd":0.0125,"usage":{"input_tokens":10,"output_tokens":5}'"$denials"'}'
      ;;
    *)
      note "other:$line"
      ;;
  esac
done
note "eof"
exit 0
