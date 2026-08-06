import { expect, it } from "vitest";
import { analyzeVectorCapabilityStructure } from "./vector-capability-spec-structure";
import {
  NAVIGATION_VECTOR_ENABLED_TEST,
  analyzeSpec,
  analyzeWithApiHelpers,
  readSpec,
} from "./vector-capability-spec-test-helpers";

it.each([
  "chat.spec.ts",
  "hybrid-search.spec.ts",
  "vector-settings.spec.ts",
  "navigation.spec.ts",
])(
  "%s registers compiled-out tests outside capability-gated setup",
  (fileName) => {
    const analysis = analyzeSpec(fileName);

    expect(analysis.compiledOutTestCount).toBeGreaterThan(0);
    expect(analysis.capabilityGatedCompiledOutTests).toEqual([]);
  },
);

it.each([
  "chat.spec.ts",
  "hybrid-search.spec.ts",
  "vector-settings.spec.ts",
  "navigation.spec.ts",
])(
  "%s skips vector-enabled tests when vector search is compiled out",
  (fileName) => {
    const analysis = analyzeSpec(fileName);

    expect(analysis.vectorEnabledTestsWithoutSkipGuard).toEqual([]);
  },
);

it.each([
  "chat.spec.ts",
  "hybrid-search.spec.ts",
  "vector-settings.spec.ts",
  "navigation.spec.ts",
])("%s exercises the real browser vector capability owner", (fileName) => {
  const analysis = analyzeSpec(fileName);

  expect(analysis.vectorEnabledTestsWithHealthCapabilityOverride).toEqual([]);
});

it("vector-settings.spec.ts waits for vector documents to become searchable after seeding", () => {
  const analysis = analyzeSpec("vector-settings.spec.ts");

  expect(analysis.vectorDocumentTestsWithoutReadinessWait).toEqual([]);
});

it("detects the live P20 proof when its seed is moved after its readiness waits", () => {
  const seedCall = `        await addDocumentsWithVectors(
          request,
          vectorTestIndex,
          VECTOR_PROOF_TARGET_DOCUMENTS,
        );
`;
  const keywordMissAssertion =
    "        await expectKeywordOnlyMissesVectorTarget(request, vectorTestIndex);\n";
  const spec = readSpec("vector-settings.spec.ts");
  expect(spec).toContain(seedCall);
  expect(spec).toContain(keywordMissAssertion);

  const mutatedSpec = spec
    .replace(seedCall, "")
    .replace(keywordMissAssertion, seedCall + keywordMissAssertion);

  const analysis = analyzeSpec("vector-settings.spec.ts", () => mutatedSpec);

  expect(analysis.vectorDocumentTestsWithoutReadinessWait).toEqual([
    "set search mode to Neural Search and verify persistence",
  ]);
});

it("detects the live P20 proof when only the semantic-target readiness wait is removed", () => {
  const semanticTargetWait = `  await waitForSearchableObjectIds(
    request,
    indexName,
    VECTOR_PROOF_QUERY,
    [VECTOR_PROOF_TARGET_ID],
    { hitsPerPage: 10, mode: 'neuralSearch' },
  );
`;
  const spec = readSpec("vector-settings.spec.ts");
  expect(spec).toContain(semanticTargetWait);

  const analysis = analyzeSpec("vector-settings.spec.ts", () =>
    spec.replace(semanticTargetWait, ""),
  );

  expect(analysis.vectorDocumentTestsWithoutReadinessWait).toEqual([
    "set search mode to Neural Search and verify persistence",
  ]);
});

it("detects the standalone navigation vector-enabled test when its skip guard is removed", () => {
  const mutatedNavigationSpec = readSpec("navigation.spec.ts").replace(
    `
    await skipWhenVectorSearchDisabled(
      request,
      testInfo,
      'Chat tab navigation requires a vector-search-enabled build',
    );
`,
    "",
  );

  const analysis = analyzeSpec(
    "navigation.spec.ts",
    () => mutatedNavigationSpec,
  );

  expect(analysis.vectorEnabledTestsWithoutSkipGuard).toContain(
    NAVIGATION_VECTOR_ENABLED_TEST,
  );
});

it("detects vector-enabled describes without a skip guard", () => {
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
    "persists vector settings",
  ]);
});

it("accepts helper-mediated vector skip guards", () => {
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

  const analysis = analyzeWithApiHelpers(source);

  expect(analysis.vectorEnabledTestsWithoutSkipGuard).toEqual([]);
});

it("recognizes namespace-qualified vector enabled operations and skip guards", () => {
  const source = `
      import * as api from './api-helpers';

      test.describe('vector-enabled settings flows', () => {
        test('guarded namespace proof', async ({ request, page }, testInfo) => {
          await api.skipWhenVectorSearchDisabled(request, testInfo, 'needs vector');
          await page.goto('/index/products');
        });
      });

      test('unguarded namespace proof', async ({ request }) => {
        await api.configureEmbedder(request, 'products', 'default', {
          source: 'userProvided',
          dimensions: 384,
        });
      });
    `;

  const analysis = analyzeWithApiHelpers(source);

  expect(analysis.vectorEnabledTestsWithoutSkipGuard).toEqual([
    "unguarded namespace proof",
  ]);
});

it("accepts namespace-qualified canonical fixture sentinels", () => {
  const source = `
      import * as api from './api-helpers';

      test.describe('vector-enabled settings flows', () => {
        test.beforeEach(async ({ request }, testInfo) => {
          await api.skipWhenVectorSearchDisabled(request, testInfo, 'needs vector');
          await api.configureEmbedder(request, 'products', 'default', {
            source: 'userProvided',
            dimensions: 384,
          });
        });

        test('compiled fallback', async ({ page }) => {
          await page.route('**/health', route => route.fulfill({
            json: { capabilities: { vectorSearch: false } }
          }));
        });

        test('semantic proof', async ({ request }) => {
          await api.addDocumentsWithVectors(request, 'products', [{ objectID: 'chair' }]);
          await api.waitForSearchableObjectIds(request, 'products', 'posture', ['chair']);
        });
      });
    `;

  const analysis = analyzeWithApiHelpers(source);

  expect(analysis.vectorEnabledTestsWithoutSkipGuard).toEqual([]);
  expect(analysis.compiledOutTestsWithoutEmbedderSetup).toEqual([]);
  expect(analysis.vectorDocumentTestsWithoutReadinessWait).toEqual([]);
});

it("detects nested-receiver vector document seeding without a readiness wait", () => {
  const source = `
      import * as api from './api-helpers';

      const fixtures = { api };

      test('nested receiver semantic proof', async ({ request }) => {
        await fixtures.api.addDocumentsWithVectors(
          request,
          'products',
          [{ objectID: 'chair' }],
        );
      });
    `;

  const analysis = analyzeWithApiHelpers(source);

  expect(analysis.vectorDocumentTestsWithoutReadinessWait).toEqual([
    "nested receiver semantic proof",
  ]);
});

it("detects an object-namespaced local health override helper", () => {
  const source = `
      import { skipWhenVectorSearchDisabled } from './api-helpers';

      async function installOverride(page) {
        await page.route('**/health', route => route.fulfill({
          json: { capabilities: { vectorSearch: true } }
        }));
      }
      const suite = { installOverride };

      test.describe('vector-enabled settings flows', () => {
        test.beforeEach(async ({ request }, testInfo) => {
          await skipWhenVectorSearchDisabled(request, testInfo, 'needs vector');
        });

        test('uses nested helper receiver', async ({ page }) => {
          await suite.installOverride(page);
          await page.goto('/index/products');
        });
      });
    `;

  const analysis = analyzeWithApiHelpers(source);

  expect(analysis.vectorEnabledTestsWithHealthCapabilityOverride).toEqual([
    "uses nested helper receiver",
  ]);
});

it("rejects namespace-qualified sentinel exports from unrelated helper modules", () => {
  const source = `
      import * as api from './fake-api-helpers';

      test.describe('vector-enabled settings flows', () => {
        test.beforeEach(async ({ request }, testInfo) => {
          await api.skipWhenVectorSearchDisabled(request, testInfo, 'needs vector');
        });

        test('spoofed helper module guard', async ({ page }) => {
          await page.goto('/index/products');
        });
      });
    `;

  const analysis = analyzeVectorCapabilityStructure(source, {
    filePath: "/specs/browser-spec.ts",
    resolveModuleSource: (specifier) =>
      specifier === "./fake-api-helpers"
        ? {
            filePath: "/specs/fake-api-helpers.ts",
            source: `
                export async function skipWhenVectorSearchDisabled() {}
              `,
          }
        : undefined,
  });

  expect(analysis.vectorEnabledTestsWithoutSkipGuard).toEqual([
    "spoofed helper module guard",
  ]);
});

it("rejects namespace-qualified sentinel exports from unrelated same-basename modules", () => {
  const source = `
      import * as api from './unrelated/api-helpers';

      test.describe('vector-enabled settings flows', () => {
        test.beforeEach(async ({ request }, testInfo) => {
          await api.skipWhenVectorSearchDisabled(request, testInfo, 'needs vector');
        });

        test('spoofed same-basename helper guard', async ({ page }) => {
          await page.goto('/index/products');
        });
      });
    `;

  const analysis = analyzeVectorCapabilityStructure(source, {
    filePath: "/specs/browser-spec.ts",
    resolveModuleSource: (specifier) =>
      specifier === "./unrelated/api-helpers"
        ? {
            filePath: "/specs/unrelated/api-helpers.ts",
            source: `
                export async function skipWhenVectorSearchDisabled() {}
              `,
          }
        : undefined,
  });

  expect(analysis.vectorEnabledTestsWithoutSkipGuard).toEqual([
    "spoofed same-basename helper guard",
  ]);
});

it("rejects an unqualified vector skip guard imported from an unrelated module", () => {
  const source = `
      import { skipWhenVectorSearchDisabled } from './unrelated/api-helpers';

      test.describe('vector-enabled settings flows', () => {
        test.beforeEach(async ({ request }, testInfo) => {
          await skipWhenVectorSearchDisabled(request, testInfo, 'needs vector');
        });

        test('spoofed named-import guard', async ({ page }) => {
          await page.goto('/index/products');
        });
      });
    `;

  const analysis = analyzeVectorCapabilityStructure(source, {
    filePath: "/specs/browser-spec.ts",
    resolveModuleSource: (specifier) =>
      specifier === "./unrelated/api-helpers"
        ? {
            filePath: "/specs/unrelated/api-helpers.ts",
            source: `
                export async function skipWhenVectorSearchDisabled() {}
              `,
          }
        : undefined,
  });

  expect(analysis.vectorEnabledTestsWithoutSkipGuard).toEqual([
    "spoofed named-import guard",
  ]);
});

it("rejects an in-file function that spoofs the unqualified vector skip guard", () => {
  const source = `
      async function skipWhenVectorSearchDisabled() {}

      test.describe('vector-enabled settings flows', () => {
        test.beforeEach(async ({ request }, testInfo) => {
          await skipWhenVectorSearchDisabled(request, testInfo, 'needs vector');
        });

        test('spoofed local guard', async ({ page }) => {
          await page.goto('/index/products');
        });
      });
    `;

  const analysis = analyzeVectorCapabilityStructure(source);

  expect(analysis.vectorEnabledTestsWithoutSkipGuard).toEqual([
    "spoofed local guard",
  ]);
});
