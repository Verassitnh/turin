export const meta = {
  name: 'turin-reqc',
  description: 'Re-QC a Turin branch after a fix round: verify every prior finding is fixed (or validly logged), review code added since the QC with skeptics, issue a verdict',
  whenToUse: 'After an agent reports a QC fix round, before merging',
  phases: [
    { title: 'Verify fixes', detail: 'verifiers each take a slice of the prior findings' },
    { title: 'New code', detail: 'review code added since the QC commit; skeptic per finding' },
    { title: 'Verdict', detail: 'lead ranks what is left and decides PASS/FAIL' },
  ],
}

const A = args || {}
const WT = A.worktree
const REPORT = A.reportPath          // prior QC report (markdown with "Confirmed findings (raw)")
const COUNT = A.findingCount || 40   // number of prior findings (upper bound is fine)
const SINCE = A.since                // commit the prior QC reviewed; new code = SINCE..HEAD
const SCOPE = A.scope || ''
const UPSTREAM = A.upstream || ''
const CONTEXT = A.context || ''
const M = Object.assign({ verify: 'fable', review: 'fable', skeptic: 'sonnet', lead: 'fable' }, A.models || {})
const E = Object.assign({ verify: 'low', review: 'low', skeptic: 'medium', lead: 'low' }, A.efforts || {})
const SLICE = A.slice || 8

const BASE = `You are a QC verifier for Turin, a faithful Rust/GPUI port of T3 Code (TypeScript/Effect).
Checkout (read-only: do NOT edit or commit): ${WT}. Read ${WT}/PORTING.md and ${WT}/AGENTS.md first. Upstream (read-only): ${WT}/upstream.
Scope: ${SCOPE}
Upstream counterpart: ${UPSTREAM}
${CONTEXT}
You may run read-only commands: cargo build/test/clippy for the touched crates (PATH=$HOME/.cargo/bin:$PATH DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer). Builds are throttled (shared rustc slots; waiting is normal); one cargo command at a time; never rm -rf target; never pattern-kill; never search the whole disk; never copy user databases; never use Codex or Cursor; never spawn sub-agents.`

const VERIFY = {
  type: 'object',
  properties: { results: { type: 'array', items: { type: 'object', properties: {
    n: { type: 'integer' }, title: { type: 'string' },
    status: { type: 'string', enum: ['fixed', 'deviation-ok', 'not-fixed', 'regressed', 'not-applicable'] },
    evidence: { type: 'string', description: 'file:line of the fix and the test that covers it (or why the deviation row is a valid reason)' } },
    required: ['n', 'title', 'status', 'evidence'] } } },
  required: ['results'],
}
const FINDINGS = {
  type: 'object',
  properties: { findings: { type: 'array', items: { type: 'object', properties: {
    title: { type: 'string' }, file: { type: 'string' }, line: { type: 'integer' },
    severity: { type: 'string', enum: ['critical', 'high', 'medium', 'low'] },
    evidence: { type: 'string' }, fix: { type: 'string' } }, required: ['title', 'file', 'severity', 'evidence', 'fix'] } },
    gateGreen: { type: 'boolean' } },
  required: ['findings', 'gateGreen'],
}
const VERDICT = { type: 'object', properties: { refuted: { type: 'boolean' }, reason: { type: 'string' } }, required: ['refuted', 'reason'] }

phase('Verify fixes')
const slices = []
for (let i = 1; i <= COUNT; i += SLICE) slices.push([i, Math.min(COUNT, i + SLICE - 1)])
const newCode = SINCE ? (async () => {
  const rev = await agent(`${BASE}

You review only code ADDED OR CHANGED since commit ${SINCE} (git diff ${SINCE}..HEAD) — i.e. the fix round and any new ports. Compare each changed module against its upstream file and find: regressions introduced by the fixes, new dropped logic, wrong semantics, panics, race/ordering bugs, missing or weakened tests, unlogged deviations. Run the gate for the touched crates (scripts/check.sh -p <crate>) and report gateGreen. Only concrete, evidenced problems with file:line.`,
    { label: 'review:new-code', phase: 'New code', schema: FINDINGS, model: M.review, effort: E.review })
  const judged = await parallel((rev?.findings || []).map((f, i) => () => agent(`${BASE}

You are a skeptic. A reviewer claims:
${JSON.stringify(f, null, 2)}
Try hard to REFUTE it: re-read the cited code and upstream, run checks. refuted=true if wrong, already handled, logged in Deviations, or not a real problem.`,
    { label: `skeptic:new#${i}`, phase: 'New code', schema: VERDICT, model: M.skeptic, effort: E.skeptic })
    .then(v => ({ ...f, upheld: v ? !v.refuted : true, reason: v?.reason }))))
  return { gateGreen: !!rev?.gateGreen, upheld: judged.filter(Boolean).filter(f => f.upheld) }
})() : Promise.resolve({ gateGreen: true, upheld: [] })

const verified = await parallel(slices.map(([a, b]) => () => agent(`${BASE}

The prior QC report is ${REPORT}. Read its "Lead report" and "Confirmed findings (raw)" sections. Number the raw confirmed findings in document order starting at 1. Handle findings ${a}..${b} (skip numbers that don't exist). For each: read the current code at HEAD and decide
- fixed: the problem is gone AND (for high/medium) a test now covers it — cite file:line of fix and test;
- deviation-ok: not fixed but a PORTING.md Deviations row gives a legitimate reason;
- not-fixed / regressed: say exactly what is still wrong;
- not-applicable: the finding was wrong to begin with (explain).`,
  { label: `verify:${a}-${b}`, phase: 'Verify fixes', schema: VERIFY, model: M.verify, effort: E.verify })))
const results = verified.filter(Boolean).flatMap(v => v.results)
const open = results.filter(r => r.status === 'not-fixed' || r.status === 'regressed')
log(`${results.length} prior findings checked: ${results.filter(r => r.status === 'fixed').length} fixed, ${results.filter(r => r.status === 'deviation-ok').length} deviation-ok, ${open.length} open`)

const nc = await newCode
log(`new code: ${nc.upheld.length} upheld findings, gate ${nc.gateGreen ? 'green' : 'RED'}`)

phase('Verdict')
const report = await agent(`${BASE}

You are the re-QC lead. Prior findings still open (not-fixed/regressed):
${JSON.stringify(open, null, 2)}
New-code findings that survived a skeptic:
${JSON.stringify(nc.upheld, null, 2)}
Gate on new code: ${nc.gateGreen ? 'green' : 'RED'}.
Prior QC report for severities: ${REPORT}.
Write a concise fix list (severity, file:line, problem, fix), then one line: PASS (no open critical/high and gate green) or FAIL. Plain text.`,
  { label: 'reqc-lead', phase: 'Verdict', model: M.lead, effort: E.lead })

return { checked: results.length, open, newFindings: nc.upheld, gateGreen: nc.gateGreen, results, report }
