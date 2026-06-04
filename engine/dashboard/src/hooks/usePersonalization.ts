/**
 */
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import api from '@/lib/api';
import { useApiLogger } from '@/hooks/useApiLogger';
import { useToast } from '@/hooks/use-toast';
import { personalizationKeys } from '@/lib/queryKeys';
import type { PersonalizationProfile, PersonalizationStrategy } from '@/lib/types';

function readHttpStatus(error: unknown): number | null {
  if (!error || typeof error !== 'object' || !('response' in error)) {
    return null;
  }

  const response = (error as { response?: { status?: unknown } }).response;
  return typeof response?.status === 'number' ? response.status : null;
}

function trimLookupToken(userToken: string | null): string {
  return (userToken ?? '').trim();
}

async function fetchNullableResource<T>(path: string): Promise<T | null> {
  try {
    const response = await api.get<T>(path);
    return response.data;
  } catch (error) {
    if (readHttpStatus(error) === 404) {
      const metadata = (error as { config?: { metadata?: { entryId?: string; startTime?: number } } }).config?.metadata;
      if (metadata?.entryId && metadata.startTime) useApiLogger.getState().updateEntry(metadata.entryId, { status: 'success', duration: Date.now() - metadata.startTime, response: null });
      return null;
    }
    throw error;
  }
}

export function usePersonalizationStrategy() {
  return useQuery<PersonalizationStrategy | null>({
    queryKey: personalizationKeys.strategy(),
    queryFn: () => fetchNullableResource<PersonalizationStrategy>('/1/strategies/personalization'),
  });
}

export function useSaveStrategy() {
  const queryClient = useQueryClient();
  const { toast } = useToast();

  return useMutation({
    mutationFn: async (strategy: PersonalizationStrategy) => {
      const response = await api.post('/1/strategies/personalization', strategy);
      return response.data;
    },
    onSuccess: async (_response, strategy) => {
      queryClient.setQueryData(personalizationKeys.strategy(), strategy);
      await queryClient.invalidateQueries({ queryKey: personalizationKeys.strategy() });
      toast({ title: 'Personalization strategy saved' });
    },
    onError: (error: Error) => {
      toast({
        variant: 'destructive',
        title: 'Failed to save personalization strategy',
        description: error.message || 'An unexpected error occurred.',
      });
    },
  });
}

export function usePersonalizationProfile(userToken: string | null) {
  const lookupToken = trimLookupToken(userToken);

  return useQuery<PersonalizationProfile | null>({
    queryKey: personalizationKeys.profile(lookupToken),
    queryFn: () =>
      fetchNullableResource<PersonalizationProfile>(
        `/1/profiles/personalization/${encodeURIComponent(lookupToken)}`,
      ),
    enabled: lookupToken.length > 0,
  });
}
