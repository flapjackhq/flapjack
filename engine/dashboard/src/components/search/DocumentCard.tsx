import { memo, useState, useMemo, lazy, Suspense } from 'react';
import DOMPurify from 'dompurify';
import { ChevronDown, ChevronRight, Copy, Check, Trash2, Code } from 'lucide-react';
import { Card } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Badge } from '@/components/ui/badge';
import type { DisplayPreferences, Document } from '@/lib/types';

/** Sanitize highlight HTML — only allow <em> tags used by the search highlighter. */
function sanitizeHighlightHtml(html: string): string {
  return DOMPurify.sanitize(html, { ALLOWED_TAGS: ['em'], ALLOWED_ATTR: [] });
}

// Lazy load Monaco Editor for performance
const Editor = lazy(() =>
  import('@monaco-editor/react').then((module) => ({
    default: module.default,
  }))
);

const PREVIEW_FIELD_COUNT = 6;
const HIGHLIGHT_MARKUP_CLASS_NAME =
  '[&>em]:bg-yellow-200 dark:[&>em]:bg-yellow-800 [&>em]:not-italic [&>em]:font-medium [&>em]:rounded-sm [&>em]:px-0.5';

interface HighlightResultValue {
  value: string;
  matchLevel: 'none' | 'partial' | 'full';
  matchedWords?: string[];
  fullyHighlighted?: boolean;
}

type HighlightResult = Record<string, HighlightResultValue | HighlightResultValue[] | Record<string, unknown>>;
type FieldData = Record<string, unknown>;

interface DocumentCardProps {
  document: Document;
  fieldOrder?: string[];
  displayPreferences?: DisplayPreferences | null;
  onDelete?: (objectID: string) => void;
  isDeleting?: boolean;
  onClick?: () => void;
}

/**
 * Get the highlighted HTML string for a field, falling back to plain value.
 * Returns an object with { html, hasMatch } so we can style matched fields.
 */
function getFieldDisplay(
  key: string,
  rawValue: unknown,
  highlightResult?: HighlightResult
): { html: string; hasMatch: boolean } {
  const hr = highlightResult?.[key];

  // Handle single highlight result
  if (hr && typeof hr === 'object' && 'value' in hr) {
    const single = hr as HighlightResultValue;
    return {
      html: sanitizeHighlightHtml(single.value),
      hasMatch: single.matchLevel !== 'none',
    };
  }

  // Handle array highlight results - join them
  if (Array.isArray(hr)) {
    const items = hr as HighlightResultValue[];
    return {
      html: sanitizeHighlightHtml(items.map((item) => item.value).join(', ')),
      hasMatch: items.some((item) => item.matchLevel !== 'none'),
    };
  }

  // Fallback: plain value
  if (rawValue === null || rawValue === undefined) {
    return { html: '<span class="text-muted-foreground italic">null</span>', hasMatch: false };
  }
  if (typeof rawValue === 'object') {
    // Escape HTML in JSON strings
    const json = JSON.stringify(rawValue);
    return { html: escapeHtml(json), hasMatch: false };
  }
  return { html: escapeHtml(String(rawValue)), hasMatch: false };
}

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

function getSafeImageSrc(rawValue: unknown): string | null {
  if (typeof rawValue !== 'string') {
    return null;
  }

  const trimmedValue = rawValue.trim();
  if (!trimmedValue) {
    return null;
  }

  // Treat only single-slash paths as same-origin. Scheme-relative URLs such as
  // `//cdn.example.test/image.png` are cross-origin network requests.
  if (trimmedValue.startsWith('/') && !trimmedValue.startsWith('//')) {
    return trimmedValue;
  }

  try {
    const parsedUrl = new URL(trimmedValue);
    if (parsedUrl.protocol !== 'http:' && parsedUrl.protocol !== 'https:') {
      return null;
    }

    return trimmedValue;
  } catch {
    return null;
  }
}

function hasOwnField(fieldData: FieldData, fieldName: string | null | undefined): fieldName is string {
  if (typeof fieldName !== 'string' || fieldName.length === 0) {
    return false;
  }

  return Object.prototype.hasOwnProperty.call(fieldData, fieldName);
}

function getConfiguredFieldDisplay(
  fieldData: FieldData,
  highlightResult: HighlightResult | undefined,
  fieldName: string | null | undefined
): { html: string; hasMatch: boolean } | null {
  if (!hasOwnField(fieldData, fieldName)) {
    return null;
  }

  return getFieldDisplay(fieldName, fieldData[fieldName], highlightResult);
}

function isHighlightValue(value: unknown): value is HighlightResultValue {
  return Boolean(value && typeof value === 'object' && 'matchLevel' in value);
}

function hasHighlightedMatchForField(
  highlightResult: HighlightResult | undefined,
  fieldName: string
): boolean {
  const value = highlightResult?.[fieldName];
  if (Array.isArray(value)) {
    return value.some((item) => isHighlightValue(item) && item.matchLevel !== 'none');
  }

  return isHighlightValue(value) && value.matchLevel !== 'none';
}

export const DocumentCard = memo(function DocumentCard({
  document,
  fieldOrder,
  displayPreferences,
  onDelete,
  isDeleting,
  onClick,
}: DocumentCardProps) {
  const [showAllFields, setShowAllFields] = useState(false);
  const [showJson, setShowJson] = useState(false);
  const [isCopied, setIsCopied] = useState(false);

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(JSON.stringify(document, null, 2));
      setIsCopied(true);
      setTimeout(() => setIsCopied(false), 2000);
    } catch (err) {
      console.error('Failed to copy:', err);
    }
  };

  const { objectID, _highlightResult, ...rawFieldData } = document;
  const fieldData = rawFieldData as FieldData;
  const highlightResult = _highlightResult as HighlightResult | undefined;
  const preferences = displayPreferences ?? null;

  const consumedFields = useMemo(() => {
    if (!preferences) {
      return new Set<string>();
    }

    const consumed = new Set<string>();
    const configuredFields = [
      preferences.titleAttribute,
      preferences.subtitleAttribute,
      preferences.imageAttribute,
      ...preferences.tagAttributes,
    ];

    for (const fieldName of configuredFields) {
      if (hasOwnField(fieldData, fieldName)) {
        consumed.add(fieldName);
      }
    }

    return consumed;
  }, [fieldData, preferences]);

  const configuredTitle = useMemo(() => {
    return getConfiguredFieldDisplay(fieldData, highlightResult, preferences?.titleAttribute);
  }, [fieldData, highlightResult, preferences?.titleAttribute]);

  const configuredSubtitle = useMemo(() => {
    return getConfiguredFieldDisplay(fieldData, highlightResult, preferences?.subtitleAttribute);
  }, [fieldData, highlightResult, preferences?.subtitleAttribute]);

  const configuredImage = useMemo(() => {
    const imageAttribute = preferences?.imageAttribute;
    if (!hasOwnField(fieldData, imageAttribute)) {
      return null;
    }

    const rawValue = fieldData[imageAttribute];
    const display = getFieldDisplay(imageAttribute, rawValue, highlightResult);
    const src = getSafeImageSrc(rawValue);

    return {
      src,
      html: display.html,
    };
  }, [fieldData, highlightResult, preferences?.imageAttribute]);

  const configuredTags = useMemo(() => {
    if (!preferences || preferences.tagAttributes.length === 0) {
      return [];
    }

    return preferences.tagAttributes.flatMap((tagAttribute) => {
      if (!hasOwnField(fieldData, tagAttribute)) {
        return [];
      }

      const rawValue = fieldData[tagAttribute];
      if (rawValue === null || rawValue === undefined) {
        return [];
      }

      if (Array.isArray(rawValue)) {
        const tagHighlights = Array.isArray(highlightResult?.[tagAttribute])
          ? (highlightResult[tagAttribute] as HighlightResultValue[])
          : [];

        return rawValue.map((item, index) => ({
          key: `${tagAttribute}-${index}`,
          html: getFieldDisplay(
            tagAttribute,
            item,
            tagHighlights[index] ? { [tagAttribute]: tagHighlights[index] } : undefined
          ).html,
        }));
      }

      return [
        {
          key: tagAttribute,
          html: getFieldDisplay(tagAttribute, rawValue, highlightResult).html,
        },
      ];
    });
  }, [fieldData, highlightResult, preferences]);

  const hasConfiguredHeader =
    Boolean(configuredTitle) ||
    Boolean(configuredSubtitle) ||
    Boolean(configuredImage) ||
    configuredTags.length > 0;

  // Use stable field order from parent if provided, falling back to this doc's own keys.
  // This ensures every card in a result set shows fields in the same order.
  const allKeys = useMemo(() => {
    if (!fieldOrder) {
      return Object.keys(fieldData);
    }

    const docKeys = Object.keys(fieldData);
    const docKeySet = new Set(docKeys);
    const canonicalFieldSet = new Set(fieldOrder);
    // Ordered keys present in this doc, then any extras not in the canonical order
    const ordered = fieldOrder.filter((key) => docKeySet.has(key));
    for (const key of docKeys) {
      if (!canonicalFieldSet.has(key)) {
        ordered.push(key);
      }
    }
    return ordered;
  }, [fieldData, fieldOrder]);
  const filteredKeys = useMemo(
    () => allKeys.filter((key) => !consumedFields.has(key)),
    [allKeys, consumedFields]
  );
  const previewKeys = useMemo(() => {
    const matchedKeys = filteredKeys.filter((key) => hasHighlightedMatchForField(highlightResult, key));
    const matchedKeySet = new Set(matchedKeys);
    const unmatchedKeys = filteredKeys.filter((key) => !matchedKeySet.has(key));

    return [...matchedKeys, ...unmatchedKeys].slice(0, PREVIEW_FIELD_COUNT);
  }, [filteredKeys, highlightResult]);
  const extraKeys = useMemo(() => {
    const previewSet = new Set(previewKeys);
    return filteredKeys.filter((key) => !previewSet.has(key));
  }, [filteredKeys, previewKeys]);
  const visibleKeys = showAllFields ? filteredKeys : previewKeys;

  return (
    <Card
      className={`overflow-hidden${onClick ? ' cursor-pointer hover:ring-1 hover:ring-primary/50 transition-shadow' : ''}`}
      data-testid="document-card"
      onClick={onClick ? (e) => {
        // Don't fire click analytics when clicking action buttons
        if ((e.target as HTMLElement).closest('button, a')) return;
        onClick();
      } : undefined}
    >
      <div className="p-4">
        {/* Header with ID and actions */}
        <div className="flex items-start justify-between gap-4 mb-3">
          <div className="flex items-center gap-2 min-w-0">
            <Badge variant="outline" className="font-mono text-xs">
              {objectID}
            </Badge>
          </div>

          <div className="flex items-center gap-1 shrink-0">
            <Button
              variant="ghost"
              size="sm"
              className="h-7 px-2 text-xs"
              onClick={() => setShowJson(!showJson)}
              title="Toggle JSON view"
            >
              <Code className="h-3 w-3 mr-1" />
              JSON
            </Button>
            <Button
              variant="ghost"
              size="sm"
              className="h-7 px-2 text-xs"
              onClick={handleCopy}
            >
              {isCopied ? (
                <>
                  <Check className="h-3 w-3 mr-1" />
                  Copied
                </>
              ) : (
                <>
                  <Copy className="h-3 w-3 mr-1" />
                  Copy
                </>
              )}
            </Button>
            {onDelete && (
              <Button
                variant="ghost"
                size="sm"
                className="h-7 px-2 text-muted-foreground hover:text-destructive"
                onClick={() => onDelete(objectID)}
                disabled={isDeleting}
                title="Delete document"
              >
                <Trash2 className="h-3.5 w-3.5" />
              </Button>
            )}
          </div>
        </div>

        {hasConfiguredHeader && (
          <div className="mb-3 flex gap-3" data-testid="document-card-configured-header">
            {configuredImage && (
              <div className="shrink-0">
                {configuredImage.src ? (
                  <img
                    src={configuredImage.src}
                    alt=""
                    className="h-16 w-16 rounded-md object-cover border"
                    data-testid="document-card-image"
                    referrerPolicy="no-referrer"
                  />
                ) : (
                  <span
                    className={`text-xs text-muted-foreground break-words ${HIGHLIGHT_MARKUP_CLASS_NAME}`}
                    dangerouslySetInnerHTML={{ __html: configuredImage.html }}
                  />
                )}
              </div>
            )}

            <div className="min-w-0 flex-1">
              {configuredTitle && (
                <div
                  className={`font-semibold leading-tight min-w-0 break-words ${HIGHLIGHT_MARKUP_CLASS_NAME}`}
                  data-testid="document-card-title"
                  dangerouslySetInnerHTML={{ __html: configuredTitle.html }}
                />
              )}
              {configuredSubtitle && (
                <div
                  className={`text-sm text-muted-foreground mt-1 min-w-0 break-words ${HIGHLIGHT_MARKUP_CLASS_NAME}`}
                  data-testid="document-card-subtitle"
                  dangerouslySetInnerHTML={{ __html: configuredSubtitle.html }}
                />
              )}
              {configuredTags.length > 0 && (
                <div className="mt-2 flex flex-wrap gap-1.5">
                  {configuredTags.map((tag) => (
                    <Badge key={tag.key} variant="secondary" className="text-xs">
                      <span
                        className={`break-words ${HIGHLIGHT_MARKUP_CLASS_NAME}`}
                        dangerouslySetInnerHTML={{ __html: tag.html }}
                      />
                    </Badge>
                  ))}
                </div>
              )}
            </div>
          </div>
        )}

        {/* Field display with highlighting */}
        {!showJson && (
          <div className="space-y-1.5 text-sm">
            {visibleKeys.map((key) => {
              const { html } = getFieldDisplay(key, fieldData[key], highlightResult);
              return (
                <div key={key} className="flex gap-2 leading-relaxed">
                  <span className="font-medium text-muted-foreground min-w-[100px] shrink-0">
                    {key}:
                  </span>
                  <span
                    className={`min-w-0 break-words ${HIGHLIGHT_MARKUP_CLASS_NAME}`}
                    dangerouslySetInnerHTML={{ __html: html }}
                  />
                </div>
              );
            })}

            {/* Expand/collapse extra fields */}
            {extraKeys.length > 0 && (
              <button
                type="button"
                onClick={() => setShowAllFields(!showAllFields)}
                className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground transition-colors pt-1"
              >
                {showAllFields ? (
                  <>
                    <ChevronDown className="h-3 w-3" />
                    Show less
                  </>
                ) : (
                  <>
                    <ChevronRight className="h-3 w-3" />
                    +{extraKeys.length} more field{extraKeys.length !== 1 ? 's' : ''}
                  </>
                )}
              </button>
            )}
          </div>
        )}

        {/* Full JSON viewer */}
        {showJson && (
          <Suspense
            fallback={
              <div className="h-64 flex items-center justify-center text-muted-foreground">
                Loading editor...
              </div>
            }
          >
            <div className="border rounded-md overflow-hidden">
              <Editor
                height="400px"
                defaultLanguage="json"
                value={JSON.stringify(document, null, 2)}
                options={{
                  readOnly: true,
                  minimap: { enabled: false },
                  scrollBeyondLastLine: false,
                  lineNumbers: 'off',
                  folding: true,
                  fontSize: 13,
                }}
                theme="vs-dark"
              />
            </div>
          </Suspense>
        )}
      </div>
    </Card>
  );
});
