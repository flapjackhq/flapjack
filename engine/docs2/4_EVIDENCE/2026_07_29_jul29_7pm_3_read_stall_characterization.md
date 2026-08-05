# Stage 1 Read Stall Characterization

Date: 2026-07-29

## Purpose

Stage 1 was intended to determine whether the observed five-second
`GET /1/usage/documents_count/<index>` sample was a real read stall or a
shutdown-timing artifact by measuring read liveness while a real
write-backpressure pause and real commit work overlapped without shutdown.

## Red Condition

The manager-level stall condition used by the implemented characterization was:

- start each count sample during the no-shutdown interval after the delayed write
  is processing, while the write-backpressure pause is held, and before the
  delayed write reaches terminal task status;
- retain a sample that starts in that interval even if the task becomes terminal
  while the count call is blocked;
- count a stall if a successful count sample exceeds 1000 ms;
- record the overlap sample denominator, count distribution, measured overlap
  window, and whether the count-stall condition fired.

The original route-level red condition also required a successful `/health`
control sample below 250 ms. The in-crate test does not claim that control:
measuring elapsed time around no health operation would not be a valid substitute.

## Measurement

The in-crate characterization lives in
`engine/src/index/write_queue/backpressure_tests.rs`:
`read_count_stays_live_while_backpressure_pause_and_commit_overlap`.

The test creates a durable tenant, commits one seed document, admits a real
write before setting pause, holds the tenant-scoped test commit delay for 1500
ms, records real write-backpressure pause state through
`backpressure::record_observation_result_for_test`, verifies a new write is
rejected with write-backpressure `IndexPaused`, and samples
`IndexManager::tenant_doc_count` until the delayed write reaches terminal task
status. A count call that begins while the task is processing remains in the
distribution if the task becomes terminal before that call returns.

The original in-crate test could not build an HTTP router around its crate-local
manager. The route-level remediation therefore added the smallest shared seam
to the backpressure owner:
`IndexManager::hold_write_backpressure_pause_for_test_support`, available only
with the existing `test-support` feature. It drives the production
write-backpressure observation rule and clears the state through a drop guard;
it does not introduce a second pause mechanism.

## Result

Focused validation command:

```text
cd engine && timeout 900 cargo test -p flapjack --lib -- index::write_queue::tests::read_count_stays_live_while_backpressure_pause_and_commit_overlap --nocapture
```

Result:

```text
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2138 filtered out; finished in 1.95s
```

Measured distribution emitted by the test:

```text
overlap_samples=121
overlap_ms=1560
count_p99_ms=0
count_max_ms=0
count_stall_detected=false
```

The count samples included the last published count, `1`, during the overlap.
The assertion also permits `2` at the terminal boundary, because a separate
sampler can legitimately observe the final reader after the delayed write
publishes; any other value fails the test. After the delayed write reached
terminal status, the tenant count was `2`, proving the delayed write did publish.

## Route-Level Measurement

The same-process route characterization is
`routes_stay_live_while_backpressure_pause_and_commit_overlap` in
`engine/flapjack-http/tests/write_runtime_isolation.rs`.

The test builds the real Axum router and creates a durable tenant. It then
unloads that tenant and requires a successful
`GET /1/usage/documents_count/runtime_isolation` response with count `0`; because
the live gauge owner first calls `get_or_load` and a direct
`tenant_doc_count` would return `None` for an unloaded tenant, this precondition
proves the route exercised the manager-loading seam.

Next, the test dispatches a real batch with the existing two-second commit delay,
waits until the task is `Processing`, holds write backpressure through three
non-improving observations in the production registry, and only then starts the
sampler. The sampler alternates real router requests to `/health` and
`/1/usage/documents_count/runtime_isolation`. Each sample records whether the
batch was incomplete when that request started, and the characterization uses
only those verified overlap samples; a request that starts during the overlap
is retained even if the batch completes before the response returns. The test
requires at least 100 verified overlap samples. No shutdown occurs in the
measurement window. Recorded latency includes only each request's router round
trip; pre-request scheduler delay remains outside the read-path measurement
while the fixed sampling schedule preserves the overlap window.

Validation command:

```text
cd engine && timeout 900 cargo test -p flapjack-http --test write_runtime_isolation -- --nocapture
```

Measured route-level distribution:

```text
total_samples=212
overlap_samples=211
overlap_ms=2109
health_samples=106
health_p99_ms=0
health_max_ms=0
count_samples=105
count_p99_ms=0
count_max_ms=0
count_stall_detected=false
```

All 212 requests were successful, and 211 began during the verified overlap.
The batch completed durably and the final route-level document count was `1`.

## Verdict

The in-crate manager-path measurement refutes the pause-plus-commit version of
the stall hypothesis under no-shutdown conditions: the write-backpressure pause
was held, real commit work was active for a 1560 ms overlap window, and count
reads stayed live with a 0 ms max latency against the 1000 ms red threshold.

The route-level measurement closes the prior gap and refutes the observed-stall
hypothesis on the requested surface. During a 2109 ms overlap with real commit
work and write backpressure simultaneously active, `/health` and
`GET /1/usage/documents_count/<index>` both had 0 ms p99 and maximum latency.
The count maximum was 1000 ms below the greater-than-1000 ms red threshold,
while the health control remained 250 ms below its 250 ms threshold. The test
also proved the count route can reload the durable tenant through `get_or_load`.

## Stage 2 Disposition

Verdict classification: `refuted`.

No product runtime behavior was changed to alter count handling. The
prerequisite remediation added only a `test-support`-gated guard in the existing
backpressure owner and the route characterization test. Because the route-level
red condition did not fire, Stage 2 makes no usage handler, usage capture,
manager read-path, migration, or admission behavior change. A stall fix would
be unsupported by the measured evidence.
