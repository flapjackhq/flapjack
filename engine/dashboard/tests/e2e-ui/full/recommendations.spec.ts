import { test, expect, TEST_INDEX } from '../helpers'
import type { APIRequestContext, Locator, Page } from '@playwright/test'
import {
  addDocuments,
  flushAnalytics,
  getDebugEvents,
  getRecommendations,
  searchIndex,
  sendEvents,
} from '../../fixtures/api-helpers'
import { RECOMMENDATION_MODEL_METADATA } from '../../../src/lib/recommendation-contract'

interface SeededRecommendationData {
  anchorObjectID: string
  relatedObjectID: string
  secondaryObjectID: string
  emptyObjectID: string
  dominantBrand: string
  secondaryBrand: string
}

function uniqueSuffix() {
  return `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`
}

function buildSeededRecommendationData(): SeededRecommendationData {
  const suffix = uniqueSuffix()
  return {
    anchorObjectID: `rec-anchor-${suffix}`,
    relatedObjectID: `rec-related-${suffix}`,
    secondaryObjectID: `rec-secondary-${suffix}`,
    emptyObjectID: `rec-empty-${suffix}`,
    dominantBrand: `RecBrandA-${suffix}`,
    secondaryBrand: `RecBrandB-${suffix}`,
  }
}

function buildRecommendationDocuments(seeded: SeededRecommendationData) {
  return [
    {
      objectID: seeded.anchorObjectID,
      name: `Anchor Product ${seeded.anchorObjectID}`,
      brand: seeded.dominantBrand,
      category: 'Recommendations',
    },
    {
      objectID: seeded.relatedObjectID,
      name: `Related Product ${seeded.relatedObjectID}`,
      brand: seeded.dominantBrand,
      category: 'Recommendations',
    },
    {
      objectID: seeded.secondaryObjectID,
      name: `Secondary Product ${seeded.secondaryObjectID}`,
      brand: seeded.secondaryBrand,
      category: 'Recommendations',
    },
    {
      objectID: seeded.emptyObjectID,
      name: `No Event Product ${seeded.emptyObjectID}`,
      brand: `RecBrandEmpty-${seeded.emptyObjectID}`,
      category: 'Recommendations',
    },
  ]
}

function buildRecommendationEvents(seeded: SeededRecommendationData) {
  return [
    {
      eventType: 'conversion',
      eventName: `rec-anchor-a-${seeded.anchorObjectID}`,
      index: TEST_INDEX,
      userToken: `rec-user-a-${seeded.anchorObjectID}`,
      objectIDs: [seeded.anchorObjectID],
    },
    {
      eventType: 'conversion',
      eventName: `rec-related-a-${seeded.relatedObjectID}`,
      index: TEST_INDEX,
      userToken: `rec-user-a-${seeded.anchorObjectID}`,
      objectIDs: [seeded.relatedObjectID],
    },
    {
      eventType: 'conversion',
      eventName: `rec-anchor-b-${seeded.anchorObjectID}`,
      index: TEST_INDEX,
      userToken: `rec-user-b-${seeded.anchorObjectID}`,
      objectIDs: [seeded.anchorObjectID],
    },
    {
      eventType: 'conversion',
      eventName: `rec-related-b-${seeded.relatedObjectID}`,
      index: TEST_INDEX,
      userToken: `rec-user-b-${seeded.anchorObjectID}`,
      objectIDs: [seeded.relatedObjectID],
    },
    {
      eventType: 'conversion',
      eventName: `rec-anchor-c-${seeded.anchorObjectID}`,
      index: TEST_INDEX,
      userToken: `rec-user-c-${seeded.anchorObjectID}`,
      objectIDs: [seeded.anchorObjectID],
    },
    {
      eventType: 'conversion',
      eventName: `rec-secondary-c-${seeded.secondaryObjectID}`,
      index: TEST_INDEX,
      userToken: `rec-user-c-${seeded.anchorObjectID}`,
      objectIDs: [seeded.secondaryObjectID],
    },
    {
      eventType: 'conversion',
      eventName: `rec-related-d-${seeded.relatedObjectID}`,
      index: TEST_INDEX,
      userToken: `rec-user-d-${seeded.anchorObjectID}`,
      objectIDs: [seeded.relatedObjectID],
    },
    {
      eventType: 'conversion',
      eventName: `rec-secondary-d-${seeded.secondaryObjectID}`,
      index: TEST_INDEX,
      userToken: `rec-user-d-${seeded.anchorObjectID}`,
      objectIDs: [seeded.secondaryObjectID],
    },
  ]
}

async function expectRecommendationHits(
  request: APIRequestContext,
  recommendationRequest: Parameters<typeof getRecommendations>[1]['requests'][number],
) {
  await expect(async () => {
    await flushAnalytics(request, TEST_INDEX)
    const response = await getRecommendations(request, {
      requests: [recommendationRequest],
    })
    expect(response.results[0]?.hits.length ?? 0).toBeGreaterThan(0)
  }).toPass({ timeout: 30_000 })
}

async function expectPreviousModelMarkerCleared(
  results: Locator,
  previousModelMarker: string | null,
) {
  if (previousModelMarker) {
    await expect(results).not.toContainText(previousModelMarker)
  }
}

async function verifyAndFillModelInputs(
  page: Page,
  model: typeof RECOMMENDATION_MODEL_METADATA[number],
  seeded: SeededRecommendationData,
) {
  const submitButton = page.getByTestId('get-recommendations-btn')
  const objectInput = page.getByTestId('recommendations-object-input')
  const facetInput = page.getByTestId('recommendations-facet-input')

  if (model.requiresObjectID) {
    await expect(objectInput).toBeVisible()
    await objectInput.fill('')
    await expect(submitButton).toBeDisabled()
    await objectInput.fill(model.id === 'looking-similar' ? seeded.relatedObjectID : seeded.anchorObjectID)
  } else {
    await expect(objectInput).toHaveCount(0)
  }

  if (model.requiresFacetName) {
    await expect(facetInput).toBeVisible()
    await facetInput.fill('')
    await expect(submitButton).toBeDisabled()
    await facetInput.fill('brand')
  } else {
    await expect(facetInput).toHaveCount(0)
  }
}

async function expectRecommendationResultForModel(
  results: Locator,
  model: typeof RECOMMENDATION_MODEL_METADATA[number],
) {
  await expect(async () => {
    const resultText = await results.textContent() ?? ''
    expect(resultText).not.toContain('Submit a preview request to view recommendations.')
    if (resultText.includes('No recommendations found.')) {
      return
    }

    expect(resultText).toContain('processingTimeMS:')
    if (model.id === 'trending-facets') {
      expect(resultText).toContain('brand')
    }
  }).toPass({ timeout: 10_000 })
}

async function getRecommendationResultMarker(
  results: Locator,
  seeded: SeededRecommendationData,
): Promise<string | null> {
  const resultText = await results.textContent() ?? ''
  if (resultText.includes('No recommendations found.')) {
    return null
  }
  if (resultText.includes(seeded.dominantBrand)) {
    return seeded.dominantBrand
  }
  if (resultText.includes(seeded.relatedObjectID)) {
    return seeded.relatedObjectID
  }
  if (resultText.includes(seeded.secondaryObjectID)) {
    return seeded.secondaryObjectID
  }
  return seeded.anchorObjectID
}

async function seedRecommendationsData(request: APIRequestContext): Promise<SeededRecommendationData> {
  const seeded = buildSeededRecommendationData()

  await addDocuments(request, TEST_INDEX, buildRecommendationDocuments(seeded))
  await expect(async () => {
    const response = await searchIndex(request, TEST_INDEX, seeded.anchorObjectID)
    expect(response.nbHits ?? 0).toBeGreaterThan(0)
  }).toPass({ timeout: 15_000 })
  await sendEvents(request, buildRecommendationEvents(seeded))
  await flushAnalytics(request, TEST_INDEX)
  await expect(async () => {
    await flushAnalytics(request, TEST_INDEX)
    const response = await getDebugEvents(request, {
      index: TEST_INDEX,
      eventType: 'conversion',
      limit: 200,
    })
    const anchorEventCount = response.events.filter((event) => {
      const objectIds = (event as { objectIds?: string[] }).objectIds ?? []
      const httpCode = (event as { httpCode?: number }).httpCode
      return httpCode === 200 && objectIds.includes(seeded.anchorObjectID)
    }).length
    const relatedEventCount = response.events.filter((event) => {
      const objectIds = (event as { objectIds?: string[] }).objectIds ?? []
      const httpCode = (event as { httpCode?: number }).httpCode
      return httpCode === 200 && objectIds.includes(seeded.relatedObjectID)
    }).length
    expect(anchorEventCount).toBeGreaterThanOrEqual(3)
    expect(relatedEventCount).toBeGreaterThanOrEqual(2)
  }).toPass({ timeout: 30_000 })

  await expectRecommendationHits(request, {
    indexName: TEST_INDEX,
    model: 'related-products',
    objectID: seeded.anchorObjectID,
  })
  await expectRecommendationHits(request, {
    indexName: TEST_INDEX,
    model: 'trending-facets',
    facetName: 'brand',
  })

  return seeded
}

test.describe.configure({ mode: 'serial' })

test.describe('Recommendations page', () => {
  test('load-and-verify: recommendations page renders real preview controls', async ({ page }) => {
    await page.goto(`/index/${TEST_INDEX}/recommendations`)

    await expect(page.getByTestId('recommendations-model-select')).toBeVisible({ timeout: 10_000 })
    await expect(page.getByTestId('get-recommendations-btn')).toBeVisible()
    await expect(page.getByTestId('recommendations-results')).toBeVisible()
  })

  test('all five recommendation models: model switching enforces inputs and renders result-or-empty states', async ({
    page,
    request,
  }) => {
    const seeded = await seedRecommendationsData(request)
    const results = page.getByTestId('recommendations-results')
    const modelSelect = page.getByTestId('recommendations-model-select')
    const submitButton = page.getByTestId('get-recommendations-btn')

    await page.goto(`/index/${TEST_INDEX}/recommendations`)
    await expect(results).toContainText('Submit a preview request to view recommendations.')

    let previousModelMarker: string | null = null

    for (const model of RECOMMENDATION_MODEL_METADATA) {
      await modelSelect.selectOption(model.id)
      await expect(modelSelect).toHaveValue(model.id)
      await expect(results).toContainText('Submit a preview request to view recommendations.')
      await expect(page.getByRole('alert')).toHaveCount(0)
      await expectPreviousModelMarkerCleared(results, previousModelMarker)
      await verifyAndFillModelInputs(page, model, seeded)

      await expect(submitButton).toBeEnabled()
      await submitButton.click()
      await expectRecommendationResultForModel(results, model)
      previousModelMarker = await getRecommendationResultMarker(results, seeded)
    }
  })

  test('keeps placeholder, success, empty, and submit-error recommendation states distinct', async ({ page, request }) => {
    const seeded = await seedRecommendationsData(request)
    const results = page.getByTestId('recommendations-results')

    await page.goto(`/index/${TEST_INDEX}/recommendations`)
    await expect(results).toContainText('Submit a preview request to view recommendations.')

    await page.getByTestId('recommendations-model-select').selectOption('related-products')
    await page.getByTestId('recommendations-object-input').fill(seeded.anchorObjectID)
    await page.getByTestId('get-recommendations-btn').click()
    await expect(async () => {
      const resultText = await results.textContent() ?? ''
      expect(resultText).toContain(seeded.relatedObjectID)
    }).toPass({ timeout: 10_000 })

    await page.getByTestId('recommendations-object-input').fill(seeded.emptyObjectID)
    await page.getByTestId('get-recommendations-btn').click()
    await expect(results).toContainText('No recommendations found.')
    await expect(results).not.toContainText(seeded.relatedObjectID)

    await page.goto('/index/e2e..bad/recommendations')
    await expect(results).toContainText('Submit a preview request to view recommendations.')
    await page.getByTestId('recommendations-model-select').selectOption('trending-items')
    await expect(page.getByTestId('get-recommendations-btn')).toBeEnabled()
    await page.getByTestId('get-recommendations-btn').click()

    const errorAlert = page.getByRole('alert')
    await expect(errorAlert).toBeVisible({ timeout: 10_000 })
    await expect(errorAlert).toContainText('400')
    await expect(results).toContainText('Submit a preview request to view recommendations.')
    await expect(results).not.toContainText('No recommendations found.')
    await expect(results).not.toContainText(seeded.relatedObjectID)
  })
})
