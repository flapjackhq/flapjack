import { expect, it } from "vitest";
import { analyzeVectorCapabilityStructure } from "./vector-capability-spec-structure";
import {
  API_HELPERS_IMPORT,
  API_HELPERS_MODULE,
  P20_CONTROL_GATE,
  P20_NEGATIVE_CONTROL_MODULE,
  P20_PROOF_TEST,
  P20_SIBLING_TEST,
  analyzeSpec,
  readSpec,
} from "./vector-capability-spec-test-helpers";

it("detects a health override helper imported through an export-star barrel", () => {
  const source = `
      import { skipWhenVectorSearchDisabled } from './api-helpers';
      import { forceBrowserVectorCapability } from './helpers';

      test.describe('vector-enabled settings flows', () => {
        test.beforeEach(async ({ request, page }, testInfo) => {
          await skipWhenVectorSearchDisabled(request, testInfo, 'needs vector');
          await forceBrowserVectorCapability(page);
        });

        test('uses export-star capability helper', async ({ page }) => {
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
          source: `export * from './impl';`,
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
    "uses export-star capability helper",
  ]);
});

it("does not treat a private helper from an imported module as an imported binding", () => {
  const source = `
      import { prepareBrowserCapability } from './browser-capability';

      test.describe('vector-enabled settings flows', () => {
        test.beforeEach(async ({ request, page }, testInfo) => {
          await skipWhenVectorSearchDisabled(request, testInfo, 'needs vector');
          await prepareBrowserCapability(page);
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
                function forceBrowserVectorCapability(page) {
                  return page.route('**/health', route => route.fulfill({
                    json: { capabilities: { vectorSearch: true } }
                  }));
                }

                export async function prepareBrowserCapability(page) {
                  await page.goto('/ready');
                }
              `,
          }
        : undefined,
  });

  expect(analysis.vectorEnabledTestsWithHealthCapabilityOverride).toEqual([]);
});

it("does not expand an unrelated object method with the same name as an imported helper", () => {
  const source = `
      import { forceBrowserVectorCapability } from './browser-capability';

      const browserCapability = {
        async forceBrowserVectorCapability(page) {
          await page.goto('/ready');
        },
      };

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

  expect(analysis.vectorEnabledTestsWithHealthCapabilityOverride).toEqual([]);
});

it("detects the live P20 control seam when its negative-control gate is removed", () => {
  const spec = readSpec("vector-settings.spec.ts");
  expect(spec).toContain(P20_CONTROL_GATE);

  const analysis = analyzeSpec("vector-settings.spec.ts", () =>
    spec.replace(P20_CONTROL_GATE, "      if (true) {"),
  );

  expect(analysis.vectorEnabledTestsWithHealthCapabilityOverride).toContain(
    P20_PROOF_TEST,
  );
  expect(analysis.vectorEnabledTestsWithHealthCapabilityOverride).toContain(
    P20_SIBLING_TEST,
  );
});

it("detects the live P20 control seam when it is not scoped to the control test", () => {
  const spec = readSpec("vector-settings.spec.ts");
  expect(spec).toContain(P20_CONTROL_GATE);

  const analysis = analyzeSpec("vector-settings.spec.ts", () =>
    spec.replace(
      P20_CONTROL_GATE,
      "      if (P20_TEXT_ONLY_NEGATIVE_CONTROL) {",
    ),
  );

  expect(analysis.vectorEnabledTestsWithHealthCapabilityOverride).toContain(
    P20_PROOF_TEST,
  );
  expect(analysis.vectorEnabledTestsWithHealthCapabilityOverride).toContain(
    P20_SIBLING_TEST,
  );
});

it("detects the live P20 control seam when it excludes the control test title", () => {
  const spec = readSpec("vector-settings.spec.ts");
  expect(spec).toContain(P20_CONTROL_GATE);

  const analysis = analyzeSpec("vector-settings.spec.ts", () =>
    spec.replace(
      P20_CONTROL_GATE,
      `      if (
        P20_TEXT_ONLY_NEGATIVE_CONTROL &&
        testInfo.title !== P20_TEXT_ONLY_CONTROL_TEST_TITLE
      ) {`,
    ),
  );

  expect(analysis.vectorEnabledTestsWithHealthCapabilityOverride).toContain(
    P20_SIBLING_TEST,
  );
});

it("detects the live P20 control seam when the control gate uses disjunction", () => {
  const spec = readSpec("vector-settings.spec.ts");
  expect(spec).toContain(P20_CONTROL_GATE);

  const analysis = analyzeSpec("vector-settings.spec.ts", () =>
    spec.replace(
      P20_CONTROL_GATE,
      `      if (
        P20_TEXT_ONLY_NEGATIVE_CONTROL ||
        testInfo.title === P20_TEXT_ONLY_CONTROL_TEST_TITLE
      ) {`,
    ),
  );

  expect(analysis.vectorEnabledTestsWithHealthCapabilityOverride).toContain(
    P20_SIBLING_TEST,
  );
});

it("detects the live P20 control seam when title equality uses an unrelated receiver", () => {
  const spec = readSpec("vector-settings.spec.ts");
  expect(spec).toContain(P20_CONTROL_GATE);

  const analysis = analyzeSpec("vector-settings.spec.ts", () =>
    spec.replace(
      P20_CONTROL_GATE,
      `      if (
        P20_TEXT_ONLY_NEGATIVE_CONTROL &&
        unrelated.title === P20_TEXT_ONLY_CONTROL_TEST_TITLE
      ) {`,
    ),
  );

  expect(analysis.vectorEnabledTestsWithHealthCapabilityOverride).toContain(
    P20_PROOF_TEST,
  );
});

it("detects a health override gated by a local shadow of the negative-control environment owner", () => {
  const source = `
      import { P20_TEXT_ONLY_CONTROL_TEST_TITLE } from './p20_negative_control';

      function isP20TextOnlyNegativeControl() {
        return true;
      }

      const TEXT_ONLY_CONTROL = isP20TextOnlyNegativeControl();

      test.describe('vector-enabled settings flows', () => {
        test.beforeEach(async ({ request, page }, testInfo) => {
          if (
            TEXT_ONLY_CONTROL &&
            testInfo.title === P20_TEXT_ONLY_CONTROL_TEST_TITLE
          ) {
            await page.route('**/health', route => route.fulfill({
              json: { capabilities: { vectorSearch: true } }
            }));
          } else {
            await skipWhenVectorSearchDisabled(request, testInfo, 'needs vector');
          }
        });

        test('set search mode to Neural Search and verify persistence', async ({ page }) => {
          await page.goto('/index/products');
        });
      });
    `;

  const analysis = analyzeVectorCapabilityStructure(source, {
    filePath: "/specs/browser-spec.ts",
    resolveModuleSource: (specifier) =>
      specifier === "./p20_negative_control"
        ? P20_NEGATIVE_CONTROL_MODULE
        : undefined,
  });

  expect(analysis.vectorEnabledTestsWithHealthCapabilityOverride).toEqual([
    P20_PROOF_TEST,
  ]);
});

it("detects a health override gated by an unrelated same-named object method", () => {
  const source = `
      import {
        P20_TEXT_ONLY_CONTROL_TEST_TITLE,
        isP20TextOnlyNegativeControl,
      } from './p20_negative_control';

      const environmentProbe = {
        isP20TextOnlyNegativeControl() {
          return true;
        },
      };
      const TEXT_ONLY_CONTROL = environmentProbe.isP20TextOnlyNegativeControl();

      test.describe('vector-enabled settings flows', () => {
        test.beforeEach(async ({ request, page }, testInfo) => {
          if (
            TEXT_ONLY_CONTROL &&
            testInfo.title === P20_TEXT_ONLY_CONTROL_TEST_TITLE
          ) {
            await page.route('**/health', route => route.fulfill({
              json: { capabilities: { vectorSearch: true } }
            }));
          } else {
            await skipWhenVectorSearchDisabled(request, testInfo, 'needs vector');
          }
        });

        test('set search mode to Neural Search and verify persistence', async ({ page }) => {
          await page.goto('/index/products');
        });
      });
    `;

  const analysis = analyzeVectorCapabilityStructure(source, {
    filePath: "/specs/browser-spec.ts",
    resolveModuleSource: (specifier) =>
      specifier === "./p20_negative_control"
        ? P20_NEGATIVE_CONTROL_MODULE
        : undefined,
  });

  expect(analysis.vectorEnabledTestsWithHealthCapabilityOverride).toEqual([
    P20_PROOF_TEST,
  ]);
});

it("detects a health override gated by an unrelated same-named title import", () => {
  const source = `
      import { isP20TextOnlyNegativeControl } from './p20_negative_control';
      import { P20_TEXT_ONLY_CONTROL_TEST_TITLE } from './positive_path_title';

      const TEXT_ONLY_CONTROL = isP20TextOnlyNegativeControl();

      test.describe('vector-enabled settings flows', () => {
        test.beforeEach(async ({ request, page }, testInfo) => {
          if (TEXT_ONLY_CONTROL && testInfo.title === P20_TEXT_ONLY_CONTROL_TEST_TITLE) {
            await page.route('**/health', route => route.fulfill({
              json: { capabilities: { vectorSearch: true } }
            }));
          } else {
            await skipWhenVectorSearchDisabled(request, testInfo, 'needs vector');
          }
        });

        test('set search mode to Neural Search and verify persistence', async ({ page }) => {
          await page.goto('/index/products');
        });

        test('displays search mode and embedders sections with seeded data', async ({ page }) => {
          await page.goto('/index/products/settings');
        });
      });
    `;

  const analysis = analyzeVectorCapabilityStructure(source, {
    filePath: "/specs/browser-spec.ts",
    resolveModuleSource: (specifier) => {
      if (specifier === "./p20_negative_control") {
        return P20_NEGATIVE_CONTROL_MODULE;
      }
      if (specifier === "./positive_path_title") {
        return {
          filePath: "/specs/positive_path_title.ts",
          source: `
              export const P20_TEXT_ONLY_CONTROL_TEST_TITLE =
                'displays search mode and embedders sections with seeded data';
            `,
        };
      }
      return undefined;
    },
  });

  expect(analysis.vectorEnabledTestsWithHealthCapabilityOverride).toContain(
    P20_SIBLING_TEST,
  );
});

it("accepts a health override gated on the negative control for the P20 test", () => {
  const source = `
      import {
        P20_TEXT_ONLY_CONTROL_TEST_TITLE,
        isP20TextOnlyNegativeControl,
      } from './p20_negative_control';

      const TEXT_ONLY_CONTROL = isP20TextOnlyNegativeControl();

      test.describe('vector-enabled settings flows', () => {
        test.beforeEach(async ({ request, page }, testInfo) => {
          if (TEXT_ONLY_CONTROL && testInfo.title === P20_TEXT_ONLY_CONTROL_TEST_TITLE) {
            await page.route('**/health', route => route.fulfill({
              json: { capabilities: { vectorSearch: true } }
            }));
          } else {
            await skipWhenVectorSearchDisabled(request, testInfo, 'needs vector');
          }
        });

        test('set search mode to Neural Search and verify persistence', async ({ page }) => {
          await page.goto('/index/products');
        });
      });
    `;

  const analysis = analyzeVectorCapabilityStructure(
    `${API_HELPERS_IMPORT}\n${source}`,
    {
      filePath: "/specs/browser-spec.ts",
      resolveModuleSource: (specifier) =>
        specifier === "./p20_negative_control"
          ? P20_NEGATIVE_CONTROL_MODULE
          : specifier === "./api-helpers"
            ? API_HELPERS_MODULE
            : undefined,
    },
  );

  expect(analysis.vectorEnabledTestsWithHealthCapabilityOverride).toEqual([]);
  expect(analysis.vectorEnabledTestsWithoutSkipGuard).toEqual([]);
});
