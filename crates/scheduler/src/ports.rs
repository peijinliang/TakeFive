use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{PolicyDecision, RuntimeContext};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateKind {
    Cyclic,
    OneShot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedCandidate {
    /// Existing non-terminal occurrence to resume after restart/defer. New plans use `None`.
    pub resume_occurrence_id: Option<String>,
    pub reminder_id: String,
    pub reminder_revision: i64,
    pub occurrence_key: String,
    pub scheduled_at_utc: DateTime<Utc>,
    pub scheduled_local: String,
    pub timezone_id: String,
    pub kind: CandidateKind,
    pub policy: ReminderDeliveryPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderDeliveryPolicy {
    pub important: bool,
    pub allow_important_bypass: bool,
    pub catch_up_one_shot_within_seconds: i64,
}

impl Default for ReminderDeliveryPolicy {
    fn default() -> Self {
        Self {
            important: false,
            allow_important_bypass: false,
            catch_up_one_shot_within_seconds: 24 * 60 * 60,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileCause {
    Startup,
    Timer,
    Wake,
    Unlock,
    TimeChanged,
    TimezoneChanged,
    ConfigurationChanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    Claimed { occurrence_id: String },
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryRequest {
    pub occurrence_id: String,
    pub reminder_id: String,
    pub scheduled_at_utc: DateTime<Utc>,
}

#[async_trait]
pub trait CandidateSource: Send + Sync {
    async fn due_candidates(
        &self,
        since: DateTime<Utc>,
        now: DateTime<Utc>,
        cause: ReconcileCause,
    ) -> Result<Vec<PlannedCandidate>, SchedulerError>;
}

#[async_trait]
pub trait RuntimeContextSource: Send + Sync {
    async fn runtime_context(
        &self,
        candidate: &PlannedCandidate,
        now: DateTime<Utc>,
    ) -> Result<RuntimeContext, SchedulerError>;
}

#[async_trait]
pub trait OccurrenceStore: Send + Sync {
    async fn claim(
        &self,
        candidate: &PlannedCandidate,
        claimed_at: DateTime<Utc>,
    ) -> Result<ClaimOutcome, StoreError>;

    async fn record_decision(
        &self,
        occurrence_id: &str,
        decision: &PolicyDecision,
        decided_at: DateTime<Utc>,
    ) -> Result<(), StoreError>;

    async fn mark_presented(
        &self,
        occurrence_id: &str,
        presented_at: DateTime<Utc>,
    ) -> Result<(), StoreError>;

    async fn mark_delivery_failed(
        &self,
        occurrence_id: &str,
        error_code: &str,
        failed_at: DateTime<Utc>,
    ) -> Result<(), StoreError>;
}

#[async_trait]
pub trait DeliveryPort: Send + Sync {
    async fn deliver(&self, request: DeliveryRequest) -> Result<(), DeliveryError>;
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("store error: {code}")]
pub struct StoreError {
    pub code: String,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("delivery error: {code}")]
pub struct DeliveryError {
    pub code: String,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SchedulerError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("candidate source error: {0}")]
    CandidateSource(String),
    #[error("runtime context source error: {0}")]
    RuntimeContextSource(String),
}
