import { Link } from 'react-router-dom';
import { AlertTriangle, ArrowRightLeft, CheckCircle2, Eye, EyeOff, Loader2, RefreshCw, XCircle } from 'lucide-react';
import type {
  AsyncMigrationStatusResponse,
  ListSourceIndexesResponse,
  MigrationPreviewReportEntry,
  MigrationPreviewResponse,
  MigrationProviderDescriptor,
  MigrationProviderId,
  MigrationResult,
  SourceIndexSummary,
} from './migrateHelpers';
import {
  formatMigrationPreviewEntryMeta,
  isHardMigrationPreviewEntry,
  orderMigrationPreviewEntries,
} from './migrateHelpers';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Switch } from '@/components/ui/switch';

interface MigrationHeaderProps {
  provider: MigrationProviderDescriptor;
  effectiveTarget: string;
}

export function MigrationHeader({ provider, effectiveTarget }: MigrationHeaderProps) {
  return (
    <div>
      <h1 className="text-3xl font-bold">Migrate from {provider.displayName}</h1>
      <p className="text-muted-foreground mt-1">
        Import a source into Flapjack
        {effectiveTarget ? ` as "${effectiveTarget}"` : ''} with settings, documents, synonyms, and rules.
      </p>
    </div>
  );
}

interface MigrationCredentialsCardProps {
  providers: readonly MigrationProviderDescriptor[];
  provider: MigrationProviderDescriptor;
  values: {
    firstCredentialValue: string;
    apiKey: string;
    showKey: boolean;
  };
  status: {
    controlsLocked: boolean;
    hasCredentials: boolean;
    canFetchSources: boolean;
    fetchSourcesPending: boolean;
  };
  actions: {
    selectProvider: (provider: MigrationProviderId) => void;
    changeFirstCredential: (value: string) => void;
    changeApiKey: (value: string) => void;
    toggleApiKeyVisibility: () => void;
    fetchSources: () => void;
  };
}

export function MigrationCredentialsCard({
  providers,
  provider,
  values,
  status,
  actions,
}: MigrationCredentialsCardProps) {
  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-lg">{provider.displayName} Credentials</CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        <MigrationProviderSelector
          providers={providers}
          provider={provider}
          controlsLocked={status.controlsLocked}
          onSelect={actions.selectProvider}
        />
        <MigrationCredentialFields
          provider={provider}
          values={values}
          controlsLocked={status.controlsLocked}
          actions={actions}
        />
        <DiscoverSourcesButton status={status} onFetch={actions.fetchSources} />
      </CardContent>
    </Card>
  );
}

function MigrationProviderSelector({
  providers,
  provider,
  controlsLocked,
  onSelect,
}: {
  providers: readonly MigrationProviderDescriptor[];
  provider: MigrationProviderDescriptor;
  controlsLocked: boolean;
  onSelect: (provider: MigrationProviderId) => void;
}) {
  return (
    <div className="flex flex-wrap gap-2" aria-label="Migration provider">
      {providers.map((candidate) => (
        <Button
          key={candidate.id}
          type="button"
          variant={candidate.id === provider.id ? 'default' : 'outline'}
          size="sm"
          onClick={() => onSelect(candidate.id)}
          disabled={controlsLocked}
          aria-pressed={candidate.id === provider.id}
          data-testid={`migration-provider-${candidate.id}`}
        >
          {candidate.displayName}
        </Button>
      ))}
    </div>
  );
}

function MigrationCredentialFields({
  provider,
  values,
  controlsLocked,
  actions,
}: Pick<MigrationCredentialsCardProps, 'provider' | 'values' | 'actions'> & {
  controlsLocked: boolean;
}) {
  return (
    <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
      <div className="space-y-2">
        <Label htmlFor="migration-first-credential">{provider.firstCredentialField.label}</Label>
        <Input
          id="migration-first-credential"
          value={values.firstCredentialValue}
          onChange={(event) => actions.changeFirstCredential(event.target.value)}
          placeholder={provider.firstCredentialField.placeholder}
          disabled={controlsLocked}
          autoComplete="off"
          data-testid="migration-first-credential"
        />
      </div>
      <div className="space-y-2">
        <Label htmlFor="api-key">{provider.apiKeyLabel}</Label>
        <div className="relative">
          <Input
            id="api-key"
            type={values.showKey ? 'text' : 'password'}
            value={values.apiKey}
            onChange={(event) => actions.changeApiKey(event.target.value)}
            placeholder={`${provider.displayName} API key`}
            disabled={controlsLocked}
            autoComplete="off"
            className="pr-10"
            data-testid="migration-api-key"
          />
          <button
            type="button"
            onClick={actions.toggleApiKeyVisibility}
            aria-label={values.showKey ? 'Hide API key' : 'Show API key'}
            className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground"
            tabIndex={-1}
            data-testid="toggle-api-key-visibility"
          >
            {values.showKey ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
          </button>
        </div>
        <p className="text-xs text-muted-foreground">Needs read access. Not stored anywhere.</p>
      </div>
    </div>
  );
}

function DiscoverSourcesButton({
  status,
  onFetch,
}: {
  status: MigrationCredentialsCardProps['status'];
  onFetch: () => void;
}) {
  return (
    <div className="pt-1">
      <Button
        variant="outline"
        size="sm"
        onClick={onFetch}
        disabled={!status.hasCredentials || !status.canFetchSources || status.controlsLocked}
        data-testid="migration-discover-sources"
      >
        {status.fetchSourcesPending ? (
          <>
            <Loader2 className="h-4 w-4 mr-2 animate-spin" />
            Discovering...
          </>
        ) : (
          <>
            <RefreshCw className="h-4 w-4 mr-2" />
            Discover sources
          </>
        )}
      </Button>
    </div>
  );
}

interface SourceIndexPickerCardProps {
  provider: MigrationProviderDescriptor;
  state: {
    discoveryResponse: ListSourceIndexesResponse | null;
    sourceIndex: string;
    controlsLocked: boolean;
    indexListError: string | null;
  };
  onSelectSourceIndex: (value: string) => void;
}

export function SourceIndexPickerCard({
  provider,
  state,
  onSelectSourceIndex,
}: SourceIndexPickerCardProps) {
  const { discoveryResponse, sourceIndex, controlsLocked, indexListError } = state;
  const sources = discoveryResponse?.indexes ?? null;
  const paginationSummary = discoveryResponse
    ? formatPaginationSummary(discoveryResponse)
    : null;
  return (
    <>
      {sources && sources.length > 0 && (
        <Card>
          <CardHeader>
            <CardTitle className="text-lg">
              Select Source
              <span className="text-muted-foreground font-normal text-sm ml-2">{sources.length} found</span>
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div
              className="space-y-1 max-h-64 overflow-y-auto"
              role="listbox"
              aria-label={`${provider.displayName} sources`}
            >
              {sources.map((source) => (
                <button
                  key={source.name}
                  type="button"
                  role="option"
                  aria-selected={sourceIndex === source.name}
                  onClick={() => onSelectSourceIndex(source.name)}
                  className={`w-full text-left px-3 py-2 rounded-md text-sm transition-colors ${
                    sourceIndex === source.name ? 'bg-primary text-primary-foreground' : 'hover:bg-muted'
                  }`}
                  disabled={controlsLocked}
                  data-testid={`migration-source-option-${source.name}`}
                >
                  <span className="font-medium">{source.name}</span>
                  {formatSourceCount(source) && (
                    <span className={`ml-2 text-xs ${
                      sourceIndex === source.name ? 'text-primary-foreground/70' : 'text-muted-foreground'
                    }`}>
                      {formatSourceCount(source)}
                    </span>
                  )}
                  {source.defaultSortingField && (
                    <span className={`ml-2 text-xs ${
                      sourceIndex === source.name ? 'text-primary-foreground/70' : 'text-muted-foreground'
                    }`}>
                      sort: {source.defaultSortingField}
                    </span>
                  )}
                </button>
              ))}
            </div>
            {paginationSummary && (
              <p
                className="mt-3 text-xs text-muted-foreground"
                data-testid="migration-source-pagination"
              >
                {paginationSummary}
              </p>
            )}
          </CardContent>
        </Card>
      )}

      {sources && sources.length === 0 && (
        <Card className="border-yellow-500/50">
          <CardContent className="pt-6">
            <p className="text-sm text-muted-foreground">
              No sources found. Check your {provider.firstCredentialField.label}.
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
  provider: MigrationProviderDescriptor;
  values: {
    sourcesLoaded: boolean;
    sourceIndex: string;
    targetIndex: string;
    trimmedSourceIndex: string;
    overwrite: boolean;
    sourceWriteFrozen: boolean;
  };
  controlsLocked: boolean;
  actions: {
    changeSourceIndex: (value: string) => void;
    changeTargetIndex: (value: string) => void;
    changeOverwrite: (value: boolean) => void;
    changeSourceWriteFrozen: (value: boolean) => void;
  };
}

export function MigrationIndexNamesCard({ provider, values, controlsLocked, actions }: MigrationIndexNamesCardProps) {
  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-lg">Index Name</CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
          <div className="space-y-2">
            <Label htmlFor="source-index">{provider.sourceFieldLabel}</Label>
            <Input id="source-index" value={values.sourceIndex}
              onChange={(event) => actions.changeSourceIndex(event.target.value)}
              placeholder={values.sourcesLoaded ? 'Select above or type name' : 'e.g., products, articles'}
              disabled={controlsLocked}
              data-testid="migration-source-index"
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="target-index">
              Target Index (Flapjack)
              <span className="text-muted-foreground font-normal ml-1">- optional</span>
            </Label>
            <Input id="target-index" value={values.targetIndex}
              onChange={(event) => actions.changeTargetIndex(event.target.value)}
              placeholder={values.trimmedSourceIndex || 'Same as source'}
              disabled={controlsLocked}
              data-testid="migration-target-index"
            />
            <p className="text-xs text-muted-foreground">Defaults to the source index name if left blank.</p>
          </div>
        </div>

        <div className="flex items-center gap-3 pt-2">
          <Switch id="overwrite" checked={values.overwrite}
            onCheckedChange={actions.changeOverwrite} disabled={controlsLocked}
            aria-label="Overwrite if exists"
            data-testid="migration-overwrite"
          />
          <div>
            <Label htmlFor="overwrite" className="cursor-pointer">Overwrite if exists</Label>
            <p className="text-xs text-muted-foreground">If the target index already exists, delete it first and re-import.</p>
          </div>
        </div>

        {provider.id === 'typesense' && (
          <div className="flex items-start gap-3 pt-2">
            <input id="typesense-source-write-frozen" type="checkbox"
              checked={values.sourceWriteFrozen}
              onChange={(event) => actions.changeSourceWriteFrozen(event.target.checked)}
              disabled={controlsLocked} data-testid="typesense-source-write-frozen"
              className="mt-1 h-4 w-4 rounded border-input accent-primary" />
            <div>
              <Label htmlFor="typesense-source-write-frozen" className="cursor-pointer">I have paused writes to the selected Typesense collection for the complete migration.</Label>
              <p className="text-xs text-muted-foreground">Keep writes paused until the migration reports completion.</p>
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

interface MigrationSubmitButtonProps {
  provider: MigrationProviderDescriptor;
  state: {
    canSubmit: boolean;
    canPreview: boolean;
    hasPreview: boolean;
    previewFailed: boolean;
    previewHasBlockers: boolean;
    previewPending: boolean;
    migrationPending: boolean;
    effectiveTarget: string;
  };
  onEditSource: () => void;
  onPreview: () => void;
  onSubmit: () => void;
}

export function MigrationSubmitButton({
  provider,
  state,
  onEditSource,
  onPreview,
  onSubmit,
}: MigrationSubmitButtonProps) {
  if (state.previewPending) {
    return (
      <Button size="lg" disabled className="w-full" data-testid="migration-preview-trigger">
        <Loader2 className="h-5 w-5 mr-2 animate-spin" />
        Previewing...
      </Button>
    );
  }

  if (state.previewFailed) {
    return (
      <Button size="lg" onClick={onEditSource} className="w-full" data-testid="migration-edit-source">
        <RefreshCw className="h-5 w-5 mr-2" />
        Edit source
      </Button>
    );
  }

  if (!state.hasPreview) {
    return (
      <Button
        size="lg"
        onClick={onPreview}
        disabled={!state.canPreview}
        className="w-full"
        data-testid="migration-preview-trigger"
      >
        <RefreshCw className="h-5 w-5 mr-2" />
        Preview migration
      </Button>
    );
  }

  if (state.previewHasBlockers) {
    return (
      <Button
        size="lg"
        disabled
        className="w-full"
        data-testid="migration-submit"
      >
        <AlertTriangle className="h-5 w-5 mr-2" />
        Review blockers
      </Button>
    );
  }

  return (
    <Button
      size="lg"
      onClick={onSubmit}
      disabled={!state.canSubmit}
      className="w-full"
      data-testid="migration-submit"
    >
      {state.migrationPending ? (
        <>
          <Loader2 className="h-5 w-5 mr-2 animate-spin" />
          Migrating from {provider.displayName}...
        </>
      ) : (
        <>
          <ArrowRightLeft className="h-5 w-5 mr-2" />
          Submit migration
        </>
      )}
    </Button>
  );
}

interface MigrationPreviewCardProps {
  preview: MigrationPreviewResponse | null;
  previewPending: boolean;
  errorMessage: string | null;
}

export function MigrationPreviewCard({
  preview,
  previewPending,
  errorMessage,
}: MigrationPreviewCardProps) {
  return (
    <>
      <Card data-testid="migration-preview-dry-run-affordance">
        <CardContent className="pt-4 pb-4">
          <p className="text-sm text-muted-foreground">
            This dry run checks migration compatibility; nothing has been written.
          </p>
        </CardContent>
      </Card>
      {preview && <MigrationPreviewReportPanel preview={preview} />}
      {previewPending && !preview && (
        <Card>
          <CardContent className="pt-6">
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <Loader2 className="h-4 w-4 animate-spin" />
              Preview report is loading.
            </div>
          </CardContent>
        </Card>
      )}
      {errorMessage && <MigrationErrorCard errorMessage={errorMessage} />}
    </>
  );
}

function MigrationPreviewReportPanel({ preview }: { preview: MigrationPreviewResponse }) {
  const entries = orderMigrationPreviewEntries(preview.report.entries);

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-lg">Preview Report</CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="grid grid-cols-2 sm:grid-cols-4 gap-3">
          <PreviewSummaryStat
            label="Total entries"
            value={preview.report.summary.totalEntries}
            testId="migration-preview-summary-total-entries"
          />
          <PreviewSummaryStat
            label="Hard rejections"
            value={preview.report.summary.hardRejections}
            testId="migration-preview-summary-hard-rejections"
          />
          <PreviewSummaryStat
            label="Warnings"
            value={preview.report.summary.warnings}
            testId="migration-preview-summary-warnings"
          />
          <PreviewSummaryStat
            label="Scope gaps"
            value={preview.report.summary.scopeGaps}
            testId="migration-preview-summary-scope-gaps"
          />
        </div>
        <div className="text-xs text-muted-foreground">
          Source: {preview.sourceCounts.indexes.toLocaleString()} index
          {preview.sourceCounts.indexes !== 1 ? 'es' : ''},{' '}
          {preview.sourceCounts.records.toLocaleString()} record
          {preview.sourceCounts.records !== 1 ? 's' : ''}
        </div>
        {entries.length > 0 && (
          <div className="space-y-2 max-h-[390px] overflow-y-auto pr-1">
            {entries.map((entry, index) => (
              <PreviewEntry key={`${entry.code}-${entry.jsonPath}-${index}`} entry={entry} />
            ))}
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function PreviewSummaryStat({
  label,
  value,
  testId,
}: {
  label: string;
  value: number;
  testId: string;
}) {
  return (
    <div className="rounded-md border p-3 text-center">
      <div className="text-xl font-bold" data-testid={testId}>
        {value.toLocaleString()}
      </div>
      <div className="text-xs text-muted-foreground">{label}</div>
    </div>
  );
}

function PreviewEntry({ entry }: { entry: MigrationPreviewReportEntry }) {
  const isHardRejection = isHardMigrationPreviewEntry(entry);
  const metadata = formatMigrationPreviewEntryMeta(entry);

  return (
    <div
      className={`rounded-md border p-3 text-sm ${
        isHardRejection ? 'border-destructive/50 bg-destructive/5' : ''
      }`}
      data-testid="migration-preview-entry"
    >
      <div className="grid grid-cols-1 sm:grid-cols-4 gap-2">
        <PreviewEntryField
          label="Severity"
          value={entry.severity}
          testId="migration-preview-entry-severity"
        />
        <PreviewEntryField
          label="Code"
          value={entry.code}
          testId="migration-preview-entry-code"
        />
        <PreviewEntryField
          label="Resource"
          value={entry.resource}
          testId="migration-preview-entry-resource"
        />
        <PreviewEntryField
          label="JSON path"
          value={entry.jsonPath}
          testId="migration-preview-entry-json-path"
        />
      </div>
      {metadata && (
        <div className="mt-2 text-xs text-muted-foreground">{metadata}</div>
      )}
    </div>
  );
}

function PreviewEntryField({
  label,
  value,
  testId,
}: {
  label: string;
  value: string;
  testId: string;
}) {
  return (
    <div className="min-w-0">
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className="break-words" data-testid={testId}>{value}</div>
    </div>
  );
}

export function MigrationProgressCard({ status }: { status: AsyncMigrationStatusResponse }) {
  return (
    <Card data-testid="migration-progress-card">
      <CardContent className="pt-6">
        <div className="grid grid-cols-1 sm:grid-cols-3 gap-3 text-sm">
          <ProgressField label="Job ID" value={status.jobId} testId="migration-job-id" />
          <ProgressField label="Phase" value={status.phase} testId="migration-phase" />
          <ProgressField label="Disposition" value={status.disposition} testId="migration-disposition" />
        </div>
      </CardContent>
    </Card>
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
                Index {effectiveTarget} is ready.
              </p>
            </div>

            <div className="grid grid-cols-2 sm:grid-cols-4 gap-3">
              <ResultStat label="Documents" value={migrationData.counts.documents} />
              <ResultStat label="Settings" value={migrationData.counts.settings ? 'Applied' : 'None'} />
              <ResultStat label="Synonyms" value={migrationData.counts.synonyms} />
              <ResultStat label="Rules" value={migrationData.counts.rules} />
            </div>

            <div className="flex gap-2 pt-1">
              <Link to={`/index/${encodeURIComponent(effectiveTarget)}`}>
                <Button size="sm">Browse Index</Button>
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

export function MigrationInfoCard({ provider }: { provider: MigrationProviderDescriptor }) {
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
              Your {provider.displayName} API key is sent directly to the Flapjack server to fetch source data. It is not stored or logged.
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

function ProgressField({
  label,
  value,
  testId,
}: {
  label: string;
  value: string;
  testId: string;
}) {
  return (
    <div>
      <div className="font-medium">{label}</div>
      <div className="text-muted-foreground break-all" data-testid={testId}>
        {value}
      </div>
    </div>
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

function formatSourceCount(source: SourceIndexSummary): string | null {
  const count = source.entries ?? source.documentCount;
  if (typeof count !== 'number') {
    return null;
  }
  return `${count.toLocaleString()} record${count !== 1 ? 's' : ''}`;
}

function formatPaginationSummary(response: ListSourceIndexesResponse): string | null {
  const { total, offset, limit } = response;
  if (typeof total !== 'number') {
    return null;
  }
  if (typeof offset !== 'number' || typeof limit !== 'number') {
    return `${total.toLocaleString()} total`;
  }

  const first = Math.min(offset + 1, total);
  const last = Math.min(offset + limit, total);
  return `Showing ${first.toLocaleString()}–${last.toLocaleString()} of ${total.toLocaleString()}`;
}
