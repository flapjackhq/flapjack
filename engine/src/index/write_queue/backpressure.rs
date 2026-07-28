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

    fn index_bytes(&self) -> Option<u64> {
        match self {
            Self::Determinate { index_bytes, .. } => Some(*index_bytes),
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
) -> crate::error::Result<()> {
    let key = tenant_key(base_path, tenant_id);
    let Some(state) = BACKPRESSURE_STATE.get(&key) else {
        return Ok(());
    };
    let Some(pause) = &state.pause else {
        return Ok(());
    };
    Err(FlapjackError::IndexPaused(format!(
        "{tenant_id} write backpressure: {}",
        pause.reason
    )))
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
        });
        return Some((
            BackpressureDecision::PauseIndeterminate,
            reason,
            "indeterminate",
            state.observations.iter().cloned().collect(),
        ));
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

    if state.pause.is_some() {
        return None;
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
    let Some(previous_bytes) = previous.index_bytes() else {
        return false;
    };
    let Some(current_bytes) = current.index_bytes() else {
        return false;
    };
    let Some(previous_orphans) = previous.orphan_file_set_count() else {
        return false;
    };
    let Some(current_orphans) = current.orphan_file_set_count() else {
        return false;
    };

    current_segments < previous_segments
        && current_bytes <= previous_bytes
        && current_orphans <= previous_orphans
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
    std::fs::write(path, payload)?;
    Ok(())
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

#[cfg(test)]
pub(crate) fn record_observation_result_for_test(
    base_path: &Path,
    tenant_id: &str,
    result: crate::error::Result<super::segment_observation::SegmentObservation>,
) -> crate::error::Result<()> {
    record_observation_result(base_path, tenant_id, result)
}

#[cfg(test)]
pub(crate) fn clear_for_test(base_path: &Path, tenant_id: &str) {
    remove_tenant_state(base_path, tenant_id);
    let _ = std::fs::remove_file(pause_artifact_path(base_path, tenant_id));
}
