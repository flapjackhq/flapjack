import { expect, it } from "vitest";
import { analyzeVectorCapabilityStructure } from "./vector-capability-spec-structure";
import {
  API_HELPERS_MODULE,
  analyzeWithApiHelpers,
} from "./vector-capability-spec-test-helpers";

it("rejects an unrelated object method that spoofs the qualified vector skip guard", () => {
  const source = `
      const unrelatedApi = {
        async skipWhenVectorSearchDisabled() {},
      };

      test.describe('vector-enabled settings flows', () => {
        test.beforeEach(async ({ request }) => {
          await unrelatedApi.skipWhenVectorSearchDisabled(request, test.info(), 'needs vector');
        });

        test('spoofed skip guard', async ({ page }) => {
          await page.goto('/index/products');
        });
      });
    `;

  const analysis = analyzeVectorCapabilityStructure(source);

  expect(analysis.vectorEnabledTestsWithoutSkipGuard).toEqual([
    "spoofed skip guard",
  ]);
});

it("rejects an unrelated object method that spoofs the qualified embedder setup", () => {
  const source = `
      const unrelatedApi = {
        async configureEmbedder() {},
      };

      test.describe('compiled-out vector behavior', () => {
        test.beforeEach(async ({ request }) => {
          await unrelatedApi.configureEmbedder(request, 'products', 'default', {
            source: 'userProvided',
            dimensions: 384,
          });
        });

        test('spoofed embedder setup', async ({ page }) => {
          await page.route('**/health', route => route.fulfill({
            json: { capabilities: { vectorSearch: false } }
          }));
        });
      });
    `;

  const analysis = analyzeVectorCapabilityStructure(source);

  expect(analysis.compiledOutTestsWithoutEmbedderSetup).toEqual([
    "spoofed embedder setup",
  ]);
});

it("rejects an unrelated object method that spoofs qualified vector document seeding", () => {
  const source = `
      const unrelatedApi = {
        async addDocumentsWithVectors() {},
      };

      test('spoofed vector seed', async ({ request }) => {
        await unrelatedApi.addDocumentsWithVectors(request, 'products', [{ objectID: 'chair' }]);
      });
    `;

  const analysis = analyzeVectorCapabilityStructure(source);

  expect(analysis.vectorDocumentTestsWithoutReadinessWait).toEqual([]);
});

it("rejects an unrelated object method that spoofs the qualified searchability wait", () => {
  const source = `
      const unrelatedApi = {
        async waitForSearchableObjectIds() {},
      };

      test('spoofed readiness wait', async ({ request }) => {
        await addDocumentsWithVectors(request, 'products', [{ objectID: 'chair' }]);
        await unrelatedApi.waitForSearchableObjectIds(
          request,
          'products',
          'posture',
          ['chair'],
        );
      });
    `;

  const analysis = analyzeWithApiHelpers(source);

  expect(analysis.vectorDocumentTestsWithoutReadinessWait).toEqual([
    "spoofed readiness wait",
  ]);
});

it("detects helper-mediated vector-enabled browser capability overrides", () => {
  const source = `
      async function forceBrowserVectorCapability(page) {
        await page.route('**/health', route => route.fulfill({
          json: { capabilities: { vectorSearch: true } }
        }));
      }

      test.describe('vector-enabled settings flows', () => {
        test.beforeEach(async ({ request }, testInfo) => {
          await skipWhenVectorSearchDisabled(request, testInfo, 'needs vector');
        });

        test('uses browse vector controls', async ({ page }) => {
          await forceBrowserVectorCapability(page);
          await page.goto('/index/products');
        });
      });
    `;

  const analysis = analyzeVectorCapabilityStructure(source);

  expect(analysis.vectorEnabledTestsWithHealthCapabilityOverride).toEqual([
    "uses browse vector controls",
  ]);
});

it("detects vector-enabled browser capability overrides in ancestor setup", () => {
  const source = `
      test.describe('vector-enabled settings flows', () => {
        test.beforeEach(async ({ request, page }, testInfo) => {
          await skipWhenVectorSearchDisabled(request, testInfo, 'needs vector');
          await page.route('**/health', route => route.fulfill({
            json: { capabilities: { vectorSearch: true } }
          }));
        });

        test('uses browse vector controls', async ({ page }) => {
          await page.goto('/index/products');
        });
      });
    `;

  const analysis = analyzeVectorCapabilityStructure(source);

  expect(analysis.vectorEnabledTestsWithHealthCapabilityOverride).toEqual([
    "uses browse vector controls",
  ]);
});

it("detects an imported helper that forces the browser vector capability", () => {
  const source = `
      import { forceBrowserVectorCapability } from './browser-capability';

      test.describe('vector-enabled settings flows', () => {
        test.beforeEach(async ({ request, page }, testInfo) => {
          await skipWhenVectorSearchDisabled(request, testInfo, 'needs vector');
          await forceBrowserVectorCapability(page);
        });

        test('uses browse vector controls', async ({ page }) => {
          await page.goto('/index/products');
        });
      });
    `;

  const analysis = analyzeVectorCapabilityStructure(source, {
    filePath: "/specs/browser-spec.ts",
    resolveModuleSource: (specifier) =>
      specifier === "./browser-capability"
        ? {
            filePath: "/specs/browser-capability.ts",
            source: `
                export async function forceBrowserVectorCapability(page) {
                  await page.route('**/health', route => route.fulfill({
                    json: { capabilities: { vectorSearch: true } }
                  }));
                }
              `,
          }
        : undefined,
  });

  expect(analysis.vectorEnabledTestsWithHealthCapabilityOverride).toEqual([
    "uses browse vector controls",
  ]);
});

it("detects a default-imported helper that forces the browser vector capability", () => {
  const source = `
      import forceBrowserVectorCapability from './browser-capability';

      test.describe('vector-enabled settings flows', () => {
        test.beforeEach(async ({ request, page }, testInfo) => {
          await skipWhenVectorSearchDisabled(request, testInfo, 'needs vector');
          await forceBrowserVectorCapability(page);
        });

        test('uses browse vector controls', async ({ page }) => {
          await page.goto('/index/products');
        });
      });
    `;

  const analysis = analyzeVectorCapabilityStructure(source, {
    filePath: "/specs/browser-spec.ts",
    resolveModuleSource: (specifier) =>
      specifier === "./browser-capability"
        ? {
            filePath: "/specs/browser-capability.ts",
            source: `
                export default async function forceBrowserVectorCapability(page) {
                  await page.route('**/health', route => route.fulfill({
                    json: { capabilities: { vectorSearch: true } }
                  }));
                }
              `,
          }
        : undefined,
  });

  expect(analysis.vectorEnabledTestsWithHealthCapabilityOverride).toEqual([
    "uses browse vector controls",
  ]);
});

it("detects a namespace-imported helper that forces the browser vector capability", () => {
  const source = `
      import * as browserCapability from './browser-capability';

      test.describe('vector-enabled settings flows', () => {
        test.beforeEach(async ({ request, page }, testInfo) => {
          await skipWhenVectorSearchDisabled(request, testInfo, 'needs vector');
          await browserCapability.forceBrowserVectorCapability(page);
        });

        test('uses browse vector controls', async ({ page }) => {
          await page.goto('/index/products');
        });
      });
    `;

  const analysis = analyzeVectorCapabilityStructure(source, {
    filePath: "/specs/browser-spec.ts",
    resolveModuleSource: (specifier) =>
      specifier === "./browser-capability"
        ? {
            filePath: "/specs/browser-capability.ts",
            source: `
                export async function forceBrowserVectorCapability(page) {
                  await page.route('**/health', route => route.fulfill({
                    json: { capabilities: { vectorSearch: true } }
                  }));
                }
              `,
          }
        : undefined,
  });

  expect(analysis.vectorEnabledTestsWithHealthCapabilityOverride).toEqual([
    "uses browse vector controls",
  ]);
});

it("detects a health override helper imported through a barrel re-export", () => {
  const source = `
      import { skipWhenVectorSearchDisabled } from './api-helpers';
      import { forceBrowserVectorCapability } from './helpers';

      test.describe('vector-enabled settings flows', () => {
        test.beforeEach(async ({ request, page }, testInfo) => {
          await skipWhenVectorSearchDisabled(request, testInfo, 'needs vector');
          await forceBrowserVectorCapability(page);
        });

        test('uses barrel-exported capability helper', async ({ page }) => {
          await page.goto('/index/products');
        });
      });
    `;

  const analysis = analyzeVectorCapabilityStructure(source, {
    filePath: "/specs/browser-spec.ts",
    resolveModuleSource: (specifier, importerPath) => {
      if (specifier === "./api-helpers") {
        return API_HELPERS_MODULE;
      }
      if (
        specifier === "./helpers" &&
        importerPath === "/specs/browser-spec.ts"
      ) {
        return {
          filePath: "/specs/helpers.ts",
          source: `export { forceBrowserVectorCapability } from './impl';`,
        };
      }
      if (specifier === "./impl" && importerPath === "/specs/helpers.ts") {
        return {
          filePath: "/specs/impl.ts",
          source: `
              export async function forceBrowserVectorCapability(page) {
                await page.route('**/health', route => route.fulfill({
                  json: { capabilities: { vectorSearch: true } }
                }));
              }
            `,
        };
      }
      return undefined;
    },
  });

  expect(analysis.vectorEnabledTestsWithHealthCapabilityOverride).toEqual([
    "uses barrel-exported capability helper",
  ]);
});
