import { useState } from 'react';
import { useMutation, useQueryClient, type QueryClient } from '@tanstack/react-query';
import api from '@/lib/api';
import {
  MigrationCredentialsCard,
  MigrationErrorCard,
  MigrationHeader,
  MigrationIndexNamesCard,
  MigrationInfoCard,
  MigrationPreviewCard,
  MigrationProgressCard,
  MigrationSubmitButton,
  MigrationSuccessCard,
  SourceIndexPickerCard,
} from './MigrateSections';
import {
  MIGRATION_PROVIDER_DESCRIPTORS,
  type AsyncMigrationStatusResponse,
  type ListSourceIndexesResponse,
  type MigrationPreviewResponse,
  type MigrationProviderDescriptor,
  type MigrationProviderId,
  type MigrationResult,
  buildAsyncMigrationViewState,
  buildDiscoveryRequestBody,
  buildMigrationRequestBody,
  getIndexListErrorMessage,
  getMigrationErrorMessage,
  getTerminalMigrationErrorMessage,
  postSensitiveMigrationRequest,
  resolveEffectiveTargetIndex,
} from './migrateHelpers';

const POLL_INTERVAL_MS = 750;

interface MigrationFormValues {
  selectedProviderId: MigrationProviderId;
  firstCredentialValue: string;
  apiKey: string;
  sourceIndex: string;
  targetIndex: string;
  overwrite: boolean;
  sourceWriteFrozen: boolean;
  showKey: boolean;
}

const INITIAL_FORM_VALUES: MigrationFormValues = {
  selectedProviderId: 'algolia',
  firstCredentialValue: '',
  apiKey: '',
  sourceIndex: '',
  targetIndex: '',
  overwrite: false,
  sourceWriteFrozen: false,
  showKey: false,
};

export function Migrate() {
  const controller = useMigrationController();
  const { form, discovery, preview, migration, actions } = controller;

  return (
    <div className="space-y-6 max-w-2xl">
      <MigrationHeader provider={form.provider} effectiveTarget={form.effectiveTarget} />
      <MigrationCredentialsCard
        providers={MIGRATION_PROVIDER_DESCRIPTORS}
        provider={form.provider}
        values={{
          firstCredentialValue: form.values.firstCredentialValue,
          apiKey: form.values.apiKey,
          showKey: form.values.showKey,
        }}
        status={{
          controlsLocked: controller.controlsLocked,
          hasCredentials: controller.hasCredentials,
          canFetchSources: controller.canFetchSources,
          fetchSourcesPending: discovery.mutation.isPending,
        }}
        actions={actions.credentials}
      />
      <SourceIndexPickerCard
        provider={form.provider}
        state={{
          discoveryResponse: discovery.response,
          sourceIndex: form.values.sourceIndex,
          controlsLocked: controller.controlsLocked,
          indexListError: discovery.error,
        }}
        onSelectSourceIndex={actions.selectSourceIndex}
      />
      <MigrationIndexNamesCard
        provider={form.provider}
        values={{
          sourcesLoaded: Boolean(discovery.response),
          sourceIndex: form.values.sourceIndex,
          targetIndex: form.values.targetIndex,
          trimmedSourceIndex: form.trimmedSourceIndex,
          overwrite: form.values.overwrite,
          sourceWriteFrozen: form.values.sourceWriteFrozen,
        }}
        controlsLocked={controller.controlsLocked}
        actions={actions.indexNames}
      />
      <MigrationSubmitButton
        provider={form.provider}
        state={{
          canSubmit: controller.canSubmit,
          canPreview: controller.canPreview,
          hasPreview: Boolean(preview.mutation.data),
          previewFailed: preview.mutation.isError,
          previewHasBlockers: controller.previewHasBlockers,
          previewPending: preview.mutation.isPending,
          migrationPending: migration.mutation.isPending,
          effectiveTarget: form.effectiveTarget,
        }}
        onEditSource={() => document.getElementById('source-index')?.focus()}
        onPreview={() => actions.preview()}
        onSubmit={() => migration.mutation.mutate()}
      />
      <MigrationPreviewCard
        preview={preview.mutation.data ?? null}
        previewPending={preview.mutation.isPending}
        errorMessage={
          preview.mutation.isError
            ? getMigrationErrorMessage(
              preview.mutation.error,
              form.provider,
              [form.trimmedFirstCredential, form.trimmedApiKey],
            )
            : null
        }
      />
      {migration.currentStatus && <MigrationProgressCard status={migration.currentStatus} />}
      {migration.mutation.isSuccess && migration.mutation.data && (
        <MigrationSuccessCard
          migrationData={migration.mutation.data}
          effectiveTarget={migration.mutation.data.status.targetIndex ?? form.effectiveTarget}
        />
      )}
      {migration.mutation.isError && (
        <MigrationErrorCard
          errorMessage={getMigrationErrorMessage(
            migration.mutation.error,
            form.provider,
            [form.trimmedFirstCredential, form.trimmedApiKey],
          )}
        />
      )}
      <MigrationInfoCard provider={form.provider} />
    </div>
  );
}

function useMigrationController() {
  const queryClient = useQueryClient();
  const form = useMigrationForm();
  const discovery = useSourceDiscovery(form);
  const preview = useMigrationPreview(form);
  const migration = useMigrationSubmission(form, queryClient);

  const resetMigrationOutput = () => {
    preview.reset();
    migration.reset();
  };
  const resetAllOutput = () => {
    discovery.reset();
    resetMigrationOutput();
  };
  const changeIndexValue = (changes: Partial<MigrationFormValues>) => {
    form.update(changes);
    resetMigrationOutput();
  };
  const hasCredentials = Boolean(form.trimmedFirstCredential && form.trimmedApiKey);
  const previewHardRejections = preview.mutation.data?.report.summary.hardRejections;
  const previewHasBlockers = (previewHardRejections ?? 0) > 0;
  const requestPending = preview.mutation.isPending || migration.mutation.isPending;
  const controlsLocked = discovery.mutation.isPending || requestPending;
  const hasRequiredWriteFreeze = form.provider.id !== 'typesense'
    || form.values.sourceWriteFrozen;

  return {
    form,
    discovery,
    preview,
    migration,
    hasCredentials,
    controlsLocked,
    canFetchSources: hasCredentials && !discovery.mutation.isPending,
    canPreview: Boolean(hasCredentials && form.trimmedSourceIndex && hasRequiredWriteFreeze)
      && !requestPending,
    previewHasBlockers,
    canSubmit: Boolean(
      preview.mutation.data && previewHardRejections === 0 && hasRequiredWriteFreeze,
    ) && !requestPending,
    actions: {
      credentials: {
        selectProvider: (selectedProviderId: MigrationProviderId) => {
          form.update({
            selectedProviderId,
            firstCredentialValue: '',
            apiKey: '',
            sourceIndex: '',
            sourceWriteFrozen: false,
          });
          resetAllOutput();
        },
        changeFirstCredential: (firstCredentialValue: string) => {
          form.update({ firstCredentialValue, sourceWriteFrozen: false });
          resetAllOutput();
        },
        changeApiKey: (apiKey: string) => {
          form.update({ apiKey, sourceWriteFrozen: false });
          resetAllOutput();
        },
        toggleApiKeyVisibility: () => form.update({ showKey: !form.values.showKey }),
        fetchSources: () => discovery.mutation.mutate(),
      },
      selectSourceIndex: (sourceIndex: string) => changeIndexValue({
        sourceIndex,
        sourceWriteFrozen: false,
      }),
      indexNames: {
        changeSourceIndex: (sourceIndex: string) => changeIndexValue({
          sourceIndex,
          sourceWriteFrozen: false,
        }),
        changeTargetIndex: (targetIndex: string) => changeIndexValue({ targetIndex }),
        changeOverwrite: (overwrite: boolean) => changeIndexValue({ overwrite }),
        changeSourceWriteFrozen: (sourceWriteFrozen: boolean) => changeIndexValue({
          sourceWriteFrozen,
        }),
      },
      preview: () => {
        migration.reset();
        preview.mutation.mutate();
      },
    },
  };
}

function useMigrationForm() {
  const [values, setValues] = useState<MigrationFormValues>(INITIAL_FORM_VALUES);
  const provider = MIGRATION_PROVIDER_DESCRIPTORS.find(
    (candidate) => candidate.id === values.selectedProviderId,
  ) ?? MIGRATION_PROVIDER_DESCRIPTORS[0];
  const trimmedFirstCredential = values.firstCredentialValue.trim();
  const trimmedApiKey = values.apiKey.trim();
  const trimmedSourceIndex = values.sourceIndex.trim();
  const trimmedTargetIndex = values.targetIndex.trim();

  return {
    values,
    provider,
    trimmedFirstCredential,
    trimmedApiKey,
    trimmedSourceIndex,
    trimmedTargetIndex,
    effectiveTarget: resolveEffectiveTargetIndex(trimmedSourceIndex, trimmedTargetIndex),
    update: (changes: Partial<MigrationFormValues>) => {
      setValues((current) => ({ ...current, ...changes }));
    },
  };
}

function useSourceDiscovery(form: ReturnType<typeof useMigrationForm>) {
  const [response, setResponse] = useState<ListSourceIndexesResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const mutation = useMutation({
    mutationFn: async () => {
      const response = await postSensitiveMigrationRequest<ListSourceIndexesResponse>(
        `/1/migrations/${form.provider.routeSegment}/list-indexes`,
        buildDiscoveryRequestBody(form.provider, form.trimmedFirstCredential, form.trimmedApiKey),
      );
      return response;
    },
    onSuccess: (nextResponse) => {
      setResponse(nextResponse);
      setError(null);
      if (nextResponse.indexes.length === 1) {
        form.update({ sourceIndex: nextResponse.indexes[0].name, sourceWriteFrozen: false });
      }
    },
    onError: (requestError) => {
      setResponse(null);
      setError(getIndexListErrorMessage(
        requestError,
        form.provider,
        [form.trimmedFirstCredential, form.trimmedApiKey],
      ));
    },
  });

  return {
    response,
    error,
    mutation,
    reset: () => {
      setResponse(null);
      setError(null);
      mutation.reset();
    },
  };
}

function useMigrationSubmission(
  form: ReturnType<typeof useMigrationForm>,
  queryClient: QueryClient,
) {
  const [currentStatus, setCurrentStatus] = useState<AsyncMigrationStatusResponse | null>(null);
  const mutation = useMutation({
    mutationFn: async (): Promise<MigrationResult> => {
      const admission = await postSensitiveMigrationRequest<AsyncMigrationStatusResponse>(
        `/1/migrations/${form.provider.routeSegment}`,
        buildCurrentMigrationRequestBody(form),
      );
      return pollMigrationToTerminal(form.provider, admission, setCurrentStatus);
    },
    onMutate: () => setCurrentStatus(null),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['indexes'] }),
  });

  return {
    currentStatus,
    mutation,
    reset: () => {
      setCurrentStatus(null);
      mutation.reset();
    },
  };
}

function useMigrationPreview(form: ReturnType<typeof useMigrationForm>) {
  const mutation = useMutation({
    mutationFn: async (): Promise<MigrationPreviewResponse> => postSensitiveMigrationRequest(
      `/1/migrations/${form.provider.routeSegment}/preview`,
      buildCurrentMigrationRequestBody(form),
    ),
  });

  return {
    mutation,
    reset: () => mutation.reset(),
  };
}

function buildCurrentMigrationRequestBody(
  form: ReturnType<typeof useMigrationForm>,
): Record<string, unknown> {
  // Preview shares the submit payload; only the route suffix and response handling differ.
  return buildMigrationRequestBody({
    provider: form.provider,
    firstCredentialValue: form.trimmedFirstCredential,
    apiKey: form.trimmedApiKey,
    sourceIndex: form.trimmedSourceIndex,
    targetIndex: form.trimmedTargetIndex,
    overwrite: form.values.overwrite,
    sourceWriteFrozen: form.values.sourceWriteFrozen,
  });
}

async function pollMigrationToTerminal(
  provider: MigrationProviderDescriptor,
  admission: AsyncMigrationStatusResponse,
  setCurrentStatus: (status: AsyncMigrationStatusResponse) => void,
): Promise<MigrationResult> {
  let status = admission;
  setCurrentStatus(status);

  for (;;) {
    const viewState = buildAsyncMigrationViewState(status);
    if (viewState.kind === 'success') {
      return { status: viewState.status, counts: viewState.counts };
    }
    if (viewState.kind === 'error') {
      throw new Error(getTerminalMigrationErrorMessage(viewState.status, provider));
    }
    await delay(POLL_INTERVAL_MS);
    const response = await api.get<AsyncMigrationStatusResponse>(
      `/1/migrations/${provider.routeSegment}/${status.jobId}`,
    );
    status = response.data;
    setCurrentStatus(status);
  }
}

function delay(milliseconds: number): Promise<void> {
  return new Promise((resolve) => {
    window.setTimeout(resolve, milliseconds);
  });
}
