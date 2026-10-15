# Hooks

Hooks are shell commands Turin runs automatically when orchestrator events
happen: notifications, sync scripts, CI kicks. A blocking `thread.created`
hook can also hand a worktree back to Turin (see
[Return protocol](#return-protocol-thread-workspace)), which is how hooks
replace the old "launch thread into a new worktree" flow.

## Configuration

Two files, both optional:

- Global: `~/.turin/hooks.toml` (or `$TURIN_HOME/hooks.toml` when `TURIN_HOME`
  is set)
- Per project: `<project>/.turin/hooks.toml`

Global hooks run first, then project hooks, in file order. Files are watched
for changes and reloaded automatically — including when `.turin/` or the
file is created, replaced or deleted later; editing takes effect on the next
event, no restart needed (project hooks must be approved first, see
[Trust](#trust-project-hooks-must-be-approved)).

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

## Trust: project hooks must be approved

A repository you clone can ship a `<project>/.turin/` directory, and an agent
or a `git pull` can rewrite it, so project hooks **never run until you approve
them**. Your own `~/.turin/hooks.toml` is trusted.

**What approval covers: everything under `<project>/.turin/`.** That is
`hooks.toml` and every other file in that directory tree — scripts such as
`.turin/hooks/new-worktree.sh`, helpers, dotfiles; nothing is skipped. The
approval is one SHA-256 over the sorted relative paths and the exact bytes of
all of them. Adding, removing, renaming or editing any one file (a one-byte
change, a `git pull`, a branch switch, an agent edit) changes the hash and puts
the project hooks back to pending.

- Approvals are stored outside the repo, in `~/.turin/trusted-hooks.json`
  (`$TURIN_HOME/trusted-hooks.json` when set): canonical project root → that
  hash.
- Until the hash matches, the project's hooks are inert. Turin shows them with
  an **Approve** action instead (a `projectHooksPending` update on the results
  channel; reason `untrusted` for a new config, `changed` when something under
  `.turin/` changed after approval, `untrustable` when it cannot be approved).
- The hash is re-checked on every reload **and again at every dispatch**, so an
  edit is caught even before the file watcher notices. Your global hooks keep
  working meanwhile.
- Symlinks under `.turin/` must resolve inside `.turin/`. A link pointing
  outside it (or dangling) makes the project hooks **untrustable**: they stay
  inert, the notice says which link, and Approve is refused until it is fixed.
  A `.turin/` that itself resolves outside the project is refused the same
  way. Trees with more than 4096 files or 32 MiB are also refused.
- Review what you approve: the pending notice lists each hook's event and
  command. Approving records the hash you were shown, and is refused if
  anything changed since.

**What approval does not cover: repo files outside `.turin/`.** A command like
`./scripts/setup.sh` or `sh build.sh` runs a file that is not hashed, so a
`git pull` or an agent edit that changes only that file runs new code without
re-approval. Turin still allows such commands, but the pending notice lists
them as `unhashedReferences` so the UI can warn you (best-effort: it scans the
command text for paths, it cannot see paths built at runtime).

- **Keep the scripts your hooks run in `.turin/hooks/`**, where they are
  covered, and call them by that path (`sh .turin/hooks/setup.sh`).
- With `cwd = "thread"`, a relative path resolves inside the thread's
  worktree, which the agent can write and approval does not cover. Reference
  your approved script through the project root instead:
  `sh "$TURIN_PROJECT_ROOT/.turin/hooks/setup.sh"`.
- Or put the logic inline in `command`, or in your global
  `~/.turin/hooks.toml` with scripts outside the repo.

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

- **environment**: the environment Turin runs with (login-shell `PATH` is
  set up once when the app starts, not per hook), minus any inherited
  `TURIN_*` variables, plus:

  | Variable | Value |
  |---|---|
  | `TURIN_EVENT` | Event name, e.g. `turn.completed` |
  | `TURIN_PROJECT_ID` | Project id |
  | `TURIN_THREAD_ID` | Thread id (when the event has one) |
  | `TURIN_PROJECT_ROOT` | Project workspace root |
  | `TURIN_CWD` | Directory the hook runs in (after `cwd` resolution) |
  | `TURIN_WORKTREE_PATH` | Thread workspace path (when the event has one) |

  `TURIN_WORKTREE_PATH` is only set when the thread already has a workspace;
  a `thread.created` hook creates one with the
  [return protocol](#return-protocol-thread-workspace). Since output is
  captured rather than displayed, `NO_COLOR=1` and
  `FORCE_COLOR=0` are also set — hooks should not emit ANSI escapes.

## Return protocol (thread workspace)

A **blocking `thread.created`** hook can tell Turin which workspace the new
thread should use. Print a JSON object as the **last non-blank line of
stdout**:

```json
{"worktreePath": "/abs/path/to/worktree", "branch": "turin/thr_abc"}
```

- Both fields are optional. `worktreePath` must be an existing directory; a
  relative path is resolved against the directory the hook ran in.
- Only honored when the hook exits 0 within its timeout and its output was not
  truncated. Malformed, missing, or earlier-line JSON means "no change" — it
  is never an error.
- Turin binds the thread to that workspace (the agent runs there). Hooks that
  run later in the same dispatch see it too: `cwd = "thread"` and
  `TURIN_WORKTREE_PATH` point at it.
- If several blocking hooks report, later ones override earlier ones field by
  field.
- Print everything else to stderr (`>&2`) or earlier on stdout. Keep the
  JSON on one line, and make sure the path needs no JSON escaping (no `"` or
  `\`).

## Execution details

- Each hook runs in its own process group. On timeout or shutdown the whole
  group is killed (even after the shell itself has already exited).
- A hook finishes when its shell exits, plus a short (250 ms) drain of the
  output it already wrote. If anything still holds the hook's stdout/stderr
  after that — `sleep 600 &` — the **entire process group is killed**,
  including jobs that redirected their own output away. A job that redirects
  its output (`nohup server >/dev/null 2>&1 &`) survives only when nothing
  else in the group holds the pipes, so do not rely on that. To leave a daemon
  running reliably, start it in its own session so it leaves the group:
  `setsid server >/dev/null 2>&1 &`. macOS ships no `setsid` binary; use
  `perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV' server >/dev/null 2>&1 &`.
- stdout and stderr are captured (1 MiB per stream; the rest is discarded and
  the result is flagged truncated). Results are shown in the app.
- A hook that exits non-zero is reported but never blocks the orchestrator.

## Examples

Create a worktree when a thread is created and move the thread into it
(project config; approve it once in the app). The script lives in
`.turin/hooks/`, so the approval covers it:

```toml
# <project>/.turin/hooks.toml
[[hook]]
event = "thread.created"
command = "sh .turin/hooks/new-worktree.sh"
cwd = "project"
blocking = true
timeout_secs = 600
```

```sh
#!/bin/sh
# <project>/.turin/hooks/new-worktree.sh
set -eu
branch="turin/$TURIN_THREAD_ID"
path="$TURIN_PROJECT_ROOT/../wt/$TURIN_THREAD_ID"
mkdir -p "$(dirname "$path")"
git worktree add -q "$path" -b "$branch" >&2
path=$(cd "$path" && pwd)
# Last stdout line: tells Turin where the thread's workspace is.
printf '{"worktreePath":"%s","branch":"%s"}\n' "$path" "$branch"
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
