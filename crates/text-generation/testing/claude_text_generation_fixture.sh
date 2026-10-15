# Port of upstream/apps/server/src/textGeneration/testing/ClaudeTextGeneration.fixture.mjs.
#
# Fake `claude` for the Claude text generation tests. The test writes the expected values
# above this line as shell assignments (T3_FAKE_CLAUDE_*), then appends this file. Each
# failed check exits with the code upstream uses, so a failing test names the assertion.

ARGS="$*"

permission_ok=0
tools_ok=0
has_dangerous=0
has_disable_slash=0
has_strict_mcp=0
settings_value=""
prev=""
for arg in "$@"; do
  case "$prev" in
    --permission-mode) if [ "$arg" = "dontAsk" ]; then permission_ok=1; fi ;;
    --tools) if [ -z "$arg" ]; then tools_ok=1; fi ;;
    --settings) settings_value="$arg" ;;
  esac
  case "$arg" in
    --dangerously-skip-permissions) has_dangerous=1 ;;
    --disable-slash-commands) has_disable_slash=1 ;;
    --strict-mcp-config) has_strict_mcp=1 ;;
  esac
  prev="$arg"
done

if [ "$permission_ok" != 1 ]; then
  echo "text generation must deny permission prompts" >&2
  exit 12
fi
if [ "$tools_ok" != 1 ]; then
  echo "text generation must receive an explicit empty tool set" >&2
  exit 6
fi
if [ "$has_dangerous" = 1 ]; then
  echo "text generation must not bypass permissions" >&2
  exit 7
fi
if [ "$has_disable_slash" != 1 ]; then
  echo "text generation must disable skills" >&2
  exit 8
fi
if [ "$has_strict_mcp" != 1 ]; then
  echo "text generation must not load configured MCP servers" >&2
  exit 9
fi
case "$settings_value" in
  *'"disableAllHooks":true'*) ;;
  *) echo "text generation must disable hooks" >&2; exit 10 ;;
esac

if [ -n "$T3_FAKE_CLAUDE_CWD_MUST_NOT_BE" ]; then
  project_dir=$(cd "$T3_FAKE_CLAUDE_CWD_MUST_NOT_BE" 2>/dev/null && pwd -P)
  if [ -n "$project_dir" ] && [ "$(pwd -P)" = "$project_dir" ]; then
    echo "text generation ran in the project directory" >&2
    exit 11
  fi
fi

stdin_content=$(cat)

if [ -n "$T3_FAKE_CLAUDE_ARGS_MUST_CONTAIN" ]; then
  case "$ARGS" in
    *"$T3_FAKE_CLAUDE_ARGS_MUST_CONTAIN"*) ;;
    *) echo "args missing expected content" >&2; exit 2 ;;
  esac
fi
if [ -n "$T3_FAKE_CLAUDE_ARGS_MUST_NOT_CONTAIN" ]; then
  case "$ARGS" in
    *"$T3_FAKE_CLAUDE_ARGS_MUST_NOT_CONTAIN"*) echo "args contained forbidden content" >&2; exit 3 ;;
  esac
fi
if [ -n "$T3_FAKE_CLAUDE_STDIN_MUST_CONTAIN" ]; then
  case "$stdin_content" in
    *"$T3_FAKE_CLAUDE_STDIN_MUST_CONTAIN"*) ;;
    *) echo "stdin missing expected content" >&2; exit 4 ;;
  esac
fi
if [ -n "$T3_FAKE_CLAUDE_CONFIG_DIR_MUST_BE" ] && [ "$CLAUDE_CONFIG_DIR" != "$T3_FAKE_CLAUDE_CONFIG_DIR_MUST_BE" ]; then
  echo "CLAUDE_CONFIG_DIR was ${CLAUDE_CONFIG_DIR:-}" >&2
  exit 5
fi

if [ -n "$T3_FAKE_CLAUDE_STDERR" ]; then
  printf '%s\n' "$T3_FAKE_CLAUDE_STDERR" >&2
fi
printf '%s' "$T3_FAKE_CLAUDE_OUTPUT"
exit "${T3_FAKE_CLAUDE_EXIT_CODE:-0}"
