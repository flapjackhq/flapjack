import { expect, it } from "vitest";
import { analyzeVectorCapabilityStructure } from "./vector-capability-spec-structure";
import {
  analyzeSpec,
  analyzeWithApiHelpers,
} from "./vector-capability-spec-test-helpers";

it("detects vector document seeding without a searchability wait", () => {
  const source = `
      test('runs a semantic proof', async ({ request }) => {
        await addDocumentsWithVectors(request, 'products', [{ objectID: 'chair' }]);
        await searchIndex(request, 'products', 'posture');
      });
    `;

  const analysis = analyzeWithApiHelpers(source);

  expect(analysis.vectorDocumentTestsWithoutReadinessWait).toEqual([
    "runs a semantic proof",
  ]);
});

it("detects namespace-qualified vector document seeding without a searchability wait", () => {
  const source = `
      import * as api from './api-helpers';

      test('runs a semantic proof', async ({ request }) => {
        await api.addDocumentsWithVectors(request, 'products', [{ objectID: 'chair' }]);
        await searchIndex(request, 'products', 'posture');
      });
    `;

  const analysis = analyzeWithApiHelpers(source);

  expect(analysis.vectorDocumentTestsWithoutReadinessWait).toEqual([
    "runs a semantic proof",
  ]);
});

it("accepts namespace-qualified vector document seeding with a later searchability wait", () => {
  const source = `
      import * as api from './api-helpers';

      test('runs a semantic proof', async ({ request }) => {
        await api.addDocumentsWithVectors(request, 'products', [{ objectID: 'chair' }]);
        await api.waitForSearchableObjectIds(request, 'products', 'posture', ['chair']);
      });
    `;

  const analysis = analyzeWithApiHelpers(source);

  expect(analysis.vectorDocumentTestsWithoutReadinessWait).toEqual([]);
});

it("detects vector document seeding without a searchability wait in ancestor setup", () => {
  const source = `
      test.describe('vector-enabled settings flows', () => {
        test.beforeAll(async ({ request }) => {
          await addDocumentsWithVectors(request, 'products', [{ objectID: 'chair' }]);
        });

        test('runs a semantic proof', async ({ page }) => {
          await page.goto('/index/products');
        });
      });
    `;

  const analysis = analyzeWithApiHelpers(source);

  expect(analysis.vectorDocumentTestsWithoutReadinessWait).toEqual([
    "runs a semantic proof",
  ]);
});

it("detects a searchability wait that runs before the vector document seed", () => {
  const source = `
      test('runs a semantic proof', async ({ request }) => {
        await waitForSearchableObjectIds(request, 'products', 'posture', ['chair']);
        await addDocumentsWithVectors(request, 'products', [{ objectID: 'chair' }]);
        await searchIndex(request, 'products', 'posture');
      });
    `;

  const analysis = analyzeWithApiHelpers(source);

  expect(analysis.vectorDocumentTestsWithoutReadinessWait).toEqual([
    "runs a semantic proof",
  ]);
});

it("rejects an inherited searchability wait for unrelated documents", () => {
  const source = `
      test.describe('vector-enabled settings flows', () => {
        test.beforeEach(async ({ request }) => {
          await waitForSearchableObjectIds(request, 'products', 'legacy', ['legacy-doc']);
        });

        test('runs a semantic proof', async ({ request }) => {
          await addDocumentsWithVectors(request, 'products', [{ objectID: 'chair' }]);
        });
      });
    `;

  const analysis = analyzeWithApiHelpers(source);

  expect(analysis.vectorDocumentTestsWithoutReadinessWait).toEqual([
    "runs a semantic proof",
  ]);
});

it("rejects a later searchability wait for documents the seed did not add", () => {
  const source = `
      test('runs a semantic proof', async ({ request }) => {
        await addDocumentsWithVectors(request, 'products', [{ objectID: 'chair' }]);
        await waitForSearchableObjectIds(request, 'products', 'legacy', ['legacy-doc']);
      });
    `;

  const analysis = analyzeWithApiHelpers(source);

  expect(analysis.vectorDocumentTestsWithoutReadinessWait).toEqual([
    "runs a semantic proof",
  ]);
});

it("rejects a later searchability wait for the same objectID in another index", () => {
  const source = `
      const SEEDED_INDEX = 'products';
      const OTHER_INDEX = 'archived-products';

      test('runs a semantic proof', async ({ request }) => {
        await addDocumentsWithVectors(request, SEEDED_INDEX, [{ objectID: 'chair' }]);
        await waitForSearchableObjectIds(request, OTHER_INDEX, 'posture', ['chair']);
      });
    `;

  const analysis = analyzeWithApiHelpers(source);

  expect(analysis.vectorDocumentTestsWithoutReadinessWait).toEqual([
    "runs a semantic proof",
  ]);
});

it("rejects a seed whose seeded objectIDs cannot be resolved statically", () => {
  const source = `
      test('runs a semantic proof', async ({ request }) => {
        await addDocumentsWithVectors(request, 'products', buildProofDocuments());
        await waitForSearchableObjectIds(request, 'products', 'posture', ['chair']);
      });
    `;

  const analysis = analyzeWithApiHelpers(source);

  expect(analysis.vectorDocumentTestsWithoutReadinessWait).toEqual([
    "runs a semantic proof",
  ]);
});

it("rejects partial searchability coverage for a multi-document seed", () => {
  const source = `
      const PROOF_TARGET_ID = 'semantic-chair';
      const PROOF_DOCUMENTS = [
        { objectID: PROOF_TARGET_ID, name: 'Lumbar Support Chair' },
        { objectID: 'keyword-decoy', name: 'Ergonomic Posture Keyword Decoy' },
      ];

      test('runs a semantic proof', async ({ request }) => {
        await addDocumentsWithVectors(request, 'products', PROOF_DOCUMENTS);
        await waitForSearchableObjectIds(request, 'products', 'posture', [PROOF_TARGET_ID], {
          mode: 'neuralSearch',
        });
      });
    `;

  const analysis = analyzeWithApiHelpers(source);

  expect(analysis.vectorDocumentTestsWithoutReadinessWait).toEqual([
    "runs a semantic proof",
  ]);
});

it("accepts complete searchability coverage that resolves shared objectID constants", () => {
  const source = `
      const PROOF_TARGET_ID = 'semantic-chair';
      const PROOF_DOCUMENTS = [
        { objectID: PROOF_TARGET_ID, name: 'Lumbar Support Chair' },
        { objectID: 'keyword-decoy', name: 'Ergonomic Posture Keyword Decoy' },
      ];

      test('runs a semantic proof', async ({ request }) => {
        await addDocumentsWithVectors(request, 'products', PROOF_DOCUMENTS);
        await waitForSearchableObjectIds(request, 'products', 'posture', [
          PROOF_TARGET_ID,
          'keyword-decoy',
        ], {
          mode: 'neuralSearch',
        });
      });
    `;

  const analysis = analyzeWithApiHelpers(source);

  expect(analysis.vectorDocumentTestsWithoutReadinessWait).toEqual([]);
});

it("requires every seed to be followed by its own searchability wait", () => {
  const source = `
      test('runs a semantic proof', async ({ request }) => {
        await addDocumentsWithVectors(request, 'products', [{ objectID: 'chair' }]);
        await waitForSearchableObjectIds(request, 'products', 'posture', ['chair']);
        await addDocumentsWithVectors(request, 'products', [{ objectID: 'desk' }]);
      });
    `;

  const analysis = analyzeWithApiHelpers(source);

  expect(analysis.vectorDocumentTestsWithoutReadinessWait).toEqual([
    "runs a semantic proof",
  ]);
});

it("accepts vector document seeding with a searchability wait", () => {
  const source = `
      async function waitForProofDocuments(request, indexName) {
        await waitForSearchableObjectIds(request, indexName, 'posture', ['chair']);
      }

      test('runs a semantic proof', async ({ request }) => {
        await addDocumentsWithVectors(request, 'products', [{ objectID: 'chair' }]);
        await waitForProofDocuments(request, 'products');
      });
    `;

  const analysis = analyzeWithApiHelpers(source);

  expect(analysis.vectorDocumentTestsWithoutReadinessWait).toEqual([]);
});

it("detects helper-mediated capability checks in ancestor setup", () => {
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

  const analysis = analyzeWithApiHelpers(source);

  expect(analysis.compiledOutTestCount).toBe(1);
  expect(analysis.capabilityGatedCompiledOutTests).toEqual(["fallback"]);
});

it("detects helper-mediated compiled-out route setup", () => {
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

  const analysis = analyzeWithApiHelpers(source);

  expect(analysis.compiledOutTestCount).toBe(2);
  expect(analysis.compiledOutTestsWithoutEmbedderSetup).toEqual([]);
});

it("detects compiled-out tests registered by a local helper", () => {
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

  const analysis = analyzeWithApiHelpers(source);

  expect(analysis.compiledOutTestCount).toBe(1);
  expect(analysis.capabilityGatedCompiledOutTests).toEqual([
    "helper-registered fallback",
  ]);
  expect(analysis.compiledOutTestsWithoutEmbedderSetup).toEqual([
    "helper-registered fallback",
  ]);
});

it("does not count an ordinary vectorSearch:false expected-value object as compiled-out coverage", () => {
  const source = `
      test('reports the compiled-out capability payload shape', async ({ request }) => {
        const health = await fetchHealth(request);
        expect(health).toEqual({ capabilities: { vectorSearch: false } });
      });
    `;

  expect(analyzeVectorCapabilityStructure(source).compiledOutTestCount).toBe(0);
});

it("does not count a helper-returned expected-value object as compiled-out coverage", () => {
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

it("ignores comments and titles that merely mention compiled-out behavior", () => {
  const source = `
      // isVectorSearchEnabled and vectorSearch: false are not executable here.
      test('compiled-out behavior is documented', async ({ page }) => {
        await page.goto('/');
      });
    `;

  expect(analyzeVectorCapabilityStructure(source).compiledOutTestCount).toBe(0);
});

it("requires every compiled-out hybrid test to establish an embedder precondition", () => {
  const analysis = analyzeSpec("hybrid-search.spec.ts");

  expect(analysis.compiledOutTestsWithoutEmbedderSetup).toEqual([]);
});
