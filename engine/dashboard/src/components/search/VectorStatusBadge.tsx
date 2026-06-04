import { memo } from 'react';
import { Badge } from '@/components/ui/badge';
import type { EmbedderConfig, IndexMode } from '@/lib/types';

interface VectorStatusBadgeProps {
  embedders: Record<string, EmbedderConfig> | undefined;
  mode: IndexMode | undefined;
  vectorSearchEnabled: boolean | undefined;
  showDisabledStatus?: boolean;
}

export const VectorStatusBadge = memo(function VectorStatusBadge({
  embedders,
  mode,
  showDisabledStatus = true,
  vectorSearchEnabled,
}: VectorStatusBadgeProps) {
  if (vectorSearchEnabled === undefined) {
    return null;
  }

  if (vectorSearchEnabled === false && showDisabledStatus) {
    return (
      <Badge
        variant="outline"
        data-testid="vector-status-badge-disabled"
        className="text-xs"
      >
        Vector Search unavailable (not compiled in)
      </Badge>
    );
  }

  if (vectorSearchEnabled === false) {
    return null;
  }

  const embedderCount = embedders ? Object.keys(embedders).length : 0;
  if (embedderCount === 0) return null;

  const modeLabel = mode === 'neuralSearch' ? 'Neural' : 'Keyword';
  const embedderLabel = embedderCount === 1 ? '1 embedder' : `${embedderCount} embedders`;

  return (
    <Badge
      variant="secondary"
      data-testid="vector-status-badge"
      className="text-xs"
    >
      Vector Search · {embedderLabel} · {modeLabel}
    </Badge>
  );
});
