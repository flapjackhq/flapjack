# Decision 0012: MIG6 Resumable Interrupt State
<!-- markdownlint-disable MD013 -->

Date: 2026-07-29
Status: Accepted

## Context

The Stage 1 evidence receipt
`engine/docs2/4_EVIDENCE/2026_07_29_jul29_12pm_3_migration_resume_design_receipt.md`
proves that Flapjack already has one export resume owner. The owner is
`engine/flapjack-http/src/handlers/migration/export.rs::resume_algolia_source`,
which is generic over `MigrationSourceReader` (`export.rs:114-125`). It resolves
the existing checkpoint handle and source identity before writing
(`export.rs:170-184`). `SpoolExportSink::open` then hydrates the completed
document, rule, and synonym ID sets from durable sidecars
(`export.rs:336-363`), and `persist_page` omits IDs already in those sets
(`export.rs:365-390`). This ADR extends that path; it does not create a second
resume loop, cursor, or checkpoint.

The missing capability is a durable state that can reach that owner after a
real interruption. Two intentional fail-closed fences currently prevent it.

**Fence A, in-process errors.** `run_export_after_admission` sends every
`stream_and_accept` error through an unconditional best-effort
`store.fail_export(job_uuid)` (`export.rs:187-205`).
`spool_lifecycle.rs::fail_export` changes the manifest lifecycle from `Running`
to `Failed` (`spool_lifecycle.rs:140-149`). `spool_support.rs::ensure_writable`
permits only `Running`; `Accepted` and `Failed` return `JobTerminal`
(`spool_support.rs:300-309`). Every object-page transaction reaches that check
before its completion lookup or artifact writes
(`spool_transaction.rs::commit_object_page`, lines 86-105). This protects a
partial or unverifiable export from later appearing writable or complete.

**Fence B, restart.** Before serving requests,
`flapjack-http/src/server.rs::run_pre_serve_barrier_with_catchup` awaits
`job_runner.rs::recover_async_jobs_before_serve` (`server.rs:130-145`).
Recovery walks every durable async job (`job_runner.rs:344-364`), and
`recover_create_async_job` fails a still-running create-only job when
`publication_transaction_id` is absent (`job_runner.rs:418-429`).
`AsyncMigrationMetadata.publication_transaction_id` is `None` at admission
(`spool.rs:251-272`, `:839-857`) and becomes `Some` only when the publication
receipt is recorded (`spool.rs:1025-1038`). The whole export window is
therefore conservatively failed on restart. This protects startup from guessing
whether a destination publication was attempted or committed.

The persisted frontier already has the facts required for safe pre-publication
resume. `spool.rs::ExportCheckpoint` carries the job, lifecycle projection,
progress, and resource completions (`spool.rs:477-483`);
`spool_lifecycle.rs::checkpoint_view` constructs it directly from
`SpoolManifest` (`spool_lifecycle.rs:223-230`). The manifest owns the lifecycle,
artifacts, counters, denominators, completed-ID sidecar manifests, and resource
completion records (`spool.rs:436-459`). A second state file would create
conflicting owners across crash boundaries.

## Decision

### Durable state model

The durable state model adds `Interrupted` to the existing
`spool.rs::LifecycleState`. It means all of the following:

- the export is incomplete and has not been accepted;
- no publication transaction exists;
- spool recovery validated the manifest/artifact/sidecar frontier;
- no export worker currently owns permission to write; and
- the existing checkpoint handle may be presented to the existing
  `resume_algolia_source` owner with fresh source credentials.

`Interrupted` is resumable but not writable and not terminal. `Running` means
one export attempt owns the write permission. `Accepted` means the complete
export passed resource verification and final source-identity proof. `Failed`,
`Deleting`, and `Deleted` remain non-resumable and non-writable.

The manifest lifecycle is the canonical interruption fact. The existing
`MigrationPhaseRecord` stays `Exporting` with disposition `Running` and no
`terminal_at`; it is the async workflow projection, not another checkpoint
model. `ExportCheckpoint` and `checkpoint_view` remain the checkpoint contract,
and `checkpoint_view` will naturally project the manifest state as
`Interrupted`. No new checkpoint record or provider cursor is introduced.

The write-state transitions are:

```text
Running --successful complete verification--> Accepted
Running --allowlisted interruption----------> Interrupted
Running --ambiguous or terminal error--------> Failed
Interrupted --validated single resume claim-> Running
Interrupted --cancel/expiry/delete----------> Failed/Deleting
```

The later implementation must add one spool-owned `interrupt_export` operation
and one spool-owned resume-claim operation. Both lock the existing root and job
locks and commit the manifest atomically. The resume claim checks the checkpoint
handle, source-identity digest, phase `Exporting`, absent
`publication_transaction_id`, and lifecycle `Interrupted`, then changes it to
`Running`. Only that transition grants write permission. A simultaneous second
claim sees `Running` and fails without writing; a crash after the claim is
classified again by restart recovery. `ensure_writable` must continue to allow
only `Running`, so neither an idle interrupted job nor a terminal job can accept
pages.

### Fence A: allowlist interruption and fail closed otherwise

`run_export_after_admission` will replace its unconditional error branch with a
closed classification:

- A source error is interruption-eligible only for the provider's explicit
  transient allowlist. For the current Algolia provider that is exactly
  `Timeout`, `Transport`, `RateLimit`, and `Server`, matching the existing
  retry predicate in `algolia_client.rs:643-651`.
- Cancellation stays terminal/cancelled. Source drift, source-identity
  mismatch, validation, upstream rejection, decode, schema, redirect,
  progress, and limit failures stay terminal.
- Every `SpoolError`, including I/O, corruption, capacity, verification,
  checkpoint, and state-transition failures, stays terminal because the
  durable frontier is ambiguous or unusable.
- An error before a manifest and checkpoint are durably admitted cannot become
  resumable.

For an allowlisted error, the spool owner first runs its existing recovery and
integrity checks, then commits `Running -> Interrupted`. If recovery, identity
validation, or the interruption commit fails, it uses the existing
`fail_export` path and surfaces the fencing failure. There is no default-to-
resumable branch. Unknown future error kinds are terminal until explicitly
classified with contract tests.

This preserves Fence A's purpose: no ambiguous partial export is writable or
apparently accepted. The relaxation applies only when the existing transaction
owner can reduce disk state to a proven committed frontier.

### Fence B: restart only the proven pre-publication export case

`run_pre_serve_barrier_with_catchup` remains a blocking startup barrier, and
`recover_async_jobs_before_serve` remains the only async-job restart owner. It
must recover the spool before classifying jobs and must not launch a provider
reader or auto-resume before serving.

`recover_create_async_job` changes only for this conjunction:

1. the migration phase is `Exporting`, disposition is `Running`, and
   `terminal_at` is absent;
2. the export manifest lifecycle is `Running` or already `Interrupted`;
3. `publication_transaction_id` is `None`;
4. cancellation was not requested; and
5. manifest, visible artifacts, completed-ID sidecars, resource completions,
   source identity, and counters pass existing recovery/consistency checks.

For a recovered `Running` manifest, startup commits `Interrupted`; an already
`Interrupted` manifest is idempotently retained. The job then remains available
for an authenticated explicit resume request. Missing/corrupt evidence fails
startup or settles the job terminal according to the existing recovery
contract; it never produces a resume handle.

The current `None -> Some` publication transition remains the boundary. A job
outside `Exporting`, an `Accepted` export, any job with a publication
transaction ID, or any job with publication evidence still follows the current
publication repair/failure logic. In particular, this ADR does not reinterpret
`None` as globally safe: it is safe only together with the proven incomplete
export state above. Replacement, cancel, staging, activation, and committed-
publication recovery keep their existing conservative behavior.

This preserves Fence B's purpose: only a job proven never to have crossed into
publication may become resumable. Any publication ambiguity remains terminal or
startup-blocking rather than guessed.

### Exactly-once argument

The guarantee is exactly-once membership in the accepted spool, not exactly-once
provider fetching. Resume may fetch a provider page more than once. For each
stable resource ID, at most one committed payload contributes to the accepted
export.

**Fetched but not committed.** If a provider page was fetched but
`commit_object_page` did not durably commit its final manifest, its IDs are not
in the authoritative completed-ID sidecar manifest. Resume traverses the source
again and commits those missing IDs. A network fetch has no spool side effect.

**Payload/sidecar published but final checkpoint write lost.**
`commit_object_page` first registers a staged artifact in the manifest, then
appends the sidecar and publishes the payload, then marks the artifact visible,
advances the sidecar manifest, and commits the final manifest
(`spool_transaction.rs:113-160`). After a crash before that final commit,
`recover_artifacts` removes files still described as staged and recomputes
counters from visible artifacts, while `recover_resource_sidecar` truncates the
sidecar to the manifest-owned length (`spool_support.rs:230-274`). The page is
therefore absent from both authoritative membership and payload and may be
fetched and committed again.

**Final checkpoint write committed.** Once the final manifest is durable, its
sidecar generation, length, digest, and count identify the committed ID set
(`spool.rs:374-405`). `completed_resource_page_check` hydrates that set and
treats a fully represented page as a no-op
(`spool_transaction.rs:236-279`). `SpoolExportSink::persist_page` also filters
individual completed IDs when provider page boundaries or ordering change.
Thus a page may be refetched, but committed IDs are not appended again.

Settings retain their existing singleton atomic/idempotent transaction.
Documents, rules, and synonyms require unique stable IDs. Acceptance still
requires every resource's committed count and hash to match its denominator and
then requires the final quiescent source identity to equal the pre-resume
identity (`export.rs:266-303`; `spool_lifecycle.rs:125-137`).

Unstable pagination must never be represented by an offset checkpoint. The
Meilisearch contract fixture explicitly records
`meili_document_order_not_contractual`
(`engine/tests/meilisearch_source_contract_kat.sh:15`). Resume therefore starts
a complete provider traversal and subtracts the existing ID-set frontier.
Reordering, insertion into earlier pages, and page-boundary shifts are safe only
when the provider can prove the same complete source identity and reproduce
every missing stable ID. A provider that cannot supply a stable identity,
complete traversal, unique stable IDs, and final drift proof is not
resume-capable; its interruption fails closed. Equal counts alone are not a
source-identity proof.

### Provider neutrality

The orchestration contract is provider-neutral at the reader boundary.
`resume_algolia_source` already accepts any `MigrationSourceReader`, whose
contract supplies settings and page callbacks for documents, rules, and
synonyms (`source_reader.rs:30-56`). Despite its name,
`AsyncMigrationSourceProvider` currently has only one true remote source
provider, `Algolia`; `BulkReplace` is an internal payload mode
(`mod.rs:100-110`). The initial implementation may expose only Algolia resume,
but provider-specific retry and identity rules must be capabilities behind the
reader/provider factory, not branches in the spool checkpoint format.

A future provider becomes resume-capable only after known-answer contract tests
prove the four properties above. The existing source identity includes the app,
source name, provider update marker, document metadata count, and per-resource
count/hash (`source_reader.rs:365-391`), but each provider owns evidence that its
update marker and traversal make that digest meaningful. The spool remains
neutral and stores only the resulting identity digest and ID-set frontier.

### Status contract

The async status contract may advertise resume only when all durable predicates
for the resume claim are true. For an authenticated owner, an `Exporting` /
`Running` response whose manifest is `Interrupted` may add
`resumable: true`, `operation: "resume"`, and the opaque checkpoint handle. A
running worker, an unadmitted job, a non-export phase, or any terminal state
must omit those fields. The public handle is not a substitute for the
checkpoint handle, and source credentials are never persisted or returned.

This is a projection of the canonical manifest state, not a parallel status
rule. The existing guard in
`engine/flapjack-http/src/handlers/migration/import_contract_tests.rs:1351-1366`
correctly forbids `resumeHandle`, `checkpointHandle`, `resume`, `resumable`, and
`operation` on terminal `Succeeded` responses. It remains intact and must be
expanded to all terminal dispositions. Existing fields and meanings on
terminal `Succeeded` responses do not change.

The later API design must choose one canonical wire name for the opaque
checkpoint handle before implementation. Until then, the normative rule is
presence only for a validated `Interrupted` export and absence everywhere
else; this ADR does not authorize multiple aliases.

## Successor implementation acceptance contract

This section is the falsifiable proof contract for the successor MIG-6 fix
lane. The tests named below must first be authored against the current code and
observed red for the missing capability, then pass because the implementation
uses the state model and owners in this ADR. Hand-seeding an already writable
checkpoint is not an acceptance substitute.

### Exact known-answer interrupt and resume

Add
`async_migration_resume_after_committed_page_has_exact_target_id_set` to
`engine/flapjack-http/src/handlers/migration/import_contract_tests.rs`. Its
source oracle must be independently authored as this literal corpus of unique
object IDs:

```rust
let expected_ids = HashSet::from([
    "resume-object-001".to_string(),
    "resume-object-002".to_string(),
    "resume-object-003".to_string(),
    "resume-object-004".to_string(),
    "resume-object-005".to_string(),
    "resume-object-006".to_string(),
]);
```

The first provider traversal must commit the named
`documents_page_0_manifest_committed` boundary containing
`resume-object-001` and `resume-object-004`, and then return one of the
allowlisted transient errors before another document page commits. The test
must observe the same durable job in a genuine `Interrupted` state and issue
an explicit authenticated resume request for that job with fresh source
credentials. The resumed provider traversal must start from the beginning with
shifted page boundaries, for example
`[003, 001]`, `[006, 002]`, `[004, 005]`, so the assertion exercises the
ID-set frontier rather than an offset.

After import succeeds, collect the target index's object IDs into a `HashSet`
and assert `actual_ids == expected_ids`. The expected set must not be built
from the provider fixture, spool sidecar, target response, or any count. Also
assert the durable completed document IDs equal the same literal set. A count
of six is insufficient: one duplicate and one omission still produce a wrong
count of six while violating exactly-once membership.

The fixture must expose independent evidence that there were exactly two
provider traversal starts and that the second traversal served pages. The
test must also observe the atomic resume claim changing this job from
`Interrupted` to `Running`; a test that only receives a refusal or reuses the
first traversal cannot pass. A simultaneous losing claim must return HTTP 409
with structured error code `migration_resume_claim_conflict`, while a resume
attempt against `Accepted`, `Failed`, `Deleting`, or `Deleted` must return HTTP
409 with `migration_resume_not_available`. Assert the code, not only the HTTP
status or message, and assert that either refusal writes no artifact, sidecar,
counter, or lifecycle change.

### Full-process crash known answer

Add
`interrupted_async_migration_resumes_exactly_once_after_process_restart` to
`engine/flapjack-server/tests/crash_durability_test.rs`. Reuse, or minimally
extend only as required,
`RunningServer::spawn_no_auth_auto_port` and
`kill_and_restart_no_auth_auto_port{,_with_env}` in
`engine/flapjack-server/tests/support/mod.rs` (currently at lines 216, 328, and
335). The process test must use the same six-ID literal oracle above.

Before any wait that could hang, assert that the local provider fixture is
reachable and that the async submission returned an admitted job ID. Hold the
provider after the first document page, then poll with a fixed deadline until
status proves `phase=exporting`, `disposition=running`, and
`exportProgress={completed: 2, total: 6}` while the fixture proves the next
page has not completed. That is the required durable committed-page setup
boundary; kill the process only after those assertions.

Restart the same data directory and prove startup recovery completed before
issuing other requests. The restarted status for the same job must honestly
report the positive arm: `phase=exporting`, `disposition=running`,
`resumable=true`, `operation="resume"`, the one chosen checkpoint-handle
field, no `terminalAt`, and the preserved `2/6` progress. Submit a real resume
with fresh credentials, assert a second provider traversal and the
`Interrupted -> Running` claim occurred, wait with a fixed deadline for
terminal success, and assert the final target ID `HashSet` equals the literal
six-ID oracle. This test is not satisfied by restarting into `Failed`, by
creating a replacement job, by replaying cached provider output, or by
asserting only `nbHits`.

### Status projection matrix

Add
`async_migration_resume_status_is_positive_only_for_interrupted_export` to
`import_contract_tests.rs`. Construct a genuine manifest lifecycle
`Interrupted` with a matching `Exporting` / `Running` phase and no publication
transaction, and assert that this arm alone serializes `resumable: true`,
`operation: "resume"`, and the chosen opaque checkpoint-handle field.
Explicitly construct and assert omission for `Running`, `Accepted`, `Failed`,
`Deleting`, and `Deleted` manifests; unadmitted jobs; non-export phases; every
terminal disposition; and any job with publication evidence. Preserve and
extend the existing terminal omission loop at
`import_contract_tests.rs:1355-1366`; do not replace it with the new positive
test or weaken its five forbidden-key assertions.

### Owned file inventory

The successor owns only the existing seams below. It must re-read this
inventory after M1G-E and M2EM land and reconcile their final shapes before
editing; it must not open a parallel lifecycle, checkpoint, resume, or status
owner.

M2ET may still be in flight while this lane edits — see `### Sequencing and
exclusions`. That is safe because the only file both lanes claim is `mod.rs`,
and they claim **disjoint concerns inside it**: this lane owns the
authenticated route, status fields, refusal codes, and OpenAPI projection (row
below), while M2ET's own `## Owned files` section restricts it to "adapter
dispatch only — not the enum or route registration". Re-derive that boundary
against `origin/main` before editing rather than assuming it still holds.

| Owned file | Successor responsibility |
| --- | --- |
| `engine/flapjack-http/src/handlers/migration/spool.rs` | Add the `Interrupted` lifecycle/schema projection and extend `ExportCheckpoint`; keep the manifest as the single durable state owner. |
| `engine/flapjack-http/src/handlers/migration/spool_lifecycle.rs` | Own atomic `Running -> Interrupted` and single-claim `Interrupted -> Running` transitions, including handle, identity, phase, and publication-boundary validation. |
| `engine/flapjack-http/src/handlers/migration/spool_support.rs` | Run integrity recovery before interruption/restart classification and preserve the `ensure_writable` fence so only `Running` can write. |
| `engine/flapjack-http/src/handlers/migration/spool_transaction.rs` | Preserve exact page payload/sidecar/manifest transactions and completed-ID idempotence; add no cursor checkpoint. |
| `engine/flapjack-http/src/handlers/migration/export.rs` | Classify only allowlisted retryable provider errors as interruptions and reuse `resume_algolia_source` / `run_export_after_admission` for the real second traversal. |
| `engine/flapjack-http/src/handlers/migration/job_runner.rs` and `engine/flapjack-http/src/server.rs` | Keep `recover_async_jobs_before_serve` behind `run_pre_serve_barrier_with_catchup`; classify only integrity-proven pre-publication exports as `Interrupted` without auto-resuming. |
| `engine/flapjack-http/src/handlers/migration/mod.rs` | Own the authenticated route, status fields, stable refusal codes, and OpenAPI projection without persisting or returning credentials. |
| `engine/flapjack-http/src/handlers/migration/export_tests.rs`, `export_resume_tests.rs`, and `import_contract_tests.rs` | Prove error classification, lifecycle claims, transaction windows, exact ID membership, status truthfulness, and losing/terminal refusal codes. |
| `engine/flapjack-server/tests/crash_durability_test.rs` and `support/mod.rs` | Prove the full process-kill/restart/explicit-resume path against one persistent data directory with bounded setup and completion waits. |

### Sequencing and exclusions

The implementation lane must wait for M1G-E and M2EM. It must **not** wait for
M2ET. This Stage 3 documentation lane need not wait for any of them.

**Revised 2026-07-30.** The original rule named all three lanes because all
claim `engine/flapjack-http/src/handlers/migration/mod.rs`. Measured against
M2EM's real diff, shared file claim is not the binding constraint — competing
*refactors of the same functions* are. M2EM converts the Algolia-only submit
path into a provider-generic one (`submit_algolia_migration_impl` becomes
`submit_source_migration_impl<P, F, R>`, plus a `match source_provider`
dispatch): 246 insertions and 25 deletions, where the deletions are signature
and type rewrites rather than removed features. Two lanes performing that
conversion concurrently would each invent an incompatible provider-neutral
design, and git cannot merge two different refactors of one function — that is
a design choice, not a textual conflict, and resolving it discards one lane's
work.

That conversion is one-time. Once M2EM is on `origin/main`, M2ET adds a match
arm plus a reader and this lane adds checkpoint/resume state; both are
additive against a stable signature and merge normally.

Mechanical dispatch gate. This lane may start once M2EM has merged, even if
M2ET is still running. Use the **same probe pair M2ET's own dispatch gate
uses** — "has M2EM merged?" is one question and gets one definition, shared by
every lane that asks it. Both must exit 0:

```bash
git fetch origin --prune
git ls-tree -r origin/main --name-only \
  | grep -qE '^engine/docs2/4_EVIDENCE/.*m2em_meilisearch_adapter_receipt\.md$'
echo "M2EM_RECEIPT_EXIT=$?"
git grep -q 'Meilisearch' origin/main -- engine/flapjack-http/src/handlers/migration/source_reader.rs
echo "M2EM_ADAPTER_EXIT=$?"
```

Neither half is sufficient alone, which is why M2ET pairs them: the receipt is
a document that could in principle land without the code, and the adapter
string could in principle appear in a comment. Requiring both closes each
other's false-positive window. The pair fails closed — a missing receipt or a
missing adapter reads as red and holds the lane.

Do **not** replace this with a probe on a private helper name such as
`submit_source_migration_impl`. That symbol has no callers outside `mod.rs`,
so a rename during clean review would break the gate without breaking anything
else, and this lane would silently never dispatch.

The successor is bounded to ADR 0012's state model. It does not include
automatic credential persistence or automatic resume, offset cursors, a
second checkpoint store, resume after export acceptance or any publication
transaction, replacement/cancel redesign, or unrelated provider work. A later
provider may use this path only after its own stable-identity, complete-
traversal, unique-ID, and drift known-answer contracts pass.

## Consequences

- A process crash or explicitly transient provider failure can preserve
  downloaded work without weakening publication recovery.
- Resume reuses the bounded disk-backed ID sets and transaction recovery already
  tested by `export_resume_tests.rs`; no in-memory offset grows with provider
  page count.
- `ensure_writable` remains a simple single-state permission check. The new
  state adds explicit claim/recovery transitions but no second checkpoint
  owner.
- Resume spends provider reads to regain a complete traversal and identity
  proof. This is intentional: correctness is preferred over an unstable cursor.
- Jobs interrupted before publication remain non-terminal and continue to
  consume spool retention/byte budget until resumed, cancelled, expired, or
  deleted. Retention behavior for `Interrupted` must be specified and tested in
  the implementation stage.
- The implementation needs red tests for every state edge, both fences, the
  three transaction windows above, duplicate resume claims, restart
  idempotence, source drift, terminal status omission, and provider capability
  refusal.

## Non-goals

The following are not being built by this ADR stage:

- no new checkpoint owner, page-offset cursor, job-runner frontier, or provider-
  specific spool manifest;
- no Rust implementation, route, schema, or test change;
- no automatic credential persistence or automatic provider resume at startup;
- no relaxation after export acceptance or after any publication transaction;
- no replacement/cancel/publication recovery redesign;
- no promise that every `MigrationSourceReader` is resume-capable; and
- no `.debbie.toml` publication change.

Active ADRs after `0006` are ignored by `engine/.gitignore` and
`.debbie.toml` does not automatically publish new ADRs. If this ADR should
become public, a later documentation/publication owner must explicitly add it
to the curated publication scope outside this stage.

## Deferred implementation questions

- Choose the single wire field name for the checkpoint handle and record it in
  the route/OpenAPI contract tests.
- Choose retention/expiry behavior for an `Interrupted` manifest without
  changing the state ownership decided here.
- Decide whether the first implementation exposes an explicit resume endpoint
  or resubmission semantics; either must call the same atomic resume claim.

These are implementation/API-shape choices, not permission to change the
durable state model, fence conditions, exactly-once argument, provider
capability requirements, or status contract decided above.

Open design questions for this ADR: none.
<!-- markdownlint-enable MD013 -->
