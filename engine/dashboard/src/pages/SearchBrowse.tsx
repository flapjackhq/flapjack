import { useState, useCallback, useMemo } from 'react';
import { useParams, Link } from 'react-router-dom';
import { ChevronLeft, Plus, HardDrive, Circle, SlidersHorizontal } from 'lucide-react';
import { Card } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Switch } from '@/components/ui/switch';
import { Label } from '@/components/ui/label';
import { SearchBox } from '@/components/search/SearchBox';
import { ResultsPanel } from '@/components/search/ResultsPanel';
import { FacetsPanel } from '@/components/search/FacetsPanel';
import { HybridSearchControls } from '@/components/search/HybridSearchControls';
import { VectorStatusBadge } from '@/components/search/VectorStatusBadge';
import { DisplayPreferencesModal } from '@/components/search/DisplayPreferencesModal';
import { AddDocumentsDialog } from '@/components/documents/AddDocumentsDialog';
import { useIndexes } from '@/hooks/useIndexes';
import { useSettings, useEmbedderNames } from '@/hooks/useSettings';
import { useHealthDetail } from '@/hooks/useSystemStatus';
import { formatBytes } from '@/lib/utils';
import api from '@/lib/api';
import type { SearchParams, HybridSearchParams } from '@/lib/types';

function readOrCreateDashboardUserToken(): string {
  const key = 'fj-dashboard-user-token';
  const existingToken = sessionStorage.getItem(key);
  if (existingToken) {
    return existingToken;
  }

  const newToken = `dashboard-${crypto.randomUUID().slice(0, 8)}`;
  sessionStorage.setItem(key, newToken);
  return newToken;
}

export function mergeSearchParams(
  previousParams: SearchParams,
  updates: Partial<SearchParams>,
): SearchParams {
  const queryOrFilterChanged =
    updates.query !== undefined ||
    updates.filters !== undefined ||
    updates.facetFilters !== undefined;
  const nextPage = updates.page ?? (queryOrFilterChanged ? 0 : previousParams.page);

  return {
    ...previousParams,
    ...updates,
    page: nextPage,
  };
}

function buildEffectiveSearchParams(
  searchParams: SearchParams,
  trackAnalytics: boolean,
  hybridParams: HybridSearchParams | null,
): SearchParams {
  const paramsWithAnalytics = trackAnalytics
    ? { ...searchParams, analytics: true, clickAnalytics: true, analyticsTags: ['source:dashboard'] }
    : searchParams;

  return hybridParams ? { ...paramsWithAnalytics, hybrid: hybridParams } : paramsWithAnalytics;
}

export function SearchBrowse() {
  const { indexName } = useParams<{ indexName: string }>();
  const resolvedIndexName = indexName ?? '';
  const { data: indexes } = useIndexes();
  const { data: settings } = useSettings(resolvedIndexName);
  const { embedderNames } = useEmbedderNames(resolvedIndexName);
  const { data: health } = useHealthDetail();
  const [trackAnalytics, setTrackAnalytics] = useState(false);
  const [dashboardUserToken, setDashboardUserToken] = useState<string | null>(null);
  const [hybridParams, setHybridParams] = useState<HybridSearchParams | null>(null);
  const [searchParams, setSearchParams] = useState<SearchParams>({
    query: '',
    hitsPerPage: 20,
    page: 0,
    attributesToHighlight: ['*'],
  });
  const [isDisplayPreferencesOpen, setIsDisplayPreferencesOpen] = useState(false);
  const [showAddDocs, setShowAddDocs] = useState(false);

  const currentIndex = indexes?.find((idx) => idx.uid === indexName);
  const vectorSearchEnabled = health?.capabilities.vectorSearch;

  // Merge analytics + hybrid params into search params
  const effectiveParams = useMemo<SearchParams>(() => {
    return buildEffectiveSearchParams(searchParams, trackAnalytics, hybridParams);
  }, [searchParams, trackAnalytics, hybridParams]);

  const handleHybridChange = useCallback((updates: Partial<SearchParams>) => {
    const hybrid = updates.hybrid;
    // Null out when ratio is 0 — no point sending hybrid params for pure keyword search
    setHybridParams(hybrid?.semanticRatio ? hybrid : null);
  }, []);

  const handleParamsChange = useCallback((updates: Partial<SearchParams>) => {
    setSearchParams((prev) => mergeSearchParams(prev, updates));
  }, []);

  const ensureDashboardUserToken = useCallback(() => {
    if (dashboardUserToken) {
      return dashboardUserToken;
    }

    const userToken = readOrCreateDashboardUserToken();
    setDashboardUserToken(userToken);
    return userToken;
  }, [dashboardUserToken]);

  const handleTrackAnalyticsChange = useCallback((checked: boolean) => {
    setTrackAnalytics(checked);
    if (checked) {
      ensureDashboardUserToken();
    }
  }, [ensureDashboardUserToken]);

  // Fire a click event when analytics tracking is on and user clicks a result
  const handleResultClick = useCallback(
    (objectID: string, position: number, queryID?: string) => {
      if (!trackAnalytics || !queryID || !indexName) return;
      const userToken = ensureDashboardUserToken();

      api.post('/1/events', {
        events: [
          {
            eventType: 'click',
            eventName: 'Result Clicked',
            index: indexName,
            userToken,
            queryID,
            objectIDs: [objectID],
            positions: [position],
            timestamp: Date.now(),
          },
        ],
      }).catch(() => {
        // Fire-and-forget - don't interrupt the user
      });
    },
    [trackAnalytics, indexName, ensureDashboardUserToken]
  );

  if (!indexName) {
    return (
      <Card className="p-8 text-center">
        <h3 className="text-lg font-semibold mb-2">No index selected</h3>
        <p className="text-muted-foreground mb-4">
          Select an index from the Overview page to start searching
        </p>
        <Link to="/overview">
          <Button>Go to Overview</Button>
        </Link>
      </Card>
    );
  }

  return (
    <div className="h-full flex flex-col gap-4">
      {/* Breadcrumb + Index stats + Add Documents */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Link to="/overview">
            <Button variant="ghost" size="sm">
              <ChevronLeft className="h-4 w-4 mr-1" />
              Overview
            </Button>
          </Link>
          <span className="text-muted-foreground">/</span>
          <h2 className="text-xl font-semibold">{indexName}</h2>
          {currentIndex && (
            <span className="flex items-center gap-1 text-sm text-muted-foreground ml-2">
              <HardDrive className="h-3.5 w-3.5" />
              {formatBytes(currentIndex.dataSize || 0)}
              <span className="mx-1">·</span>
              {(currentIndex.entries || 0).toLocaleString()} docs
            </span>
          )}
          <VectorStatusBadge
            embedders={settings?.embedders}
            mode={settings?.mode}
            showDisabledStatus={false}
            vectorSearchEnabled={vectorSearchEnabled}
          />
        </div>
        <div className="flex items-center gap-3">
          {/* Analytics tracking toggle */}
          <div className="flex items-center gap-2">
            <Switch
              id="track-analytics"
              checked={trackAnalytics}
              onCheckedChange={handleTrackAnalyticsChange}
            />
            <Label htmlFor="track-analytics" className="text-sm cursor-pointer select-none flex items-center gap-1.5">
              {trackAnalytics && (
                <Circle className="h-2 w-2 fill-red-500 text-red-500 animate-pulse" data-testid="recording-indicator" />
              )}
              Track Analytics
            </Label>
          </div>

          <div className="h-4 w-px bg-border" />
          <Button variant="outline" size="sm" onClick={() => setIsDisplayPreferencesOpen(true)}>
            <SlidersHorizontal className="h-4 w-4 mr-1" />
            Display Preferences
          </Button>
          <Button size="sm" onClick={() => setShowAddDocs(true)}>
            <Plus className="h-4 w-4 mr-1" />
            Add Documents
          </Button>
        </div>
      </div>

      <SearchBox
        indexName={indexName}
        params={searchParams}
        onParamsChange={handleParamsChange}
      />

      <HybridSearchControls
        embedderNames={vectorSearchEnabled === true ? embedderNames : []}
        onParamsChange={handleHybridChange}
      />

      <div className="flex-1 grid grid-cols-1 lg:grid-cols-[1fr_300px] gap-4 min-h-0">
        <ResultsPanel
          indexName={indexName}
          params={effectiveParams}
          onParamsChange={handleParamsChange}
          onResultClick={trackAnalytics ? handleResultClick : undefined}
          userToken={trackAnalytics ? dashboardUserToken ?? undefined : undefined}
        />

        <FacetsPanel
          indexName={indexName}
          params={effectiveParams}
          onParamsChange={handleParamsChange}
        />
      </div>

      <AddDocumentsDialog
        open={showAddDocs}
        onOpenChange={setShowAddDocs}
        indexName={indexName}
      />
      <DisplayPreferencesModal
        open={isDisplayPreferencesOpen}
        onOpenChange={setIsDisplayPreferencesOpen}
        indexName={indexName}
      />
    </div>
  );
}
