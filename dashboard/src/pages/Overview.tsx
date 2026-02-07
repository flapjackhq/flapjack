import { useState, useMemo } from 'react';
import { Link, useNavigate } from 'react-router-dom';
import { useIndices } from '@/hooks/useIndices';
import { useHealth } from '@/hooks/useHealth';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Plus, ChevronLeft, ChevronRight } from 'lucide-react';
import { Skeleton } from '@/components/ui/skeleton';
import { CreateIndexDialog } from '@/components/indices/CreateIndexDialog';
import { formatBytes, formatDate } from '@/lib/utils';

const ITEMS_PER_PAGE = 10;

export function Overview() {
  const { data: indices, isLoading, error } = useIndices();
  const { data: health, isLoading: healthLoading, error: healthError } = useHealth();
  const [currentPage, setCurrentPage] = useState(1);
  const [showCreateDialog, setShowCreateDialog] = useState(false);
  const navigate = useNavigate();

  const totalDocs = indices?.reduce((sum, idx) => sum + (idx.entries || 0), 0) || 0;
  const totalSize = indices?.reduce((sum, idx) => sum + (idx.dataSize || 0), 0) || 0;

  // Pagination
  const totalPages = Math.ceil((indices?.length || 0) / ITEMS_PER_PAGE);
  const paginatedIndices = useMemo(() => {
    if (!indices) return [];
    const start = (currentPage - 1) * ITEMS_PER_PAGE;
    return indices.slice(start, start + ITEMS_PER_PAGE);
  }, [indices, currentPage]);

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h1 className="text-3xl font-bold">Overview</h1>
        <Button onClick={() => setShowCreateDialog(true)}>
          <Plus className="mr-2 h-4 w-4" /> Create Index
        </Button>
      </div>

      {/* Stats Cards */}
      <div className="grid gap-4 grid-cols-2 md:grid-cols-4">
        <Card data-testid="stat-card">
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">
              Indices
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold">{indices?.length || 0}</div>
          </CardContent>
        </Card>
        <Card data-testid="stat-card">
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">
              Documents
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold">{totalDocs.toLocaleString()}</div>
          </CardContent>
        </Card>
        <Card data-testid="stat-card">
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">
              Storage
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold">{formatBytes(totalSize)}</div>
          </CardContent>
        </Card>
        <Card data-testid="stat-card">
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">
              Status
            </CardTitle>
          </CardHeader>
          <CardContent>
            {healthLoading ? (
              <Skeleton className="h-8 w-28" />
            ) : healthError ? (
              <div className="text-2xl font-bold text-red-600">Disconnected</div>
            ) : health?.status === 'ok' ? (
              <div className="text-2xl font-bold text-green-600">Healthy</div>
            ) : (
              <div className="text-2xl font-bold text-yellow-600">Unknown</div>
            )}
          </CardContent>
        </Card>
      </div>

      {/* Index List */}
      <Card>
        <CardHeader>
          <CardTitle>Indices</CardTitle>
        </CardHeader>
        <CardContent>
          {isLoading ? (
            <div className="space-y-2 py-2">
              {[1, 2, 3].map((i) => (
                <div key={i} className="flex items-center justify-between p-4 rounded-md border border-border">
                  <div className="space-y-2 flex-1">
                    <Skeleton className="h-5 w-40" />
                    <Skeleton className="h-4 w-64" />
                  </div>
                  <div className="flex gap-2">
                    <Skeleton className="h-8 w-20 rounded-md" />
                    <Skeleton className="h-8 w-20 rounded-md" />
                  </div>
                </div>
              ))}
            </div>
          ) : error ? (
            <div className="text-center py-8 text-red-600">
              Error loading indices: {error instanceof Error ? error.message : 'Unknown error'}
            </div>
          ) : indices && indices.length > 0 ? (
            <div className="space-y-4">
              <div className="space-y-2">
                {paginatedIndices.map((index) => (
                  <div
                    key={index.uid}
                    className="flex items-center justify-between p-4 rounded-md border border-border hover:bg-accent/50 transition-colors cursor-pointer"
                    onClick={() => navigate(`/search/${encodeURIComponent(index.uid)}`)}
                  >
                    <div>
                      <h3 className="font-medium">{index.uid}</h3>
                      <p className="text-sm text-muted-foreground">
                        {index.entries?.toLocaleString() || 0} documents · {formatBytes(index.dataSize || 0)}
                        {index.updatedAt && ` · Updated ${formatDate(index.updatedAt)}`}
                      </p>
                    </div>
                    <div className="flex gap-2" onClick={(e) => e.stopPropagation()}>
                      <Link to="/settings">
                        <Button variant="outline" size="sm">
                          Settings
                        </Button>
                      </Link>
                    </div>
                  </div>
                ))}
              </div>

              {/* Pagination */}
              {totalPages > 1 && (
                <div className="flex items-center justify-between pt-4 border-t">
                  <div className="text-sm text-muted-foreground">
                    Showing {((currentPage - 1) * ITEMS_PER_PAGE) + 1}-{Math.min(currentPage * ITEMS_PER_PAGE, indices.length)} of {indices.length} indices
                  </div>
                  <div className="flex gap-2">
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => setCurrentPage(p => Math.max(1, p - 1))}
                      disabled={currentPage === 1}
                    >
                      <ChevronLeft className="h-4 w-4" />
                      Previous
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => setCurrentPage(p => Math.min(totalPages, p + 1))}
                      disabled={currentPage === totalPages}
                    >
                      Next
                      <ChevronRight className="h-4 w-4" />
                    </Button>
                  </div>
                </div>
              )}
            </div>
          ) : (
            <div className="text-center py-8 text-muted-foreground">
              No indices yet. Create your first index to get started.
            </div>
          )}
        </CardContent>
      </Card>

      <CreateIndexDialog
        open={showCreateDialog}
        onOpenChange={setShowCreateDialog}
      />
    </div>
  );
}
