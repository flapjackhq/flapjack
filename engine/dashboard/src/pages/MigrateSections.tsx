import { Link } from 'react-router-dom';
import {
  ArrowRightLeft,
  Loader2,
  CheckCircle2,
  XCircle,
  Eye,
  EyeOff,
  AlertTriangle,
  RefreshCw,
} from 'lucide-react';
import type { AlgoliaIndexInfo, MigrationResult } from './migrateHelpers';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Switch } from '@/components/ui/switch';

interface MigrationHeaderProps {
  effectiveTarget: string;
}

export function MigrationHeader({ effectiveTarget }: MigrationHeaderProps) {
  return (
    <div>
      <h1 className="text-3xl font-bold">Migrate from Algolia</h1>
      <p className="text-muted-foreground mt-1">
        Import an index from Algolia{effectiveTarget ? ` into "${effectiveTarget}"` : ''} — settings, documents, synonyms, and rules.
      </p>
    </div>
  );
}

interface MigrationCredentialsCardProps {
  appId: string;
  apiKey: string;
  showKey: boolean;
  migrationPending: boolean;
  hasCredentials: boolean;
  canFetchIndexes: boolean;
  fetchIndexesPending: boolean;
  algoliaIndexesLoaded: boolean;
  onAppIdChange: (value: string) => void;
  onApiKeyChange: (value: string) => void;
  onToggleShowKey: () => void;
  onFetchIndexes: () => void;
}

export function MigrationCredentialsCard({
  appId,
  apiKey,
  showKey,
  migrationPending,
  hasCredentials,
  canFetchIndexes,
  fetchIndexesPending,
  algoliaIndexesLoaded,
  onAppIdChange,
  onApiKeyChange,
  onToggleShowKey,
  onFetchIndexes,
}: MigrationCredentialsCardProps) {
  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-lg">Algolia Credentials</CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
          <div className="space-y-2">
            <Label htmlFor="app-id">Application ID</Label>
            <Input
              id="app-id"
              value={appId}
              onChange={(event) => onAppIdChange(event.target.value)}
              placeholder="YourAlgoliaAppId"
              disabled={migrationPending}
              autoComplete="off"
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="api-key">Admin API Key</Label>
            <div className="relative">
              <Input
                id="api-key"
                type={showKey ? 'text' : 'password'}
                value={apiKey}
                onChange={(event) => onApiKeyChange(event.target.value)}
                placeholder="Your Algolia Admin API key"
                disabled={migrationPending}
                autoComplete="off"
                className="pr-10"
              />
              <button
                type="button"
                onClick={onToggleShowKey}
                aria-label={showKey ? 'Hide API key' : 'Show API key'}
                className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground"
                tabIndex={-1}
                data-testid="toggle-api-key-visibility"
              >
                {showKey ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
              </button>
            </div>
            <p className="text-xs text-muted-foreground">
              Needs read access. Not stored anywhere.
            </p>
          </div>
        </div>

        {hasCredentials && (
          <div className="pt-1">
            <Button
              variant="outline"
              size="sm"
              onClick={onFetchIndexes}
              disabled={!canFetchIndexes || migrationPending}
            >
              {fetchIndexesPending ? (
                <>
                  <Loader2 className="h-4 w-4 mr-2 animate-spin" />
                  Loading indexes...
                </>
              ) : (
                <>
                  <RefreshCw className="h-4 w-4 mr-2" />
                  {algoliaIndexesLoaded ? 'Refresh Indexes' : 'Load Indexes from Algolia'}
                </>
              )}
            </Button>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

interface AlgoliaIndexPickerCardProps {
  algoliaIndexes: AlgoliaIndexInfo[] | null;
  sourceIndex: string;
  migrationPending: boolean;
  indexListError: string | null;
  onSelectSourceIndex: (value: string) => void;
}

export function AlgoliaIndexPickerCard({
  algoliaIndexes,
  sourceIndex,
  migrationPending,
  indexListError,
  onSelectSourceIndex,
}: AlgoliaIndexPickerCardProps) {
  return (
    <>
      {algoliaIndexes && algoliaIndexes.length > 0 && (
        <Card>
          <CardHeader>
            <CardTitle className="text-lg">
              Select Source Index
              <span className="text-muted-foreground font-normal text-sm ml-2">
                {algoliaIndexes.length} index{algoliaIndexes.length !== 1 ? 'es' : ''} found
              </span>
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className="space-y-1 max-h-64 overflow-y-auto">
              {algoliaIndexes.map((index) => (
                <button
                  key={index.name}
                  type="button"
                  onClick={() => onSelectSourceIndex(index.name)}
                  className={`w-full text-left px-3 py-2 rounded-md text-sm transition-colors ${
                    sourceIndex === index.name ? 'bg-primary text-primary-foreground' : 'hover:bg-muted'
                  }`}
                  disabled={migrationPending}
                >
                  <span className="font-medium">{index.name}</span>
                  <span className={`ml-2 text-xs ${
                    sourceIndex === index.name ? 'text-primary-foreground/70' : 'text-muted-foreground'
                  }`}>
                    {index.entries.toLocaleString()} record{index.entries !== 1 ? 's' : ''}
                  </span>
                </button>
              ))}
            </div>
          </CardContent>
        </Card>
      )}

      {algoliaIndexes && algoliaIndexes.length === 0 && (
        <Card className="border-yellow-500/50">
          <CardContent className="pt-6">
            <p className="text-sm text-muted-foreground">
              No indexes found in this Algolia account. Check your Application ID.
            </p>
          </CardContent>
        </Card>
      )}

      {indexListError && (
        <Card className="border-yellow-500/50">
          <CardContent className="pt-6">
            <p className="text-sm text-muted-foreground">{indexListError}</p>
          </CardContent>
        </Card>
      )}
    </>
  );
}

interface MigrationIndexNamesCardProps {
  algoliaIndexesLoaded: boolean;
  sourceIndex: string;
  targetIndex: string;
  trimmedSourceIndex: string;
  overwrite: boolean;
  migrationPending: boolean;
  onSourceIndexChange: (value: string) => void;
  onTargetIndexChange: (value: string) => void;
  onOverwriteChange: (value: boolean) => void;
}

export function MigrationIndexNamesCard({
  algoliaIndexesLoaded,
  sourceIndex,
  targetIndex,
  trimmedSourceIndex,
  overwrite,
  migrationPending,
  onSourceIndexChange,
  onTargetIndexChange,
  onOverwriteChange,
}: MigrationIndexNamesCardProps) {
  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-lg">Index Name</CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
          <div className="space-y-2">
            <Label htmlFor="source-index">Source Index (Algolia)</Label>
            <Input
              id="source-index"
              value={sourceIndex}
              onChange={(event) => onSourceIndexChange(event.target.value)}
              placeholder={algoliaIndexesLoaded ? 'Select above or type name' : 'e.g., products, articles'}
              disabled={migrationPending}
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="target-index">
              Target Index (Flapjack)
              <span className="text-muted-foreground font-normal ml-1">— optional</span>
            </Label>
            <Input
              id="target-index"
              value={targetIndex}
              onChange={(event) => onTargetIndexChange(event.target.value)}
              placeholder={trimmedSourceIndex || 'Same as source'}
              disabled={migrationPending}
            />
            <p className="text-xs text-muted-foreground">
              Defaults to the source index name if left blank.
            </p>
          </div>
        </div>

        <div className="flex items-center gap-3 pt-2">
          <Switch
            id="overwrite"
            checked={overwrite}
            onCheckedChange={onOverwriteChange}
            disabled={migrationPending}
          />
          <div>
            <Label htmlFor="overwrite" className="cursor-pointer">
              Overwrite if exists
            </Label>
            <p className="text-xs text-muted-foreground">
              If the target index already exists, delete it first and re-import.
            </p>
          </div>
        </div>
      </CardContent>
    </Card>
  );
}

interface MigrationSubmitButtonProps {
  canSubmit: boolean;
  migrationPending: boolean;
  effectiveTarget: string;
  onSubmit: () => void;
}

export function MigrationSubmitButton({
  canSubmit,
  migrationPending,
  effectiveTarget,
  onSubmit,
}: MigrationSubmitButtonProps) {
  return (
    <Button size="lg" onClick={onSubmit} disabled={!canSubmit} className="w-full">
      {migrationPending ? (
        <>
          <Loader2 className="h-5 w-5 mr-2 animate-spin" />
          Migrating from Algolia...
        </>
      ) : (
        <>
          <ArrowRightLeft className="h-5 w-5 mr-2" />
          Migrate{effectiveTarget ? ` "${effectiveTarget}"` : ''}
        </>
      )}
    </Button>
  );
}

interface MigrationSuccessCardProps {
  migrationData: MigrationResult;
  effectiveTarget: string;
}

export function MigrationSuccessCard({ migrationData, effectiveTarget }: MigrationSuccessCardProps) {
  return (
    <Card className="border-green-500/50">
      <CardContent className="pt-6">
        <div className="flex items-start gap-3">
          <CheckCircle2 className="h-6 w-6 text-green-500 shrink-0 mt-0.5" />
          <div className="space-y-3 flex-1">
            <div>
              <h3 className="font-semibold text-lg">Migration complete</h3>
              <p className="text-sm text-muted-foreground">
                Index <span className="font-medium">{effectiveTarget}</span> is ready.
              </p>
            </div>

            <div className="grid grid-cols-2 sm:grid-cols-4 gap-3">
              <ResultStat label="Documents" value={migrationData.objects.imported} />
              <ResultStat label="Settings" value={migrationData.settings ? 'Applied' : 'None'} />
              <ResultStat label="Synonyms" value={migrationData.synonyms.imported} />
              <ResultStat label="Rules" value={migrationData.rules.imported} />
            </div>

            <div className="flex gap-2 pt-1">
              <Link to={`/index/${encodeURIComponent(effectiveTarget)}`}>
                <Button size="sm">Browse Index</Button>
              </Link>
              <Link to={`/index/${encodeURIComponent(effectiveTarget)}/settings`}>
                <Button variant="outline" size="sm">
                  View Settings
                </Button>
              </Link>
            </div>
          </div>
        </div>
      </CardContent>
    </Card>
  );
}

export function MigrationErrorCard({ errorMessage }: { errorMessage: string }) {
  return (
    <Card className="border-destructive/50" data-testid="migration-error-card">
      <CardContent className="pt-6">
        <div className="flex items-start gap-3">
          <XCircle className="h-6 w-6 text-destructive shrink-0 mt-0.5" />
          <div className="space-y-1">
            <h3 className="font-semibold">Migration failed</h3>
            <p className="text-sm text-muted-foreground">{errorMessage}</p>
          </div>
        </div>
      </CardContent>
    </Card>
  );
}

export function MigrationInfoCard() {
  return (
    <Card className="bg-muted/30">
      <CardContent className="pt-6">
        <div className="flex items-start gap-3">
          <AlertTriangle className="h-5 w-5 text-muted-foreground shrink-0 mt-0.5" />
          <div className="space-y-2 text-sm text-muted-foreground">
            <p>
              <span className="font-medium text-foreground">What gets migrated:</span>{' '}
              Settings (searchable attributes, facets, ranking), all documents, synonyms, and query rules.
            </p>
            <p>
              <span className="font-medium text-foreground">Credentials:</span>{' '}
              Your Algolia API key is sent directly to the Flapjack server to fetch data from Algolia&apos;s API. It is not stored or logged.
            </p>
            <p>
              <span className="font-medium text-foreground">Large indexes:</span>{' '}
              Documents are fetched in batches. Migration may take a few minutes for indexes with millions of records.
            </p>
          </div>
        </div>
      </CardContent>
    </Card>
  );
}

function ResultStat({
  label,
  value,
}: {
  label: string;
  value: number | string;
}) {
  return (
    <div className="rounded-md border p-3 text-center">
      <div className="text-xl font-bold" data-testid={`migrate-stat-${label.toLowerCase()}`}>
        {typeof value === 'number' ? value.toLocaleString() : value}
      </div>
      <div className="text-xs text-muted-foreground">{label}</div>
    </div>
  );
}
