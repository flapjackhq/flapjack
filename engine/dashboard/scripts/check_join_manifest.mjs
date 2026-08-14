#!/usr/bin/env node
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { DEFAULTS, die, parseArgs, readJoinManifest } from './join_proof_common.mjs';

const SCRIPT_NAME = 'check_join_manifest';
const DOCUMENT_REF_RE = /^FEATURES\.md:(?<line>[1-9]\d*) (?<section>\S(?:.*\S)?)$/;
const SHAPE_EXEMPT_REF_RE = /^MIG-21\b.+$/;
export const EXPECTED_SHAPE_EXEMPT_ROWS = Object.freeze(
  [
    {
      capability: 'Meilisearch source migration through the console',
      features_ref: 'MIG-21 Meilisearch console migration',
    },
    {
      capability: 'Typesense source migration through the console',
      features_ref: 'MIG-21 Typesense console migration',
    },
    {
      capability: 'Meilisearch migration dry-run preview report through the console',
      features_ref: 'MIG-21 Meilisearch console migration dry-run',
    },
    {
      capability: 'Typesense migration dry-run preview report through the console',
      features_ref: 'MIG-21 Typesense console migration dry-run',
    },
    {
      capability: 'Meilisearch private-address preview refusal guidance through the console',
      features_ref: 'MIG-21 Meilisearch console migration dry-run refusal',
    },
    {
      capability: 'Algolia migration dry-run preview report through the console',
      features_ref: 'MIG-21 Algolia console migration dry-run',
    },
  ].map((row) => Object.freeze(row)),
);
const EXPECTED_SHAPE_EXEMPT_KEYS = new Set(EXPECTED_SHAPE_EXEMPT_ROWS.map(shapeExemptKey));

export function validateJoinManifest({ manifestPath = DEFAULTS.manifest, featuresPath = DEFAULTS.features }) {
  const manifest = readJoinManifest(manifestPath, SCRIPT_NAME);
  const featuresLines = readFeaturesLines(featuresPath);
  const featureIndex = indexFeatures(featuresLines);
  const problems = [];
  const mismatchedRows = new Set();
  const hasRowsDenominator =
    manifest !== null && typeof manifest === 'object' && !Array.isArray(manifest) && Array.isArray(manifest.rows);
  const rows = hasRowsDenominator ? manifest.rows : [];
  const summary = emptySummary(rows);
  const shapeExemptRows = [];

  if (!hasRowsDenominator) {
    problems.push({ kind: 'missing-denominator', message: 'manifest must be an object with a rows array' });
  }

  for (const [index, row] of rows.entries()) {
    if (row === null || typeof row !== 'object' || Array.isArray(row)) {
      problems.push({
        kind: 'invalid-manifest-row',
        rowIndex: index,
        message: 'manifest row must be a non-null object',
      });
      mismatchedRows.add(index);
      continue;
    }
    const context = { rowIndex: index, capability: row.capability, featuresRef: row.features_ref };
    const parsedRef = parseFeaturesRef(row.features_ref);
    if (parsedRef.kind === 'shape-exempt') {
      summary.shapeExemptRows += 1;
      shapeExemptRows.push({ row, context });
      continue;
    }
    if (parsedRef.kind === 'invalid') {
      problems.push({ kind: parsedRef.reason, ...context });
      mismatchedRows.add(index);
      continue;
    }

    summary.documentRefsChecked += 1;
    const hits = featureIndex.byCapability.get(row.capability) ?? [];
    if (hits.length === 0) {
      problems.push({ kind: 'missing-referent', ...context });
      mismatchedRows.add(index);
      continue;
    }

    const hit = resolveFeatureHit({ hits, parsedRef, problems, context });
    if (!hit) {
      mismatchedRows.add(index);
      continue;
    }

    summary.rowsResolved += 1;
    if (compareResolvedRef({ parsedRef, hit, problems, summary, context })) {
      mismatchedRows.add(index);
    }
  }

  const missingShapeExemptRows = validateShapeExemptRows({ shapeExemptRows, problems, mismatchedRows });
  if (summary.documentRefsChecked === 0) {
    problems.push({ kind: 'vacuous-document-check', message: 'zero document-shaped refs were checked' });
  }
  summary.rowsMismatched = mismatchedRows.size + missingShapeExemptRows;

  return { ok: problems.length === 0, summary, problems };
}

export function refreshJoinManifestLineRefs({
  manifestPath = DEFAULTS.manifest,
  featuresPath = DEFAULTS.features,
}) {
  const manifest = readJoinManifest(manifestPath, SCRIPT_NAME);
  if (manifest === null || typeof manifest !== 'object' || !Array.isArray(manifest.rows)) {
    throw new Error('manifest must be an object with a rows array');
  }
  const featureIndex = indexFeatures(readFeaturesLines(featuresPath));
  let rowsUpdated = 0;

  for (const [index, row] of manifest.rows.entries()) {
    const parsedRef = parseFeaturesRef(row?.features_ref);
    if (parsedRef.kind === 'shape-exempt') continue;
    if (parsedRef.kind !== 'document') {
      throw new Error(`row ${index + 1} has no refreshable FEATURES.md reference`);
    }
    const hits = featureIndex.byCapability.get(row.capability) ?? [];
    const sectionHits = hits.filter((hit) => hit.section === parsedRef.section);
    if (sectionHits.length !== 1) {
      throw new Error(
        `row ${index + 1} ${row.capability} resolves to ${sectionHits.length} `
        + `FEATURES.md rows in section ${parsedRef.section}`,
      );
    }
    const refreshedRef = `FEATURES.md:${sectionHits[0].line} ${parsedRef.section}`;
    if (row.features_ref !== refreshedRef) {
      row.features_ref = refreshedRef;
      rowsUpdated += 1;
    }
  }

  if (rowsUpdated > 0) writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
  return { rowsUpdated };
}

function emptySummary(rows) {
  return {
    rowsChecked: rows.length,
    documentRefsChecked: 0,
    rowsResolved: 0,
    rowsMismatched: 0,
    shapeExemptRows: 0,
    driftBands: {},
  };
}

function validateShapeExemptRows({ shapeExemptRows, problems, mismatchedRows }) {
  const seenExpectedKeys = new Set();
  let missingExpectedRows = 0;
  for (const { row, context } of shapeExemptRows) {
    const key = shapeExemptKey(row);
    if (!EXPECTED_SHAPE_EXEMPT_KEYS.has(key)) {
      problems.push({ kind: 'unexpected-shape-exempt-ref', ...context });
      mismatchedRows.add(context.rowIndex);
      continue;
    }
    if (seenExpectedKeys.has(key)) {
      problems.push({ kind: 'duplicate-shape-exempt-ref', ...context });
      mismatchedRows.add(context.rowIndex);
      continue;
    }
    seenExpectedKeys.add(key);
  }

  for (const expectedRow of EXPECTED_SHAPE_EXEMPT_ROWS) {
    const key = shapeExemptKey(expectedRow);
    if (!seenExpectedKeys.has(key)) {
      problems.push({
        kind: 'missing-shape-exempt-ref',
        message: `${expectedRow.capability} ${expectedRow.features_ref}`,
      });
      missingExpectedRows += 1;
    }
  }
  return missingExpectedRows;
}

function shapeExemptKey(row) {
  return JSON.stringify([row.capability, row.features_ref]);
}

function readFeaturesLines(featuresPath) {
  if (!existsSync(featuresPath)) die(SCRIPT_NAME, `FEATURES.md not found: ${featuresPath}`);
  return readFileSync(featuresPath, 'utf8').split(/\r?\n/);
}

function parseFeaturesRef(featuresRef) {
  if (typeof featuresRef !== 'string' || featuresRef.trim() === '') {
    return { kind: 'invalid', reason: 'missing-features-ref' };
  }
  const documentMatch = featuresRef.match(DOCUMENT_REF_RE);
  if (documentMatch) {
    return {
      kind: 'document',
      line: Number(documentMatch.groups.line),
      section: documentMatch.groups.section,
    };
  }
  if (SHAPE_EXEMPT_REF_RE.test(featuresRef)) return { kind: 'shape-exempt' };
  return { kind: 'invalid', reason: 'unparsable-features-ref' };
}

function indexFeatures(lines) {
  const byCapability = new Map();
  let section = null;
  lines.forEach((line, index) => {
    const heading = line.match(/^##\s+(?<title>.+?)\s*$/);
    if (heading) section = stableSectionIdentity(heading.groups.title);
    const capability = parseFeatureTableCapability(line);
    if (!capability) return;
    const hits = byCapability.get(capability) ?? [];
    hits.push({ line: index + 1, section, text: line });
    byCapability.set(capability, hits);
  });
  return { byCapability };
}

function stableSectionIdentity(headingTitle) {
  return headingTitle.split(/\s+—\s+/, 1)[0].trim();
}

function resolveFeatureHit({ hits, parsedRef, problems, context }) {
  const exactMatches = hits.filter((hit) => hit.line === parsedRef.line && hit.section === parsedRef.section);
  if (exactMatches.length === 1) return exactMatches[0];
  if (exactMatches.length > 1) {
    problems.push({ kind: 'ambiguous-full-contract-match', matches: exactMatches.length, ...context });
    return null;
  }
  if (hits.length === 1) return hits[0];

  const lineMatches = hits.filter((hit) => hit.line === parsedRef.line);
  const sectionMatches = hits.filter((hit) => hit.section === parsedRef.section);
  if (lineMatches.length === 1 && sectionMatches.length > 0) {
    problems.push({ kind: 'ambiguous-capability-match', matches: hits.length, ...context });
    return null;
  }
  if (lineMatches.length === 1) return lineMatches[0];
  if (sectionMatches.length === 1) return sectionMatches[0];

  problems.push({ kind: 'ambiguous-capability-match', matches: hits.length, ...context });
  return null;
}

function parseFeatureTableCapability(line) {
  if (!line.startsWith('|')) return null;
  const cells = line.split('|').slice(1, -1).map((cell) => cell.trim());
  if (cells.length < 3 || cells[0] === 'Feature' || /^-+$/.test(cells[0])) return null;
  return cells[0];
}

function compareResolvedRef({ parsedRef, hit, problems, summary, context }) {
  let mismatched = false;
  if (hit.line !== parsedRef.line) {
    const drift = hit.line - parsedRef.line;
    summary.driftBands[String(drift)] = (summary.driftBands[String(drift)] ?? 0) + 1;
    problems.push({
      kind: 'stored-line-drift',
      expectedLine: parsedRef.line,
      actualLine: hit.line,
      drift,
      ...context,
    });
    mismatched = true;
  }
  if (hit.section !== parsedRef.section) {
    problems.push({
      kind: 'section-tail-drift',
      expectedSection: parsedRef.section,
      actualSection: hit.section,
      ...context,
    });
    mismatched = true;
  }
  return mismatched;
}

function formatSummary(result, { manifest, features }) {
  const driftText = Object.entries(result.summary.driftBands)
    .sort(([left], [right]) => Number(left) - Number(right))
    .map(([drift, count]) => `${drift}: ${count}`)
    .join(', ');
  const lines = [
    'JOIN manifest validator',
    `  manifest: ${manifest}`,
    `  features: ${features}`,
    '',
    `  rows checked        ${result.summary.rowsChecked}`,
    `  rows resolved       ${result.summary.rowsResolved}`,
    `  rows mismatched     ${result.summary.rowsMismatched}`,
    `  drift-band detail   ${driftText || 'none'}`,
    `  shape-exempt rows   ${result.summary.shapeExemptRows}`,
  ];
  if (result.problems.length > 0) {
    lines.push('', '  PROBLEMS');
    const head = result.problems.slice(0, 12);
    for (const problem of head) lines.push(`    ${formatProblem(problem)}`);
    const hidden = result.problems.length - head.length;
    if (hidden > 0) lines.push(`    ... ${hidden} more problem records omitted from CLI summary`);
    lines.push('', '  SUMMARY');
    lines.push(`    rows checked        ${result.summary.rowsChecked}`);
    lines.push(`    rows resolved       ${result.summary.rowsResolved}`);
    lines.push(`    rows mismatched     ${result.summary.rowsMismatched}`);
    lines.push(`    drift-band detail   ${driftText || 'none'}`);
    lines.push(`    shape-exempt rows   ${result.summary.shapeExemptRows}`);
  }
  return `${lines.join('\n')}\n`;
}

function formatProblem(problem) {
  if (problem.rowIndex === undefined) return `${problem.kind}: ${problem.message}`;
  if (problem.message) return `${problem.kind}: row ${problem.rowIndex + 1} ${problem.message}`;
  const suffix = problem.drift === undefined ? '' : ` drift=${problem.drift}`;
  return `${problem.kind}: row ${problem.rowIndex + 1} ${problem.capability} ${problem.featuresRef}${suffix}`;
}

function main() {
  const args = parseArgs(process.argv.slice(2), {
    defaults: {
      manifest: DEFAULTS.manifest,
      features: DEFAULTS.features,
      json: false,
      'refresh-lines': false,
    },
    booleanFlags: ['--json', '--refresh-lines'],
    valueFlags: ['--manifest', '--features'],
    scriptName: SCRIPT_NAME,
  });
  if (args['refresh-lines']) {
    try {
      const refreshed = refreshJoinManifestLineRefs({
        manifestPath: args.manifest,
        featuresPath: args.features,
      });
      process.stdout.write(`JOIN manifest line refs refreshed: ${refreshed.rowsUpdated}\n`);
    } catch (error) {
      die(SCRIPT_NAME, `cannot refresh line refs: ${error.message}`);
    }
  }
  const result = validateJoinManifest({ manifestPath: args.manifest, featuresPath: args.features });
  if (args.json) {
    process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
  } else {
    process.stdout.write(formatSummary(result, args));
  }
  if (!result.ok) process.exitCode = 1;
}

if (import.meta.url === `file://${process.argv[1]}`) main();
