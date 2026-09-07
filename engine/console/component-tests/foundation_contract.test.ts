import { mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';

const temporaryRoots: string[] = [];
const REQUIRED_DIRECTORIES = [
  'src/lib/design',
  'src/lib/ui',
  'src/lib/features',
  'src/lib/transport',
  'component-tests',
  'browser-tests-mocked',
  'browser-tests-unmocked',
] as const;
const REQUIRED_THEMES = ['flapjack', 'fjcloud'] as const;

function write(root: string, path: string, contents: string): void {
  const target = join(root, path);
  mkdirSync(dirname(target), { recursive: true });
  writeFileSync(target, contents);
}

function sourceFiles(root: string, directory: string): string[] {
  const absoluteDirectory = join(root, directory);
  try {
    return readdirSync(absoluteDirectory, { withFileTypes: true }).flatMap((entry) => {
      const path = join(absoluteDirectory, entry.name);
      return entry.isDirectory()
        ? sourceFiles(root, relative(root, path))
        : /\.(css|svelte|ts)$/.test(entry.name)
          ? [path]
          : [];
    });
  } catch {
    return [];
  }
}

function hasTrackedBoundary(root: string, directory: string): boolean {
  try {
    return readdirSync(join(root, directory)).length > 0;
  } catch {
    return false;
  }
}

function themeTokens(css: string, theme: string): Set<string> {
  const escapedTheme = theme.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const block = css.match(
    new RegExp(`\\[data-console-theme=['\"]${escapedTheme}['\"]\\]\\s*\\{([^}]*)\\}`)
  )?.[1];
  return new Set(block?.match(/--console-[a-z0-9-]+(?=\s*:)/g) ?? []);
}

function themeTokenValues(css: string, theme: string): Map<string, string> {
  const escapedTheme = theme.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const block = css.match(
    new RegExp(`\\[data-console-theme=['"]${escapedTheme}['"]\\]\\s*\\{([^}]*)\\}`)
  )?.[1];
  return new Map(
    [...(block?.matchAll(/(--console-[a-z0-9-]+)\s*:\s*(#[0-9a-f]{6})/gi) ?? [])].map(
      (match) => [match[1] ?? '', match[2] ?? '']
    )
  );
}

function relativeLuminance(hex: string): number {
  const channels = [1, 3, 5].map((offset) => Number.parseInt(hex.slice(offset, offset + 2), 16) / 255);
  const linear = channels.map((channel) =>
    channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4
  );
  return 0.2126 * (linear[0] ?? 0) + 0.7152 * (linear[1] ?? 0) + 0.0722 * (linear[2] ?? 0);
}

function contrastRatio(left: string, right: string): number {
  const leftLuminance = relativeLuminance(left);
  const rightLuminance = relativeLuminance(right);
  return (Math.max(leftLuminance, rightLuminance) + 0.05) / (Math.min(leftLuminance, rightLuminance) + 0.05);
}

function validateFoundation(root: string): string[] {
  const findings: string[] = [];

  for (const directory of REQUIRED_DIRECTORIES) {
    if (!hasTrackedBoundary(root, directory)) {
      findings.push(`missing tracked boundary: ${directory}`);
    }
  }

  const tokenPath = join(root, 'src/lib/design/tokens.css');
  let tokenCss = '';
  try {
    tokenCss = readFileSync(tokenPath, 'utf8');
  } catch {
    findings.push('missing semantic token owner: src/lib/design/tokens.css');
  }

  const expectedThemeTokens = themeTokens(tokenCss, REQUIRED_THEMES[0]);
  for (const theme of REQUIRED_THEMES) {
    const tokens = themeTokens(tokenCss, theme);
    if (tokens.size === 0 || [...tokens].some((token) => !expectedThemeTokens.has(token))) {
      findings.push(`theme semantic set differs: ${theme}`);
    }
    if ([...expectedThemeTokens].some((token) => !tokens.has(token))) {
      findings.push(`theme semantic set differs: ${theme}`);
    }
  }

  for (const path of sourceFiles(root, 'src/lib')) {
    const contents = readFileSync(path, 'utf8');
    const relativePath = relative(root, path);
    if (relativePath !== 'src/lib/design/tokens.css' && /#[0-9a-f]{3,8}\b|\b(?:rgb|hsl)a?\(/i.test(contents)) {
      findings.push(`raw color outside token owner: ${relativePath}`);
    }
    const imports = [
      ...contents.matchAll(/from\s+['\"]([^'\"]+)['\"]/g),
      ...contents.matchAll(/import\s+['\"]([^'\"]+)['\"]/g),
    ];
    for (const match of imports) {
      const source = match[1] ?? '';
      if (/fjcloud|\$lib\/server|(^|\/)host(\/|$)/i.test(source)) {
        findings.push(`host or managed import in shared source: ${relativePath}`);
      }
    }
  }

  const exportPath = join(root, 'src/lib/ui/index.ts');
  let exports = '';
  try {
    exports = readFileSync(exportPath, 'utf8');
  } catch {
    findings.push('missing sole public UI export boundary: src/lib/ui/index.ts');
  }
  for (const match of exports.matchAll(/export\s+\{[^}]+\}\s+from\s+['\"]\.\/([^'\"]+)['\"]/g)) {
    const moduleName = (match[1] ?? '').replace(/\.svelte$/, '');
    const requiredArtifacts = [
      [`src/lib/ui/_component_${moduleName.toLowerCase()}.md`, 'component contract'],
      [`src/lib/ui/${moduleName}.stories.ts`, 'executable story'],
      [`component-tests/${moduleName}.test.ts`, 'component behavior test'],
    ] as const;
    for (const [path, kind] of requiredArtifacts) {
      try {
        readFileSync(join(root, path), 'utf8');
      } catch {
        findings.push(`public export lacks ${kind}: ${moduleName}`);
      }
    }
  }

  return [...new Set(findings)].sort();
}

function validFixture(): string {
  const root = mkdtempSync(join(tmpdir(), 'flapjack-console-contract-'));
  temporaryRoots.push(root);
  for (const directory of REQUIRED_DIRECTORIES) {
    write(root, `${directory}/owner.ts`, 'export {};\n');
  }
  write(
    root,
    'src/lib/design/tokens.css',
    "[data-console-theme='flapjack'] { --console-text: #111; }\n" +
      "[data-console-theme='fjcloud'] { --console-text: #222; }\n"
  );
  write(root, 'src/lib/ui/index.ts', 'export {};\n');
  return root;
}

afterEach(() => {
  for (const root of temporaryRoots) {
    rmSync(root, { recursive: true, force: true });
  }
  temporaryRoots.length = 0;
});

describe('console foundation contract', () => {
  it('accepts the checked-in shared boundary', () => {
    expect(validateFoundation(process.cwd())).toEqual([]);
  });

  it('rejects a public interactive export without its proof artifacts', () => {
    const root = validFixture();
    write(root, 'src/lib/ui/index.ts', "export { default as Button } from './Button.svelte';\n");
    write(root, 'src/lib/ui/Button.svelte', '<button>Continue</button>\n');

    expect(validateFoundation(root)).toEqual(
      expect.arrayContaining([
        'public export lacks component contract: Button',
        'public export lacks executable story: Button',
        'public export lacks component behavior test: Button',
      ])
    );
  });

  it('rejects raw colors and host imports in shared source', () => {
    const root = validFixture();
    write(
      root,
      'src/lib/features/BadFeature.svelte',
      "<script>import '../../host/auth';</script><p style='color: #fff'>Bad</p>\n"
    );

    expect(validateFoundation(root)).toEqual(
      expect.arrayContaining([
        'host or managed import in shared source: src/lib/features/BadFeature.svelte',
        'raw color outside token owner: src/lib/features/BadFeature.svelte',
      ])
    );
  });

  it('rejects theme semantic drift', () => {
    const root = validFixture();
    write(
      root,
      'src/lib/design/tokens.css',
      "[data-console-theme='flapjack'] { --console-text: #111; --console-focus: #333; }\n" +
        "[data-console-theme='fjcloud'] { --console-text: #222; }\n"
    );

    expect(validateFoundation(root)).toContain('theme semantic set differs: fjcloud');
  });

  it('keeps text, actions, controls, statuses, and focus indicators at AA contrast', () => {
    const tokenCss = readFileSync(join(process.cwd(), 'src/lib/design/tokens.css'), 'utf8');
    const requiredPairs = [
      ['--console-text', '--console-surface', 4.5],
      ['--console-text-muted', '--console-surface', 4.5],
      ['--console-on-accent', '--console-accent', 4.5],
      ['--console-danger', '--console-danger-surface', 4.5],
      ['--console-status', '--console-status-surface', 4.5],
      ['--console-border', '--console-surface-muted', 3],
      ['--console-focus', '--console-surface', 3],
    ] as const;

    for (const theme of REQUIRED_THEMES) {
      const values = themeTokenValues(tokenCss, theme);
      for (const [foregroundToken, backgroundToken, minimum] of requiredPairs) {
        const foreground = values.get(foregroundToken) ?? '';
        const background = values.get(backgroundToken) ?? '';
        expect(foreground, `${theme} is missing ${foregroundToken}`).toMatch(/^#[0-9a-f]{6}$/i);
        expect(background, `${theme} is missing ${backgroundToken}`).toMatch(/^#[0-9a-f]{6}$/i);
        expect(
          contrastRatio(foreground, background),
          `${theme} ${foregroundToken} on ${backgroundToken}`
        ).toBeGreaterThanOrEqual(minimum);
      }
    }
  });
});
