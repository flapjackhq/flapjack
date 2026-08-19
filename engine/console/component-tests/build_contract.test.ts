import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import { PAGE_PATHS, probeConsoleBuild, validateConsoleBuild } from '../scripts/check_build.mjs';

const temporaryRoots: string[] = [];

function buildFixture(): string {
  const root = mkdtempSync(join(tmpdir(), 'flapjack-console-build-'));
  temporaryRoots.push(root);
  mkdirSync(join(root, '_app/assets'), { recursive: true });
  writeFileSync(
    join(root, 'index.html'),
    '<script>const config = { base: "/dashboard" }; kit.start();</script>' +
      '<link rel="stylesheet" href="/dashboard/_app/assets/index.css">' +
      '<script type="module" src="/dashboard/_app/assets/index.js"></script>'
  );
  writeFileSync(join(root, '_app/assets/index.js'), 'data-console-host standalone Flapjack Console');
  writeFileSync(join(root, '_app/assets/index.css'), 'data-console-theme --console-surface');
  return root;
}

afterEach(() => {
  for (const root of temporaryRoots) {
    rmSync(root, { recursive: true, force: true });
  }
  temporaryRoots.length = 0;
});

describe('standalone build contract', () => {
  it('probes every owned deep route through the static SPA fallback', () => {
    expect(PAGE_PATHS).toEqual([
      '/dashboard/',
      '/dashboard/index/catalog.v2',
      '/dashboard/keys',
      '/dashboard/security-sources',
    ]);
  });

  it('serves dotted client routes through the SPA fallback and compiled assets as files', async () => {
    await expect(probeConsoleBuild(buildFixture())).resolves.toBeUndefined();
  });

  it('does not fall back to HTML for a missing compiled asset', async () => {
    const root = buildFixture();
    rmSync(join(root, '_app/assets/index.css'));

    await expect(probeConsoleBuild(root)).rejects.toThrow(
      'preview returned 404 for /dashboard/_app/assets/index.css'
    );
  });

  it('accepts a compiled entry with host and semantic-token assets', () => {
    expect(validateConsoleBuild(buildFixture())).toEqual([]);
  });

  it('rejects a missing referenced stylesheet', () => {
    const root = buildFixture();
    rmSync(join(root, '_app/assets/index.css'));

    expect(validateConsoleBuild(root)).toContain('missing compiled asset: _app/assets/index.css');
  });

  it('rejects a JavaScript entry without the standalone host marker', () => {
    const root = buildFixture();
    writeFileSync(join(root, '_app/assets/index.js'), 'Flapjack Console');

    expect(validateConsoleBuild(root)).toContain('compiled JavaScript lacks standalone host marker');
  });

  it('rejects an asset URL outside the dashboard base', () => {
    const root = buildFixture();
    const index = join(root, 'index.html');
    writeFileSync(index, readFileSync(index, 'utf8').replace('/dashboard/_app/assets/index.js', '/_app/assets/index.js'));

    expect(validateConsoleBuild(root)).toContain('compiled asset lacks dashboard base: /_app/assets/index.js');
  });
});
