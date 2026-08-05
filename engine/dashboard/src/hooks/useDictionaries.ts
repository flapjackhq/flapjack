/**
 */
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import api from '@/lib/api';
import { extractApiErrorMessage } from '@/lib/apiErrorMessage';
import { useToast } from '@/hooks/use-toast';
import { dictionariesKeys } from '@/lib/queryKeys';
import type { DictionaryEntry, DictionaryName, DictionarySearchResponse } from '@/lib/types';

export function useDictionarySearch(dictName: DictionaryName, query = '') {
  return useQuery({
    queryKey: dictionariesKeys.list(dictName, query),
    queryFn: async () => {
      const response = await api.post<DictionarySearchResponse>(
        `/1/dictionaries/${dictName}/search`,
        { query },
      );
      return response.data;
    },
  });
}

export function useAddDictionaryEntry(dictName: DictionaryName) {
  const queryClient = useQueryClient();
  const { toast } = useToast();

  return useMutation({
    mutationFn: async (entry: DictionaryEntry) => {
      const response = await api.post(`/1/dictionaries/${dictName}/batch`, {
        requests: [{ action: 'addEntry', body: entry }],
      });
      return response.data;
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: dictionariesKeys.dictionary(dictName) });
      toast({ title: 'Dictionary entry added' });
    },
    onError: (error: unknown) => {
      toast({
        title: 'Failed to add dictionary entry',
        description: extractApiErrorMessage(error, 'Request failed'),
        variant: 'destructive',
      });
    },
  });
}

export function useDeleteDictionaryEntry(dictName: DictionaryName) {
  const queryClient = useQueryClient();
  const { toast } = useToast();

  return useMutation({
    mutationFn: async (objectID: string) => {
      const response = await api.post(`/1/dictionaries/${dictName}/batch`, {
        requests: [{ action: 'deleteEntry', body: { objectID } }],
      });
      return response.data;
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: dictionariesKeys.dictionary(dictName) });
      toast({ title: 'Dictionary entry deleted' });
    },
    onError: (error: unknown) => {
      toast({
        title: 'Failed to delete dictionary entry',
        description: extractApiErrorMessage(error, 'Request failed'),
        variant: 'destructive',
      });
    },
  });
}
