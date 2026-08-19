import { mkdirSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative, sep } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';

const temporaryRoots: string[] = [];

function routes(root: string): string[] {
  const routesDirectory = join(root, 'src/routes');
  const walk = (directory: string): string[] =>
    readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) return walk(path);
      if (entry.name !== '+page.svelte') return [];
      const routeDirectory = relative(routesDirectory, dirname(path));
      return [routeDirectory ? `/${routeDirectory.split(sep).join('/')}` : '/'];
    });
  return walk(routesDirectory).sort();
}

function routeFixture(): string {
  const root = mkdtempSync(join(tmpdir(), 'flapjack-console-routes-'));
  temporaryRoots.push(root);
  mkdirSync(join(root, 'src/routes'), { recursive: true });
  writeFileSync(join(root, 'src/routes/+page.svelte'), '<h1>Root</h1>');
  return root;
}

afterEach(() => {
  for (const root of temporaryRoots) rmSync(root, { recursive: true, force: true });
  temporaryRoots.length = 0;
});

describe('standalone filesystem route contract', () => {
  it('keeps exactly the owned routes under the /dashboard base', () => {
    expect(routes(process.cwd())).toEqual([
      '/',
      '/index/[indexName]',
      '/keys',
      '/security-sources',
    ]);
  });

  it('detects an added unowned route', () => {
    const root = routeFixture();
    mkdirSync(join(root, 'src/routes/extra'), { recursive: true });
    writeFileSync(join(root, 'src/routes/extra/+page.svelte'), '<h1>Extra</h1>');

    expect(routes(root)).toEqual(['/', '/extra']);
  });
});
