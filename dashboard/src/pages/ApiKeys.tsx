import { useState, useCallback } from 'react';
import { Plus, Copy, Trash2, Check } from 'lucide-react';
import { useApiKeys, useDeleteApiKey } from '@/hooks/useApiKeys';
import { Card } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Badge } from '@/components/ui/badge';
import { Skeleton } from '@/components/ui/skeleton';
import { CreateKeyDialog } from '@/components/keys/CreateKeyDialog';

export function ApiKeys() {
  const { data: keys, isLoading } = useApiKeys();
  const deleteKey = useDeleteApiKey();
  const [showCreateDialog, setShowCreateDialog] = useState(false);
  const [copiedKey, setCopiedKey] = useState<string | null>(null);

  const handleCopy = useCallback(async (keyValue: string) => {
    try {
      await navigator.clipboard.writeText(keyValue);
      setCopiedKey(keyValue);
      setTimeout(() => setCopiedKey(null), 2000);
    } catch (err) {
      console.error('Failed to copy:', err);
    }
  }, []);

  const handleDelete = useCallback(
    async (keyValue: string, description: string) => {
      const confirmed = confirm(
        `Are you sure you want to delete the API key "${description || keyValue}"? This action cannot be undone.`
      );
      if (!confirmed) return;

      try {
        await deleteKey.mutateAsync(keyValue);
      } catch (err) {
        console.error('Failed to delete key:', err);
      }
    },
    [deleteKey]
  );

  if (isLoading) {
    return (
      <div className="space-y-6">
        <div className="flex items-center justify-between">
          <div className="space-y-2">
            <Skeleton className="h-8 w-32" />
            <Skeleton className="h-4 w-64" />
          </div>
          <Skeleton className="h-10 w-28 rounded-md" />
        </div>
        {[1, 2].map((i) => (
          <Card key={i} className="p-6 space-y-4">
            <div className="flex items-start justify-between">
              <div className="space-y-2">
                <Skeleton className="h-5 w-36" />
                <Skeleton className="h-6 w-64 rounded" />
              </div>
              <Skeleton className="h-8 w-8 rounded-md" />
            </div>
            <div className="space-y-2">
              <Skeleton className="h-4 w-24" />
              <div className="flex gap-2">
                <Skeleton className="h-6 w-16 rounded-full" />
                <Skeleton className="h-6 w-16 rounded-full" />
              </div>
            </div>
          </Card>
        ))}
      </div>
    );
  }

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-2xl font-bold">API Keys</h2>
          <p className="text-sm text-muted-foreground mt-1">
            Manage API keys for authenticating requests to Flapjack
          </p>
        </div>
        <Button onClick={() => setShowCreateDialog(true)}>
          <Plus className="h-4 w-4 mr-1" />
          Create Key
        </Button>
      </div>

      {/* Keys list */}
      {!keys || keys.length === 0 ? (
        <Card className="p-8 text-center">
          <h3 className="text-lg font-semibold mb-2">No API keys</h3>
          <p className="text-sm text-muted-foreground mb-4">
            Create an API key to start making authenticated requests
          </p>
          <Button onClick={() => setShowCreateDialog(true)}>
            <Plus className="h-4 w-4 mr-1" />
            Create Your First Key
          </Button>
        </Card>
      ) : (
        <div className="space-y-4" data-testid="keys-list">
          {keys.map((key) => (
            <Card key={key.value} className="p-6">
              <div className="space-y-4" data-testid="keys-list">
                {/* Header */}
                <div className="flex items-start justify-between">
                  <div>
                    <h3 className="font-semibold">
                      {key.description || 'Untitled Key'}
                    </h3>
                    <div className="flex items-center gap-2 mt-2">
                      <code className="text-sm bg-muted px-2 py-1 rounded font-mono">
                        {key.value}
                      </code>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => handleCopy(key.value)}
                      >
                        {copiedKey === key.value ? (
                          <>
                            <Check className="h-3 w-3 mr-1" />
                            Copied
                          </>
                        ) : (
                          <>
                            <Copy className="h-3 w-3 mr-1" />
                            Copy
                          </>
                        )}
                      </Button>
                    </div>
                  </div>

                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => handleDelete(key.value, key.description || '')}
                    disabled={deleteKey.isPending}
                  >
                    <Trash2 className="h-4 w-4 text-destructive" />
                  </Button>
                </div>

                {/* ACL */}
                <div>
                  <div className="text-sm font-medium mb-2">Permissions</div>
                  <div className="flex flex-wrap gap-2">
                    {key.acl.map((permission) => (
                      <Badge key={permission} variant="secondary">
                        {permission}
                      </Badge>
                    ))}
                  </div>
                </div>

                {/* Details */}
                <div className="grid grid-cols-2 md:grid-cols-4 gap-4 text-sm">
                  {key.indexes && key.indexes.length > 0 && (
                    <div>
                      <div className="text-muted-foreground">Indices</div>
                      <div className="font-medium">{key.indexes.join(', ')}</div>
                    </div>
                  )}

                  {key.maxHitsPerQuery && (
                    <div>
                      <div className="text-muted-foreground">Max Hits/Query</div>
                      <div className="font-medium">
                        {key.maxHitsPerQuery.toLocaleString()}
                      </div>
                    </div>
                  )}

                  {key.maxQueriesPerIPPerHour && (
                    <div>
                      <div className="text-muted-foreground">Max Queries/IP/Hour</div>
                      <div className="font-medium">
                        {key.maxQueriesPerIPPerHour.toLocaleString()}
                      </div>
                    </div>
                  )}

                  {key.expiresAt && (
                    <div>
                      <div className="text-muted-foreground">Expires</div>
                      <div className="font-medium">
                        {new Date(key.expiresAt * 1000).toLocaleDateString()}
                      </div>
                    </div>
                  )}

                  <div>
                    <div className="text-muted-foreground">Created</div>
                    <div className="font-medium">
                      {new Date(key.createdAt * 1000).toLocaleDateString()}
                    </div>
                  </div>
                </div>
              </div>
            </Card>
          ))}
        </div>
      )}

      {/* Create dialog */}
      <CreateKeyDialog
        open={showCreateDialog}
        onOpenChange={setShowCreateDialog}
      />
    </div>
  );
}
