# 2026-08-08 aug08_9pm_1 Sync Receipt

## Purpose

Record the completed `no_publish` arm for `aug08_9pm_1` as the public citation target for
`SYNC-1`. This receipt copies the Stage 1-3 evidence only; it does not re-run mirror probes,
reopen the gate, or authorize a prod publish.

## Disposition

The recorded `arm_taken` value is `no_publish`.

Prod was intentionally left unchanged because
`engine/docs2/4_EVIDENCE/aug08_9pm_1_sync/gate_verdict.json` records
`all_clauses_passed=false`. Clauses `b`, `c`, and `d` are false:

| Clause | Name | Verdict | Evidence |
| --- | --- | --- | --- |
| `a` | `nightly_sha_identity` | `true` | Nightly head SHA equals staging mirror SHA `ddce67893c165c2cf0b28497b3171c3b3614f90a`. |
| `b` | `nightly_success` | `false` | Nightly run `31288303499` completed with conclusion `failure`; failed job: `Rust all tests`. |
| `c` | `named_runtime_skip_provenance` | `false` | Required artifact `playwright-report-pages` was missing; available artifacts were `migration-import-contract-evidence`, `dashboard-all-tests-report`, and `flapjack-server`. |
| `d` | `push_ci_success` | `false` | CI run `31287808412` completed with conclusion `failure`; failed jobs: `Rust tests (all)` and `Rust tests (fast)`. |

No `debbie sync prod` ran, no prod mirror commit or push happened, no `release.yml`
dispatch ran, and no tag, GitHub release, or container publish happened. The prod manifest
`dev_sha` before and after the arm remained
`1b32cf727a89b00a1abff5c5d830fa40ec4a2c21`. `SYNC-1` exit clauses (1) and (2) — the
numbered `ROADMAP.md` exit, not the Stage 2 gate's `a`-`d` above — both remain unmet:
prod `dev_sha` is unchanged and prod still has `0/8` workflow exports in the recorded
probe.

Rollback anchor for any future publish arm: `35da0206f8d5cf567750da8d3c6fcb34859c5c69`.

## Pins

| Fact | Value | Source evidence |
| --- | --- | --- |
| Pinned source SHA | `83234fa9c92f2fda2e3df34b5c1e3360ef8e1723` | `engine/docs2/4_EVIDENCE/aug08_9pm_1_sync/pinned_source_sha.txt` |
| Staging mirror SHA | `ddce67893c165c2cf0b28497b3171c3b3614f90a` | `engine/docs2/4_EVIDENCE/aug08_9pm_1_sync/staging_mirror_sha.txt` |
| CI run ID | `31287808412` | `engine/docs2/4_EVIDENCE/aug08_9pm_1_sync/selected_run_ids.txt` |
| Nightly run ID | `31288303499` | `engine/docs2/4_EVIDENCE/aug08_9pm_1_sync/selected_run_ids.txt` |
| Stage 3 arm | `no_publish` | `engine/docs2/4_EVIDENCE/aug08_9pm_1_sync/arm_taken.txt` |

## Recorded Manifest And Lag Evidence

All commands below are the original evidence-producing probes from Stage 1-3. Values are copied
from the named files, not re-measured in this maintenance stage.

| Evidence file | Producing command or probe | Recorded value |
| --- | --- | --- |
| `prod_manifest_before.json` | `gh api '/repos/flapjackhq/flapjack/contents/.debbie/sync_manifest.json?ref=main' --jq .content \| base64 -d` | `dev_sha=1b32cf727a89b00a1abff5c5d830fa40ec4a2c21`, `synced_at=2026-08-06T11:13:07Z`. |
| `prod_manifest_after.json` | `gh api '/repos/flapjackhq/flapjack/contents/.debbie/sync_manifest.json?ref=main' --jq .content \| base64 -d` | `dev_sha=1b32cf727a89b00a1abff5c5d830fa40ec4a2c21`, `synced_at=2026-08-06T11:13:07Z`; unchanged from before. |
| `staging_manifest_after.json` | `gh api '/repos/gridl-staging/flapjack/contents/.debbie/sync_manifest.json?ref=main' --jq .content \| base64 -d` | `dev_sha=83234fa9c92f2fda2e3df34b5c1e3360ef8e1723`, `synced_at=2026-08-09T01:11:26Z`. |
| `lag_before.txt` | `bash engine/_dev/s/mirror_lag_probe.sh` | Baseline `origin_main_sha=83234fa9c92f2fda2e3df34b5c1e3360ef8e1723`; staging lag `107`, exports `8/8`; prod lag `521`, exports `0/8`; `LAG_BEFORE_EXIT=1`. |
| `lag_after_staging.txt` | `bash engine/_dev/s/mirror_lag_probe.sh` | Baseline `origin_main_sha=83234fa9c92f2fda2e3df34b5c1e3360ef8e1723`; staging lag `0`, exports `8/8`; prod lag `521`, exports `0/8`; `LAG_EXIT=1`. |
| `lag_before_arm.txt` | `bash engine/_dev/s/mirror_lag_probe.sh` | Baseline `origin_main_sha=e8460c02d72c5b99eb97dce87a8444a281925409`; staging lag `1`, exports `8/8`; prod lag `522`, exports `0/8`; `LAG_BEFORE_EXIT=1`. |
| `lag_after_arm.txt` | `bash engine/_dev/s/mirror_lag_probe.sh` | Baseline `origin_main_sha=e8460c02d72c5b99eb97dce87a8444a281925409`; staging lag `1`, exports `8/8`; prod lag `522`, exports `0/8`; `LAG_AFTER_EXIT=1`. |

The `lag_before_arm.txt` and `lag_after_arm.txt` prod lines match on `dev_sha`,
lag, exports, checked workflow denominator, and missing export slots, which is the recorded
proof that the `no_publish` arm did not mutate prod.

## NIGHT-1 Hand-Forward

This no-publish arm produced zero scheduled prod greens. A publish would only make future
scheduled prod nightlies meaningful; it would not itself satisfy `NIGHT-1`. Future `NIGHT-1`
work waits for a successful prod mirror publish plus two consecutive scheduled prod
`nightly.yml` successes. Before attributing any future prod nightly failure, materialize the
workflow file from that run head SHA:

```bash
gh api '/repos/flapjackhq/flapjack/contents/.github/workflows/nightly.yml?ref=<run headSha>' --jq .content | base64 -d
```

## Source Evidence

Every path below is dev-repository-only provenance and is deliberately outside the public
sync surface; only this receipt is synced. The values those files carry are copied into the
tables above, so this receipt stands alone on the mirror — the list is a pointer for readers
who have the dev repo, not a link target.

- `engine/docs2/4_EVIDENCE/aug08_9pm_1_sync/arm_taken.txt`
- `engine/docs2/4_EVIDENCE/aug08_9pm_1_sync/gate_verdict.json`
- `engine/docs2/4_EVIDENCE/aug08_9pm_1_sync/decision_menu.md`
- `engine/docs2/4_EVIDENCE/aug08_9pm_1_sync/stage_02_findings.md`
- `engine/docs2/4_EVIDENCE/aug08_9pm_1_sync/prod_manifest_before.json`
- `engine/docs2/4_EVIDENCE/aug08_9pm_1_sync/prod_manifest_after.json`
- `engine/docs2/4_EVIDENCE/aug08_9pm_1_sync/staging_manifest_after.json`
- `engine/docs2/4_EVIDENCE/aug08_9pm_1_sync/lag_before.txt`
- `engine/docs2/4_EVIDENCE/aug08_9pm_1_sync/lag_after_staging.txt`
- `engine/docs2/4_EVIDENCE/aug08_9pm_1_sync/lag_before_arm.txt`
- `engine/docs2/4_EVIDENCE/aug08_9pm_1_sync/lag_after_arm.txt`
- `engine/docs2/4_EVIDENCE/aug08_9pm_1_sync/pinned_source_sha.txt`
- `engine/docs2/4_EVIDENCE/aug08_9pm_1_sync/staging_mirror_sha.txt`
- `engine/docs2/4_EVIDENCE/aug08_9pm_1_sync/selected_run_ids.txt`
- `engine/docs2/4_EVIDENCE/aug08_9pm_1_sync/prod_rollback_anchor.txt`
