import { FormEvent, useEffect, useMemo, useRef, useState } from 'react'
import { Link, useParams } from 'react-router-dom'
import { Card } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { useRecommendations } from '@/hooks/useRecommendations'
import { RECOMMENDATION_MODEL_METADATA } from '@/lib/recommendation-contract'
import type {
  RecommendationHit,
  RecommendationModel,
  RecommendationTrendingFacetHit,
  RecommendationResult,
} from '@/lib/types'

function isTrendingFacetHit(hit: RecommendationHit): hit is RecommendationTrendingFacetHit {
  return typeof (hit as RecommendationTrendingFacetHit).facetName === 'string'
    && typeof (hit as RecommendationTrendingFacetHit).facetValue === 'string'
}

function hasAnyHits(results: RecommendationResult[]): boolean {
  return results.some((result) => result.hits.length > 0)
}

const DEFAULT_MODEL = RECOMMENDATION_MODEL_METADATA[0]?.id ?? 'related-products'

export function Recommendations() {
  const { indexName } = useParams<{ indexName: string }>()
  const { mutateAsync, isPending } = useRecommendations()
  const [selectedModel, setSelectedModel] = useState<RecommendationModel>(DEFAULT_MODEL)
  const [objectID, setObjectID] = useState('')
  const [facetName, setFacetName] = useState('')
  const [facetValue, setFacetValue] = useState('')
  const [results, setResults] = useState<RecommendationResult[] | null>(null)
  const [submitError, setSubmitError] = useState<string | null>(null)
  const previewRequestGenerationRef = useRef(0)

  const activeModel = useMemo(
    () => RECOMMENDATION_MODEL_METADATA.find((model) => model.id === selectedModel) ?? RECOMMENDATION_MODEL_METADATA[0],
    [selectedModel],
  )

  useEffect(() => {
    previewRequestGenerationRef.current += 1
    setResults(null)
    setSubmitError(null)
  }, [indexName])

  const trimmedObjectID = objectID.trim()
  const trimmedFacetName = facetName.trim()
  const trimmedFacetValue = facetValue.trim()
  const missingObjectID = activeModel.requiresObjectID && trimmedObjectID.length === 0
  const missingFacetName = activeModel.requiresFacetName && trimmedFacetName.length === 0
  const isSubmitDisabled = !indexName || isPending || missingObjectID || missingFacetName

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (!indexName || isSubmitDisabled) {
      return
    }

    const request = {
      indexName,
      model: selectedModel,
      ...(activeModel.requiresObjectID ? { objectID: trimmedObjectID } : {}),
      ...(activeModel.requiresFacetName ? { facetName: trimmedFacetName } : {}),
      ...(trimmedFacetValue ? { facetValue: trimmedFacetValue } : {}),
    }

    setSubmitError(null)
    const requestGeneration = previewRequestGenerationRef.current + 1
    previewRequestGenerationRef.current = requestGeneration

    try {
      const previewResults = await mutateAsync(request)
      if (requestGeneration !== previewRequestGenerationRef.current) {
        return
      }
      setResults(previewResults)
    } catch (error) {
      if (requestGeneration !== previewRequestGenerationRef.current) {
        return
      }
      const message = error instanceof Error ? error.message : 'Failed to load recommendations.'
      setSubmitError(message)
      setResults(null)
    }
  }

  if (!indexName) {
    return (
      <Card className="p-8 text-center">
        <h3 className="text-lg font-semibold mb-2">No index selected</h3>
        <p className="text-muted-foreground mb-4">
          Select an index from the Overview page to preview recommendations.
        </p>
        <Link to="/overview">
          <Button>Go to Overview</Button>
        </Link>
      </Card>
    )
  }

  return (
    <div className="space-y-6">
      <div className="space-y-2">
        <h1 className="text-2xl font-semibold">Recommendations</h1>
        <p className="text-sm text-muted-foreground" data-testid="recommendations-index-name">
          {indexName}
        </p>
      </div>

      <Card className="p-6">
        <form className="space-y-4" onSubmit={handleSubmit}>
          <div className="space-y-2">
            <label className="text-sm font-medium" htmlFor="recommendations-model-select">
              Model
            </label>
            <select
              id="recommendations-model-select"
              className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm"
              data-testid="recommendations-model-select"
              value={selectedModel}
              onChange={(event) => {
                previewRequestGenerationRef.current += 1
                setSelectedModel(event.target.value as RecommendationModel)
                setObjectID('')
                setFacetName('')
                setFacetValue('')
                setResults(null)
                setSubmitError(null)
              }}
            >
              {RECOMMENDATION_MODEL_METADATA.map((model) => (
                <option key={model.id} value={model.id}>
                  {model.label}
                </option>
              ))}
            </select>
          </div>

          {activeModel.requiresObjectID && (
            <div className="space-y-2">
              <label className="text-sm font-medium" htmlFor="recommendations-object-input">
                objectID
              </label>
              <input
                id="recommendations-object-input"
                className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm"
                data-testid="recommendations-object-input"
                value={objectID}
                onChange={(event) => setObjectID(event.target.value)}
                required
              />
            </div>
          )}

          {activeModel.requiresFacetName && (
            <div className="space-y-2">
              <label className="text-sm font-medium" htmlFor="recommendations-facet-input">
                facetName
              </label>
              <input
                id="recommendations-facet-input"
                className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm"
                data-testid="recommendations-facet-input"
                value={facetName}
                onChange={(event) => setFacetName(event.target.value)}
                required
              />
              <label className="text-sm font-medium block" htmlFor="recommendations-facet-value-input">
                facetValue (optional)
              </label>
              <input
                id="recommendations-facet-value-input"
                className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm"
                value={facetValue}
                onChange={(event) => setFacetValue(event.target.value)}
              />
            </div>
          )}

          <Button data-testid="get-recommendations-btn" disabled={isSubmitDisabled} type="submit">
            {isPending ? 'Loading...' : 'Get Recommendations'}
          </Button>
        </form>
      </Card>

      <Card className="p-6 space-y-3" data-testid="recommendations-results">
        {submitError && (
          <p className="text-sm text-red-600" role="alert">
            {submitError}
          </p>
        )}
        {results === null && (
          <p className="text-sm text-muted-foreground">Submit a preview request to view recommendations.</p>
        )}
        {results !== null && !hasAnyHits(results) && (
          <p className="text-sm text-muted-foreground">No recommendations found.</p>
        )}
        {results?.map((result, resultIndex) => (
          <div key={`recommendation-result-${resultIndex}`} className="space-y-2 border-t pt-3 first:border-t-0 first:pt-0">
            <p className="text-xs text-muted-foreground">processingTimeMS: {result.processingTimeMS}</p>
            {result.hits.map((hit, hitIndex) => (
              <div key={`recommendation-hit-${resultIndex}-${hitIndex}`} className="min-w-0 rounded-md border p-3 text-sm">
                {isTrendingFacetHit(hit) ? (
                  <p className="break-all">{hit.facetName}: {hit.facetValue}</p>
                ) : (
                  <p className="break-all">{String(hit.objectID)}</p>
                )}
              </div>
            ))}
          </div>
        ))}
      </Card>
    </div>
  )
}
