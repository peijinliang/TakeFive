use crate::{RuleRevision, ScheduleCandidate};
use chrono::{DateTime, NaiveDateTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OccurrenceKey(String);

impl OccurrenceKey {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn from_schedule_candidate(candidate: &ScheduleCandidate) -> Self {
        Self(candidate.occurrence_key())
    }

    pub fn aligned(
        timezone: Tz,
        anchor_local: NaiveDateTime,
        interval_minutes: u32,
        interval_index: u64,
    ) -> Self {
        Self(format!(
            "v1|aligned|tz={timezone}|anchor={}|every={interval_minutes}m|index={interval_index}",
            anchor_local.format("%Y-%m-%dT%H:%M:%S")
        ))
    }

    pub fn session(session_id: &str, cycle: u64) -> Self {
        Self(format!("v1|session|id={session_id}|cycle={cycle}"))
    }

    pub fn activity(activity_cycle_id: &str, threshold_crossing_index: u64) -> Self {
        Self(format!(
            "v1|activity|cycle={activity_cycle_id}|crossing={threshold_crossing_index}"
        ))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OccurrenceKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OccurrenceState {
    Planned,
    Claimed,
    Suppressed,
    Delivering,
    Presented,
    Snoozed,
    DeliveryFailed,
    Completed,
    Skipped,
    Unhandled,
    IgnoredByDnd,
    MissedBySleep,
    MergedReplaced,
    Archived,
}

impl OccurrenceState {
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Skipped
                | Self::Unhandled
                | Self::IgnoredByDnd
                | Self::MissedBySleep
                | Self::MergedReplaced
                | Self::Archived
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OccurrenceResult {
    Completed,
    Skipped,
    Unhandled,
    IgnoredByDnd,
    MissedBySleep,
    MergedReplaced,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonCode {
    ReminderDisabled,
    RuleRevised,
    OutsideActiveDate,
    OutsideActiveWindow,
    ReminderPaused,
    GlobalPaused,
    DoNotDisturb,
    Fullscreen,
    Sleeping,
    Locked,
    DeliveryChannelsFailed,
    TimedOut,
    Merged,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OccurrenceAction {
    Claim,
    Suppress { reason: ReasonCode },
    BeginDelivery,
    MarkPresented,
    MarkDeliveryFailed { reason: ReasonCode },
    Complete,
    Skip,
    Snooze { due_at_utc: DateTime<Utc> },
    MarkUnhandled { reason: ReasonCode },
    IgnoreByDnd,
    MissBySleep,
    MergeReplaced { into_occurrence_id: String },
    Archive,
}

impl OccurrenceAction {
    const fn target_state(&self) -> OccurrenceState {
        match self {
            Self::Claim => OccurrenceState::Claimed,
            Self::Suppress { .. } => OccurrenceState::Suppressed,
            Self::BeginDelivery => OccurrenceState::Delivering,
            Self::MarkPresented => OccurrenceState::Presented,
            Self::MarkDeliveryFailed { .. } => OccurrenceState::DeliveryFailed,
            Self::Complete => OccurrenceState::Completed,
            Self::Skip => OccurrenceState::Skipped,
            Self::Snooze { .. } => OccurrenceState::Snoozed,
            Self::MarkUnhandled { .. } => OccurrenceState::Unhandled,
            Self::IgnoreByDnd => OccurrenceState::IgnoredByDnd,
            Self::MissBySleep => OccurrenceState::MissedBySleep,
            Self::MergeReplaced { .. } => OccurrenceState::MergedReplaced,
            Self::Archive => OccurrenceState::Archived,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Occurrence {
    id: String,
    reminder_id: String,
    reminder_revision: RuleRevision,
    occurrence_key: OccurrenceKey,
    scheduled_at_utc: DateTime<Utc>,
    scheduled_local: NaiveDateTime,
    timezone: Tz,
    state: OccurrenceState,
    result: Option<OccurrenceResult>,
    reason_code: Option<ReasonCode>,
    snooze_due_at_utc: Option<DateTime<Utc>>,
    snooze_count: u32,
    presented_at_utc: Option<DateTime<Utc>>,
    handled_at_utc: Option<DateTime<Utc>>,
    merged_into_id: Option<String>,
    created_at_utc: DateTime<Utc>,
}

impl Occurrence {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: impl Into<String>,
        reminder_id: impl Into<String>,
        reminder_revision: RuleRevision,
        occurrence_key: OccurrenceKey,
        scheduled_at_utc: DateTime<Utc>,
        scheduled_local: NaiveDateTime,
        timezone: Tz,
        created_at_utc: DateTime<Utc>,
    ) -> Self {
        Self {
            id: id.into(),
            reminder_id: reminder_id.into(),
            reminder_revision,
            occurrence_key,
            scheduled_at_utc,
            scheduled_local,
            timezone,
            state: OccurrenceState::Planned,
            result: None,
            reason_code: None,
            snooze_due_at_utc: None,
            snooze_count: 0,
            presented_at_utc: None,
            handled_at_utc: None,
            merged_into_id: None,
            created_at_utc,
        }
    }

    pub fn apply(
        &mut self,
        action: OccurrenceAction,
        at_utc: DateTime<Utc>,
    ) -> Result<(), TransitionError> {
        let from = self.state;
        let to = action.target_state();
        if !is_legal_transition(from, to) {
            return Err(TransitionError::IllegalTransition { from, to });
        }

        if let OccurrenceAction::Snooze { due_at_utc } = &action {
            if *due_at_utc <= at_utc {
                return Err(TransitionError::InvalidSnoozeDeadline);
            }
            self.snooze_count = self
                .snooze_count
                .checked_add(1)
                .ok_or(TransitionError::SnoozeCountOverflow)?;
            self.snooze_due_at_utc = Some(*due_at_utc);
        }

        match &action {
            OccurrenceAction::Suppress { reason }
            | OccurrenceAction::MarkDeliveryFailed { reason }
            | OccurrenceAction::MarkUnhandled { reason } => self.reason_code = Some(*reason),
            OccurrenceAction::IgnoreByDnd => self.reason_code = Some(ReasonCode::DoNotDisturb),
            OccurrenceAction::MissBySleep => self.reason_code = Some(ReasonCode::Sleeping),
            OccurrenceAction::MergeReplaced { into_occurrence_id } => {
                self.reason_code = Some(ReasonCode::Merged);
                self.merged_into_id = Some(into_occurrence_id.clone());
            }
            OccurrenceAction::Archive => self.reason_code = Some(ReasonCode::Deleted),
            _ => {}
        }

        if matches!(action, OccurrenceAction::MarkPresented) {
            self.presented_at_utc = Some(at_utc);
        }
        if to.is_terminal() {
            self.handled_at_utc = Some(at_utc);
            self.result = result_for(to);
        }

        self.state = to;
        Ok(())
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn reminder_id(&self) -> &str {
        &self.reminder_id
    }

    pub const fn reminder_revision(&self) -> RuleRevision {
        self.reminder_revision
    }

    pub fn occurrence_key(&self) -> &OccurrenceKey {
        &self.occurrence_key
    }

    pub const fn state(&self) -> OccurrenceState {
        self.state
    }

    pub const fn result(&self) -> Option<OccurrenceResult> {
        self.result
    }

    pub const fn reason_code(&self) -> Option<ReasonCode> {
        self.reason_code
    }

    pub const fn scheduled_at_utc(&self) -> DateTime<Utc> {
        self.scheduled_at_utc
    }

    pub const fn scheduled_local(&self) -> NaiveDateTime {
        self.scheduled_local
    }

    pub const fn timezone(&self) -> Tz {
        self.timezone
    }

    pub const fn snooze_due_at_utc(&self) -> Option<DateTime<Utc>> {
        self.snooze_due_at_utc
    }

    pub const fn snooze_count(&self) -> u32 {
        self.snooze_count
    }

    pub const fn presented_at_utc(&self) -> Option<DateTime<Utc>> {
        self.presented_at_utc
    }

    pub const fn handled_at_utc(&self) -> Option<DateTime<Utc>> {
        self.handled_at_utc
    }

    pub fn merged_into_id(&self) -> Option<&str> {
        self.merged_into_id.as_deref()
    }

    pub const fn created_at_utc(&self) -> DateTime<Utc> {
        self.created_at_utc
    }
}

fn is_legal_transition(from: OccurrenceState, to: OccurrenceState) -> bool {
    use OccurrenceState as S;
    matches!(
        (from, to),
        (S::Planned, S::Claimed | S::MergedReplaced | S::Archived)
            | (
                S::Claimed,
                S::Suppressed | S::Delivering | S::MergedReplaced | S::Archived
            )
            | (
                S::Suppressed,
                S::Delivering
                    | S::IgnoredByDnd
                    | S::MissedBySleep
                    | S::MergedReplaced
                    | S::Archived
            )
            | (S::Delivering, S::Presented | S::DeliveryFailed)
            | (
                S::Presented,
                S::Completed | S::Skipped | S::Snoozed | S::Unhandled
            )
            | (S::Snoozed, S::Delivering | S::Archived)
            | (S::DeliveryFailed, S::Unhandled)
    )
}

const fn result_for(state: OccurrenceState) -> Option<OccurrenceResult> {
    match state {
        OccurrenceState::Completed => Some(OccurrenceResult::Completed),
        OccurrenceState::Skipped => Some(OccurrenceResult::Skipped),
        OccurrenceState::Unhandled => Some(OccurrenceResult::Unhandled),
        OccurrenceState::IgnoredByDnd => Some(OccurrenceResult::IgnoredByDnd),
        OccurrenceState::MissedBySleep => Some(OccurrenceResult::MissedBySleep),
        OccurrenceState::MergedReplaced => Some(OccurrenceResult::MergedReplaced),
        OccurrenceState::Archived => Some(OccurrenceResult::Archived),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionError {
    IllegalTransition {
        from: OccurrenceState,
        to: OccurrenceState,
    },
    InvalidSnoozeDeadline,
    SnoozeCountOverflow,
}

impl fmt::Display for TransitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IllegalTransition { from, to } => {
                write!(
                    formatter,
                    "illegal occurrence transition: {from:?} -> {to:?}"
                )
            }
            Self::InvalidSnoozeDeadline => {
                formatter.write_str("snooze deadline must be after the action time")
            }
            Self::SnoozeCountOverflow => formatter.write_str("snooze count overflow"),
        }
    }
}

impl std::error::Error for TransitionError {}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeDelta, TimeZone};

    fn occurrence() -> Occurrence {
        let scheduled = Utc.with_ymd_and_hms(2026, 7, 14, 2, 0, 0).unwrap();
        Occurrence::new(
            "occ-1",
            "reminder-1",
            RuleRevision::INITIAL,
            OccurrenceKey::new("fixed-key"),
            scheduled,
            scheduled
                .with_timezone(&chrono_tz::Asia::Shanghai)
                .naive_local(),
            chrono_tz::Asia::Shanghai,
            scheduled - TimeDelta::minutes(1),
        )
    }

    fn present(value: &mut Occurrence, at: DateTime<Utc>) {
        value.apply(OccurrenceAction::Claim, at).unwrap();
        value.apply(OccurrenceAction::BeginDelivery, at).unwrap();
        value.apply(OccurrenceAction::MarkPresented, at).unwrap();
    }

    #[test]
    fn presented_can_complete_skip_snooze_or_time_out() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 2, 0, 0).unwrap();
        let terminal_actions = [
            OccurrenceAction::Complete,
            OccurrenceAction::Skip,
            OccurrenceAction::MarkUnhandled {
                reason: ReasonCode::TimedOut,
            },
        ];

        for action in terminal_actions {
            let mut value = occurrence();
            present(&mut value, now);
            value.apply(action, now).unwrap();
            assert!(value.state().is_terminal());
            assert!(value.result().is_some());
        }

        let mut snoozed = occurrence();
        present(&mut snoozed, now);
        snoozed
            .apply(
                OccurrenceAction::Snooze {
                    due_at_utc: now + TimeDelta::minutes(5),
                },
                now,
            )
            .unwrap();
        assert_eq!(snoozed.state(), OccurrenceState::Snoozed);
        assert_eq!(snoozed.result(), None);
    }

    #[test]
    fn snoozed_occurrence_can_be_delivered_and_snoozed_again() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 2, 0, 0).unwrap();
        let mut value = occurrence();
        let original_key = value.occurrence_key().clone();
        let original_schedule = value.scheduled_at_utc();
        present(&mut value, now);

        for delay in [5, 10] {
            value
                .apply(
                    OccurrenceAction::Snooze {
                        due_at_utc: now + TimeDelta::minutes(delay),
                    },
                    now,
                )
                .unwrap();
            value
                .apply(
                    OccurrenceAction::BeginDelivery,
                    now + TimeDelta::minutes(delay),
                )
                .unwrap();
            value
                .apply(
                    OccurrenceAction::MarkPresented,
                    now + TimeDelta::minutes(delay),
                )
                .unwrap();
        }

        assert_eq!(value.snooze_count(), 2);
        assert_eq!(value.occurrence_key(), &original_key);
        assert_eq!(value.scheduled_at_utc(), original_schedule);
    }

    #[test]
    fn illegal_transition_does_not_mutate_occurrence() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 2, 0, 0).unwrap();
        let mut value = occurrence();

        let error = value.apply(OccurrenceAction::Complete, now).unwrap_err();

        assert_eq!(value.state(), OccurrenceState::Planned);
        assert_eq!(value.result(), None);
        assert_eq!(
            error,
            TransitionError::IllegalTransition {
                from: OccurrenceState::Planned,
                to: OccurrenceState::Completed,
            }
        );
    }

    #[test]
    fn every_terminal_state_rejects_all_actions() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 2, 0, 0).unwrap();
        let terminal_states = [
            OccurrenceState::Completed,
            OccurrenceState::Skipped,
            OccurrenceState::Unhandled,
            OccurrenceState::IgnoredByDnd,
            OccurrenceState::MissedBySleep,
            OccurrenceState::MergedReplaced,
            OccurrenceState::Archived,
        ];
        let actions = [
            OccurrenceAction::Claim,
            OccurrenceAction::BeginDelivery,
            OccurrenceAction::MarkPresented,
            OccurrenceAction::Complete,
            OccurrenceAction::Archive,
        ];

        for state in terminal_states {
            for action in &actions {
                let mut value = occurrence();
                value.state = state;
                value.result = result_for(state);
                assert!(value.apply(action.clone(), now).is_err());
                assert_eq!(value.state(), state);
            }
        }
    }

    #[test]
    fn invalid_snooze_deadline_is_rejected_without_partial_mutation() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 2, 0, 0).unwrap();
        let mut value = occurrence();
        present(&mut value, now);

        assert_eq!(
            value.apply(OccurrenceAction::Snooze { due_at_utc: now }, now),
            Err(TransitionError::InvalidSnoozeDeadline)
        );
        assert_eq!(value.state(), OccurrenceState::Presented);
        assert_eq!(value.snooze_count(), 0);
    }

    #[test]
    fn occurrence_round_trips_through_json() {
        let value = occurrence();
        let json = serde_json::to_string(&value).unwrap();
        let decoded: Occurrence = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, value);
    }
}
