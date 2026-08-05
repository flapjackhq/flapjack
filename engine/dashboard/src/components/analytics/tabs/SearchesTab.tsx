import { useMemo, useState } from 'react';
import { Search } from 'lucide-react';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import type { TabProps } from '@/lib/analytics-types';
import {
  useDeviceBreakdown,
  useGeoBreakdown,
  useTopSearches,
} from '@/hooks/useAnalytics';
import {
  EmptyState,
  ErrorState,
  formatAnalyticsErrorMessage,
  TableSkeleton,
} from '@/components/analytics/AnalyticsShared';
import { COUNTRY_NAMES } from '@/components/analytics/geography-utils';

export function SearchesTab({ index, range }: TabProps) {
  const [countryFilter, setCountryFilter] = useState('');
  const [deviceFilter, setDeviceFilter] = useState('');
  const [sortColumn, setSortColumn] = useState('count');
  const [sortDirection, setSortDirection] = useState<'asc' | 'desc'>('desc');
  const [queryFilter, setQueryFilter] = useState('');
  const tagsParam = deviceFilter ? `platform:${deviceFilter}` : undefined;
  const { data, isLoading, error } = useTopSearches(index, range, 100, false, countryFilter || undefined, tagsParam);
  const { data: geoData } = useGeoBreakdown(index, range);
  const { data: deviceData } = useDeviceBreakdown(index, range);

  const countries: any[] = geoData?.countries || [];
  const platforms: any[] = (deviceData?.platforms || []).filter((platform: any) => platform.platform !== 'unknown');
  const maxCount = useMemo(() => {
    if (!data?.searches?.length) return 1;
    return Math.max(...data.searches.map((searchRow: any) => searchRow.count || 0));
  }, [data]);

  const sortedSearches = useMemo(() => {
    if (!data?.searches) return [];
    let searches = [...data.searches];
    if (queryFilter) {
      searches = searches.filter((searchRow: any) => (searchRow.search || '').toLowerCase().includes(queryFilter.toLowerCase()));
    }
    searches.sort((left: any, right: any) => {
      const leftValue = left[sortColumn] ?? 0;
      const rightValue = right[sortColumn] ?? 0;
      const comparison = compareSearchSortValues(leftValue, rightValue);
      return sortDirection === 'desc' ? -comparison : comparison;
    });
    return searches;
  }, [data, queryFilter, sortColumn, sortDirection]);

  function toggleSort(column: string) {
    if (sortColumn === column) {
      setSortDirection(sortDirection === 'desc' ? 'asc' : 'desc');
      return;
    }
    setSortColumn(column);
    setSortDirection('desc');
  }

  return (
    <div className="mt-4 space-y-4">
      <SearchFiltersToolbar
        countries={countries}
        countryFilter={countryFilter}
        deviceFilter={deviceFilter}
        filter={queryFilter}
        onCountryChange={setCountryFilter}
        onDeviceChange={setDeviceFilter}
        onFilterChange={setQueryFilter}
        platforms={platforms}
      />
      <TopSearchesTableCard
        error={error}
        isLoading={isLoading}
        maxCount={maxCount}
        onToggleSort={toggleSort}
        searches={sortedSearches}
        sortColumn={sortColumn}
        sortDirection={sortDirection}
      />
    </div>
  );
}

function compareSearchSortValues(leftValue: unknown, rightValue: unknown): number {
  if (typeof leftValue === 'string' || typeof rightValue === 'string') {
    return String(leftValue ?? '').localeCompare(String(rightValue ?? ''), undefined, {
      sensitivity: 'base',
    });
  }

  return Number(leftValue ?? 0) - Number(rightValue ?? 0);
}

function SearchSortHeader({
  activeColumn,
  align,
  label,
  onToggle,
  sortColumn,
  sortDirection,
}: {
  activeColumn: string;
  align?: string;
  label: string;
  onToggle: (column: string) => void;
  sortColumn: string;
  sortDirection: 'asc' | 'desc';
}) {
  const isActive = sortColumn === activeColumn;

  return (
    <th
      className={`py-2.5 pr-4 font-medium cursor-pointer select-none hover:text-foreground transition-colors ${align || 'text-left'}`}
      onClick={() => onToggle(activeColumn)}
    >
      <span className="inline-flex items-center gap-1">
        {label}
        {isActive && <span className="text-xs">{sortDirection === 'desc' ? '\u2193' : '\u2191'}</span>}
      </span>
    </th>
  );
}

function SearchFiltersToolbar({
  countries,
  countryFilter,
  deviceFilter,
  filter,
  onCountryChange,
  onDeviceChange,
  onFilterChange,
  platforms,
}: {
  countries: any[];
  countryFilter: string;
  deviceFilter: string;
  filter: string;
  onCountryChange: (value: string) => void;
  onDeviceChange: (value: string) => void;
  onFilterChange: (value: string) => void;
  platforms: any[];
}) {
  return (
    <div className="flex items-center gap-2 flex-wrap" data-testid="searches-filter">
      <Search className="h-4 w-4 text-muted-foreground" />
      <input
        type="text"
        placeholder="Filter queries..."
        value={filter}
        onChange={(event) => onFilterChange(event.target.value)}
        className="h-8 rounded-md border border-input bg-background px-3 text-sm flex-1 max-w-xs"
        data-testid="searches-filter-input"
      />
      {countries.length > 0 && (
        <select
          value={countryFilter}
          onChange={(event) => onCountryChange(event.target.value)}
          className="h-8 rounded-md border border-input bg-background px-2 text-sm"
          data-testid="searches-country-filter"
        >
          <option value="">All Countries</option>
          {countries.map((country) => (
            <option key={country.country} value={country.country}>
              {COUNTRY_NAMES[country.country as string] || country.country} ({country.count?.toLocaleString()})
            </option>
          ))}
        </select>
      )}
      {platforms.length > 0 && (
        <select
          value={deviceFilter}
          onChange={(event) => onDeviceChange(event.target.value)}
          className="h-8 rounded-md border border-input bg-background px-2 text-sm"
          data-testid="searches-device-filter"
        >
          <option value="">All Devices</option>
          {platforms.map((platform) => (
            <option key={platform.platform} value={platform.platform}>
              {platform.platform.charAt(0).toUpperCase() + platform.platform.slice(1)} ({platform.count?.toLocaleString()})
            </option>
          ))}
        </select>
      )}
    </div>
  );
}

function TopSearchesTableCard({
  error,
  isLoading,
  maxCount,
  onToggleSort,
  searches,
  sortColumn,
  sortDirection,
}: {
  error: unknown;
  isLoading: boolean;
  maxCount: number;
  onToggleSort: (column: string) => void;
  searches: any[];
  sortColumn: string;
  sortDirection: 'asc' | 'desc';
}) {
  const errorMessage = formatAnalyticsErrorMessage(error);

  return (
    <Card data-testid="top-searches-table">
      <CardHeader className="pb-2">
        <div className="flex items-center justify-between">
          <CardTitle className="text-base font-medium">Top Searches</CardTitle>
          {searches.length > 0 && (
            <span className="text-xs text-muted-foreground">{searches.length} queries</span>
          )}
        </div>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <TableSkeleton rows={8} />
        ) : errorMessage ? (
          <ErrorState message={errorMessage} />
        ) : searches.length > 0 ? (
          <TopSearchesTableBody
            maxCount={maxCount}
            onToggleSort={onToggleSort}
            searches={searches}
            sortColumn={sortColumn}
            sortDirection={sortDirection}
          />
        ) : (
          <EmptyState
            icon={Search}
            title="No searches recorded yet"
            description="Top search queries will appear here as users search your index."
          />
        )}
      </CardContent>
    </Card>
  );
}

function TopSearchesTableBody({
  maxCount,
  onToggleSort,
  searches,
  sortColumn,
  sortDirection,
}: {
  maxCount: number;
  onToggleSort: (column: string) => void;
  searches: any[];
  sortColumn: string;
  sortDirection: 'asc' | 'desc';
}) {
  return (
    <div className="overflow-x-auto">
      <table className="w-full text-sm">
        <thead>
          <tr className="border-b border-border text-muted-foreground">
            <th className="py-2.5 pr-4 font-medium text-left w-8">#</th>
            <SearchSortHeader
              activeColumn="search"
              label="Query"
              onToggle={onToggleSort}
              sortColumn={sortColumn}
              sortDirection={sortDirection}
            />
            <SearchSortHeader
              activeColumn="count"
              align="text-right"
              label="Count"
              onToggle={onToggleSort}
              sortColumn={sortColumn}
              sortDirection={sortDirection}
            />
            <th className="py-2.5 pr-4 font-medium text-right w-32">Volume</th>
            <SearchSortHeader
              activeColumn="nbHits"
              align="text-right"
              label="Avg Hits"
              onToggle={onToggleSort}
              sortColumn={sortColumn}
              sortDirection={sortDirection}
            />
          </tr>
        </thead>
        <tbody>
          {searches.map((searchRow: any, index: number) => (
            <TopSearchesTableRow
              key={`${searchRow.search}-${index}`}
              maxCount={maxCount}
              rowIndex={index}
              searchRow={searchRow}
            />
          ))}
        </tbody>
      </table>
    </div>
  );
}

function TopSearchesTableRow({
  maxCount,
  rowIndex,
  searchRow,
}: {
  maxCount: number;
  rowIndex: number;
  searchRow: any;
}) {
  const percentage = ((searchRow.count || 0) / maxCount) * 100;

  return (
    <tr className="border-b border-border/50 hover:bg-accent/30 transition-colors">
      <td className="py-2.5 pr-4 text-muted-foreground text-xs">{rowIndex + 1}</td>
      <td className="py-2.5 pr-4" data-testid="search-query">
        <span className="font-mono text-sm">
          {searchRow.search || <span className="text-muted-foreground italic">(empty query)</span>}
        </span>
      </td>
      <td className="py-2.5 pr-4 text-right tabular-nums" data-testid="search-count">
        {searchRow.count?.toLocaleString()}
      </td>
      <td className="py-2.5 pr-4" data-testid="search-volume">
        <div className="flex items-center justify-end gap-2">
          <div className="w-24 h-2 bg-muted rounded-full overflow-hidden" data-testid="search-volume-track">
            <div
              className="h-full bg-primary/60 rounded-full transition-all"
              data-testid="search-volume-fill"
              style={{ width: `${percentage}%` }}
            />
          </div>
        </div>
      </td>
      <td className="py-2.5 text-right tabular-nums" data-testid="search-hits">
        {searchRow.nbHits ?? '-'}
      </td>
    </tr>
  );
}
