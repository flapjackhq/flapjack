import { expect, test } from '../helpers'
import {
  addDocuments,
  createIndex,
  deleteIndex,
  updateSettings,
} from '../../fixtures/api-helpers'

const suffix = `${Date.now()}`
const recommendationIndex = `e2e-recommend-${suffix}`
const seedObjectID = `rec-seed-${suffix}`
const relatedObjectID = `rec-related-${'x'.repeat(96)}-${suffix}`

test.describe('Recommendations page', () => {
  test.beforeAll(async ({ request }) => {
    await createIndex(request, recommendationIndex)
    await updateSettings(request, recommendationIndex, {
      searchableAttributes: ['name'],
    })
    await addDocuments(request, recommendationIndex, [
      {
        objectID: seedObjectID,
        name: 'Wireless Bluetooth headphones with active noise cancelling',
      },
      {
        objectID: relatedObjectID,
        name: 'Wireless Bluetooth headphones with active noise cancelling travel',
      },
      {
        objectID: `rec-unrelated-${suffix}`,
        name: 'Ceramic coffee grinder',
      },
    ])
  })

  test.afterAll(async ({ request }) => {
    await deleteIndex(request, recommendationIndex)
  })

  test('previews one exact non-vector result through the real Full/OSS host', async ({ page }) => {
    await page.goto(`/index/${recommendationIndex}/recommendations`)

    await expect(page.getByRole('heading', { name: 'Recommendations', exact: true })).toBeVisible()
    await expect(page.getByTestId('recommendations-index-name')).toHaveText(recommendationIndex)

    const modelSelect = page.getByTestId('recommendations-model-select')
    const objectInput = page.getByTestId('recommendations-object-input')
    const submit = page.getByTestId('get-recommendations-btn')
    await expect(modelSelect.getByRole('option')).toHaveText([
      'Related Products',
      'Bought Together',
      'Trending Items',
      'Trending Facets',
      'Looking Similar',
    ])
    await modelSelect.selectOption('looking-similar')
    await objectInput.fill(seedObjectID)
    await submit.click()

    const results = page.getByTestId('recommendations-results')
    await expect(results).toContainText(relatedObjectID)
    await expect(results).not.toContainText(seedObjectID)
    await expect(results).toContainText('processingTimeMS:')

    await page.setViewportSize({ width: 390, height: 844 })
    await expect(page.getByRole('heading', { name: 'Recommendations', exact: true })).toBeVisible()
    await expect(modelSelect).toBeVisible()
    await expect(objectInput).toBeVisible()
    await expect(submit).toBeVisible()
    await expect(modelSelect).toBeInViewport()
    await expect(objectInput).toBeInViewport()
    await expect(submit).toBeInViewport()
    await expect(results.getByText(relatedObjectID)).toHaveCSS('word-break', 'break-all')
  })
})
