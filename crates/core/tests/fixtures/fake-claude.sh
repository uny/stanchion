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
#   FAKE_CLAUDE_NOT_LOGGED_IN=1 answer every input as an unauthenticated CLI does
#                              (2.1.281: `error` beside the message); `=2.1.266` the
#                              same without it, as that version answered
#   FAKE_CLAUDE_CRASH_AFTER=N  die by SIGKILL after the N-th result line
#   FAKE_CLAUDE_EXIT_AFTER=N   exit 3 after the N-th result line
#   FAKE_CLAUDE_NO_DENIALS=1   omit `permission_denials` from the result line
#   FAKE_CLAUDE_STDERR=text    write `text` to stderr at startup
#   FAKE_CLAUDE_STATE=path     append one line per event to `path` (what it received)
#   FAKE_CLAUDE_HOLD=path      on the default turn, after the assistant line, wait until
#                              `path` exists before the tool results and the result line
#                              (a turn held open with its calls reported)
#   Either path may name a directory: the file is then `<path>/<account>`, where
#   `<account>` is the last component of CLAUDE_CONFIG_DIR, so two accounts run from
#   one backend (one environment) keep separate logs and are released separately.
#   FAKE_CLAUDE_TOOLS=path     prefix for the markers of the "tools" turn (below)
#   Inputs whose text is `background` or `background-open` end their own turn at once
#   and are followed, with no input, by a turn the fake starts itself (below).
#   FAKE_CLAUDE_ASK=1          on the default turn, ask about `toolu_denied` the way the
#                              real CLI does: spawn the helper named in --mcp-config, drive
#                              the MCP handshake and one tools/call, and act on the reply
#                              — allow runs it, deny lists it in permission_denials
#
# The session id is the placeholder AGENTS.md prescribes; `.github/scripts/hygiene.sh`
# rejects a UUID-shaped one.
set -u

# `$1` as given, or the per-account file inside it when it is a directory.
per_account() {
  if [ -d "$1" ]; then
    printf '%s/%s' "$1" "$(basename "${CLAUDE_CONFIG_DIR:-none}")"
  else
    printf '%s' "$1"
  fi
}
if [ -n "${FAKE_CLAUDE_STATE:-}" ]; then
  FAKE_CLAUDE_STATE=$(per_account "$FAKE_CLAUDE_STATE")
fi
if [ -n "${FAKE_CLAUDE_HOLD:-}" ]; then
  FAKE_CLAUDE_HOLD=$(per_account "$FAKE_CLAUDE_HOLD")
fi
if [ -n "${FAKE_CLAUDE_TOOLS:-}" ]; then
  FAKE_CLAUDE_TOOLS=$(per_account "$FAKE_CLAUDE_TOOLS")
fi

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

# The helper command and its one argument, from the --mcp-config JSON the core builds
# (`approval::mcp_config`; the shape is the core's own, so two fixed fields suffice).
helper_cmd=""
helper_arg=""
prev=""
for a in "$@"; do
  if [ "$prev" = "--mcp-config" ]; then
    helper_cmd=$(printf '%s' "$a" | sed -n 's/.*"command":"\([^"]*\)".*/\1/p')
    helper_arg=$(printf '%s' "$a" | sed -n 's/.*"args":\["\([^"]*\)"\].*/\1/p')
  fi
  prev="$a"
done

# Asks the helper about one call; prints the JSON text it returned.
ask_helper() {
  printf '%s\n%s\n%s\n' \
    '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fake-claude","version":"0"}}}' \
    '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
    '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"approve","arguments":{"tool_name":"'"$1"'","input":'"$2"',"tool_use_id":"'"$3"'"}}}' \
    | "$helper_cmd" "$helper_arg" | sed -n 's/.*"id":2,.*"text":"\(.*\)"}\]}}$/\1/p' | sed 's/\\"/"/g'
}

if [ -n "${FAKE_CLAUDE_STDERR:-}" ]; then
  printf '%s\n' "$FAKE_CLAUDE_STDERR" >&2
fi
note "config_dir=${CLAUDE_CONFIG_DIR:-}"
note "api_key=${ANTHROPIC_API_KEY:-unset}"
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
      if [ "${FAKE_CLAUDE_NOT_LOGGED_IN:-0}" != "0" ]; then
        if [ "$FAKE_CLAUDE_NOT_LOGGED_IN" = "2.1.266" ]; then
          printf '%s\n' '{"type":"assistant","message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"Not logged in · Please run /login"}]}}'
        else
          printf '%s\n' '{"type":"assistant","message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"Not logged in · Please run /login"}]},"session_id":"'"$SESSION"'","error":"authentication_failed","is_api_error_message":true}'
        fi
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
        *'"text":"tools"'*)
          # A turn whose call is running, as the real CLI runs a Bash call (measured on
          # 2.1.280): a command in a session of its own, which a kill of this process or
          # of its group does not reach. And one child left in this process's group. Each
          # writes `<prefix>.<name>.started` at once and `<prefix>.<name>` three seconds
          # later, unless killed first; neither holds this process's pipes.
          tools="${FAKE_CLAUDE_TOOLS:?}"
          perl -MPOSIX -e 'POSIX::setsid() or die; open(F, ">", "$ARGV[0].started"); close F; sleep 3; open(F, ">", $ARGV[0]); close F' \
            "$tools.session" </dev/null >/dev/null 2>&1 &
          ( : > "$tools.group.started"; sleep 3; : > "$tools.group" ) </dev/null >/dev/null 2>&1 &
          printf '%s\n' '{"type":"assistant","message":{"model":"fake-model","role":"assistant","content":[{"type":"tool_use","id":"toolu_running","name":"Bash","input":{"command":"sleep 3 && touch marker"}}]}}'
          continue
          ;;
        *'"text":"background"'* | *'"text":"background-open"'*)
          # A turn that leaves a task running in the background and ends; then, with no
          # input, the turn the real CLI (2.1.280) starts of its own when that task
          # finishes (#63): `init` again, the model's reply and its calls, a `result`.
          # `background-open` leaves that turn open, a call in flight, until interrupted
          # or killed. `background` holds it, as the default turn, on FAKE_CLAUDE_HOLD.
          printf '%s\n' '{"type":"assistant","message":{"model":"fake-model","role":"assistant","content":[{"type":"text","text":"started"}]}}'
          emit_result '{"type":"result","subtype":"success","is_error":false,"session_id":"'"$SESSION"'","total_cost_usd":0.001,"usage":{"input_tokens":2,"output_tokens":1},"permission_denials":[]}'
          init_done=0
          emit_init
          note "unprompted"
          printf '%s\n' '{"type":"system","subtype":"status","status":"requesting"}'
          case "$line" in
            *'"text":"background-open"'*)
              printf '%s\n' '{"type":"assistant","message":{"model":"fake-model","role":"assistant","content":[{"type":"tool_use","id":"toolu_bg_cut","name":"Bash","input":{"command":"sleep 600"}}]}}'
              continue
              ;;
          esac
          printf '%s\n' '{"type":"assistant","message":{"model":"fake-model","role":"assistant","content":[{"type":"text","text":"background done"},{"type":"tool_use","id":"toolu_bg_ran","name":"Read","input":{"file_path":"out.txt"}},{"type":"tool_use","id":"toolu_bg_asked","name":"Bash","input":{"command":"echo after"}}]}}'
          if [ -n "${FAKE_CLAUDE_HOLD:-}" ]; then
            i=0
            while [ ! -e "$FAKE_CLAUDE_HOLD" ] && [ "$i" -lt 500 ]; do
              sleep 0.02
              i=$((i + 1))
            done
          fi
          printf '%s\n' '{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_bg_ran","content":"done","is_error":false},{"type":"tool_result","tool_use_id":"toolu_bg_asked","content":"after","is_error":false}]}}'
          emit_result '{"type":"result","subtype":"success","is_error":false,"session_id":"'"$SESSION"'","total_cost_usd":0.002,"usage":{"input_tokens":4,"output_tokens":3},"permission_denials":[]}'
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
      if [ -n "${FAKE_CLAUDE_HOLD:-}" ]; then
        i=0
        while [ ! -e "$FAKE_CLAUDE_HOLD" ] && [ "$i" -lt 500 ]; do
          sleep 0.02
          i=$((i + 1))
        done
      fi
      denied_result='{"type":"tool_result","tool_use_id":"toolu_denied","content":[{"type":"text","text":"Permission to use Bash has been denied."}],"is_error":true}'
      denied_denial='{"tool_name":"Bash","tool_use_id":"toolu_denied","tool_input":{"command":"rm -rf /"}}'
      if [ "${FAKE_CLAUDE_ASK:-0}" = "1" ]; then
        # As the real CLI: `Read` its own rules allow runs unasked; the `Bash` call is
        # asked, with the call's own id and input, and the reply decides its fate.
        reply=$(ask_helper Bash '{"command":"rm -rf /"}' toolu_denied)
        note "reply:$reply"
        case "$reply" in
          *'"behavior":"allow"'*)
            denied_result='{"type":"tool_result","tool_use_id":"toolu_denied","content":"(ran)","is_error":false}'
            denied_denial=''
            ;;
          *)
            msg=$(printf '%s' "$reply" | sed -n 's/.*"message":"\([^"]*\)".*/\1/p')
            denied_result='{"type":"tool_result","tool_use_id":"toolu_denied","content":"'"$msg"'","is_error":true}'
            ;;
        esac
      fi
      printf '%s\n' '{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_ran","content":"hello\n<b>","is_error":false},'"$denied_result"']}}'
      denials=',"permission_denials":['"$denied_denial"']'
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
