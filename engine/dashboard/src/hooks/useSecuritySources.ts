/**
 */
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import api from '@/lib/api';
import { extractApiErrorMessage } from '@/lib/apiErrorMessage';
import { useToast } from '@/hooks/use-toast';
import { securitySourcesKeys } from '@/lib/queryKeys';
import type { SecuritySourceEntry, SecuritySourceMutationResponse } from '@/lib/types';

export function useSecuritySources() {
  return useQuery({
    queryKey: securitySourcesKeys.list(),
    queryFn: async () => {
      const response = await api.get<SecuritySourceEntry[]>('/1/security/sources');
      return response.data;
    },
  });
}

/**
 * TODO: Document useAppendSecuritySource.
 */
export function useAppendSecuritySource() {
  const queryClient = useQueryClient();
  const { toast } = useToast();

  return useMutation({
    mutationFn: async (entry: SecuritySourceEntry) => {
      const response = await api.post<SecuritySourceMutationResponse>(
        '/1/security/sources/append',
        entry,
      );
      return response.data;
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: securitySourcesKeys.all });
      toast({ title: 'Security source added' });
    },
    onError: (error: unknown) => {
      toast({
        title: 'Failed to add security source',
        description: extractApiErrorMessage(error, 'Request failed'),
        variant: 'destructive',
      });
    },
  });
}

/**
 * TODO: Document useDeleteSecuritySource.
 */
export function useDeleteSecuritySource() {
  const queryClient = useQueryClient();
  const { toast } = useToast();

  return useMutation({
    mutationFn: async (source: string) => {
      const response = await api.delete<SecuritySourceMutationResponse>(
        `/1/security/sources/${encodeURIComponent(source)}`,
      );
      return response.data;
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: securitySourcesKeys.all });
      toast({ title: 'Security source deleted' });
    },
    onError: (error: unknown) => {
      toast({
        title: 'Failed to delete security source',
        description: extractApiErrorMessage(error, 'Request failed'),
        variant: 'destructive',
      });
    },
  });
}
