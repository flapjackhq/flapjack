import { memo, useCallback, useMemo, useState } from 'react';
import { ChevronLeft, ChevronRight } from 'lucide-react';
import { useSearch } from '@/hooks/useSearch';
import { useDeleteDocument } from '@/hooks/useDocuments';
import { useDisplayPreferences } from '@/hooks/useDisplayPreferences';
import { Card } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Skeleton } from '@/components/ui/skeleton';
import { ConfirmDialog } from '@/components/ui/confirm-dialog';
import { DocumentCard } from '@/components/search/DocumentCard';
import type { SearchParams } from '@/lib/types';

interface ResultsPanelProps {
  indexName: string;
  params: SearchParams;
  onParamsChange: (updates: Partial<SearchParams>) => void;
  onResultClick?: (objectID: string, position: number, queryID?: string) => void;
  userToken?: string;
}

function buildFieldOrder(hits: Array<Record<string, unknown>> | undefined): string[] {
  if (!hits || hits.length === 0) {
    return [];
  }

  const seen = new Set<string>();
  const order: string[] = [];

  for (const hit of hits) {
    for (const key of Object.keys(hit)) {
      if (key === 'objectID' || key === '_highlightResult' || seen.has(key)) {
        continue;
      }

      seen.add(key);
      order.push(key);
    }
  }

  return order;
}

function getHitPosition(params: SearchParams, index: number): number {
  const page = params.page || 0;
  const hitsPerPage = params.hitsPerPage || 20;
  return page * hitsPerPage + index + 1;
}

export const ResultsPanel = memo(function ResultsPanel({
  indexName,
  params,
  onParamsChange,
  onResultClick,
  userToken,
}: ResultsPanelProps) {
  const { data, isLoading, error } = useSearch({
    indexName,
    params,
    userToken,
  });
  const deleteDoc = useDeleteDocument(indexName);
  const { preferences: displayPreferences } = useDisplayPreferences(indexName);
  const [pendingDeleteId, setPendingDeleteId] = useState<string | null>(null);

  const handlePageChange = useCallback(
    (newPage: number) => {
      onParamsChange({ page: newPage });
    },
    [onParamsChange]
  );

  const handleDelete = useCallback(
    (objectID: string) => {
      setPendingDeleteId(objectID);
    },
    []
  );

  const confirmDelete = useCallback(() => {
    if (!pendingDeleteId) {
      return;
    }

    deleteDoc.mutate(pendingDeleteId, {
      onSettled: () => setPendingDeleteId(null),
    });
  }, [pendingDeleteId, deleteDoc]);

  const handleDeleteDialogOpenChange = useCallback((open: boolean) => {
    if (!open) {
      setPendingDeleteId(null);
    }
  }, []);

  // Compute a stable field order from all hits so every DocumentCard
  // shows fields in the same order (first-seen across the result set).
  const fieldOrder = useMemo(() => {
    return buildFieldOrder(data?.hits);
  }, [data?.hits]);

  const currentPage = params.page || 0;
  const totalPages = data?.nbPages || 0;
  const hasPrevPage = currentPage > 0;
  const hasNextPage = currentPage < totalPages - 1;

  if (error) {
    return (
      <Card className="p-8 text-center">
        <h3 className="text-lg font-semibold text-destructive mb-2">Search Error</h3>
        <p className="text-sm text-muted-foreground">
          {error instanceof Error ? error.message : 'Failed to search'}
        </p>
      </Card>
    );
  }

  if (isLoading) {
    return (
      <div className="flex flex-col gap-4">
        <Card className="p-4">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              <Skeleton className="h-4 w-20" />
              <Skeleton className="h-4 w-12" />
            </div>
          </div>
        </Card>
        {[1, 2, 3].map((i) => (
          <Card key={i} className="p-4 space-y-3">
            <div className="flex items-center justify-between">
              <Skeleton className="h-5 w-48" />
              <Skeleton className="h-6 w-16 rounded-full" />
            </div>
            <Skeleton className="h-4 w-full" />
            <Skeleton className="h-4 w-3/4" />
          </Card>
        ))}
      </div>
    );
  }

  if (!data?.hits?.length) {
    return (
      <Card className="p-8 text-center">
        <h3 className="text-lg font-semibold mb-2">No results found</h3>
        <p className="text-sm text-muted-foreground">
          {params.query
            ? `No documents match "${params.query}"`
            : 'Try a different search query'}
        </p>
      </Card>
    );
  }

  return (
    <div className="flex flex-col gap-4 h-full" data-testid="results-panel">
      {/* Results header */}
      <Card className="p-4">
        <div className="flex items-center justify-between">
          <div className="text-sm">
            <span className="font-semibold" data-testid="results-count">{data.nbHits.toLocaleString()}</span>
            {' '}
            <span className="text-muted-foreground ml-1" data-testid="results-label">
              {data.nbHits === 1 ? 'result' : 'results'}
            </span>
            <span className="text-muted-foreground mx-2">•</span>
            <span className="text-muted-foreground">
              {data.processingTimeMS}ms
            </span>
          </div>

          {/* Pagination */}
          {totalPages > 1 && (
            <div className="flex items-center gap-2" data-testid="pagination-controls">
              <Button
                variant="outline"
                size="sm"
                aria-label="Previous page"
                onClick={() => handlePageChange(currentPage - 1)}
                disabled={!hasPrevPage}
              >
                <ChevronLeft className="h-4 w-4" aria-hidden="true" />
              </Button>
              <span className="text-sm text-muted-foreground">
                Page {currentPage + 1} of {totalPages}
              </span>
              <Button
                variant="outline"
                size="sm"
                aria-label="Next page"
                onClick={() => handlePageChange(currentPage + 1)}
                disabled={!hasNextPage}
              >
                <ChevronRight className="h-4 w-4" aria-hidden="true" />
              </Button>
            </div>
          )}
        </div>
      </Card>

      {/* Results list - scrollable */}
      <div className="flex-1 overflow-y-auto space-y-3">
        {data.hits.map((hit, index) => (
          <DocumentCard
            key={hit.objectID || index}
            document={hit}
            fieldOrder={fieldOrder}
            displayPreferences={displayPreferences}
            onDelete={handleDelete}
            isDeleting={deleteDoc.isPending}
            onClick={
              onResultClick
                ? () => {
                    onResultClick(hit.objectID, getHitPosition(params, index), data.queryID);
                  }
                : undefined
            }
          />
        ))}
      </div>

      <ConfirmDialog
        open={pendingDeleteId !== null}
        onOpenChange={handleDeleteDialogOpenChange}
        title="Delete Document"
        description={
          <>
            Are you sure you want to delete{' '}
            <code className="font-mono text-sm bg-muted px-1 py-0.5 rounded">
              {pendingDeleteId}
            </code>
            ? This action cannot be undone.
          </>
        }
        confirmLabel="Delete"
        variant="destructive"
        onConfirm={confirmDelete}
        isPending={deleteDoc.isPending}
      />
    </div>
  );
});
