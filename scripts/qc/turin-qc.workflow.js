export const meta = {
  name: 'turin-qc',
  description: 'Opus 5.5 quality-control gate for a Turin branch/worktree: dimension reviews, adversarial verify, ranked report',
  whenToUse: 'Before merging any Turin task branch, and for cross-crate audits of main',
  phases: [
    { title: 'Review', detail: 'one reviewer per quality dimension' },
    { title: 'Verify', detail: 'two skeptics try to refute each finding' },
    { title: 'Report', detail: 'rank confirmed findings into a fix list' },
  ],
}

const A = args || {}
const WT = A.worktree
const SCOPE = A.scope
const UPSTREAM = A.upstream
const CONTEXT = A.context || ''

const BASE = `You are a QC reviewer (Claude Opus 5.5) for Turin, a faithful Rust/GPUI port of T3 Code (TypeScript/Effect).
Checkout to review (read-only, do NOT edit or commit anything): ${WT}
Read ${WT}/PORTING.md and ${WT}/AGENTS.md first: they define the porting rules, Effect->Rust mapping, keep/cut lists, and Deviations log.
Upstream reference (read-only): ${WT}/upstream  (pingdotgg/t3code@ec1313c8).
Scope under review: ${SCOPE}
Upstream counterpart: ${UPSTREAM}
${CONTEXT}
You may run read-only commands (cargo build/test/clippy with PATH=$HOME/.cargo/bin:$PATH DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer, small throwaway test programs in /tmp, node one-liners to check upstream behaviour). Never modify files in the checkout.
Report only concrete, evidenced problems with file:line. No style nits unless they violate PORTING.md. If nothing is wrong in your dimension, return an empty list.`

const DIMENSIONS = A.dimensions || [
  { key: 'fidelity', prompt: 'DIMENSION: upstream fidelity. Compare each Rust module line-by-line against its upstream file. Find dropped logic, branches, fields, constants, defaults, error cases, or comments-that-explain-why that were lost or changed without a Deviations entry. Also find code that reinvents instead of translating.' },
  { key: 'correctness', prompt: 'DIMENSION: correctness. Find bugs in the Rust itself: wrong logic, panics/unwrap on fallible input, off-by-one, wrong trait semantics (Eq/Hash/Ord/Borrow consistency), unsound unsafe, error swallowing, broken invariants.' },
  { key: 'wire-compat', prompt: 'DIMENSION: JSON/DB compatibility. Turin must read/write the exact JSON upstream Effect Schema produces (SQLite rows, event payloads, settings). Find serde mismatches: field names, rename rules, tag fields, optional-vs-null, defaults, trimming, number types, unknown-member handling, forward-compat behaviour. Verify claims by encoding samples with upstream (node + effect in upstream/node_modules if installed, otherwise reason from the schema source) and with serde in a /tmp scratch crate.' },
  { key: 'perf-concurrency', prompt: 'DIMENSION: performance and concurrency. Find blocking calls in async, locks held across await, unbounded queues/caches, needless clones/allocations on hot paths, quadratic work, missing indexes, N+1 queries, and upstream perf bugs that were carried over but could be fixed.' },
  { key: 'process', prompt: 'DIMENSION: process and conventions. Check: PORTING.md rules followed (module headers `//! Port of upstream/...`, one file -> one module, names kept), cut list respected (no cut features ported, no kept features dropped), every deviation logged in the Deviations table, pure-logic upstream tests ported, scripts/check.sh green, commits carry `Model: <id> (<provider>)` trailers (git log in the checkout). Also check that the docs themselves (PORTING.md mapping table, AGENTS.md, scripts) are correct and would not mislead other agents.' },
]

const FINDINGS = {
  type: 'object',
  properties: {
    findings: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          title: { type: 'string' },
          file: { type: 'string' },
          line: { type: 'integer' },
          severity: { type: 'string', enum: ['critical', 'high', 'medium', 'low'] },
          evidence: { type: 'string', description: 'quote code / upstream / command output proving it' },
          fix: { type: 'string' },
        },
        required: ['title', 'file', 'severity', 'evidence', 'fix'],
      },
    },
  },
  required: ['findings'],
}

const VERDICT = {
  type: 'object',
  properties: {
    refuted: { type: 'boolean' },
    reason: { type: 'string' },
    adjustedSeverity: { type: 'string', enum: ['critical', 'high', 'medium', 'low'] },
  },
  required: ['refuted', 'reason'],
}

const results = await pipeline(
  DIMENSIONS,
  d => agent(`${BASE}\n\n${d.prompt}`, { label: `review:${d.key}`, phase: 'Review', schema: FINDINGS, effort: 'high' }),
  (review, d) => parallel((review?.findings || []).map((f, i) => () =>
    parallel([0, 1].map(k => () => agent(
      `${BASE}\n\nYou are skeptic #${k + 1}. Another reviewer (dimension: ${d.key}) claims:\n${JSON.stringify(f, null, 2)}\n\nTry hard to REFUTE it: re-read the cited code and upstream, run checks. refuted=true if the claim is wrong, already handled, intentionally logged in Deviations, or not actually a problem. If it is real, refuted=false and give adjustedSeverity.`,
      { label: `verify:${d.key}#${i}.${k}`, phase: 'Verify', schema: VERDICT, effort: 'high' },
    ))).then(vs => {
      const ok = vs.filter(Boolean)
      const upheld = ok.filter(v => !v.refuted)
      return { ...f, dimension: d.key, upheld: upheld.length, votes: ok.length, severity: upheld[0]?.adjustedSeverity || f.severity, verdicts: ok.map(v => v.reason) }
    })
  )),
)

const all = results.flat().filter(Boolean)
const confirmed = all.filter(f => f.upheld >= 1 && f.upheld >= f.votes / 2)
const rejected = all.length - confirmed.length
log(`${all.length} findings raised, ${confirmed.length} survived verification, ${rejected} refuted`)

phase('Report')
const report = await agent(
  `${BASE}\n\nYou are the QC lead. Confirmed findings (survived adversarial verification):\n${JSON.stringify(confirmed, null, 2)}\n\nDeduplicate overlapping findings, rank by severity, and write a concise fix list for the implementing agent: each item = severity, file:line, problem, required fix. Then a one-line verdict: PASS (no critical/high) or FAIL. Plain text, no preamble.`,
  { label: 'qc-lead', phase: 'Report', effort: 'high' },
)
return { confirmed, rejectedCount: rejected, report }
