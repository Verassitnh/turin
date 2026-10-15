# Hooks

Hooks are shell commands Turin runs automatically when orchestrator events
happen. They replace the old "launch thread into a new worktree" flow: you
can recreate it yourself, or run anything else — notifications, sync scripts,
CI kicks.

## Configuration

Two files, both optional:

- Global: `~/.turin/hooks.toml` (or `$TURIN_HOME/hooks.toml` when `TURIN_HOME`
  is set)
- Per project: `<project>/.turin/hooks.toml`

Global hooks run first, then project hooks, in file order. Files are watched
for changes and reloaded automatically; editing takes effect on the next
event, no restart needed.

```toml
[[hook]]
event = "thread.created"        # required
command = "./scripts/setup.sh"  # required, run as /bin/sh -c <command>
cwd = "project"                 # optional: "project" (default) or "thread"
blocking = false                # optional, default false
timeout_secs = 300              # optional, default 300
```

| Field        | Meaning |
|--------------|---------|
| `event`      | One of the events below. |
| `command`    | Shell snippet. Runs with `/bin/sh -c`, so pipes and `&&` work. |
| `cwd`        | `"project"`: the project's workspace root. `"thread"`: the thread's workspace path (its worktree); falls back to the project root when the event has none. |
| `blocking`   | `true` only for `thread.created`: the orchestrator waits for the hook before continuing. On any other event it is ignored (with a warning) and the hook runs in the background. |
| `timeout_secs` | After this long the hook is killed and reported as timed out. |

Invalid entries are skipped (the rest still run) and reported in the logs with
the file and hook index, e.g. `~/.turin/hooks.toml: hook[2]: unknown hook
event 'thread.create'`. Unknown fields are rejected, so typos fail loudly
instead of silently doing nothing.

## Events

| Event | Fires |
|---|---|
| `thread.created` | A thread is created. The only event where `blocking` works. |
| `turn.started` | A turn starts. |
| `turn.completed` | A turn finishes successfully. |
| `turn.failed` | A turn fails. |
| `approval.requested` | A turn is waiting for user approval. |
| `pr.linked` | A pull request is linked to the thread. |
| `pr.checks.completed` | Checks finish on a linked pull request. |
| `git.committed` | A commit is made from the app. |

## What the hook receives

- **stdin**: a JSON envelope describing the event:

  ```json
  {
    "event": "thread.created",
    "projectId": "prj_...",
    "threadId": "thr_...",
    "projectRoot": "/path/to/project",
    "cwd": "/path/the/hook/runs/in",
    "payload": { }
  }
  ```

  `threadId` is omitted when the event has no thread. `payload` carries the
  event-specific data. Read it with `jq` (`jq -r .payload.someField`), or
  ignore it — stdin is closed right after the write.

- **environment**: the full environment Turin runs with, plus:

  | Variable | Value |
  |---|---|
  | `TURIN_EVENT` | Event name, e.g. `turn.completed` |
  | `TURIN_PROJECT_ID` | Project id |
  | `TURIN_THREAD_ID` | Thread id (when the event has one) |
  | `TURIN_PROJECT_ROOT` | Project workspace root |
  | `TURIN_CWD` | Directory the hook runs in (after `cwd` resolution) |
  | `TURIN_WORKTREE_PATH` | Thread workspace path (when the event has one) |

  Since output is captured rather than displayed, `NO_COLOR=1` and
  `FORCE_COLOR=0` are also set — hooks should not emit ANSI escapes.

## Execution details

- Each hook runs in its own process group. On timeout or shutdown the whole
  group is killed, so background jobs or dev servers a hook started cannot
  outlive it.
- stdout and stderr are captured (1 MiB per stream; the rest is discarded and
  the result is flagged truncated). Results are shown in the app.
- A hook that exits non-zero is reported but never blocks the orchestrator.

## Examples

Create a worktree when a thread is created, and wait for setup before the
agent starts (project config, project root):

```toml
# <project>/.turin/hooks.toml
[[hook]]
event = "thread.created"
command = "./scripts/new-worktree.sh"
cwd = "project"
blocking = true
timeout_secs = 600
```

```sh
# scripts/new-worktree.sh — thread id and worktree path come from the env
git worktree add "../wt/$TURIN_THREAD_ID" -b "turin/$TURIN_THREAD_ID"
```

macOS notification when a turn finishes (global config):

```toml
# ~/.turin/hooks.toml
[[hook]]
event = "turn.completed"
command = "osascript -e 'display notification \"turn finished\" with title \"Turin\"'"
```

Push after every commit made from the app:

```toml
[[hook]]
event = "git.committed"
command = "git push --quiet"
cwd = "thread"
```
