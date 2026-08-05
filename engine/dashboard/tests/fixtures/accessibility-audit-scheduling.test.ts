import fs from 'node:fs';
import path from 'node:path';
import { describe, expect, it } from 'vitest';

const SERIAL_DESCRIBE_PATTERN =
  /\bdescribe\s*\.\s*(?:serial\s*(?:\.\s*only\s*)?\(|configure\s*\(\s*\{[^}]*\bmode\s*:\s*['"]serial['"][^}]*\}\s*\))/;

const FULL_DENOMINATOR_SPEC_PATHS = [
  path.resolve(process.cwd(), 'tests/e2e-ui/full/accessibility.spec.ts'),
  path.resolve(process.cwd(), 'tests/e2e-ui/jun04_pm_lane_c_audit.spec.ts'),
] as const;

const ROUTE_AUDIT_MANIFEST_PATH = path.resolve(
  process.cwd(),
  'tests/e2e-ui/route_audit_manifest.ts',
);

function readFullDenominatorSpec(specPath: string): string {
  return fs.readFileSync(specPath, 'utf8');
}

describe('accessibility audit route scheduling', () => {
  it('accepts every rendered cluster status branch as route-ready', () => {
    const source = readFullDenominatorSpec(ROUTE_AUDIT_MANIFEST_PATH);

    expect(source).toContain("page.getByTestId('cluster-standalone-state')");
    expect(source).toContain("page.getByTestId('cluster-peer-table')");
    expect(source).toContain("page.getByTestId('cluster-ha-empty-state')");
    expect(source).toContain("page.getByTestId('cluster-error-state')");
  });

  it.each(FULL_DENOMINATOR_SPEC_PATHS)(
    'keeps denominator route cases independently scheduled in %s',
    (specPath) => {
      const source = readFullDenominatorSpec(specPath);

      expect(source).toContain('for (const appPath of AUDITED_DASHBOARD_ROUTE_PATTERNS)');
      expect(source).not.toMatch(SERIAL_DESCRIBE_PATTERN);
    },
  );

  it.each([
    "test.describe.configure({ mode: 'serial' })",
    'test.describe.configure({ retries: 1, mode: "serial" })',
    "test.describe.serial('Accessibility audit', () => {})",
    "test.describe.serial.only('Accessibility audit', () => {})",
  ])('recognizes forbidden serial scheduling in %s', (source) => {
    expect(source).toMatch(SERIAL_DESCRIBE_PATTERN);
  });
});
