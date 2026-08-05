import { Trash2 } from 'lucide-react';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';
import type { DictionaryEntry, DictionaryName } from '@/lib/types';
import {
  DICTIONARY_EMPTY_STATES,
  buildEntryDescription,
  getListTestId,
  isStopwordEntry,
} from './shared';

interface DictionaryEntriesPanelProps {
  dictName: DictionaryName;
  entries: DictionaryEntry[];
  count: number;
  isLoading: boolean;
  isDeleting: boolean;
  onDelete: (objectID: string) => Promise<void>;
}

export function DictionaryEntriesPanel({
  dictName,
  entries,
  count,
  isLoading,
  isDeleting,
  onDelete,
}: DictionaryEntriesPanelProps) {
  if (isLoading) {
    return (
      <div className="space-y-3">
        {[1, 2, 3].map((row) => (
          <Skeleton key={row} className="h-12 w-full" />
        ))}
      </div>
    );
  }

  if (entries.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">{DICTIONARY_EMPTY_STATES[dictName]}</p>
    );
  }

  return (
    <div className="space-y-2" data-testid={getListTestId(dictName)}>
      {entries.map((entry) => (
        <Card key={entry.objectID} data-testid="dictionary-entry" className="p-3">
          <div className="flex items-center justify-between gap-3">
            <div className="min-w-0">
              <p className="truncate text-sm font-medium">{buildEntryDescription(entry)}</p>
              <div className="mt-1 flex items-center gap-2">
                <Badge variant="outline" className="text-xs">
                  {entry.language}
                </Badge>
                {isStopwordEntry(entry) && (
                  <Badge variant="secondary" className="text-xs">
                    {entry.state}
                  </Badge>
                )}
                <Badge variant="outline" className="text-xs">
                  {count} total
                </Badge>
              </div>
            </div>

            <Button
              variant="ghost"
              size="sm"
              onClick={() => onDelete(entry.objectID)}
              data-testid="dictionary-entry-delete"
              aria-label="Delete entry"
              disabled={isDeleting}
            >
              <Trash2 className="h-4 w-4 text-destructive" />
            </Button>
          </div>
        </Card>
      ))}
    </div>
  );
}
