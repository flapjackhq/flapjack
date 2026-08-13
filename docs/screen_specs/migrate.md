# Migrate

## Task

Import one source index or collection from Algolia, Meilisearch, or Typesense into a Flapjack index. The first-screen flow must support a dry-run preview before any write path is offered.

## Layout

1. Header: title `Migrate`, provider-aware source and target summary.
2. Provider selector: Algolia, Meilisearch, Typesense; defaults to Algolia.
3. Source credentials: provider-owned field (`Application ID`, `Endpoint`, or `Node URL`) plus shared `API Key` with visibility toggle.
4. Source discovery action: loads source indexes or collections from `/1/migrations/{provider}/list-indexes`.
5. Source picker: visible after discovery succeeds; shows source name plus provider count metadata when present.
6. Index names: shared `sourceIndex`, optional `targetIndex`, and shared `overwrite` switch. For Typesense only, this card also shows an unchecked write-freeze attestation beside the source and target controls.
7. Preview action: sends `/1/migrations/{provider}/preview` and is disabled until required provider credential, `apiKey`, and `sourceIndex` are present. Typesense additionally requires the write-freeze attestation to be checked.
8. Persistent dry-run affordance: visible beside the preview/report area and says this is a dry run and nothing has been written.
9. Preview report panel: shows the four summary counts (`totalEntries`, `hardRejections`, `warnings`, `scopeGaps`) and each entry's `severity`, `code`, `resource`, and `jsonPath`; page and item indexes may be shown when present.
10. Submit action: sends `/1/migrations/{provider}` only from a preview state that allows migration.
11. Result panel: async admission and polling progress, terminal completion counts, error, or private-address refusal guidance.
12. Migration notes: credential handling and long-running import expectations.

Backend/API facts stay with `MigrationPreviewResponse` in `engine/flapjack-http/src/handlers/migration/mod.rs`; served preview parity and the Typesense mixed-severity oracle stay with `engine/tests/source_migration_provider_parity_http_probe.sh`.

## State contract

### Loading
- The selected provider controls stay visible.
- The active discovery, preview, or submit button shows an inline spinner and disables edits that would change the pending request.

### Error
- Shows backend `message` and `code` when present.
- Does not render credential values outside their fields.
- Keeps the form populated so the user can correct one field and retry.

### No source credentials
- Provider selector, provider-owned credential field, `API Key`, and manual `Source index` input are visible.
- Discovery, preview, and submit are disabled until the required provider credential and `API Key` are non-empty.

### Source discovery succeeded
- Shows the shared response envelope `indexes`.
- Each option uses `name` as the submitted `sourceIndex`.
- Count display prefers `entries` for Algolia and `documentCount` for Meilisearch and Typesense; missing counts are omitted.
- Meilisearch pagination may show `total`, `offset`, and `limit`.

### Typesense write freeze not attested
- Shows an unchecked checkbox only when Typesense is selected.
- The checkbox says the user has paused writes to the selected Typesense collection for the complete migration.
- Preview and submit remain disabled until the user checks the attestation.
- Changing the provider, Typesense credentials, or source collection clears the attestation because it no longer describes the selected source.

### preview-running
- Primary control: disabled `Previewing...`.
- Locks provider, credentials, source, target, and overwrite controls until the preview resolves.
- Keeps the dry-run affordance visible.

### preview-clean
- Primary control: `Submit migration`.
- Shows zero hard rejections, zero warnings, and zero scope gaps.
- Submit is enabled because the dry run found no reported blockers.

### preview-has-warnings
- Primary control: `Submit migration`.
- Shows warning and/or scope-gap entries while keeping hard rejections at zero.
- Submit remains enabled because warnings and scope gaps are advisory preview findings.

### preview-has-hard-rejections
- Primary control: `Review blockers`.
- Submit is disabled because a hard rejection means the backend reported data that is not safe to override.
- The panel highlights hard-rejection entries first and preserves the full report list.

### preview-refused-private-address
- Primary control: `Edit source`.
- Shows the backend refusal message and provider-specific opt-in guidance: `FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK=1` or `FJ_ENABLE_TYPESENSE_PREVIEW_LOOPBACK=1`.
- Does not mention an opt-in for Algolia.

### Submitting
- Submit is disabled and reads as an in-progress migration for the selected provider.
- Provider selector, credentials, source, target, and overwrite controls are locked until the request resolves.
- The `202 Accepted` admission snapshot supplies `jobId`, `phase`, and `disposition`.
- While `disposition` is `running`, poll `GET /1/migrations/{provider}/{jobId}` and show latest `Job ID`, `Phase`, and `Disposition`.

### Submit succeeded
- Treat `disposition: succeeded` as terminal only when `terminalAt` is present.
- Shows `Migration complete` and `Index {target} is ready.` using the terminal status target, falling back to the effective form target.
- Shows terminal import outcome in `migrate-stat-documents`, `migrate-stat-settings`, `migrate-stat-synonyms`, and `migrate-stat-rules`.
- Offers `Browse Index` navigation to `/index/{targetIndex}` only after terminal success status arrives.

### Submit failed or cancelled
- Treat `disposition: failed` and `disposition: cancelled` as terminal errors.
- Stop polling and show the same `migration-error-card` used for submit failures.

## Navigation

- Route: `/migrate`
- Entry: main dashboard navigation item `Migrate`
- Back: browser/app back returns to the previous dashboard route without clearing server state.
- Browse target: navigates to `/index/{targetIndex}` after a submitted migration has a target name.

## Acceptance criteria

- Given no provider credentials, when the screen loads, then discovery, preview, and submit are disabled and no credential value is logged or displayed outside its field.
- Given discovery succeeds, when the provider returns counts, then the picker displays the provider-owned source count metadata.
- Given preview is running, then `Previewing...` is the disabled primary control and the dry-run affordance remains visible.
- Given a clean preview, then the report shows all four summary counts and `Submit migration` is primary.
- Given a warning-only preview, then every entry shows `severity`, `code`, `resource`, and `jsonPath`, and `Submit migration` is primary.
- Given a hard-rejection preview, then `Review blockers` is primary and submit is disabled with the hard-rejection rationale.
- Given a private-address preview refusal, then `Edit source` is primary and the provider-specific opt-in guidance is shown.
- Given Typesense is selected, then the write-freeze attestation is visible and unchecked, and preview and submit remain unavailable until it is checked.
- Given the Typesense write-freeze attestation copy is visible, then it says the user has paused writes to the selected collection for the complete migration and does not imply that Flapjack pauses, locks, or verifies the source.
- Given Algolia or Meilisearch is selected, then no write-freeze attestation is shown and their existing preview and submit enablement is unchanged.
- Given a submit admission, while status is running, then the result panel shows latest `jobId`, `phase`, and `disposition`.
- Given terminal success with `terminalAt`, then the screen shows `Migration complete`, effective target, and all four `migrate-stat-*` values.
- Given terminal failure or cancellation, then polling stops and `migration-error-card` is shown.

## Edge cases

- Discovery returns `indexes: []`: keep manual `sourceIndex` entry available and show an empty-source message.
- Preview returns `sourceCounts.records: 0`: keep submit disabled until the user chooses another source or accepts an explicit no-record path supplied by backend policy.
- Preview transport fails or times out: show retryable error without clearing credentials.
- Backend returns `Invalid migration request body`: show a generic request-shape error and keep fields editable.
- `targetIndex` blank: preview and submit omit `targetIndex`; display the effective target as the selected source name.
- `overwrite` off: omit `overwrite`; `overwrite` on: send `overwrite:true`.

## Current implementation gaps

- Stage 3 shipped the provider-neutral dry-run flow in the console: preview trigger, persistent dry-run affordance, summary/report rendering, preview refusal guidance, and submit gating behind a completed preview with zero hard rejections.
- Stage 3 browser evidence is `PASS` as of 2026-08-07: Algolia preview summary `14 / 0 / 9 / 5`, Meilisearch `11 / 0 / 6 / 5`, Typesense `12 / 0 / 7 / 5`, with submit and served-search proofs still covered for all three providers.
