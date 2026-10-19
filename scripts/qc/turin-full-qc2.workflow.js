export const meta = {
  name: 'turin-full-qc2',
  description: 'QC round 2: one GUI walker drives the real app through every feature (no shared-mouse races) + parallel headless testers; Haiku skeptics; Sonnet fixers; retest with the GUI walker',
  phases: [
    { title: 'Build', detail: 'build the app once' },
    { title: 'Test', detail: 'GUI walker (serial, real clicks) + headless testers (tests, logs, code trace)' },
    { title: 'Verify', detail: 'Haiku skeptic per bug' },
    { title: 'Fix', detail: 'Sonnet fixers per area' },
    { title: 'Retest', detail: 'GUI walker re-runs repros; final gate' },
  ],
}

const A = args || {}
const WT = A.worktree
const RULES = `Turin is a faithful native Rust/GPUI port of T3 Code (desktop app for coding agents: projects, threads, chat with providers Claude/Codex/Cursor/OpenCode, composer, git/PR tools, settings). Worktree: ${WT}. Upstream (read-only): ${WT}/upstream. Read ${WT}/AGENTS.md, ${WT}/PROGRESS.md and skim ${WT}/PORTING.md (cut list + Deviations: a logged cut is not a bug).
Exercising the app: binary ${WT}/target/debug/turin; ALWAYS TURIN_DATA_DIR=$(mktemp -d) and HOME=$(mktemp -d) (never the real ~/.t3, ~/.turin or credentials); RUST_BACKTRACE=1 with stdout/stderr captured. Fake providers (no network, no paid turns): crates/app/tests/fixtures/fake_{claude,codex,cursor_agent,opencode}.py; docs/handoff/T22.md explains how settings.json points providers at them. Demo switches: grep crates/app/src for 'TURIN_'. Screenshots: scripts/shot.sh. Panics, error logs, hangs (>20s), blank windows, dead controls, lost settings/data = bugs. Kill only PIDs you started.
Builds are throttled (shared rustc slots; waiting is normal); one cargo command at a time; never rm -rf target; never pattern-kill; never search the whole disk; never use Codex or Cursor as agents; never spawn sub-agents.`

const AREAS = [
  'projects and threads (add/open project, new thread incl. without a project, rename, pin, archive/unarchive, delete, fork, merge-back, snooze, search, sidebar groups/favicons/manual order)',
  'messaging (send, stream, Stop, steer, queue controls, retry, restart recovery, resume, revert, load earlier, titles, large paste)',
  'approvals/questions/plans/subagents (approve/decline/always, answer/dismiss questions, plan accept/refine/implement in new thread, subagent bar/stop)',
  'providers (status, models, settings cards, sign-in UI without real creds, switch provider/model mid-thread, usage limits, update notices, missing binaries)',
  'composer (mentions/@path search, skills, slash commands, code blocks, attachments, pickers, modes, stash persistence, drafts across threads, shortcuts)',
  'chat view (all timeline item types, markdown/code/citations/images, find bar incl. earlier history, thread links, copy, scroll anchoring, details/lineage panel, Open in editor, PR tab, banners)',
  'git/PR/diff (branch toolbar, commit/push/publish on a temp repo, worktrees, checkpoints/rollback, diff panel, PR list/detail/actions, create PR links to thread)',
  'settings (every panel loads/edits/persists across relaunch, themes incl. theme editor/import/search, fonts, keybindings, scoped settings, provider instances, icon picker Open in Finder)',
  'onboarding/usage/palette (first-run wizard incl. import landing, recovery and DB-failure dialog, usage page, every palette action)',
  'shell/platform (windows, menus incl. About/Minimize/Zoom, shortcuts, hold-to-quit from every window, bounds persistence, appearance, notifications, dock badge, startup, corrupt settings, stress fixture)',
]

const BUGS = {
  type: 'object',
  properties: {
    tested: { type: 'array', items: { type: 'string' } },
    bugs: { type: 'array', items: { type: 'object', properties: {
      title: { type: 'string' },
      severity: { type: 'string', enum: ['blocker', 'high', 'medium', 'low'] },
      repro: { type: 'string' }, file: { type: 'string' }, line: { type: 'integer' }, fix: { type: 'string' } },
      required: ['title', 'severity', 'repro', 'file', 'fix'] } },
    untestable: { type: 'array', items: { type: 'string' } },
  },
  required: ['tested', 'bugs', 'untestable'],
}
const VERDICT = { type: 'object', properties: { refuted: { type: 'boolean' }, reason: { type: 'string' } }, required: ['refuted', 'reason'] }
const FIXREP = { type: 'object', properties: { fixed: { type: 'array', items: { type: 'string' } }, notFixed: { type: 'array', items: { type: 'string' } }, gateGreen: { type: 'boolean' }, lastCommit: { type: 'string' } }, required: ['fixed', 'notFixed', 'gateGreen', 'lastCommit'] }
const RETEST = { type: 'object', properties: { results: { type: 'array', items: { type: 'object', properties: { title: { type: 'string' }, status: { type: 'string', enum: ['fixed', 'not-fixed', 'regressed'] }, note: { type: 'string' } }, required: ['title', 'status'] } }, newBugs: BUGS.properties.bugs, gateGreen: { type: 'boolean' } }, required: ['results', 'gateGreen'] }

phase('Build')
await agent(`${RULES}\n\nBuild agent: in ${WT} run \`cargo build -p turin\` and wait. If it fails, fix minimally and commit (trailer \`Model: claude-sonnet-5-5 (claude)\`). Report whether the screen is locked (ioreg -n Root -d1 -a | grep -A1 CGSSessionScreenIsLocked).`,
  { label: 'build', phase: 'Build', model: 'sonnet', effort: 'medium' })

// The GUI walker is the ONLY agent allowed synthetic mouse/keyboard input, so runs never race.
const gui = agent(`${RULES}

You are the GUI walker: the ONLY agent allowed to send synthetic mouse/keyboard input or bring windows to front. Launch ONE app instance at a time (fresh data dir, fake providers configured) and drive it like a user through EVERY area below, clicking real controls (use CGEvent/osascript helpers; the earlier QC left helpers in /tmp/qc-*/ — read before reuse; post events to your own pid). After each action take a screenshot with scripts/shot.sh, Read it, and check the log. Areas:
${AREAS.map((a, i) => `${i + 1}. ${a}`).join('\n')}
Report every bug that makes the app hard to use, with exact click/key repro and screenshot paths; list what you verified working. Do NOT edit code.`,
  { label: 'test:gui-walker', phase: 'Test', schema: BUGS, model: 'sonnet', effort: 'high' })

const headless = parallel(AREAS.map((a, i) => () => agent(`${RULES}

You are a headless tester for: ${a}. You must NOT send synthetic mouse/keyboard input or focus windows (a separate GUI walker owns the screen); you MAY launch the app headless-style to read logs/panics and use demo switches + scripts/shot.sh screenshots. Run the relevant tests (cargo test -p <crate> <filter>, the e2e fake-provider tests), and trace every user flow in code from control to handler to runtime: wired? feedback? error handling? matches upstream? Report bugs that make the app hard to use with exact repro and file:line; list what passed. Do NOT edit code.`,
  { label: `test:headless#${i}`, phase: 'Test', schema: BUGS, model: 'sonnet', effort: 'medium' })))

const [g, hs] = await Promise.all([gui, headless])
const reports = [g, ...hs].filter(Boolean)
const raw = reports.flatMap((r, i) => (r.bugs || []).map(b => ({ ...b, source: i === 0 ? 'gui' : `headless#${i - 1}` })))
// cheap de-dup by normalised title + file
const seen = new Set(), bugs = []
for (const b of raw) { const k = (b.file + '|' + b.title.toLowerCase().replace(/[^a-z0-9]+/g, ' ').slice(0, 60)); if (!seen.has(k)) { seen.add(k); bugs.push(b) } }

const judged = await parallel(bugs.map((b, i) => () => agent(`${RULES}

Skeptic: a tester reports:
${JSON.stringify(b, null, 2)}
Try to reproduce it without synthetic input (run tests/commands, read code, launch with demo switches, inspect logs/screenshots the tester saved). refuted=true if it doesn't reproduce, is a logged cut/deviation, is intended upstream behaviour, or is not a real usability problem. Do not edit anything.`,
  { label: `verify#${i}`, phase: 'Verify', schema: VERDICT, model: 'haiku', effort: 'medium' })
  .then(v => ({ ...b, upheld: v ? !v.refuted : true }))))
const upheld = judged.filter(Boolean).filter(b => b.upheld)
const rank = { blocker: 0, high: 1, medium: 2, low: 3 }
upheld.sort((x, y) => rank[x.severity] - rank[y.severity])
log(`${raw.length} reports, ${bugs.length} unique, ${upheld.length} reproduced`)

phase('Fix')
const fixes = []
for (let i = 0; i < upheld.length; i += 10) {
  const batch = upheld.slice(i, i + 10)
  const r = await agent(`${RULES}

Fixer. Work ONLY in ${WT} (git status/log first). Fix each bug (each reproduced by a skeptic) to match upstream; add a regression test per logic bug (unit or e2e with fake providers). Gate: cargo check --workspace --all-targets, cargo test -p turin (+ touched crates), cargo clippy --workspace --all-targets -- -D warnings, cargo fmt --all --check — all green. Commit per bug (trailer \`Model: claude-sonnet-5-5 (claude)\`), push (git push -q -u origin HEAD).
Bugs:
${JSON.stringify(batch, null, 2)}`,
    { label: `fix#${i / 10}`, phase: 'Fix', schema: FIXREP, model: batch.every(b => b.severity === 'low') ? 'haiku' : 'sonnet', effort: 'high' })
  if (r) fixes.push(r)
}

phase('Retest')
const retest = await agent(`${RULES}

You are the GUI walker again (the only agent using synthetic input now). Rebuild (cargo build -p turin) at ${WT} HEAD. Re-run the repro of every bug below in the running app and mark fixed / not-fixed / regressed; also report any NEW bug you hit (newBugs). Then run cargo check --workspace --all-targets, cargo test --workspace, cargo clippy --workspace --all-targets -- -D warnings, cargo fmt --all --check and report gateGreen. Do not edit code.
Bugs:
${JSON.stringify(upheld.map(b => ({ title: b.title, severity: b.severity, repro: b.repro })), null, 1)}`,
  { label: 'retest:gui-walker', phase: 'Retest', schema: RETEST, model: 'sonnet', effort: 'high' })

return {
  verifiedWorking: reports.flatMap(r => r.tested || []).length,
  untestable: reports.flatMap(r => r.untestable || []),
  upheld: upheld.map(b => `${b.severity} ${b.title} (${b.file}${b.line ? ':' + b.line : ''})`),
  fixed: fixes.flatMap(f => f.fixed || []),
  notFixed: fixes.flatMap(f => f.notFixed || []),
  retestOpen: retest ? retest.results.filter(r => r.status !== 'fixed') : null,
  newBugs: retest?.newBugs || [],
  gateGreen: retest?.gateGreen,
  lastCommit: fixes.at(-1)?.lastCommit,
}
