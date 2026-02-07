import { useState } from 'react';
import { useAddDocuments } from '@/hooks/useDocuments';
import { Button } from '@/components/ui/button';
import { Database, Loader2 } from 'lucide-react';
import moviesData from '@/data/movies.json';

interface SampleDataTabContentProps {
  indexName: string;
  onSuccess: () => void;
}

const COUNTS = [10, 25, 50, 100] as const;

export function SampleDataTabContent({ indexName, onSuccess }: SampleDataTabContentProps) {
  const [count, setCount] = useState<number>(25);
  const addDocuments = useAddDocuments(indexName);

  const selected = moviesData.slice(0, count);
  const preview = selected.slice(0, 5);

  const handleLoad = async () => {
    try {
      await addDocuments.mutateAsync(selected);
      onSuccess();
    } catch {
      // error handled by the hook's onError toast
    }
  };

  return (
    <div className="space-y-4">
      {/* Description */}
      <div className="flex items-start gap-3 p-3 rounded-md bg-muted/50">
        <Database className="h-5 w-5 text-muted-foreground mt-0.5 shrink-0" />
        <div>
          <p className="text-sm font-medium">Movies Dataset</p>
          <p className="text-xs text-muted-foreground mt-0.5">
            100 popular movies with title, year, genre, rating, overview, and director.
            Great for testing search, faceting, and filtering.
          </p>
        </div>
      </div>

      {/* Count selector */}
      <div className="space-y-1.5">
        <p className="text-xs text-muted-foreground font-medium">How many movies?</p>
        <div className="flex gap-1.5">
          {COUNTS.map((n) => (
            <Button
              key={n}
              variant={count === n ? 'default' : 'outline'}
              size="sm"
              onClick={() => setCount(n)}
              className="min-w-[3rem]"
            >
              {n}
            </Button>
          ))}
        </div>
      </div>

      {/* Preview table */}
      <div className="border rounded-md overflow-hidden">
        <table className="w-full text-sm">
          <thead>
            <tr className="bg-muted/50">
              <th className="text-left p-2 font-medium text-muted-foreground">Title</th>
              <th className="text-left p-2 font-medium text-muted-foreground w-16">Year</th>
              <th className="text-left p-2 font-medium text-muted-foreground hidden sm:table-cell">Genre</th>
              <th className="text-right p-2 font-medium text-muted-foreground w-16">Rating</th>
            </tr>
          </thead>
          <tbody>
            {preview.map((m) => (
              <tr key={m.objectID} className="border-t">
                <td className="p-2 truncate max-w-[200px]">{m.title}</td>
                <td className="p-2">{m.year}</td>
                <td className="p-2 text-muted-foreground truncate max-w-[150px] hidden sm:table-cell">
                  {m.genre.join(', ')}
                </td>
                <td className="p-2 text-right">{m.rating}</td>
              </tr>
            ))}
          </tbody>
        </table>
        <div className="p-2 text-xs text-muted-foreground bg-muted/30 border-t">
          Showing 5 of {count} selected movies
        </div>
      </div>

      {/* Load button */}
      <Button
        onClick={handleLoad}
        disabled={addDocuments.isPending}
        className="w-full"
      >
        {addDocuments.isPending ? (
          <>
            <Loader2 className="h-4 w-4 mr-2 animate-spin" />
            Loading...
          </>
        ) : (
          <>
            <Database className="h-4 w-4 mr-2" />
            Load {count} Movies
          </>
        )}
      </Button>
    </div>
  );
}
