import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';

import {
  EXPECTED_SHAPE_EXEMPT_ROWS,
  refreshJoinManifestLineRefs,
  validateJoinManifest,
} from './check_join_manifest.mjs';

const validatorScriptPath = join(process.cwd(), 'scripts/check_join_manifest.mjs');
const tempDirs = [];

afterEach(() => {
  while (tempDirs.length > 0) rmSync(tempDirs.pop(), { recursive: true, force: true });
});

function tempFixture() {
  const dir = mkdtempSync(join(tmpdir(), 'join-manifest-'));
  tempDirs.push(dir);
  return {
    manifestPath: join(dir, 'join_proof_manifest.json'),
    featuresPath: join(dir, 'FEATURES.md'),
  };
}

function writeFixture({ rows, featuresLines }) {
  const fixture = tempFixture();
  writeFileSync(
    fixture.manifestPath,
    `${JSON.stringify({ schema: 1, proof_keys: {}, rows }, null, 2)}\n`,
  );
  writeFileSync(fixture.featuresPath, `${featuresLines.join('\n')}\n`);
  return fixture;
}

function featuresWithCapability({ section = 'Search', capability = 'Typo tolerance', line = 6 }) {
  const lines = ['# Features', '', `## ${section}`, '', '| Feature | Status | Notes |', '|---|---|---|'];
  while (lines.length < line - 1) lines.push('');
  lines.push(`| ${capability} | Done | |`);
  return lines;
}

function expectedShapeExemptRows() {
  return EXPECTED_SHAPE_EXEMPT_ROWS.map((row, index) => ({
    ...row,
    surface: 'dashboard route',
    proof_key: `P${index + 30}`,
  }));
}

function documentRow(featuresRef = 'FEATURES.md:7 Search') {
  return {
    capability: 'Typo tolerance',
    features_ref: featuresRef,
    surface: 'dashboard route',
    proof_key: 'P1',
  };
}

describe('validateJoinManifest', () => {
  it('emits complete parseable JSON before exiting on a large invalid manifest', () => {
    const rows = Array.from({ length: 1_000 }, (_, index) => ({
      capability: `Missing capability ${index}`,
      features_ref: 'FEATURES.md:7 Search',
      surface: 'dashboard route',
      proof_key: 'P1',
    }));
    const fixture = writeFixture({
      rows,
      featuresLines: featuresWithCapability({ line: 7 }),
    });

    const command = spawnSync(
      process.execPath,
      [validatorScriptPath, '--json', '--manifest', fixture.manifestPath, '--features', fixture.featuresPath],
      { encoding: 'utf8', maxBuffer: 10 * 1024 * 1024 },
    );

    expect(command.status).toBe(1);
    expect(command.error).toBeUndefined();
    expect(JSON.parse(command.stdout)).toMatchObject({
      ok: false,
      summary: { rowsChecked: 1_000, rowsMismatched: 1_006 },
      problems: expect.arrayContaining([
        expect.objectContaining({ kind: 'missing-referent', rowIndex: 999 }),
      ]),
    });
  });

  it('owns the six expected MIG-21 capability/ref pairs', () => {
    expect(EXPECTED_SHAPE_EXEMPT_ROWS).toEqual([
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
    ]);
  });

  it('passes a minimal manifest whose FEATURES.md ref matches capability, line, and section tail', () => {
    const fixture = writeFixture({
      rows: [
        documentRow(),
        ...expectedShapeExemptRows(),
      ],
      featuresLines: featuresWithCapability({ line: 7 }),
    });

    const result = validateJoinManifest(fixture);

    expect(result.ok).toBe(true);
    expect(result.summary).toMatchObject({
      rowsChecked: 7,
      documentRefsChecked: 1,
      rowsResolved: 1,
      rowsMismatched: 0,
      shapeExemptRows: 6,
    });
  });

  it.each(['FEATURES.md:0007 Search', 'FEATURES.md:7  Search', 'FEATURES.md:7 Search '])(
    'rejects noncanonical document ref %j instead of normalizing it',
    (featuresRef) => {
      const fixture = writeFixture({
        rows: [documentRow(featuresRef), ...expectedShapeExemptRows()],
        featuresLines: featuresWithCapability({ line: 7 }),
      });

      const result = validateJoinManifest(fixture);

      expect(result.ok).toBe(false);
      expect(result.summary).toMatchObject({
        documentRefsChecked: 0,
        rowsResolved: 0,
        rowsMismatched: 1,
      });
      expect(result.problems).toContainEqual(
        expect.objectContaining({
          kind: 'unparsable-features-ref',
          rowIndex: 0,
          featuresRef,
        }),
      );
    },
  );

  it('resolves a document ref by capability, stored line, and section tail when capability text is duplicated', () => {
    const fixture = writeFixture({
      rows: [
        documentRow('FEATURES.md:13 Analytics'),
        ...expectedShapeExemptRows(),
      ],
      featuresLines: [
        '# Features',
        '',
        '## Search',
        '',
        '| Feature | Status | Notes |',
        '|---|---|---|',
        '| Typo tolerance | Done | |',
        '',
        '## Analytics',
        '',
        '| Feature | Status | Notes |',
        '|---|---|---|',
        '| Typo tolerance | Done | |',
      ],
    });

    const result = validateJoinManifest(fixture);

    expect(result.ok).toBe(true);
    expect(result.summary).toMatchObject({
      documentRefsChecked: 1,
      rowsResolved: 1,
      rowsMismatched: 0,
    });
    expect(result.problems).not.toContainEqual(
      expect.objectContaining({
        kind: 'ambiguous-capability-match',
      }),
    );
  });

  it('fails when duplicate-capability line and section partial matches disagree', () => {
    const fixture = writeFixture({
      rows: [
        documentRow('FEATURES.md:7 Analytics'),
        ...expectedShapeExemptRows(),
      ],
      featuresLines: [
        '# Features',
        '',
        '## Search',
        '',
        '| Feature | Status | Notes |',
        '|---|---|---|',
        '| Typo tolerance | Done | |',
        '',
        '## Analytics',
        '',
        '| Feature | Status | Notes |',
        '|---|---|---|',
        '| Typo tolerance | Done | |',
      ],
    });

    const result = validateJoinManifest(fixture);

    expect(result.ok).toBe(false);
    expect(result.summary).toMatchObject({
      documentRefsChecked: 1,
      rowsResolved: 0,
      rowsMismatched: 1,
    });
    expect(result.problems).toContainEqual(
      expect.objectContaining({
        kind: 'ambiguous-capability-match',
        rowIndex: 0,
        matches: 2,
      }),
    );
    expect(result.problems).not.toContainEqual(
      expect.objectContaining({
        kind: 'section-tail-drift',
      }),
    );
  });

  it('fails when a unique line match conflicts with multiple section matches', () => {
    const fixture = writeFixture({
      rows: [
        documentRow('FEATURES.md:7 Analytics'),
        ...expectedShapeExemptRows(),
      ],
      featuresLines: [
        '# Features',
        '',
        '## Search',
        '',
        '| Feature | Status | Notes |',
        '|---|---|---|',
        '| Typo tolerance | Done | |',
        '',
        '## Analytics',
        '',
        '| Feature | Status | Notes |',
        '|---|---|---|',
        '| Typo tolerance | Done | |',
        '| Typo tolerance | Done | duplicate |',
      ],
    });

    const result = validateJoinManifest(fixture);

    expect(result.ok).toBe(false);
    expect(result.summary).toMatchObject({
      documentRefsChecked: 1,
      rowsResolved: 0,
      rowsMismatched: 1,
    });
    expect(result.problems).toContainEqual(
      expect.objectContaining({
        kind: 'ambiguous-capability-match',
        rowIndex: 0,
        matches: 3,
      }),
    );
    expect(result.problems).not.toContainEqual(
      expect.objectContaining({
        kind: 'section-tail-drift',
      }),
    );
  });

  it('reports the six MIG-21 refs as shape-exempt rows with their own denominator', () => {
    const fixture = writeFixture({
      rows: expectedShapeExemptRows(),
      featuresLines: ['# Features'],
    });

    const result = validateJoinManifest(fixture);

    expect(result.ok).toBe(false);
    expect(result.summary.shapeExemptRows).toBe(6);
    expect(result.summary.documentRefsChecked).toBe(0);
    expect(result.problems).toContainEqual(
      expect.objectContaining({
        kind: 'vacuous-document-check',
      }),
    );
  });

  it('fails when an unexpected MIG-21 row is reclassified as shape-exempt', () => {
    const fixture = writeFixture({
      rows: [
        documentRow(),
        ...expectedShapeExemptRows(),
        {
          capability: 'Typo tolerance',
          features_ref: 'MIG-21 Reclassified document ref',
          surface: 'dashboard route',
          proof_key: 'P99',
        },
      ],
      featuresLines: featuresWithCapability({ line: 7 }),
    });

    const result = validateJoinManifest(fixture);

    expect(result.ok).toBe(false);
    expect(result.summary.documentRefsChecked).toBe(1);
    expect(result.summary.shapeExemptRows).toBe(7);
    expect(result.summary.rowsMismatched).toBe(1);
    expect(result.problems).toContainEqual(
      expect.objectContaining({
        kind: 'unexpected-shape-exempt-ref',
        rowIndex: 7,
        capability: 'Typo tolerance',
        featuresRef: 'MIG-21 Reclassified document ref',
      }),
    );
  });

  it('counts a missing canonical MIG-21 row as a mismatched row', () => {
    const omittedRow = EXPECTED_SHAPE_EXEMPT_ROWS.at(-1);
    const fixture = writeFixture({
      rows: [
        documentRow(),
        ...expectedShapeExemptRows().slice(0, -1),
      ],
      featuresLines: featuresWithCapability({ line: 7 }),
    });

    const result = validateJoinManifest(fixture);

    expect(result.ok).toBe(false);
    expect(result.summary.documentRefsChecked).toBe(1);
    expect(result.summary.shapeExemptRows).toBe(5);
    expect(result.summary.rowsMismatched).toBe(1);
    expect(result.problems).toContainEqual(
      expect.objectContaining({
        kind: 'missing-shape-exempt-ref',
        message: `${omittedRow.capability} ${omittedRow.features_ref}`,
      }),
    );
  });

  it('fails when a document-shaped ref drifts from the stored section tail', () => {
    const fixture = writeFixture({
      rows: [
        documentRow(),
        ...expectedShapeExemptRows(),
      ],
      featuresLines: featuresWithCapability({ section: 'Index Settings', line: 7 }),
    });

    const result = validateJoinManifest(fixture);

    expect(result.ok).toBe(false);
    expect(result.summary.rowsResolved).toBe(1);
    expect(result.summary.rowsMismatched).toBe(1);
    expect(result.problems).toContainEqual(
      expect.objectContaining({
        kind: 'section-tail-drift',
        expectedSection: 'Search',
        actualSection: 'Index Settings',
      }),
    );
  });

  it('uses heading text before a mutable status suffix as the stable section identity', () => {
    const fixture = writeFixture({
      rows: [
        documentRow('FEATURES.md:7 Source migration'),
        ...expectedShapeExemptRows(),
      ],
      featuresLines: featuresWithCapability({
        section: 'Source migration — PROVIDER-NEUTRAL CORE + ALGOLIA RESUME SHIPPED',
        line: 7,
      }),
    });

    const result = validateJoinManifest(fixture);

    expect(result.ok).toBe(true);
    expect(result.summary.rowsMismatched).toBe(0);
  });

  it.each([
    'Source migration — PROVIDER-NEUTRAL CORE + ALGOLIA RESUME SHIPPED',
    'Algolia migration',
  ])('rejects noncanonical section identity %j', (storedSection) => {
    const fixture = writeFixture({
      rows: [
        documentRow(`FEATURES.md:7 ${storedSection}`),
        ...expectedShapeExemptRows(),
      ],
      featuresLines: featuresWithCapability({
        section: 'Source migration — PROVIDER-NEUTRAL CORE + ALGOLIA RESUME SHIPPED',
        line: 7,
      }),
    });

    const result = validateJoinManifest(fixture);

    expect(result.ok).toBe(false);
    expect(result.summary.rowsMismatched).toBe(1);
    expect(result.problems).toContainEqual(
      expect.objectContaining({
        kind: 'section-tail-drift',
        expectedSection: storedSection,
        actualSection: 'Source migration',
      }),
    );
  });

  it('fails when a document-shaped ref drifts from the stored line number', () => {
    const fixture = writeFixture({
      rows: [
        documentRow('FEATURES.md:9 Search'),
        ...expectedShapeExemptRows(),
      ],
      featuresLines: featuresWithCapability({ line: 7 }),
    });

    const result = validateJoinManifest(fixture);

    expect(result.ok).toBe(false);
    expect(result.summary.rowsResolved).toBe(1);
    expect(result.summary.rowsMismatched).toBe(1);
    expect(result.summary.driftBands).toEqual({ '-2': 1 });
    expect(result.problems).toContainEqual(
      expect.objectContaining({
        kind: 'stored-line-drift',
        expectedLine: 9,
        actualLine: 7,
      }),
    );
  });

  it('refreshes line-only drift after FEATURES.md compaction', () => {
    const fixture = writeFixture({
      rows: [
        documentRow('FEATURES.md:9 Search'),
        ...expectedShapeExemptRows(),
      ],
      featuresLines: featuresWithCapability({ line: 7 }),
    });

    expect(validateJoinManifest(fixture).ok).toBe(false);
    expect(refreshJoinManifestLineRefs(fixture)).toEqual({ rowsUpdated: 1 });
    expect(validateJoinManifest(fixture)).toMatchObject({
      ok: true,
      summary: { rowsMismatched: 0 },
    });
  });

  it('refuses to refresh an ambiguous capability and section match', () => {
    const fixture = writeFixture({
      rows: [
        documentRow('FEATURES.md:9 Search'),
        ...expectedShapeExemptRows(),
      ],
      featuresLines: [
        ...featuresWithCapability({ line: 7 }),
        '| Typo tolerance | Done | duplicate |',
      ],
    });

    expect(() => refreshJoinManifestLineRefs(fixture)).toThrow(
      'row 1 Typo tolerance resolves to 2 FEATURES.md rows in section Search',
    );
    expect(validateJoinManifest(fixture).ok).toBe(false);
  });

  it('fails when no document-shaped refs were checked', () => {
    const fixture = writeFixture({
      rows: [],
      featuresLines: featuresWithCapability({ line: 7 }),
    });

    const result = validateJoinManifest(fixture);

    expect(result.ok).toBe(false);
    expect(result.summary.documentRefsChecked).toBe(0);
    expect(result.problems).toContainEqual(
      expect.objectContaining({
        kind: 'vacuous-document-check',
      }),
    );
  });

  it('fails when the manifest has no rows denominator', () => {
    const fixture = tempFixture();
    writeFileSync(fixture.manifestPath, `${JSON.stringify({ schema: 1 }, null, 2)}\n`);
    writeFileSync(fixture.featuresPath, `${featuresWithCapability({ line: 7 }).join('\n')}\n`);

    const result = validateJoinManifest(fixture);

    expect(result.ok).toBe(false);
    expect(result.summary.rowsChecked).toBe(0);
    expect(result.problems).toContainEqual(
      expect.objectContaining({
        kind: 'missing-denominator',
      }),
    );
  });

  it('returns structured denominator evidence when the manifest root is null', () => {
    const fixture = tempFixture();
    writeFileSync(fixture.manifestPath, 'null\n');
    writeFileSync(fixture.featuresPath, `${featuresWithCapability({ line: 7 }).join('\n')}\n`);

    const result = validateJoinManifest(fixture);

    expect(result.ok).toBe(false);
    expect(result.summary).toMatchObject({
      rowsChecked: 0,
      documentRefsChecked: 0,
      rowsResolved: 0,
      rowsMismatched: 6,
      shapeExemptRows: 0,
    });
    expect(result.problems).toContainEqual({
      kind: 'missing-denominator',
      message: 'manifest must be an object with a rows array',
    });
  });

  it.each([null, 'not an object', 42, true, []])(
    'reports a structured problem for malformed manifest row %#',
    (malformedRow) => {
      const fixture = writeFixture({
        rows: [
          documentRow(),
          ...expectedShapeExemptRows(),
          malformedRow,
        ],
        featuresLines: featuresWithCapability({ line: 7 }),
      });

      const result = validateJoinManifest(fixture);

      expect(result.ok).toBe(false);
      expect(result.summary).toMatchObject({
        rowsChecked: 8,
        documentRefsChecked: 1,
        rowsResolved: 1,
        rowsMismatched: 1,
        shapeExemptRows: 6,
      });
      expect(result.problems).toContainEqual({
        kind: 'invalid-manifest-row',
        rowIndex: 7,
        message: 'manifest row must be a non-null object',
      });
    },
  );
});
