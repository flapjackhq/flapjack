import { memo, useState, useCallback } from 'react';
import { useCreateIndex } from '@/hooks/useIndices';
import { useAddDocuments } from '@/hooks/useDocuments';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from '@/components/ui/dialog';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Database } from 'lucide-react';
import { cn } from '@/lib/utils';
import moviesData from '@/data/movies.json';

type IndexTemplate = 'empty' | 'movies-10' | 'movies-100';

const TEMPLATES = [
  {
    value: 'empty' as const,
    label: 'Empty index',
    desc: 'Start from scratch — add your own documents later',
    icon: null,
  },
  {
    value: 'movies-10' as const,
    label: 'Movies (10 documents)',
    desc: 'Quick demo dataset — great for getting started',
    icon: Database,
  },
  {
    value: 'movies-100' as const,
    label: 'Movies (100 documents)',
    desc: 'Full demo dataset — test search, faceting, and filtering',
    icon: Database,
  },
];

interface CreateIndexDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export const CreateIndexDialog = memo(function CreateIndexDialog({
  open,
  onOpenChange,
}: CreateIndexDialogProps) {
  const createIndex = useCreateIndex();
  const [uid, setUid] = useState('');
  const [error, setError] = useState('');
  const [template, setTemplate] = useState<IndexTemplate>('empty');
  const [isLoadingData, setIsLoadingData] = useState(false);

  const addDocuments = useAddDocuments(uid.trim());

  const handleTemplateChange = useCallback((newTemplate: IndexTemplate) => {
    setTemplate(newTemplate);
    if (newTemplate === 'movies-10' || newTemplate === 'movies-100') {
      setUid('movies');
    } else {
      setUid('');
    }
    setError('');
  }, []);

  const handleCreate = useCallback(async () => {
    const trimmed = uid.trim();
    if (!trimmed) {
      setError('Index name is required');
      return;
    }
    if (!/^[a-zA-Z0-9_-]+$/.test(trimmed)) {
      setError('Only letters, numbers, hyphens, and underscores allowed');
      return;
    }

    try {
      await createIndex.mutateAsync({ uid: trimmed });

      if (template === 'movies-10' || template === 'movies-100') {
        setIsLoadingData(true);
        const count = template === 'movies-10' ? 10 : 100;
        await addDocuments.mutateAsync(moviesData.slice(0, count));
        setIsLoadingData(false);
      }

      setUid('');
      setTemplate('empty');
      setError('');
      onOpenChange(false);
    } catch (err) {
      setIsLoadingData(false);
      console.error('Failed to create index:', err);
    }
  }, [uid, template, createIndex, addDocuments, onOpenChange]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === 'Enter' && !createIndex.isPending && !isLoadingData) {
        handleCreate();
      }
    },
    [handleCreate, createIndex.isPending, isLoadingData]
  );

  const handleOpenChange = useCallback(
    (open: boolean) => {
      if (!open) {
        setUid('');
        setTemplate('empty');
        setError('');
      }
      onOpenChange(open);
    },
    [onOpenChange]
  );

  const isPending = createIndex.isPending || isLoadingData;

  const buttonText = isPending
    ? createIndex.isPending
      ? 'Creating...'
      : 'Loading data...'
    : template === 'empty'
    ? 'Create Index'
    : `Create & Load ${template === 'movies-10' ? '10' : '100'} Movies`;

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>Create Index</DialogTitle>
          <DialogDescription>
            Create a new search index to start adding documents
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          <div className="space-y-2">
            <Label htmlFor="index-uid">Index Name</Label>
            <Input
              id="index-uid"
              value={uid}
              onChange={(e) => {
                setUid(e.target.value);
                setError('');
              }}
              onKeyDown={handleKeyDown}
              placeholder="e.g., products, articles, users"
              autoFocus
            />
            {error && (
              <p className="text-sm text-destructive">{error}</p>
            )}
          </div>

          <div className="space-y-2">
            <Label>Starting data</Label>
            <div className="space-y-2">
              {TEMPLATES.map((opt) => (
                <label
                  key={opt.value}
                  className={cn(
                    'flex items-start gap-3 p-3 rounded-md border cursor-pointer transition-colors',
                    template === opt.value
                      ? 'border-primary bg-primary/5'
                      : 'border-border hover:border-muted-foreground/50'
                  )}
                >
                  <input
                    type="radio"
                    name="template"
                    value={opt.value}
                    checked={template === opt.value}
                    onChange={() => handleTemplateChange(opt.value)}
                    className="mt-0.5"
                  />
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-1.5">
                      {opt.icon && <opt.icon className="h-3.5 w-3.5 text-muted-foreground" />}
                      <span className="text-sm font-medium">{opt.label}</span>
                    </div>
                    <div className="text-xs text-muted-foreground mt-0.5">{opt.desc}</div>
                  </div>
                </label>
              ))}
            </div>
          </div>
        </div>

        <DialogFooter>
          <Button
            variant="outline"
            onClick={() => handleOpenChange(false)}
            disabled={isPending}
          >
            Cancel
          </Button>
          <Button onClick={handleCreate} disabled={isPending}>
            {buttonText}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
});
