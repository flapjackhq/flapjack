use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::PeerConfig;

const AUTOHEAL_JOURNAL_FILE: &str = "autoheal_decisions.jsonl";
const DEFAULT_AUTOHEAL_JOURNAL_MAX_BYTES: u64 = 1024 * 1024;
pub const DEFAULT_AUTOHEAL_SUSTAINED_FAILURE_THRESHOLD: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerObservation {
    pub node_id: String,
    pub state: PeerObservationState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PeerObservationState {
    Healthy,
    Failed { consecutive_failures: u32 },
    Indeterminate { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvictionDecision {
    Evict { node_id: String, reason: String },
    Hold { observations_remaining: u32 },
    RefuseDisabled,
    RefuseWouldBreakQuorum { current: usize, required: usize },
    RefuseIndeterminate { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProbeOutcome {
    Healthy,
    Unreachable,
    Indeterminate { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutohealJournalEvent {
    pub decision_id: String,
    pub timestamp_millis: u64,
    pub membership_peer_ids: Vec<String>,
    pub candidate_peer_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_peer_config: Option<PeerConfig>,
    pub decision: EvictionDecision,
    pub action: AutohealActionRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutohealActionRecord {
    pub phase: String,
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedReadmissionCandidate {
    pub eviction_decision_id: String,
    pub peer_config: PeerConfig,
    pub eviction_outcome: String,
}

pub struct AutohealJournal {
    path: PathBuf,
    max_bytes: u64,
    next_sequence: u64,
}

impl AutohealJournal {
    pub fn new(data_dir: &Path) -> Result<Self, String> {
        Self::with_max_bytes(data_dir, DEFAULT_AUTOHEAL_JOURNAL_MAX_BYTES)
    }

    pub fn with_max_bytes(data_dir: &Path, max_bytes: u64) -> Result<Self, String> {
        std::fs::create_dir_all(data_dir)
            .map_err(|error| format!("failed to create {}: {error}", data_dir.display()))?;
        let path = Self::path_in_data_dir(data_dir);
        let created = !path.exists();
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|error| format!("failed to open {}: {error}", path.display()))?
            .sync_data()
            .map_err(|error| format!("failed to sync {}: {error}", path.display()))?;
        if created {
            sync_directory(data_dir)?;
        }

        let mut journal = Self {
            path,
            max_bytes,
            next_sequence: 1,
        };
        let events = journal.read_events_repairing_final_fragment()?;
        journal.next_sequence = next_decision_sequence(&events);
        journal.close_dangling_intents(&events)?;
        journal.compact_if_needed()?;
        Ok(journal)
    }

    pub fn path_in_data_dir(data_dir: &Path) -> PathBuf {
        data_dir.join(AUTOHEAL_JOURNAL_FILE)
    }

    pub fn fresh_observation_window<I, S>(
        &mut self,
        member_peer_ids: I,
    ) -> AutohealObservationWindow
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let member_peer_ids = member_peer_ids
            .into_iter()
            .map(Into::into)
            .collect::<Vec<_>>();
        AutohealObservationWindow::new(member_peer_ids.len(), member_peer_ids)
    }

    pub fn record_decision(
        &mut self,
        membership_peer_ids: &[String],
        candidate_peer_id: &str,
        decision: EvictionDecision,
    ) -> Result<String, String> {
        let decision_id = self.allocate_decision_id();
        let event = self.event(
            decision_id.clone(),
            membership_peer_ids,
            candidate_peer_id,
            None,
            decision,
            action("decision_recorded", "not_required", None),
        );
        self.append_event(&event)?;
        Ok(decision_id)
    }

    pub fn record_eviction<F>(
        &mut self,
        membership_peer_ids: &[String],
        candidate_peer_id: &str,
        candidate_peer_config: Option<PeerConfig>,
        decision: EvictionDecision,
        action_fn: F,
    ) -> Result<String, String>
    where
        F: FnOnce() -> Result<(), String>,
    {
        let decision_id = self.record_eviction_intent(
            membership_peer_ids,
            candidate_peer_id,
            candidate_peer_config.clone(),
            decision.clone(),
        )?;
        match action_fn() {
            Ok(()) => {
                let event = self.event(
                    decision_id.clone(),
                    membership_peer_ids,
                    candidate_peer_id,
                    candidate_peer_config,
                    decision,
                    action("eviction_outcome", "success", None),
                );
                self.append_event(&event)?;
                Ok(decision_id)
            }
            Err(error) => {
                let event = self.event(
                    decision_id,
                    membership_peer_ids,
                    candidate_peer_id,
                    candidate_peer_config,
                    decision,
                    action("eviction_outcome", "failure", Some(error.clone())),
                );
                self.append_event(&event)?;
                Err(error)
            }
        }
    }

    pub fn record_eviction_intent(
        &mut self,
        membership_peer_ids: &[String],
        candidate_peer_id: &str,
        candidate_peer_config: Option<PeerConfig>,
        decision: EvictionDecision,
    ) -> Result<String, String> {
        let decision_id = self.allocate_decision_id();
        let event = self.event(
            decision_id.clone(),
            membership_peer_ids,
            candidate_peer_id,
            candidate_peer_config,
            decision,
            action("eviction_intent", "pending", None),
        );
        self.append_event(&event)?;
        Ok(decision_id)
    }

    pub fn record_readmission<T, E, F>(
        &mut self,
        membership_peer_ids: &[String],
        peer_config: &PeerConfig,
        eviction_decision_id: String,
        action_fn: F,
    ) -> Result<T, E>
    where
        E: From<String> + ToString,
        F: FnOnce() -> Result<T, E>,
    {
        let decision = EvictionDecision::Evict {
            node_id: peer_config.node_id.clone(),
            reason: "readmission retry for prior auto-heal eviction".to_string(),
        };
        let intent = self.event(
            eviction_decision_id.clone(),
            membership_peer_ids,
            &peer_config.node_id,
            Some(peer_config.clone()),
            decision.clone(),
            action("readmission_intent", "pending", None),
        );
        self.append_event(&intent).map_err(E::from)?;

        match action_fn() {
            Ok(value) => {
                let outcome = self.event(
                    eviction_decision_id,
                    membership_peer_ids,
                    &peer_config.node_id,
                    Some(peer_config.clone()),
                    decision,
                    action("readmission_outcome", "success", None),
                );
                self.append_event(&outcome).map_err(E::from)?;
                Ok(value)
            }
            Err(error) => {
                let outcome = self.event(
                    eviction_decision_id,
                    membership_peer_ids,
                    &peer_config.node_id,
                    Some(peer_config.clone()),
                    decision,
                    action("readmission_outcome", "failure", Some(error.to_string())),
                );
                self.append_event(&outcome).map_err(E::from)?;
                Err(error)
            }
        }
    }

    pub fn events(&self) -> Result<Vec<AutohealJournalEvent>, String> {
        self.read_events_repairing_final_fragment()
    }

    pub fn unresolved_readmission_candidates(
        &self,
    ) -> Result<Vec<UnresolvedReadmissionCandidate>, String> {
        let events = self.read_events_repairing_final_fragment()?;
        Ok(transactions_by_decision(&events)
            .into_iter()
            .filter_map(|transaction| unresolved_readmission_candidate(&transaction))
            .collect())
    }

    fn allocate_decision_id(&mut self) -> String {
        let decision_id = format!("autoheal-{sequence:016}", sequence = self.next_sequence);
        self.next_sequence = self.next_sequence.saturating_add(1);
        decision_id
    }

    fn event(
        &self,
        decision_id: String,
        membership_peer_ids: &[String],
        candidate_peer_id: &str,
        candidate_peer_config: Option<PeerConfig>,
        decision: EvictionDecision,
        action: AutohealActionRecord,
    ) -> AutohealJournalEvent {
        AutohealJournalEvent {
            decision_id,
            timestamp_millis: current_timestamp_millis(),
            membership_peer_ids: membership_peer_ids.to_vec(),
            candidate_peer_id: candidate_peer_id.to_string(),
            candidate_peer_config,
            decision,
            action,
        }
    }

    fn append_event(&mut self, event: &AutohealJournalEvent) -> Result<(), String> {
        self.ensure_append_target_openable()?;
        self.ensure_event_can_be_compacted(event)?;
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path)
            .map_err(|error| format!("failed to open {}: {error}", self.path.display()))?;
        serde_json::to_writer(&mut file, event)
            .map_err(|error| format!("failed to serialize auto-heal event: {error}"))?;
        file.write_all(b"\n")
            .map_err(|error| format!("failed to finish {}: {error}", self.path.display()))?;
        file.sync_data()
            .map_err(|error| format!("failed to sync {}: {error}", self.path.display()))?;
        self.compact_if_needed()
    }

    fn read_events_repairing_final_fragment(&self) -> Result<Vec<AutohealJournalEvent>, String> {
        let file = File::open(&self.path)
            .map_err(|error| format!("failed to open {}: {error}", self.path.display()))?;
        let mut offset = 0_u64;
        let mut events = Vec::new();
        for line in BufReader::new(file).split(b'\n') {
            let line =
                line.map_err(|error| format!("failed to read {}: {error}", self.path.display()))?;
            if line.is_empty() {
                offset += 1;
                continue;
            }
            match serde_json::from_slice::<AutohealJournalEvent>(&line) {
                Ok(event) => {
                    offset += line.len() as u64 + 1;
                    events.push(event);
                }
                Err(error) => {
                    let metadata_len = std::fs::metadata(&self.path)
                        .map_err(|metadata_error| {
                            format!("failed to stat {}: {metadata_error}", self.path.display())
                        })?
                        .len();
                    if offset + line.len() as u64 >= metadata_len {
                        truncate_file(&self.path, offset)?;
                        return Ok(events);
                    }
                    return Err(format!(
                        "malformed auto-heal journal line before final fragment: {error}"
                    ));
                }
            }
        }
        Ok(events)
    }

    fn close_dangling_intents(&mut self, events: &[AutohealJournalEvent]) -> Result<(), String> {
        let transactions = transactions_by_decision(events);
        for transaction in transactions {
            let Some(last) = transaction.events.last() else {
                continue;
            };
            if last.action.phase == "eviction_intent" {
                let recovery = self.event(
                    last.decision_id.clone(),
                    &last.membership_peer_ids,
                    &last.candidate_peer_id,
                    last.candidate_peer_config.clone(),
                    last.decision.clone(),
                    action("eviction_recovery", "outcome_unknown", None),
                );
                self.append_event_without_compaction(&recovery)?;
            }
        }
        Ok(())
    }

    fn append_event_without_compaction(
        &mut self,
        event: &AutohealJournalEvent,
    ) -> Result<(), String> {
        self.ensure_append_target_openable()?;
        self.ensure_event_can_be_compacted(event)?;
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path)
            .map_err(|error| format!("failed to open {}: {error}", self.path.display()))?;
        serde_json::to_writer(&mut file, event)
            .map_err(|error| format!("failed to serialize auto-heal event: {error}"))?;
        file.write_all(b"\n")
            .map_err(|error| format!("failed to finish {}: {error}", self.path.display()))?;
        file.sync_data()
            .map_err(|error| format!("failed to sync {}: {error}", self.path.display()))
    }

    fn ensure_append_target_openable(&self) -> Result<(), String> {
        OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path)
            .map(|_| ())
            .map_err(|error| format!("failed to open {}: {error}", self.path.display()))
    }

    fn ensure_event_can_be_compacted(&self, event: &AutohealJournalEvent) -> Result<(), String> {
        let mut events = self.read_events_repairing_final_fragment()?;
        events.push(event.clone());
        compacted_events(events, self.max_bytes).map(|_| ())
    }

    fn compact_if_needed(&mut self) -> Result<(), String> {
        if std::fs::metadata(&self.path)
            .map_err(|error| format!("failed to stat {}: {error}", self.path.display()))?
            .len()
            <= self.max_bytes
        {
            return Ok(());
        }
        let events = self.read_events_repairing_final_fragment()?;
        let retained = compacted_events(events, self.max_bytes)?;
        self.replace_events(retained)
    }

    fn replace_events(&self, events: Vec<AutohealJournalEvent>) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", self.path.display()))?;
        let temp_path = parent.join(format!(
            ".autoheal_decisions.{}.{}.tmp",
            std::process::id(),
            current_timestamp_millis()
        ));
        let result = write_events_to_file(&temp_path, &events).and_then(|()| {
            std::fs::rename(&temp_path, &self.path).map_err(|error| {
                format!(
                    "failed to replace {} with {}: {error}",
                    self.path.display(),
                    temp_path.display()
                )
            })
        });
        if result.is_err() {
            let _ = std::fs::remove_file(&temp_path);
        }
        result?;
        sync_directory(parent)
    }
}

#[derive(Clone)]
struct JournalTransaction {
    decision_id: String,
    events: Vec<AutohealJournalEvent>,
}

pub struct AutohealObservationWindow {
    peer_count_at_observation_start: usize,
    member_peer_ids: Vec<String>,
    observations_by_peer: BTreeMap<String, PeerObservationState>,
}

impl AutohealObservationWindow {
    pub fn new<I, S>(peer_count_at_observation_start: usize, member_peer_ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let member_peer_ids = member_peer_ids
            .into_iter()
            .map(Into::into)
            .collect::<Vec<_>>();
        let observations_by_peer = member_peer_ids
            .iter()
            .map(|peer_id| (peer_id.clone(), PeerObservationState::Healthy))
            .collect();
        Self {
            peer_count_at_observation_start,
            member_peer_ids,
            observations_by_peer,
        }
    }

    pub fn record_success(&mut self, peer_id: &str) {
        self.observations_by_peer
            .insert(peer_id.to_string(), PeerObservationState::Healthy);
    }

    pub fn record_failure(&mut self, peer_id: &str) {
        let consecutive_failures = match self.observations_by_peer.get(peer_id) {
            Some(PeerObservationState::Failed {
                consecutive_failures,
            }) => consecutive_failures.saturating_add(1),
            _ => 1,
        };
        self.observations_by_peer.insert(
            peer_id.to_string(),
            PeerObservationState::Failed {
                consecutive_failures,
            },
        );
    }

    pub fn record_indeterminate(&mut self, peer_id: &str, reason: impl Into<String>) {
        self.observations_by_peer.insert(
            peer_id.to_string(),
            PeerObservationState::Indeterminate {
                reason: reason.into(),
            },
        );
    }

    pub fn decide(
        &self,
        autoheal_enabled: bool,
        sustained_failure_threshold: u32,
        candidate_failed_peer_id: &str,
    ) -> EvictionDecision {
        if !autoheal_enabled {
            return EvictionDecision::RefuseDisabled;
        }
        if let Some(reason) = self.validation_reason() {
            return EvictionDecision::RefuseIndeterminate { reason };
        }
        decide(
            autoheal_enabled,
            self.peer_count_at_observation_start,
            sustained_failure_threshold,
            &self.observations(),
            candidate_failed_peer_id,
        )
    }

    pub fn observations(&self) -> Vec<PeerObservation> {
        self.observations_by_peer
            .iter()
            .map(|(node_id, state)| PeerObservation {
                node_id: node_id.clone(),
                state: state.clone(),
            })
            .collect()
    }

    fn validation_reason(&self) -> Option<String> {
        if self.peer_count_at_observation_start != self.member_peer_ids.len() {
            return Some(format!(
                "peer_count_at_observation_start {} does not match membership snapshot size {}",
                self.peer_count_at_observation_start,
                self.member_peer_ids.len()
            ));
        }

        let mut member_ids = BTreeSet::new();
        for peer_id in &self.member_peer_ids {
            if !member_ids.insert(peer_id.as_str()) {
                return Some(format!(
                    "observation window membership contains duplicate peer {peer_id}"
                ));
            }
        }

        for peer_id in self.observations_by_peer.keys() {
            if !member_ids.contains(peer_id.as_str()) {
                return Some(format!(
                    "peer {peer_id} was observed outside the auto-heal membership snapshot"
                ));
            }
        }
        None
    }
}

pub struct AutohealCycle {
    autoheal_enabled: bool,
    sustained_failure_threshold: u32,
    window: AutohealObservationWindow,
    successfully_evicted_peer_ids: BTreeSet<String>,
}

impl AutohealCycle {
    pub fn new<I, S>(
        autoheal_enabled: bool,
        sustained_failure_threshold: u32,
        member_peer_ids: I,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let member_peer_ids = member_peer_ids
            .into_iter()
            .map(Into::into)
            .collect::<Vec<_>>();
        Self {
            autoheal_enabled,
            sustained_failure_threshold,
            window: AutohealObservationWindow::new(member_peer_ids.len(), member_peer_ids),
            successfully_evicted_peer_ids: BTreeSet::new(),
        }
    }

    pub fn replace_membership<I, S>(&mut self, member_peer_ids: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let member_peer_ids = member_peer_ids
            .into_iter()
            .map(Into::into)
            .collect::<Vec<_>>();
        self.window = AutohealObservationWindow::new(member_peer_ids.len(), member_peer_ids);
        self.successfully_evicted_peer_ids.clear();
    }

    pub fn member_peer_ids(&self) -> &[String] {
        &self.window.member_peer_ids
    }

    pub fn record_eviction_succeeded(&mut self, peer_id: &str) {
        self.successfully_evicted_peer_ids
            .insert(peer_id.to_string());
    }

    pub fn observation_counts(&self) -> BTreeMap<String, u32> {
        self.window
            .observations()
            .into_iter()
            .map(|observation| {
                let count = match observation.state {
                    PeerObservationState::Failed {
                        consecutive_failures,
                    } => consecutive_failures,
                    PeerObservationState::Healthy | PeerObservationState::Indeterminate { .. } => 0,
                };
                (observation.node_id, count)
            })
            .collect()
    }

    pub fn record_probe_pass(
        &mut self,
        outcomes_by_peer: &BTreeMap<String, ProbeOutcome>,
    ) -> Vec<(String, EvictionDecision)> {
        let member_peer_ids = self.window.member_peer_ids.clone();
        for peer_id in &member_peer_ids {
            let outcome = outcomes_by_peer.get(peer_id).cloned().unwrap_or_else(|| {
                ProbeOutcome::Indeterminate {
                    reason: "peer missing from completed health-probe pass".to_string(),
                }
            });
            self.record_probe_observation(peer_id, outcome);
        }

        member_peer_ids
            .into_iter()
            .filter(|peer_id| {
                outcomes_by_peer
                    .get(peer_id)
                    .is_some_and(|outcome| !matches!(outcome, ProbeOutcome::Healthy))
            })
            .map(|peer_id| {
                let decision = self.decide_for_peer(&peer_id);
                (peer_id, decision)
            })
            .collect()
    }

    pub fn record_probe_result(
        &mut self,
        peer_id: &str,
        outcome: ProbeOutcome,
    ) -> EvictionDecision {
        self.record_probe_observation(peer_id, outcome);
        self.decide_for_peer(peer_id)
    }

    fn record_probe_observation(&mut self, peer_id: &str, outcome: ProbeOutcome) {
        match outcome {
            ProbeOutcome::Healthy => self.window.record_success(peer_id),
            ProbeOutcome::Unreachable => self.window.record_failure(peer_id),
            ProbeOutcome::Indeterminate { reason } => {
                self.window.record_indeterminate(peer_id, reason)
            }
        }
    }

    fn decide_for_peer(&mut self, peer_id: &str) -> EvictionDecision {
        if self.successfully_evicted_peer_ids.contains(peer_id) {
            return EvictionDecision::Hold {
                observations_remaining: 0,
            };
        }

        self.window.decide(
            self.autoheal_enabled,
            self.sustained_failure_threshold,
            peer_id,
        )
    }
}

pub fn decide(
    autoheal_enabled: bool,
    peer_count_at_observation_start: usize,
    sustained_failure_threshold: u32,
    observations: &[PeerObservation],
    candidate_failed_peer_id: &str,
) -> EvictionDecision {
    if !autoheal_enabled {
        return EvictionDecision::RefuseDisabled;
    }
    if sustained_failure_threshold == 0 {
        return EvictionDecision::RefuseIndeterminate {
            reason: "sustained failure threshold must be greater than zero".to_string(),
        };
    }
    if let Some(reason) = validate_observation_set(peer_count_at_observation_start, observations) {
        return EvictionDecision::RefuseIndeterminate { reason };
    }

    let Some(candidate) = observations
        .iter()
        .find(|observation| observation.node_id == candidate_failed_peer_id)
    else {
        return EvictionDecision::RefuseIndeterminate {
            reason: format!("candidate peer {candidate_failed_peer_id} has no observation"),
        };
    };

    let candidate_failures = match &candidate.state {
        PeerObservationState::Healthy => {
            return EvictionDecision::Hold {
                observations_remaining: sustained_failure_threshold,
            };
        }
        PeerObservationState::Failed {
            consecutive_failures,
        } => *consecutive_failures,
        PeerObservationState::Indeterminate { reason } => {
            return EvictionDecision::RefuseIndeterminate {
                reason: format!(
                    "candidate peer {candidate_failed_peer_id} observation indeterminate: {reason}"
                ),
            };
        }
    };

    if candidate_failures < sustained_failure_threshold {
        return EvictionDecision::Hold {
            observations_remaining: sustained_failure_threshold - candidate_failures,
        };
    }

    if let Some(reason) = failed_majority_reason(peer_count_at_observation_start, observations) {
        return EvictionDecision::RefuseIndeterminate { reason };
    }

    for observation in observations {
        if let PeerObservationState::Indeterminate { reason } = &observation.state {
            return EvictionDecision::RefuseIndeterminate {
                reason: format!(
                    "peer {} observation indeterminate: {reason}",
                    observation.node_id
                ),
            };
        }
    }

    let total_members = peer_count_at_observation_start + 1;
    let required = quorum_required(total_members);
    let unavailable_peer_count = observations
        .iter()
        .filter(|observation| matches!(observation.state, PeerObservationState::Failed { .. }))
        .count();
    let current_after_eviction = total_members.saturating_sub(unavailable_peer_count);
    if current_after_eviction < required {
        return EvictionDecision::RefuseWouldBreakQuorum {
            current: current_after_eviction,
            required,
        };
    }

    EvictionDecision::Evict {
        node_id: candidate_failed_peer_id.to_string(),
        reason: "sustained failure threshold reached and quorum remains".to_string(),
    }
}

fn validate_observation_set(
    peer_count_at_observation_start: usize,
    observations: &[PeerObservation],
) -> Option<String> {
    let mut seen = BTreeSet::new();
    for observation in observations {
        if !seen.insert(observation.node_id.as_str()) {
            return Some(format!(
                "peer {} has duplicate observations in the auto-heal decision input",
                observation.node_id
            ));
        }
    }
    if observations.len() != peer_count_at_observation_start {
        return Some(format!(
            "peer_count_at_observation_start {peer_count_at_observation_start} does not match membership snapshot size {}",
            observations.len()
        ));
    }
    None
}

fn failed_majority_reason(
    peer_count_at_observation_start: usize,
    observations: &[PeerObservation],
) -> Option<String> {
    let failed_peer_count = observations
        .iter()
        .filter(|observation| matches!(observation.state, PeerObservationState::Failed { .. }))
        .count();
    if failed_peer_count >= 2 && failed_peer_count * 2 > peer_count_at_observation_start {
        return Some(format!(
            "{failed_peer_count} failed peers constitute a majority of configured peers; local node may be isolated"
        ));
    }
    None
}

fn quorum_required(total_members: usize) -> usize {
    (total_members / 2) + 1
}

fn action(phase: &str, outcome: &str, error: Option<String>) -> AutohealActionRecord {
    AutohealActionRecord {
        phase: phase.to_string(),
        outcome: outcome.to_string(),
        error,
    }
}

fn current_timestamp_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn sync_directory(path: &Path) -> Result<(), String> {
    File::open(path)
        .map_err(|error| format!("failed to open directory {}: {error}", path.display()))?
        .sync_all()
        .map_err(|error| format!("failed to sync directory {}: {error}", path.display()))
}

fn truncate_file(path: &Path, len: u64) -> Result<(), String> {
    OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|error| format!("failed to open {} for repair: {error}", path.display()))?
        .set_len(len)
        .map_err(|error| format!("failed to truncate {}: {error}", path.display()))
}

fn next_decision_sequence(events: &[AutohealJournalEvent]) -> u64 {
    events
        .iter()
        .filter_map(|event| {
            event
                .decision_id
                .strip_prefix("autoheal-")
                .and_then(|suffix| suffix.parse::<u64>().ok())
        })
        .max()
        .unwrap_or(0)
        .saturating_add(1)
}

fn write_events_to_file(path: &Path, events: &[AutohealJournalEvent]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("failed to create {}: {error}", path.display()))?;
    for event in events {
        serde_json::to_writer(&mut file, event)
            .map_err(|error| format!("failed to serialize auto-heal event: {error}"))?;
        file.write_all(b"\n")
            .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
    }
    file.sync_data()
        .map_err(|error| format!("failed to sync {}: {error}", path.display()))
}

fn transactions_by_decision(events: &[AutohealJournalEvent]) -> Vec<JournalTransaction> {
    let mut transactions = Vec::<JournalTransaction>::new();
    for event in events {
        if let Some(transaction) = transactions
            .iter_mut()
            .find(|transaction| transaction.decision_id == event.decision_id)
        {
            transaction.events.push(event.clone());
            continue;
        }
        transactions.push(JournalTransaction {
            decision_id: event.decision_id.clone(),
            events: vec![event.clone()],
        });
    }
    transactions
}

fn compacted_events(
    events: Vec<AutohealJournalEvent>,
    max_bytes: u64,
) -> Result<Vec<AutohealJournalEvent>, String> {
    let transactions = transactions_by_decision(&events);
    let mut selected_indexes = BTreeSet::new();
    let mut retained_bytes = 0_u64;

    for (index, transaction) in transactions.iter().enumerate() {
        if unresolved_readmission_candidate(transaction).is_some() {
            let transaction_len = serialized_transaction_len(transaction)?;
            if transaction_len > max_bytes {
                return Err(oversized_transaction_error(
                    &transaction.decision_id,
                    transaction_len,
                    max_bytes,
                ));
            }
            selected_indexes.insert(index);
            retained_bytes = retained_bytes.saturating_add(transaction_len);
        }
    }

    for index in (0..transactions.len()).rev() {
        if selected_indexes.contains(&index) {
            continue;
        }
        let transaction_len = serialized_transaction_len(&transactions[index])?;
        if transaction_len > max_bytes {
            return Err(oversized_transaction_error(
                &transactions[index].decision_id,
                transaction_len,
                max_bytes,
            ));
        }
        if retained_bytes + transaction_len <= max_bytes {
            selected_indexes.insert(index);
            retained_bytes = retained_bytes.saturating_add(transaction_len);
        }
    }

    Ok(selected_indexes
        .into_iter()
        .flat_map(|index| transactions[index].events.clone())
        .collect())
}

fn unresolved_readmission_candidate(
    transaction: &JournalTransaction,
) -> Option<UnresolvedReadmissionCandidate> {
    let candidate_event = transaction
        .events
        .iter()
        .find(|event| matches!(event.decision, EvictionDecision::Evict { .. }))?;
    let peer_config = candidate_event.candidate_peer_config.clone()?;
    let mut eviction_outcome = None;
    let mut readmission_succeeded = false;

    for event in &transaction.events {
        match (event.action.phase.as_str(), event.action.outcome.as_str()) {
            ("eviction_outcome", "success") => eviction_outcome = Some("success".to_string()),
            ("eviction_recovery", "outcome_unknown") => {
                eviction_outcome = Some("outcome_unknown".to_string());
            }
            ("eviction_intent", "pending") => {
                eviction_outcome.get_or_insert_with(|| "outcome_unknown".to_string());
            }
            ("readmission_outcome", "success") => readmission_succeeded = true,
            ("readmission_intent", "pending") | ("readmission_outcome", "failure") => {
                readmission_succeeded = false;
            }
            _ => {}
        }
    }

    if readmission_succeeded {
        return None;
    }

    Some(UnresolvedReadmissionCandidate {
        eviction_decision_id: transaction.decision_id.clone(),
        peer_config,
        eviction_outcome: eviction_outcome?,
    })
}

fn serialized_transaction_len(transaction: &JournalTransaction) -> Result<u64, String> {
    transaction.events.iter().try_fold(0_u64, |total, event| {
        serde_json::to_vec(event)
            .map(|bytes| total + bytes.len() as u64 + 1)
            .map_err(|error| format!("failed to size auto-heal event: {error}"))
    })
}

fn oversized_transaction_error(decision_id: &str, transaction_len: u64, max_bytes: u64) -> String {
    format!(
        "auto-heal journal transaction {decision_id} is {transaction_len} bytes and exceeds auto-heal journal max_bytes {max_bytes}"
    )
}

#[cfg(test)]
mod tests;
