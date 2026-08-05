import fs from 'node:fs';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import { analyzeVectorCapabilityStructure } from './vector-capability-spec-structure';

const E2E_FULL_DIR = path.resolve(__dirname, '../e2e-ui/full');
const NAVIGATION_VECTOR_ENABLED_TEST =
  'chat tab visible and navigable when NeuralSearch mode is enabled';

function readSpec(fileName: string): string {
  return fs.readFileSync(path.join(E2E_FULL_DIR, fileName), 'utf8');
}

describe('vector capability browser spec structure', () => {
  it.each([
    'chat.spec.ts',
    'hybrid-search.spec.ts',
    'vector-settings.spec.ts',
    'navigation.spec.ts',
  ])('%s registers compiled-out tests outside capability-gated setup', (fileName) => {
    const analysis = analyzeVectorCapabilityStructure(readSpec(fileName));

    expect(analysis.compiledOutTestCount).toBeGreaterThan(0);
    expect(analysis.capabilityGatedCompiledOutTests).toEqual([]);
  });

  it.each([
    'chat.spec.ts',
    'hybrid-search.spec.ts',
    'vector-settings.spec.ts',
    'navigation.spec.ts',
  ])('%s skips vector-enabled tests when vector search is compiled out', (fileName) => {
    const analysis = analyzeVectorCapabilityStructure(readSpec(fileName));

    expect(analysis.vectorEnabledTestsWithoutSkipGuard).toEqual([]);
  });

  it('detects the standalone navigation vector-enabled test when its skip guard is removed', () => {
    const mutatedNavigationSpec = readSpec('navigation.spec.ts').replace(
      `
    await skipWhenVectorSearchDisabled(
      request,
      testInfo,
      'Chat tab navigation requires a vector-search-enabled build',
    );
`,
      '',
    );

    const analysis = analyzeVectorCapabilityStructure(mutatedNavigationSpec);

    expect(analysis.vectorEnabledTestsWithoutSkipGuard).toContain(
      NAVIGATION_VECTOR_ENABLED_TEST,
    );
  });

  it('detects vector-enabled describes without a skip guard', () => {
    const source = `
      test.describe('vector-enabled settings flows', () => {
        test.beforeEach(async ({ request }) => {
          expect(await isVectorSearchEnabled(request)).toBe(true);
        });

        test('persists vector settings', async ({ page }) => {
          await page.goto('/settings');
        });
      });
    `;

    const analysis = analyzeVectorCapabilityStructure(source);

    expect(analysis.vectorEnabledTestsWithoutSkipGuard).toEqual([
      'persists vector settings',
    ]);
  });

  it('accepts helper-mediated vector skip guards', () => {
    const source = `
      async function requireVectorEnabled(request, testInfo) {
        await skipWhenVectorSearchDisabled(request, testInfo, 'needs vector');
      }

      test.describe('vector-enabled settings flows', () => {
        test.beforeEach(async ({ request }, testInfo) => {
          await requireVectorEnabled(request, testInfo);
        });

        test('persists vector settings', async ({ page }) => {
          await page.goto('/settings');
        });
      });
    `;

    const analysis = analyzeVectorCapabilityStructure(source);

    expect(analysis.vectorEnabledTestsWithoutSkipGuard).toEqual([]);
  });

  it('detects helper-mediated capability checks in ancestor setup', () => {
    const source = `
      async function requireVectorCapability(request) {
        expect(await isVectorSearchEnabled(request)).toBe(true);
      }
      test.describe('hybrid', () => {
        test.beforeAll(async ({ request }) => {
          await requireVectorCapability(request);
        });
        test('fallback', async ({ page }) => {
          await page.route('**/health', route => route.fulfill({
            json: { capabilities: { vectorSearch: false } }
          }));
        });
      });
    `;

    const analysis = analyzeVectorCapabilityStructure(source);

    expect(analysis.compiledOutTestCount).toBe(1);
    expect(analysis.capabilityGatedCompiledOutTests).toEqual(['fallback']);
  });

  it('detects helper-mediated compiled-out route setup', () => {
    const source = `
      async function mockCompiledOutHealth(page) {
        await page.route('**/health', route => route.fulfill({
          json: { capabilities: { vectorSearch: false } }
        }));
      }

      test.describe('hybrid', () => {
        test.beforeEach(async ({ request }) => {
          await configureEmbedder(request, 'products', 'default', {
            source: 'userProvided',
            dimensions: 384,
          });
        });

        test('direct fallback', async ({ page }) => {
          await page.route('**/health', route => route.fulfill({
            json: { capabilities: { vectorSearch: false } }
          }));
        });

        test('helper fallback', async ({ page }) => {
          await mockCompiledOutHealth(page);
        });
      });
    `;

    const analysis = analyzeVectorCapabilityStructure(source);

    expect(analysis.compiledOutTestCount).toBe(2);
    expect(analysis.compiledOutTestsWithoutEmbedderSetup).toEqual([]);
  });

  it('detects compiled-out tests registered by a local helper', () => {
    const source = `
      function registerCompiledOutTests() {
        test.describe('compiled-out vector behavior', () => {
          test.beforeAll(async ({ request }) => {
            expect(await isVectorSearchEnabled(request)).toBe(true);
          });

          test('helper-registered fallback', async ({ page }) => {
            await page.route('**/health', route => route.fulfill({
              json: { capabilities: { vectorSearch: false } }
            }));
          });
        });
      }

      registerCompiledOutTests();
    `;

    const analysis = analyzeVectorCapabilityStructure(source);

    expect(analysis.compiledOutTestCount).toBe(1);
    expect(analysis.capabilityGatedCompiledOutTests).toEqual(['helper-registered fallback']);
    expect(analysis.compiledOutTestsWithoutEmbedderSetup).toEqual([
      'helper-registered fallback',
    ]);
  });

  it('does not count an ordinary vectorSearch:false expected-value object as compiled-out coverage', () => {
    const source = `
      test('reports the compiled-out capability payload shape', async ({ request }) => {
        const health = await fetchHealth(request);
        expect(health).toEqual({ capabilities: { vectorSearch: false } });
      });
    `;

    expect(analyzeVectorCapabilityStructure(source).compiledOutTestCount).toBe(0);
  });

  it('does not count a helper-returned expected-value object as compiled-out coverage', () => {
    const source = `
      function expectedCompiledOutHealth() {
        return { capabilities: { vectorSearch: false } };
      }

      test('asserts compiled-out capability payload matches expectation', async ({ request }) => {
        const health = await fetchHealth(request);
        expect(health).toEqual(expectedCompiledOutHealth());
      });
    `;

    expect(analyzeVectorCapabilityStructure(source).compiledOutTestCount).toBe(0);
  });

  it('ignores comments and titles that merely mention compiled-out behavior', () => {
    const source = `
      // isVectorSearchEnabled and vectorSearch: false are not executable here.
      test('compiled-out behavior is documented', async ({ page }) => {
        await page.goto('/');
      });
    `;

    expect(analyzeVectorCapabilityStructure(source).compiledOutTestCount).toBe(0);
  });

  it('requires every compiled-out hybrid test to establish an embedder precondition', () => {
    const analysis = analyzeVectorCapabilityStructure(readSpec('hybrid-search.spec.ts'));

    expect(analysis.compiledOutTestsWithoutEmbedderSetup).toEqual([]);
  });
});
