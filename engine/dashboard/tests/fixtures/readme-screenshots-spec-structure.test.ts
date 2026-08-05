import fs from 'node:fs';
import path from 'node:path';
import { describe, expect, it } from 'vitest';

const README_SCREENSHOTS_SPEC = path.resolve(
  __dirname,
  '../e2e-ui/full/readme-screenshots.spec.ts',
);

function readReadmeScreenshotsSpec(): string {
  return fs.readFileSync(README_SCREENSHOTS_SPEC, 'utf8');
}

describe('README screenshot browser spec structure', () => {
  it('keeps search readiness independent of one fixture row ordering', () => {
    const source = readReadmeScreenshotsSpec();

    expect(source).not.toContain('PRODUCTS[0]');
    expect(source).not.toContain('SEEDED_PRODUCT_NAME');
    expect(source).not.toContain("filter({ hasText: SEEDED_PRODUCT_NAME })");
    expect(source).toContain("getByTestId('document-card').first()");
  });
});
