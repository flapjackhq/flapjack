#!/usr/bin/env node
// join_proof_report.mjs — compute the JOIN-1 number from a Playwright JSON run.
//
// WHY THIS EXISTS
// ---------------
// JOIN-1 ("backend<->frontend joined proof for the dashboard") read 0 / 90 for
// three consecutive lanes. The reason was never that the tests failed — it was
// that nobody could cheaply answer "which of the 27 named proof specs passed at
// this SHA?". The 90-row capability matrix lives in a 1,639-line markdown
// receipt, and Playwright was configured with the HTML reporter only, so there
// was no machine-readable result artifact to join it against. Every lane
// re-derived the answer by hand, ran out of budget, and wrote "not executed at
// the audited SHA" — which is true, and is also what you write when the join is
// too expensive to perform rather than when the tests are actually red.
//
// This script performs that join. It reads Playwright's JSON results plus the
// row->key manifest and prints the denominator. That turns a multi-hour manual
// audit into a command, and it means any future full-suite run updates the
// number as a side effect of running at all.
//
// DENOMINATORS — read these carefully, they are not interchangeable
// ----------------------------------------------------------------
// The historical "0 / 90" framing is misleading and this script deliberately
// does not reproduce it. Of the 90 backend capability rows:
//   - 27 have no dashboard surface at all (19 API-only, 7 config-only, 1
//     CLI-only). They can NEVER have a dashboard joined proof. Counting them in
//     the denominator sets a target that is unreachable by construction.
//   - 4 more have a dashboard route but no candidate spec exists yet, so they
//     are unreachable until someone writes one.
//   - 59 have both a route and a named candidate spec. That is the joinable
//     denominator, and it is the one worth moving.
// The script reports all four numbers so no reader has to guess which is meant.
//
// USAGE
//   node scripts/join_proof_report.mjs [--results <path>] [--manifest <path>] [--json]
//
// Default results path matches the `json` reporter configured in
// playwright.config.ts. Exit code is 0 when the report is produced and 1 when it
// cannot be produced honestly (missing results, unreadable manifest, or a proof
// key that no longer resolves to a test in the corpus). A key that stopped
// resolving is a HARD failure rather than a silent zero: a renamed test title
// must show up as a broken mapping, not as a capability that quietly stopped
// being proven.

import { readFileSync, existsSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const DASHBOARD_DIR = resolve(HERE, '..');

const DEFAULTS = {
  results: resolve(DASHBOARD_DIR, 'test-results/results.json'),
  manifest: resolve(DASHBOARD_DIR, 'tests/e2e-ui/join_proof_manifest.json'),
};

function parseArgs(argv) {
  const out = { ...DEFAULTS, json: false };
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    if (flag === '--json') {
      out.json = true;
    } else if (flag === '--results' || flag === '--manifest') {
      const value = argv[i + 1];
      if (!value) die(`${flag} needs a value`);
      out[flag.slice(2)] = resolve(process.cwd(), value);
      i += 1;
    } else {
      die(`unknown argument: ${flag}`);
    }
  }
  return out;
}

function die(message) {
  process.stderr.write(`join_proof_report: ${message}\n`);
  process.exit(1);
}

// Playwright's JSON reporter nests suites arbitrarily deep and repeats the file
// path on every level, so collect leaf specs rather than assuming a shape.
//
// `ok` is NOT sufficient on its own. TestCase.ok() in
// playwright/lib/common/test.js returns true for outcome "expected", "flaky",
// AND "skipped" — so a skipped spec reports ok: true. Trusting it alone would
// count a skipped capability as proven, and the most recent full run skipped 20
// specs. Keep the raw per-result statuses and require an actual `passed` below.
function collectSpecs(node, into) {
  if (!node || typeof node !== 'object') return;
  for (const spec of node.specs ?? []) {
    into.push({
      file: spec.file ?? node.file ?? '',
      title: spec.title ?? '',
      ok: spec.ok === true,
      statuses: (spec.tests ?? []).flatMap((t) => (t.results ?? []).map((r) => r.status)),
    });
  }
  for (const child of node.suites ?? []) collectSpecs(child, into);
}

// The manifest stores specs as `full/search.spec.ts`; Playwright reports them
// relative to its testDir, which may render as `full/search.spec.ts` or with a
// leading segment. Compare on suffix so neither side has to know the other's
// root, while still requiring a path-boundary match so `a/b.spec.ts` never
// matches `xa/b.spec.ts`.
function specMatches(reportedFile, manifestSpec) {
  if (reportedFile === manifestSpec) return true;
  return reportedFile.endsWith(`/${manifestSpec}`);
}

function main() {
  const args = parseArgs(process.argv.slice(2));

  if (!existsSync(args.manifest)) die(`manifest not found: ${args.manifest}`);
  const manifest = JSON.parse(readFileSync(args.manifest, 'utf8'));

  if (!existsSync(args.results)) {
    die(
      `results not found: ${args.results}\n` +
        '  Run the browser suite first. The `json` reporter in playwright.config.ts writes this file.',
    );
  }
  const report = JSON.parse(readFileSync(args.results, 'utf8'));

  const specs = [];
  collectSpecs(report, specs);
  for (const suite of report.suites ?? []) collectSpecs(suite, specs);
  if (specs.length === 0) die('the results file contained no specs; refusing to report a vacuous zero');

  // Resolve every proof key against the run.
  const keyOutcome = {};
  const unresolved = [];
  for (const [key, { spec, title }] of Object.entries(manifest.proof_keys)) {
    const hits = specs.filter((s) => specMatches(s.file, spec) && s.title === title);
    if (hits.length === 0) {
      keyOutcome[key] = 'not-run';
      continue;
    }
    // A capability is proven only by a spec that actually ran and passed.
    // Skipped is called out separately rather than folded into either side: it
    // is not a failure, and treating it as proof is the false positive this
    // whole report exists to avoid.
    const ran = hits.filter((h) => h.statuses.some((s) => s === 'passed' || s === 'failed'));
    if (ran.length === 0) {
      keyOutcome[key] = 'skipped';
      continue;
    }
    keyOutcome[key] = ran.every((h) => h.ok && h.statuses.includes('passed')) ? 'passed' : 'failed';
  }

  // A key that no longer resolves anywhere in the corpus is a broken mapping,
  // not a not-run test. Distinguish the two by checking the spec file exists at
  // all in this run: if the file ran but the title is gone, the title drifted.
  for (const [key, { spec, title }] of Object.entries(manifest.proof_keys)) {
    if (keyOutcome[key] !== 'not-run') continue;
    const fileRan = specs.some((s) => specMatches(s.file, spec));
    if (fileRan) unresolved.push({ key, spec, title });
  }

  const rows = manifest.rows;
  const route = rows.filter((r) => r.surface === 'dashboard route');
  const joinable = route.filter((r) => r.proof_key);
  const routeNoCandidate = route.filter((r) => !r.proof_key);
  const structural = rows.filter((r) => r.surface !== 'dashboard route');

  const joined = joinable.filter((r) => keyOutcome[r.proof_key] === 'passed');
  const red = joinable.filter((r) => keyOutcome[r.proof_key] === 'failed');
  const notRun = joinable.filter((r) => keyOutcome[r.proof_key] === 'not-run');
  const skipped = joinable.filter((r) => keyOutcome[r.proof_key] === 'skipped');

  const summary = {
    joinable_denominator: joinable.length,
    joined_proof_yes: joined.length,
    joined_proof_red: red.length,
    joined_proof_skipped: skipped.length,
    joined_proof_not_run: notRun.length,
    route_without_candidate_spec: routeNoCandidate.length,
    not_joinable_by_construction: structural.length,
    backend_denominator: rows.length,
    proof_keys_total: Object.keys(manifest.proof_keys).length,
    proof_keys_unresolved: unresolved.length,
  };

  if (args.json) {
    process.stdout.write(`${JSON.stringify({ summary, keyOutcome, unresolved }, null, 2)}\n`);
  } else {
    const lines = [
      'JOIN-1 joined-proof report',
      `  results:  ${args.results}`,
      `  manifest: ${manifest.derived_from}`,
      '',
      `  joined proof yes                       ${summary.joined_proof_yes} / ${summary.joinable_denominator}`,
      `  joinable rows whose spec failed        ${summary.joined_proof_red}`,
      `  joinable rows whose spec was skipped   ${summary.joined_proof_skipped}  (skipped is not proof)`,
      `  joinable rows whose spec did not run   ${summary.joined_proof_not_run}`,
      '',
      `  dashboard route, no candidate spec     ${summary.route_without_candidate_spec}`,
      `  not joinable by construction           ${summary.not_joinable_by_construction}  (API-only, CLI-only, config-only)`,
      `  backend denominator                    ${summary.backend_denominator}`,
    ];
    if (unresolved.length > 0) {
      lines.push('', '  BROKEN MAPPINGS — the spec file ran but the title is gone:');
      for (const u of unresolved) lines.push(`    ${u.key}  ${u.spec}  "${u.title}"`);
    }
    process.stdout.write(`${lines.join('\n')}\n`);
  }

  // Broken mappings make the number untrustworthy, so they are the one condition
  // that fails the command. A merely low number is a true measurement and exits 0.
  if (unresolved.length > 0) process.exit(1);
}

main();
