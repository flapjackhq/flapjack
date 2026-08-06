use crate::error::FlapjackError;
use once_cell::sync::Lazy;
use serde::Serialize;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(crate) const WRITE_BACKPRESSURE_PAUSE_FILE_NAME: &str = "write_backpressure_pause.json";
const BACKPRESSURE_WINDOW_SIZE: usize = 3;
const BACKPRESSURE_SAMPLE_INTERVAL: Duration = Duration::from_secs(30);

static BACKPRESSURE_STATE: Lazy<dashmap::DashMap<String, TenantBackpressureState>> =
    Lazy::new(dashmap::DashMap::new);

#[cfg(test)]
type PauseArtifactPublicationHook = std::sync::Arc<dyn Fn(&Path) + Send + Sync>;
#[cfg(test)]
static PAUSE_ARTIFACT_PUBLICATION_HOOK: Lazy<
    std::sync::Mutex<std::collections::HashMap<PathBuf, (u64, PauseArtifactPublicationHook)>>,
> = Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
#[cfg(test)]
static PAUSE_ARTIFACT_PUBLICATION_HOOK_ID: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

#[derive(Clone)]
struct TenantBackpressureState {
    observations: VecDeque<ObservationSample>,
    pause: Option<PauseState>,
    last_sampled_at: Option<Instant>,
}

impl TenantBackpressureState {
    fn new() -> Self {
        Self {
            observations: VecDeque::with_capacity(BACKPRESSURE_WINDOW_SIZE),
            pause: None,
            last_sampled_at: None,
        }
    }
}

#[derive(Clone)]
struct PauseState {
    reason: String,
    peak_live_segment_count: Option<usize>,
    admission_rejected: bool,
    #[cfg(any(debug_assertions, test, feature = "test-support"))]
    clear_after_rejection: bool,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum BackpressureDecision {
    Admit,
    Pause,
    PauseIndeterminate,
}

impl BackpressureDecision {
    fn as_str(self) -> &'static str {
        match self {
            Self::Admit => "admit",
            Self::Pause => "pause",
            Self::PauseIndeterminate => "pause_indeterminate",
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum ObservationSample {
    Determinate {
        sampled_at_ms: u64,
        live_segment_count: usize,
        live_docs: u64,
        per_segment_doc_counts: std::collections::BTreeMap<String, u64>,
        managed_index_file_count: u64,
        index_bytes: u64,
        orphan_file_set_count: usize,
        orphan_file_set_ids: Vec<String>,
    },
    Indeterminate {
        sampled_at_ms: u64,
        error: String,
    },
}

impl ObservationSample {
    fn from_result(
        result: crate::error::Result<super::segment_observation::SegmentObservation>,
    ) -> Self {
        match result {
            Ok(observation) => Self::Determinate {
                sampled_at_ms: current_time_ms(),
                live_segment_count: observation.live_segment_count,
                live_docs: observation.live_docs,
                per_segment_doc_counts: observation.per_segment_doc_counts,
                managed_index_file_count: observation.managed_index_file_count,
                index_bytes: observation.index_bytes,
                orphan_file_set_count: observation.orphan_file_set_ids.len(),
                orphan_file_set_ids: observation.orphan_file_set_ids.into_iter().collect(),
            },
            Err(error) => Self::Indeterminate {
                sampled_at_ms: current_time_ms(),
                error: error.to_string(),
            },
        }
    }

    fn live_segment_count(&self) -> Option<usize> {
        match self {
            Self::Determinate {
                live_segment_count, ..
            } => Some(*live_segment_count),
            Self::Indeterminate { .. } => None,
        }
    }

    fn orphan_file_set_count(&self) -> Option<usize> {
        match self {
            Self::Determinate {
                orphan_file_set_count,
                ..
            } => Some(*orphan_file_set_count),
            Self::Indeterminate { .. } => None,
        }
    }
}

#[derive(Serialize)]
struct BackpressurePauseArtifact<'a> {
    tenant_id: &'a str,
    decision: &'static str,
    reason: &'a str,
    selected_segment_band: [usize; 2],
    selected_segment_ceiling: usize,
    window_size: usize,
    improvement_verdict: &'static str,
    observations: Vec<ObservationSample>,
}

pub(crate) fn pause_artifact_path(base_path: &Path, tenant_id: &str) -> PathBuf {
    base_path
        .join(tenant_id)
        .join(WRITE_BACKPRESSURE_PAUSE_FILE_NAME)
}

pub(crate) fn ensure_bulk_admission_allowed(
    base_path: &Path,
    tenant_id: &str,
    index: &crate::index::Index,
) -> crate::error::Result<()> {
    let key = tenant_key(base_path, tenant_id);
    let Some(mut state) = BACKPRESSURE_STATE.get_mut(&key) else {
        return Ok(());
    };
    let Some(pause) = &mut state.pause else {
        return Ok(());
    };
    let reason = pause.reason.clone();
    if !pause.admission_rejected {
        pause.admission_rejected = true;
        #[cfg(any(debug_assertions, test, feature = "test-support"))]
        if pause.clear_after_rejection {
            state.pause = None;
        }
        drop(state);
        return Err(backpressure_error(tenant_id, &reason));
    }
    #[cfg(any(debug_assertions, test, feature = "test-support"))]
    let clear_after_rejection = pause.clear_after_rejection;
    #[cfg(any(debug_assertions, test, feature = "test-support"))]
    let should_resample = !clear_after_rejection;
    #[cfg(not(any(debug_assertions, test, feature = "test-support")))]
    let should_resample = true;
    drop(state);
    if should_resample {
        record_observation_result(
            base_path,
            tenant_id,
            super::segment_observation::observe_segments(index),
        )?;
        if !tenant_is_paused(base_path, tenant_id) {
            return Ok(());
        }
    }
    #[cfg(any(debug_assertions, test, feature = "test-support"))]
    if clear_after_rejection {
        if let Some(mut state) = BACKPRESSURE_STATE.get_mut(&key) {
            if state
                .pause
                .as_ref()
                .is_some_and(|pause| pause.clear_after_rejection)
            {
                state.pause = None;
            }
        }
    }
    Err(backpressure_error(tenant_id, &reason))
}

fn backpressure_error(tenant_id: &str, reason: &str) -> FlapjackError {
    FlapjackError::IndexPaused(format!("{tenant_id} write backpressure: {reason}"))
}

fn tenant_is_paused(base_path: &Path, tenant_id: &str) -> bool {
    BACKPRESSURE_STATE
        .get(&tenant_key(base_path, tenant_id))
        .is_some_and(|state| state.pause.is_some())
}

pub(crate) fn remove_tenant_state(base_path: &Path, tenant_id: &str) {
    BACKPRESSURE_STATE.remove(&tenant_key(base_path, tenant_id));
}

pub(super) fn sample_after_worker_event(ctx: &super::WriteQueueContext) {
    if !mark_sample_due(&ctx.base_path, &ctx.tenant_id) {
        return;
    }
    let result = super::segment_observation::observe_segments(&ctx.index);
    if let Err(error) = record_observation_result(&ctx.base_path, &ctx.tenant_id, result) {
        tracing::error!(
            "[WQ {}] failed to persist write-backpressure observation: {}",
            ctx.tenant_id,
            error
        );
    }
}

fn mark_sample_due(base_path: &Path, tenant_id: &str) -> bool {
    let key = tenant_key(base_path, tenant_id);
    let now = Instant::now();
    let mut state = BACKPRESSURE_STATE
        .entry(key)
        .or_insert_with(TenantBackpressureState::new);
    if state
        .last_sampled_at
        .is_some_and(|sampled_at| now.duration_since(sampled_at) < BACKPRESSURE_SAMPLE_INTERVAL)
    {
        return false;
    }
    state.last_sampled_at = Some(now);
    true
}

fn record_observation_result(
    base_path: &Path,
    tenant_id: &str,
    result: crate::error::Result<super::segment_observation::SegmentObservation>,
) -> crate::error::Result<()> {
    let sample = ObservationSample::from_result(result);
    let decision = update_state(base_path, tenant_id, sample);
    if let Some((decision, reason, improvement_verdict, observations)) = decision {
        persist_decision_artifact(
            base_path,
            tenant_id,
            decision,
            &reason,
            improvement_verdict,
            observations,
        )?;
    }
    Ok(())
}

fn update_state(
    base_path: &Path,
    tenant_id: &str,
    sample: ObservationSample,
) -> Option<(
    BackpressureDecision,
    String,
    &'static str,
    Vec<ObservationSample>,
)> {
    let key = tenant_key(base_path, tenant_id);
    let mut state = BACKPRESSURE_STATE
        .entry(key)
        .or_insert_with(TenantBackpressureState::new);
    push_sample(&mut state.observations, sample);

    if matches!(
        state.observations.back(),
        Some(ObservationSample::Indeterminate { .. })
    ) {
        let reason = "segment observation is indeterminate".to_string();
        state.pause = Some(PauseState {
            reason: reason.clone(),
            peak_live_segment_count: None,
            admission_rejected: state
                .pause
                .as_ref()
                .is_some_and(|pause| pause.admission_rejected),
            #[cfg(any(debug_assertions, test, feature = "test-support"))]
            clear_after_rejection: false,
        });
        return Some((
            BackpressureDecision::PauseIndeterminate,
            reason,
            "indeterminate",
            state.observations.iter().cloned().collect(),
        ));
    }

    if state.pause.is_some() {
        if !state
            .pause
            .as_ref()
            .is_some_and(|pause| pause.admission_rejected)
        {
            return None;
        }
        if state
            .observations
            .back()
            .is_some_and(sample_is_at_or_below_selected_ceiling)
        {
            state.pause = None;
            return Some((
                BackpressureDecision::Admit,
                "segment count returned to or below the selected ceiling".to_string(),
                "at_or_below_ceiling",
                state.observations.iter().cloned().collect(),
            ));
        }
        if recover_paused_state_when_segment_growth_stops(&mut state) {
            return Some((
                BackpressureDecision::Admit,
                "segment count stopped growing at or below the paused-window peak".to_string(),
                "not_above_paused_peak",
                state.observations.iter().cloned().collect(),
            ));
        }
        return None;
    }

    if state
        .observations
        .back()
        .is_some_and(sample_is_at_or_below_selected_ceiling)
    {
        return Some((
            BackpressureDecision::Admit,
            "segment count returned to or below the selected ceiling".to_string(),
            "at_or_below_ceiling",
            state.observations.iter().cloned().collect(),
        ));
    }

    if state.observations.len() == BACKPRESSURE_WINDOW_SIZE
        && all_samples_above_selected_ceiling(&state.observations)
    {
        let improving = samples_are_strictly_improving(&state.observations);
        if !improving {
            let reason = "segment ceiling persisted without improvement across the bounded window"
                .to_string();
            state.pause = Some(PauseState {
                reason: reason.clone(),
                peak_live_segment_count: observed_segment_peak(&state.observations),
                admission_rejected: false,
                #[cfg(any(debug_assertions, test, feature = "test-support"))]
                clear_after_rejection: false,
            });
            return Some((
                BackpressureDecision::Pause,
                reason,
                "not_improving",
                state.observations.iter().cloned().collect(),
            ));
        }
    }

    None
}

fn recover_paused_state_when_segment_growth_stops(state: &mut TenantBackpressureState) -> bool {
    let mut determinate_segment_counts = state
        .observations
        .iter()
        .rev()
        .filter_map(ObservationSample::live_segment_count);
    let Some(current) = determinate_segment_counts.next() else {
        return false;
    };
    let Some(previous) = determinate_segment_counts.next() else {
        return false;
    };
    let Some(pause) = state.pause.as_mut() else {
        return false;
    };
    let growth_stopped = current <= previous
        && pause
            .peak_live_segment_count
            .is_some_and(|paused_peak| current <= paused_peak);
    if growth_stopped {
        state.pause = None;
    } else {
        pause.peak_live_segment_count = Some(
            pause
                .peak_live_segment_count
                .map_or(current, |paused_peak| paused_peak.max(current)),
        );
    }
    growth_stopped
}

fn observed_segment_peak(samples: &VecDeque<ObservationSample>) -> Option<usize> {
    samples
        .iter()
        .filter_map(ObservationSample::live_segment_count)
        .max()
}

fn push_sample(samples: &mut VecDeque<ObservationSample>, sample: ObservationSample) {
    if samples.len() == BACKPRESSURE_WINDOW_SIZE {
        samples.pop_front();
    }
    samples.push_back(sample);
}

fn sample_is_at_or_below_selected_ceiling(sample: &ObservationSample) -> bool {
    let Some(live_segment_count) = sample.live_segment_count() else {
        return false;
    };
    live_segment_count <= super::SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1
}

fn all_samples_above_selected_ceiling(samples: &VecDeque<ObservationSample>) -> bool {
    let ceiling = super::SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1;
    samples.iter().all(|sample| {
        sample
            .live_segment_count()
            .is_some_and(|count| count > ceiling)
    })
}

fn samples_are_strictly_improving(samples: &VecDeque<ObservationSample>) -> bool {
    samples
        .iter()
        .zip(samples.iter().skip(1))
        .all(|(previous, current)| sample_pair_is_improving(previous, current))
}

fn sample_pair_is_improving(previous: &ObservationSample, current: &ObservationSample) -> bool {
    let Some(previous_segments) = previous.live_segment_count() else {
        return false;
    };
    let Some(current_segments) = current.live_segment_count() else {
        return false;
    };
    let Some(previous_orphans) = previous.orphan_file_set_count() else {
        return false;
    };
    let Some(current_orphans) = current.orphan_file_set_count() else {
        return false;
    };

    current_segments < previous_segments && current_orphans <= previous_orphans
}

fn persist_decision_artifact(
    base_path: &Path,
    tenant_id: &str,
    decision: BackpressureDecision,
    reason: &str,
    improvement_verdict: &'static str,
    observations: Vec<ObservationSample>,
) -> crate::error::Result<()> {
    let path = pause_artifact_path(base_path, tenant_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let artifact = BackpressurePauseArtifact {
        tenant_id,
        decision: decision.as_str(),
        reason,
        selected_segment_band: [
            super::SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.0,
            super::SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1,
        ],
        selected_segment_ceiling: super::SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1,
        window_size: BACKPRESSURE_WINDOW_SIZE,
        improvement_verdict,
        observations,
    };
    let payload = serde_json::to_vec_pretty(&artifact)?;
    write_decision_artifact(&path, &payload)?;
    Ok(())
}

fn write_decision_artifact(path: &Path, payload: &[u8]) -> std::io::Result<()> {
    crate::index::utils::atomic_write_with_before_rename(path, payload, |_temp_path| {
        #[cfg(test)]
        run_pause_artifact_publication_hook(path, _temp_path);
    })
}

fn tenant_key(base_path: &Path, tenant_id: &str) -> String {
    base_path.join(tenant_id).to_string_lossy().into_owned()
}

fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn segment_observation_for_test(
    live_segment_count: usize,
    index_bytes: u64,
    orphan_file_set_count: usize,
) -> super::segment_observation::SegmentObservation {
    let live_segment_ids = (0..live_segment_count)
        .map(|index| format!("{index:032x}"))
        .collect::<std::collections::BTreeSet<_>>();
    let per_segment_doc_counts = live_segment_ids
        .iter()
        .map(|segment_id| (segment_id.clone(), 1))
        .collect::<std::collections::BTreeMap<_, _>>();
    let orphan_file_set_ids = (0..orphan_file_set_count)
        .map(|index| format!("{:032x}", index + 10_000))
        .collect::<std::collections::BTreeSet<_>>();

    super::segment_observation::SegmentObservation {
        live_segment_count,
        live_segment_ids,
        live_docs: live_segment_count as u64,
        per_segment_doc_counts,
        managed_index_file_count: live_segment_count as u64,
        index_bytes,
        orphan_file_set_ids,
    }
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) struct TestBackpressurePauseGuard {
    base_path: PathBuf,
    tenant_id: String,
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for TestBackpressurePauseGuard {
    fn drop(&mut self) {
        clear_for_test(&self.base_path, &self.tenant_id);
    }
}

/// Hold the production write-backpressure state using the same bounded,
/// non-improving observation rule as the live sampler.
///
/// Existing test state is cleared first so the synthetic observation window is
/// deterministic. Any setup error drops the guard and clears partial state.
///
/// HTTP integration tests cannot manufacture `SegmentObservation` because its
/// fields are intentionally crate-private. Keeping the fixture here makes the
/// backpressure owner the single source of truth for how a deterministic pause
/// is established and cleared.
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn hold_non_improving_pause_for_test(
    base_path: &Path,
    tenant_id: &str,
) -> crate::error::Result<TestBackpressurePauseGuard> {
    clear_for_test(base_path, tenant_id);
    let pause_guard = TestBackpressurePauseGuard {
        base_path: base_path.to_path_buf(),
        tenant_id: tenant_id.to_string(),
    };
    let live_segment_count = super::SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 2;

    for _ in 0..BACKPRESSURE_WINDOW_SIZE {
        record_observation_result(
            base_path,
            tenant_id,
            Ok(segment_observation_for_test(live_segment_count, 40_000, 0)),
        )?;
    }

    let key = tenant_key(base_path, tenant_id);
    let Some(mut state) = BACKPRESSURE_STATE.get_mut(&key) else {
        return Err(FlapjackError::Tantivy(
            "test-support observations did not establish write backpressure".to_string(),
        ));
    };
    let Some(pause) = &mut state.pause else {
        return Err(FlapjackError::Tantivy(
            "test-support observations did not establish write backpressure".to_string(),
        ));
    };
    #[cfg(any(debug_assertions, test, feature = "test-support"))]
    {
        pause.clear_after_rejection = false;
    }

    Ok(pause_guard)
}

#[cfg(any(debug_assertions, test, feature = "test-support"))]
pub fn force_backpressure_pause_for_test(
    base_path: &Path,
    tenant_id: &str,
) -> crate::error::Result<()> {
    record_observation_result(
        base_path,
        tenant_id,
        Err(FlapjackError::Io(
            "forced indeterminate segment observation".to_string(),
        )),
    )?;
    if let Some(mut state) = BACKPRESSURE_STATE.get_mut(&tenant_key(base_path, tenant_id)) {
        if let Some(pause) = &mut state.pause {
            pause.clear_after_rejection = true;
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn record_observation_result_for_test(
    base_path: &Path,
    tenant_id: &str,
    result: crate::error::Result<super::segment_observation::SegmentObservation>,
) -> crate::error::Result<()> {
    record_observation_result(base_path, tenant_id, result)
}

#[cfg(test)]
pub(crate) fn tenant_is_paused_for_test(base_path: &Path, tenant_id: &str) -> bool {
    tenant_is_paused(base_path, tenant_id)
}

#[cfg(test)]
pub(crate) struct PauseArtifactPublicationHookGuard {
    artifact_path: PathBuf,
    hook_id: u64,
}

#[cfg(test)]
impl Drop for PauseArtifactPublicationHookGuard {
    fn drop(&mut self) {
        let mut hooks = PAUSE_ARTIFACT_PUBLICATION_HOOK.lock().unwrap();
        if hooks
            .get(&self.artifact_path)
            .is_some_and(|(hook_id, _)| *hook_id == self.hook_id)
        {
            hooks.remove(&self.artifact_path);
        }
    }
}

#[cfg(test)]
pub(crate) fn set_pause_artifact_publication_hook_for_test(
    artifact_path: &Path,
    hook: PauseArtifactPublicationHook,
) -> PauseArtifactPublicationHookGuard {
    let hook_id =
        PAUSE_ARTIFACT_PUBLICATION_HOOK_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let artifact_path = artifact_path.to_path_buf();
    PAUSE_ARTIFACT_PUBLICATION_HOOK
        .lock()
        .unwrap()
        .insert(artifact_path.clone(), (hook_id, hook));
    PauseArtifactPublicationHookGuard {
        artifact_path,
        hook_id,
    }
}

#[cfg(test)]
fn run_pause_artifact_publication_hook(artifact_path: &Path, temp_path: &Path) {
    let hook = PAUSE_ARTIFACT_PUBLICATION_HOOK
        .lock()
        .unwrap()
        .get(artifact_path)
        .map(|(_, hook)| std::sync::Arc::clone(hook));
    if let Some(hook) = hook {
        hook(temp_path);
    }
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn clear_for_test(base_path: &Path, tenant_id: &str) {
    remove_tenant_state(base_path, tenant_id);
    let _ = std::fs::remove_file(pause_artifact_path(base_path, tenant_id));
}
