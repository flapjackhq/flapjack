/**
 * @module Stub summary for ruleHelpers.ts.
 */
import type { Rule, RuleCondition, RuleHide, RulePromote } from './types'

export interface ParseRuleEditorJsonResult {
  rule?: Rule
  error?: string
}

export interface PrepareRuleEditorSaveResult extends ParseRuleEditorJsonResult {
  json?: string
}

interface CreateMerchandisingRuleInput {
  query: string
  description?: string
  pins: RulePromote[]
  hides: RuleHide[]
  timestamp?: number
}

function slugify(input: string): string {
  const slug = input
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')

  return slug || 'query'
}

export function normalizeRule(rule: Partial<Rule>): Rule {
  return {
    ...rule,
    conditions: Array.isArray(rule.conditions) ? rule.conditions : [],
    consequence: rule.consequence ?? {},
  } as Rule
}

export function createEmptyRule(timestamp = Date.now()): Rule {
  return {
    objectID: `rule-${timestamp}`,
    conditions: [{ pattern: '' }],
    consequence: {},
    description: '',
    enabled: true,
  }
}

/**
 * Builds a merchandising rule that pins or hides specific results for an exact-match query.
 * 
 * Generates a slugified `objectID` from the query and timestamp, sets an `is` anchoring condition,
 * and populates `promote`/`hide` consequences only when the corresponding arrays are non-empty.
 * 
 * @param input - Configuration for the merchandising rule.
 * @param input.query - The search query this rule targets (exact match).
 * @param input.description - Optional human-readable label; defaults to `Merchandising: "<query>"`.
 * @param input.pins - Documents to promote (pin) to specific positions.
 * @param input.hides - Documents to hide from results.
 * @param input.timestamp - Optional epoch ms used in the objectID; defaults to `Date.now()`.
 * @returns A fully-formed, enabled `Rule` ready for persistence.
 */
export function createMerchandisingRule({
  query,
  description,
  pins,
  hides,
  timestamp = Date.now(),
}: CreateMerchandisingRuleInput): Rule {
  return {
    objectID: `merch-${slugify(query)}-${timestamp}`,
    conditions: [{ pattern: query, anchoring: 'is' }],
    consequence: {
      ...(pins.length > 0 ? { promote: pins } : {}),
      ...(hides.length > 0 ? { hide: hides } : {}),
    },
    description: description || `Merchandising: "${query}"`,
    enabled: true,
  }
}

/**
 * Produces a human-readable summary of a rule's conditions and consequences.
 * 
 * Inspects the first condition for pattern/anchoring, context, and filters, then counts
 * promoted and hidden results and checks for query modifications.
 * 
 * @param rule - The rule to describe.
 * @returns A comma-separated description string, or `"No conditions or consequences"` if the rule is empty.
 */
export function buildRuleDescription(rule: Rule): string {
  const parts: string[] = []
  const condition = rule.conditions[0]

  if (condition) {
    if (condition.pattern && condition.anchoring) {
      parts.push(`When query ${condition.anchoring} "${condition.pattern}"`)
    }
    if (condition.context) {
      parts.push(`When context "${condition.context}"`)
    }
    if (condition.filters) {
      parts.push(`When filters "${condition.filters}"`)
    }
  }

  const promotes = rule.consequence.promote?.length || 0
  const hides = rule.consequence.hide?.length || 0

  if (promotes) parts.push(`pin ${promotes} result${promotes > 1 ? 's' : ''}`)
  if (hides) parts.push(`hide ${hides} result${hides > 1 ? 's' : ''}`)
  if (rule.consequence.params?.query !== undefined) parts.push('modify query')

  return parts.join(', ') || 'No conditions or consequences'
}

function cleanParams(params?: Record<string, unknown>): Record<string, unknown> | undefined {
  if (!params) return undefined

  const clean: Record<string, unknown> = {}
  for (const [key, value] of Object.entries(params)) {
    if (value === undefined || value === null || value === '') continue
    if (Array.isArray(value) && value.length === 0) continue
    clean[key] = value
  }

  return Object.keys(clean).length > 0 ? clean : undefined
}

/**
 * TODO: Document normalizeRuleForSerialization.
 */
export function normalizeRuleForSerialization(rule: Rule): Rule {
  const normalizedRule = normalizeRule(rule)
  const conditions = normalizedRule.conditions
    .map((condition) => {
      const pattern = condition.pattern?.trim()
      const context = condition.context?.trim()
      const filters = condition.filters?.trim()
      const hasAnchoring = condition.anchoring !== undefined
      const hasPattern = Boolean(pattern)
      const hasContext = Boolean(context)
      const hasFilters = Boolean(filters)
      const hasAlternatives = condition.alternatives === true

      if (!hasPattern && !hasAnchoring && !hasContext && !hasFilters && !hasAlternatives) {
        return null
      }

      return {
        ...(hasPattern ? { pattern } : {}),
        ...(hasAnchoring ? { anchoring: condition.anchoring } : {}),
        ...(hasAlternatives ? { alternatives: true } : {}),
        ...(hasContext ? { context } : {}),
        ...(hasFilters ? { filters } : {}),
      } as RuleCondition
    })
    .filter((condition): condition is RuleCondition => condition !== null)

  const consequence = { ...normalizedRule.consequence }
  if (consequence.params) {
    consequence.params = cleanParams(
      consequence.params as unknown as Record<string, unknown>,
    ) as typeof consequence.params
  }
  if (!consequence.promote?.length) delete consequence.promote
  if (!consequence.hide?.length) delete consequence.hide

  const result: Rule = {
    ...normalizedRule,
    conditions,
    consequence,
  }

  if (result.validity && result.validity.length === 0) {
    delete result.validity
  }

  return result
}

/**
 * TODO: Document validateRule.
 */
export function validateRule(rule: Rule): string[] {
  const errors: string[] = []

  rule.conditions.forEach((condition, index) => {
    const hasPattern = Boolean(condition.pattern?.trim())
    const hasAnchoring = condition.anchoring !== undefined

    if (hasPattern && !hasAnchoring) {
      errors.push(`Condition ${index + 1}: anchoring is required when pattern is provided.`)
    }
    if (!hasPattern && hasAnchoring) {
      errors.push(`Condition ${index + 1}: pattern is required when anchoring is selected.`)
    }
  })

  if (rule.consequence.userData !== undefined && rule.consequence.userData !== '') {
    if (typeof rule.consequence.userData === 'string') {
      try {
        JSON.parse(rule.consequence.userData)
      } catch {
        errors.push('Invalid JSON in User Data field.')
      }
    }
  }

  if (rule.consequence.params?.renderingContent !== undefined) {
    if (typeof rule.consequence.params.renderingContent === 'string') {
      try {
        JSON.parse(rule.consequence.params.renderingContent as unknown as string)
      } catch {
        errors.push('Invalid JSON in Rendering Content field.')
      }
    }
  }

  if (rule.consequence.promote?.length) {
    const ids = rule.consequence.promote.map((promote) =>
      'objectID' in promote ? promote.objectID : (promote.objectIDs || []).join(','),
    )
    const seen = new Set<string>()
    for (const id of ids) {
      if (id && seen.has(id)) {
        errors.push('Duplicate objectID in promoted items.')
        break
      }
      if (id) seen.add(id)
    }
  }

  return errors
}

/**
 * TODO: Document prepareRuleEditorSave.
 */
export function prepareRuleEditorSave(rule: Rule): PrepareRuleEditorSaveResult {
  const candidateRule = normalizeRuleForSerialization(rule)
  const validationErrors = validateRule(candidateRule)

  if (validationErrors.length > 0) {
    return { error: validationErrors[0] }
  }

  const serializedRule: Rule = {
    ...candidateRule,
    consequence: {
      ...candidateRule.consequence,
      params: candidateRule.consequence.params
        ? { ...candidateRule.consequence.params }
        : undefined,
    },
  }

  if (typeof serializedRule.consequence.userData === 'string' && serializedRule.consequence.userData) {
    serializedRule.consequence.userData = JSON.parse(serializedRule.consequence.userData)
  } else if (!serializedRule.consequence.userData) {
    delete serializedRule.consequence.userData
  }

  if (
    serializedRule.consequence.params?.renderingContent &&
    typeof serializedRule.consequence.params.renderingContent === 'string'
  ) {
    serializedRule.consequence.params.renderingContent = JSON.parse(
      serializedRule.consequence.params.renderingContent as unknown as string,
    )
  }

  const json = JSON.stringify(serializedRule, null, 2)
  const parsed = parseRuleEditorJson(json)

  return {
    ...parsed,
    json,
  }
}

/**
 * Parses and validates a JSON string from the rule editor into a `Rule`.
 * 
 * Enforces that the parsed value is an object with a non-empty string `objectID` and
 * an object `consequence`, then normalizes the result via `normalizeRule`.
 * 
 * @param json - Raw JSON string to parse.
 * @returns An object containing either the validated `rule` or an `error` message describing the first validation failure.
 */
export function parseRuleEditorJson(json: string): ParseRuleEditorJsonResult {
  try {
    const parsed = JSON.parse(json) as Partial<Rule>

    if (!parsed || typeof parsed !== 'object') {
      return { error: 'rule must be a JSON object' }
    }

    if (parsed.objectID === undefined || parsed.objectID === null) {
      return { error: 'objectID is required' }
    }

    if (typeof parsed.objectID !== 'string' || parsed.objectID.trim().length === 0) {
      return { error: 'objectID must be a non-empty string' }
    }

    if (parsed.consequence === undefined || parsed.consequence === null) {
      return { error: 'consequence is required' }
    }

    if (typeof parsed.consequence !== 'object' || Array.isArray(parsed.consequence)) {
      return { error: 'consequence must be an object' }
    }

    return { rule: normalizeRule(parsed) }
  } catch (error) {
    return {
      error: error instanceof Error ? error.message : 'Invalid JSON',
    }
  }
}
