export const meta = {
  name: 'turin-full-qc3-headless',
  description: 'Headless full QC (user is using the Mac: no app windows, no synthetic input, no screenshots): area testers + stub audit via tests, e2e with fake providers and code tracing; Haiku skeptics; Sonnet fixers; headless retest',
  phases: [
    { title: 'Build', detail: 'build the app once' },
    { title: 'Test', detail: 'headless area testers + user-reachable stub audit' },
    { title: 'Verify', detail: 'Haiku skeptic per bug' },
    { title: 'Fix', detail: 'Sonnet fixers per area' },
    { title: 'Retest', detail: 'headless retest + final gate' },
  ],
}

const A = args || {}
const WT = A.worktree
const RULES = `Turin is a faithful native Rust/GPUI port of T3 Code (desktop app for coding agents: projects, threads, chat with providers Claude/Codex/Cursor/OpenCode, composer, git/PR tools, settings). Worktree: ${WT}. Upstream (read-only): ${WT}/upstream. Read ${WT}/AGENTS.md, ${WT}/PROGRESS.md and skim ${WT}/PORTING.md (cut list + Deviations: a logged cut is not a bug).
STRICT USER RULE: the user is working on this Mac. NEVER launch the GUI app (target/debug/turin, Turin.app), NEVER take screenshots (scripts/shot.sh, screencapture), NEVER send synthetic mouse/keyboard input (CGEvent, osascript, System Events, cliclick) or change window focus. Test headlessly only: cargo unit tests, the app_runtime e2e tests with fake providers (crates/app/tests/fixtures/fake_{claude,codex,cursor_agent,opencode}.py; see docs/handoff/T22.md), and careful code tracing from each UI control's handler through the runtime/orchestrator/provider to the effect, comparing with upstream. Use temp dirs only; never the real ~/.t3, ~/.turin or credentials.
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
await agent(`${RULES}\n\nBuild agent: in ${WT} run \`cargo build -p turin\` and wait. If it fails, fix minimally and commit (trailer \`Model: claude-sonnet-5-5 (claude)\`).`,
  { label: 'build', phase: 'Build', model: 'sonnet', effort: 'medium' })

const KNOWN = (A.known || []).map(k => ({ ...k, source: 'user' }))
const stubAudit = agent(`${RULES}

You are the STUB AUDITOR. Users hit errors like "restart is not wired yet". Enumerate EVERY user-reachable path that ends in a stub/placeholder instead of real behaviour: grep crates/ for 'not wired', 'not available yet', 'not_wired(', 'unimplemented!', 'todo!', 'Task::ready(Err', 'Err("' returning placeholder text, default trait methods that return Err/None for live seams (e.g. orchestrator services.rs defaults: checkpoint scopes, provider switching, project roots, workspace claims, git workflow), demo-only paths, and UI controls (buttons, menu items, palette commands, slash commands, shortcuts) whose handler does nothing. For each: is it reachable from the live app UI (trace from the control), and is the live composition root overriding the default? Report each reachable stub as a bug (severity high if a visible control errors or does nothing) with the upstream behaviour to implement. Do NOT edit code.`,
  { label: 'test:stub-audit', phase: 'Test', schema: BUGS, model: 'sonnet', effort: 'high' })

const headless = parallel(AREAS.map((a, i) => () => agent(`${RULES}

You are a headless tester for: ${a}. Run the relevant tests (cargo test -p <crate> <filter>, the e2e fake-provider tests: cargo test -p turin write::e2e), and trace every user flow in code from control to handler to runtime: wired? feedback? error handling? matches upstream? persists? Report bugs that make the app hard to use with exact repro (test/command or code path) and file:line; list what passed. Do NOT edit code.`,
  { label: `test:headless#${i}`, phase: 'Test', schema: BUGS, model: 'sonnet', effort: 'medium' })))

const [g, hs] = await Promise.all([stubAudit, headless])
const reports = [g, ...hs].filter(Boolean)
const raw = KNOWN.concat(reports.flatMap((r, i) => (r.bugs || []).map(b => ({ ...b, source: i === 0 ? 'stub-audit' : `headless#${i - 1}` }))))
// cheap de-dup by normalised title + file
const seen = new Set(), bugs = []
for (const b of raw) { const k = (b.file + '|' + b.title.toLowerCase().replace(/[^a-z0-9]+/g, ' ').slice(0, 60)); if (!seen.has(k)) { seen.add(k); bugs.push(b) } }

const judged = await parallel(bugs.map((b, i) => () => agent(`${RULES}

Skeptic: a tester reports:
${JSON.stringify(b, null, 2)}
Try to reproduce it headlessly (run tests/commands, read code; never launch the GUI app or take screenshots). refuted=true if it doesn't reproduce, is a logged cut/deviation, is intended upstream behaviour, or is not a real usability problem. Do not edit anything.`,
  { label: `verify#${i}`, phase: 'Verify', schema: VERDICT, model: 'haiku', effort: 'medium' })
  .then(v => ({ ...b, upheld: b.source === 'user' ? true : (v ? !v.refuted : true) }))))
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

You are the headless retester. At ${WT} HEAD, for every bug below re-check headlessly (run its regression test / the e2e tests / re-trace the code path) and mark fixed / not-fixed / regressed; also report any NEW bug you notice (newBugs). Then run cargo check --workspace --all-targets, cargo test --workspace, cargo clippy --workspace --all-targets -- -D warnings, cargo fmt --all --check and report gateGreen. Do not edit code.
Bugs:
${JSON.stringify(upheld.map(b => ({ title: b.title, severity: b.severity, repro: b.repro })), null, 1)}`,
  { label: 'retest:headless', phase: 'Retest', schema: RETEST, model: 'sonnet', effort: 'high' })

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
