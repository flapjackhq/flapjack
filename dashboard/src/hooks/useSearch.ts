import { useQuery } from '@tanstack/react-query';
import api from '@/lib/api';
import type { SearchParams, SearchResponse, Document } from '@/lib/types';

interface UseSearchOptions {
  indexName: string;
  params: SearchParams;
  enabled?: boolean;
}

export function useSearch<T = Document>({ indexName, params, enabled = true }: UseSearchOptions) {
  return useQuery({
    queryKey: ['search', indexName, params],
    queryFn: async () => {
      const response = await api.post<SearchResponse<T>>(
        `/1/indexes/${indexName}/query`,
        params
      );
      return response.data;
    },
    enabled: enabled && !!indexName,
    staleTime: 0, // Always refetch for fresh results
    retry: false,
  });
}

export function useFacetSearch(indexName: string, facetName: string, facetQuery?: string) {
  return useQuery({
    queryKey: ['facetSearch', indexName, facetName, facetQuery],
    queryFn: async () => {
      const response = await api.post(
        `/1/indexes/${indexName}/facets/${facetName}/query`,
        { facetQuery }
      );
      return response.data;
    },
    enabled: !!indexName && !!facetName,
  });
}
