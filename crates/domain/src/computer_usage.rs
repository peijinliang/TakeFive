use crate::OccurrenceKey;
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

const MIN_USAGE_MILLIS: i64 = 10 * 60 * 1_000;
const MAX_USAGE_MILLIS: i64 = 4 * 60 * 60 * 1_000;
const MIN_AWAY_MILLIS: i64 = 2 * 60 * 1_000;
const MAX_AWAY_MILLIS: i64 = 30 * 60 * 1_000;

/// A continuous-use rule consumes only durations reported by the platform. It
/// never receives key codes, pointer positions, window titles or input content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputerUsageRule {
    usage_threshold_millis: i64,
    away_threshold_millis: i64,
    valid_rest_millis: i64,
}

impl ComputerUsageRule {
    pub fn new(
        usage_threshold: TimeDelta,
        away_threshold: TimeDelta,
        valid_rest_duration: TimeDelta,
    ) -> Result<Self, ComputerUsageError> {
        let usage_threshold_millis = usage_threshold.num_milliseconds();
        let away_threshold_millis = away_threshold.num_milliseconds();
        let valid_rest_millis = valid_rest_duration.num_milliseconds();

        if !(MIN_USAGE_MILLIS..=MAX_USAGE_MILLIS).contains(&usage_threshold_millis) {
            return Err(ComputerUsageError::UsageThresholdOutOfRange);
        }
        if !(MIN_AWAY_MILLIS..=MAX_AWAY_MILLIS).contains(&away_threshold_millis) {
            return Err(ComputerUsageError::AwayThresholdOutOfRange);
        }
        if valid_rest_millis < away_threshold_millis {
            return Err(ComputerUsageError::ValidRestTooShort);
        }

        Ok(Self {
            usage_threshold_millis,
            away_threshold_millis,
            valid_rest_millis,
        })
    }

    pub fn usage_threshold(&self) -> TimeDelta {
        TimeDelta::milliseconds(self.usage_threshold_millis)
    }

    pub fn away_threshold(&self) -> TimeDelta {
        TimeDelta::milliseconds(self.away_threshold_millis)
    }

    pub fn valid_rest_duration(&self) -> TimeDelta {
        TimeDelta::milliseconds(self.valid_rest_millis)
    }

    pub fn observe(
        &self,
        state: &mut ComputerUsageState,
        now_utc: DateTime<Utc>,
        idle_duration: TimeDelta,
        session: ComputerSessionState,
    ) -> Result<ComputerUsageObservation, ComputerUsageError> {
        state.observe(self, now_utc, idle_duration, session)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerSessionState {
    Active,
    Locked,
    Sleeping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerActivityPhase {
    Active,
    TemporarilyIdle,
    RestQualified,
    Locked,
    Sleeping,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityCycle {
    id: String,
    sequence: u64,
    started_at_utc: DateTime<Utc>,
    accumulated_millis: i64,
    threshold_crossing_index: u64,
    threshold_emitted: bool,
}

impl ActivityCycle {
    fn new(tracker_id: &str, sequence: u64, started_at_utc: DateTime<Utc>) -> Self {
        Self {
            id: format!("{tracker_id}:{sequence}"),
            sequence,
            started_at_utc,
            accumulated_millis: 0,
            threshold_crossing_index: 0,
            threshold_emitted: false,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub const fn started_at_utc(&self) -> DateTime<Utc> {
        self.started_at_utc
    }

    pub fn accumulated(&self) -> TimeDelta {
        TimeDelta::milliseconds(self.accumulated_millis)
    }

    pub const fn threshold_was_emitted(&self) -> bool {
        self.threshold_emitted
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputerUsageState {
    tracker_id: String,
    cycle: ActivityCycle,
    phase: ComputerActivityPhase,
    last_observed_at_utc: DateTime<Utc>,
    last_idle_millis: i64,
    absence_started_at_utc: Option<DateTime<Utc>>,
    rest_qualified_for_current_absence: bool,
}

impl ComputerUsageState {
    pub fn new(tracker_id: impl Into<String>, now_utc: DateTime<Utc>) -> Self {
        let tracker_id = tracker_id.into();
        Self {
            cycle: ActivityCycle::new(&tracker_id, 0, now_utc),
            tracker_id,
            phase: ComputerActivityPhase::Active,
            last_observed_at_utc: now_utc,
            last_idle_millis: 0,
            absence_started_at_utc: None,
            rest_qualified_for_current_absence: false,
        }
    }

    pub fn observe(
        &mut self,
        rule: &ComputerUsageRule,
        now_utc: DateTime<Utc>,
        idle_duration: TimeDelta,
        session: ComputerSessionState,
    ) -> Result<ComputerUsageObservation, ComputerUsageError> {
        if now_utc < self.last_observed_at_utc {
            return Err(ComputerUsageError::ObservationMovedBackwards);
        }
        let idle_millis = idle_duration.num_milliseconds();
        if idle_millis < 0 {
            return Err(ComputerUsageError::NegativeIdleDuration);
        }

        let previous_phase = self.phase;
        let elapsed_millis = now_utc
            .signed_duration_since(self.last_observed_at_utc)
            .num_milliseconds();
        let mut cycle_reset = false;
        let mut trigger = None;

        match session {
            ComputerSessionState::Locked | ComputerSessionState::Sleeping => {
                if !matches!(
                    previous_phase,
                    ComputerActivityPhase::Locked | ComputerActivityPhase::Sleeping
                ) {
                    let added = active_contribution(
                        previous_phase,
                        elapsed_millis,
                        self.last_idle_millis,
                        idle_millis,
                        rule.away_threshold_millis,
                    );
                    trigger = self.add_active_duration(rule, added, now_utc);
                    self.absence_started_at_utc = Some(now_utc);
                    self.rest_qualified_for_current_absence = false;
                }

                if self.qualify_suspended_rest(rule, now_utc)? {
                    cycle_reset = true;
                    trigger = None;
                }
                self.phase = match session {
                    ComputerSessionState::Locked => ComputerActivityPhase::Locked,
                    ComputerSessionState::Sleeping => ComputerActivityPhase::Sleeping,
                    ComputerSessionState::Active => unreachable!(),
                };
            }
            ComputerSessionState::Active => {
                if matches!(
                    previous_phase,
                    ComputerActivityPhase::Locked | ComputerActivityPhase::Sleeping
                ) {
                    if self.qualify_suspended_rest(rule, now_utc)? {
                        cycle_reset = true;
                    }
                    self.absence_started_at_utc = None;
                    self.rest_qualified_for_current_absence = false;
                } else if idle_millis >= rule.valid_rest_millis {
                    if previous_phase != ComputerActivityPhase::RestQualified {
                        let qualified_at = now_utc
                            .checked_sub_signed(TimeDelta::milliseconds(
                                idle_millis - rule.valid_rest_millis,
                            ))
                            .unwrap_or(now_utc);
                        self.reset_cycle(qualified_at)?;
                        cycle_reset = true;
                    }
                    self.absence_started_at_utc =
                        now_utc.checked_sub_signed(TimeDelta::milliseconds(idle_millis));
                    self.rest_qualified_for_current_absence = true;
                    self.phase = ComputerActivityPhase::RestQualified;
                } else {
                    let added = if matches!(
                        previous_phase,
                        ComputerActivityPhase::Locked
                            | ComputerActivityPhase::Sleeping
                            | ComputerActivityPhase::RestQualified
                    ) {
                        idle_millis.min(elapsed_millis)
                    } else {
                        active_contribution(
                            previous_phase,
                            elapsed_millis,
                            self.last_idle_millis,
                            idle_millis,
                            rule.away_threshold_millis,
                        )
                    };
                    trigger = self.add_active_duration(rule, added, now_utc);

                    if idle_millis >= rule.away_threshold_millis {
                        self.phase = ComputerActivityPhase::TemporarilyIdle;
                        self.absence_started_at_utc =
                            now_utc.checked_sub_signed(TimeDelta::milliseconds(idle_millis));
                    } else {
                        self.phase = ComputerActivityPhase::Active;
                        self.absence_started_at_utc = None;
                    }
                    self.rest_qualified_for_current_absence = false;
                }
            }
        }

        self.last_observed_at_utc = now_utc;
        self.last_idle_millis = idle_millis;

        Ok(ComputerUsageObservation {
            phase: self.phase,
            accumulated: self.cycle.accumulated(),
            cycle_reset,
            trigger,
        })
    }

    /// Starts a fresh activity cycle after the user finishes an explicit rest.
    pub fn complete_rest(&mut self, now_utc: DateTime<Utc>) -> Result<(), ComputerUsageError> {
        if now_utc < self.last_observed_at_utc {
            return Err(ComputerUsageError::ObservationMovedBackwards);
        }
        self.reset_cycle(now_utc)?;
        self.last_observed_at_utc = now_utc;
        self.last_idle_millis = 0;
        self.absence_started_at_utc = None;
        self.rest_qualified_for_current_absence = false;
        self.phase = ComputerActivityPhase::Active;
        Ok(())
    }

    pub fn tracker_id(&self) -> &str {
        &self.tracker_id
    }

    pub fn cycle(&self) -> &ActivityCycle {
        &self.cycle
    }

    pub const fn phase(&self) -> ComputerActivityPhase {
        self.phase
    }

    pub const fn last_observed_at_utc(&self) -> DateTime<Utc> {
        self.last_observed_at_utc
    }

    fn qualify_suspended_rest(
        &mut self,
        rule: &ComputerUsageRule,
        now_utc: DateTime<Utc>,
    ) -> Result<bool, ComputerUsageError> {
        let Some(started_at) = self.absence_started_at_utc else {
            return Ok(false);
        };
        if self.rest_qualified_for_current_absence
            || now_utc.signed_duration_since(started_at).num_milliseconds() < rule.valid_rest_millis
        {
            return Ok(false);
        }

        let qualified_at = started_at
            .checked_add_signed(TimeDelta::milliseconds(rule.valid_rest_millis))
            .unwrap_or(now_utc);
        self.reset_cycle(qualified_at)?;
        self.rest_qualified_for_current_absence = true;
        Ok(true)
    }

    fn add_active_duration(
        &mut self,
        rule: &ComputerUsageRule,
        added_millis: i64,
        observed_at_utc: DateTime<Utc>,
    ) -> Option<ComputerUsageTrigger> {
        if added_millis <= 0 {
            return None;
        }
        let before = self.cycle.accumulated_millis;
        self.cycle.accumulated_millis = self.cycle.accumulated_millis.saturating_add(added_millis);

        if self.cycle.threshold_emitted
            || before >= rule.usage_threshold_millis
            || self.cycle.accumulated_millis < rule.usage_threshold_millis
        {
            return None;
        }

        self.cycle.threshold_crossing_index += 1;
        self.cycle.threshold_emitted = true;
        Some(ComputerUsageTrigger {
            occurrence_key: OccurrenceKey::activity(
                &self.cycle.id,
                self.cycle.threshold_crossing_index,
            ),
            activity_cycle_id: self.cycle.id.clone(),
            threshold_crossing_index: self.cycle.threshold_crossing_index,
            observed_at_utc,
        })
    }

    fn reset_cycle(&mut self, started_at_utc: DateTime<Utc>) -> Result<(), ComputerUsageError> {
        let sequence = self
            .cycle
            .sequence
            .checked_add(1)
            .ok_or(ComputerUsageError::CycleOverflow)?;
        self.cycle = ActivityCycle::new(&self.tracker_id, sequence, started_at_utc);
        Ok(())
    }
}

fn active_contribution(
    previous_phase: ComputerActivityPhase,
    elapsed_millis: i64,
    previous_idle_millis: i64,
    idle_millis: i64,
    away_threshold_millis: i64,
) -> i64 {
    if elapsed_millis <= 0 {
        return 0;
    }

    match previous_phase {
        ComputerActivityPhase::Active => {
            if idle_millis < away_threshold_millis {
                elapsed_millis
            } else {
                let idle_at_interval_start = idle_millis.saturating_sub(elapsed_millis);
                (away_threshold_millis - idle_at_interval_start).clamp(0, elapsed_millis)
            }
        }
        ComputerActivityPhase::TemporarilyIdle => {
            if idle_millis < previous_idle_millis.saturating_add(elapsed_millis) {
                idle_millis.min(away_threshold_millis).min(elapsed_millis)
            } else {
                0
            }
        }
        ComputerActivityPhase::RestQualified
        | ComputerActivityPhase::Locked
        | ComputerActivityPhase::Sleeping => 0,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputerUsageObservation {
    pub phase: ComputerActivityPhase,
    pub accumulated: TimeDelta,
    pub cycle_reset: bool,
    pub trigger: Option<ComputerUsageTrigger>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputerUsageTrigger {
    pub occurrence_key: OccurrenceKey,
    pub activity_cycle_id: String,
    pub threshold_crossing_index: u64,
    pub observed_at_utc: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputerUsageError {
    UsageThresholdOutOfRange,
    AwayThresholdOutOfRange,
    ValidRestTooShort,
    NegativeIdleDuration,
    ObservationMovedBackwards,
    CycleOverflow,
}

impl fmt::Display for ComputerUsageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UsageThresholdOutOfRange => {
                formatter.write_str("usage threshold must be between 10 minutes and 4 hours")
            }
            Self::AwayThresholdOutOfRange => {
                formatter.write_str("away threshold must be between 2 and 30 minutes")
            }
            Self::ValidRestTooShort => {
                formatter.write_str("valid rest duration must not be shorter than away threshold")
            }
            Self::NegativeIdleDuration => formatter.write_str("idle duration cannot be negative"),
            Self::ObservationMovedBackwards => {
                formatter.write_str("observation time cannot move backwards")
            }
            Self::CycleOverflow => formatter.write_str("activity cycle sequence overflow"),
        }
    }
}

impl std::error::Error for ComputerUsageError {}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use pretty_assertions::assert_eq;

    fn at(hour: u32, minute: u32, second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 14, hour, minute, second)
            .unwrap()
    }

    fn rule() -> ComputerUsageRule {
        ComputerUsageRule::new(
            TimeDelta::minutes(10),
            TimeDelta::minutes(2),
            TimeDelta::minutes(5),
        )
        .unwrap()
    }

    #[test]
    fn validates_product_threshold_boundaries() {
        assert!(ComputerUsageRule::new(
            TimeDelta::minutes(9),
            TimeDelta::minutes(5),
            TimeDelta::minutes(10)
        )
        .is_err());
        assert!(ComputerUsageRule::new(
            TimeDelta::hours(4) + TimeDelta::milliseconds(1),
            TimeDelta::minutes(5),
            TimeDelta::minutes(10)
        )
        .is_err());
        assert!(ComputerUsageRule::new(
            TimeDelta::minutes(10),
            TimeDelta::minutes(1),
            TimeDelta::minutes(10)
        )
        .is_err());
        assert!(ComputerUsageRule::new(
            TimeDelta::minutes(10),
            TimeDelta::minutes(31),
            TimeDelta::minutes(31)
        )
        .is_err());
        assert!(ComputerUsageRule::new(
            TimeDelta::minutes(10),
            TimeDelta::minutes(5),
            TimeDelta::minutes(4)
        )
        .is_err());
    }

    #[test]
    fn short_away_period_preserves_accumulated_usage() {
        let rule = rule();
        let start = at(9, 0, 0);
        let mut state = ComputerUsageState::new("usage", start);

        rule.observe(
            &mut state,
            start + TimeDelta::minutes(6),
            TimeDelta::zero(),
            ComputerSessionState::Active,
        )
        .unwrap();
        let idle = rule
            .observe(
                &mut state,
                start + TimeDelta::minutes(9),
                TimeDelta::minutes(3),
                ComputerSessionState::Active,
            )
            .unwrap();
        assert_eq!(idle.phase, ComputerActivityPhase::TemporarilyIdle);
        assert_eq!(idle.accumulated, TimeDelta::minutes(8));

        let resumed = rule
            .observe(
                &mut state,
                start + TimeDelta::minutes(9) + TimeDelta::seconds(5),
                TimeDelta::seconds(5),
                ComputerSessionState::Active,
            )
            .unwrap();
        assert_eq!(resumed.phase, ComputerActivityPhase::Active);
        assert_eq!(
            resumed.accumulated,
            TimeDelta::minutes(8) + TimeDelta::seconds(5)
        );
        assert_eq!(state.cycle().sequence(), 0);
    }

    #[test]
    fn valid_idle_rest_resets_cycle_once() {
        let rule = rule();
        let start = at(9, 0, 0);
        let mut state = ComputerUsageState::new("usage", start);
        rule.observe(
            &mut state,
            start + TimeDelta::minutes(6),
            TimeDelta::zero(),
            ComputerSessionState::Active,
        )
        .unwrap();

        let rested = rule
            .observe(
                &mut state,
                start + TimeDelta::minutes(12),
                TimeDelta::minutes(6),
                ComputerSessionState::Active,
            )
            .unwrap();
        assert!(rested.cycle_reset);
        assert_eq!(rested.accumulated, TimeDelta::zero());
        assert_eq!(state.cycle().sequence(), 1);

        let still_idle = rule
            .observe(
                &mut state,
                start + TimeDelta::minutes(13),
                TimeDelta::minutes(7),
                ComputerSessionState::Active,
            )
            .unwrap();
        assert!(!still_idle.cycle_reset);
        assert_eq!(state.cycle().sequence(), 1);
    }

    #[test]
    fn lock_and_sleep_do_not_accumulate_and_long_absence_resets() {
        let rule = rule();
        let start = at(9, 0, 0);
        let mut state = ComputerUsageState::new("usage", start);
        rule.observe(
            &mut state,
            start + TimeDelta::minutes(4),
            TimeDelta::zero(),
            ComputerSessionState::Active,
        )
        .unwrap();
        rule.observe(
            &mut state,
            start + TimeDelta::minutes(4),
            TimeDelta::zero(),
            ComputerSessionState::Locked,
        )
        .unwrap();
        let sleeping = rule
            .observe(
                &mut state,
                start + TimeDelta::minutes(7),
                TimeDelta::zero(),
                ComputerSessionState::Sleeping,
            )
            .unwrap();
        assert_eq!(sleeping.accumulated, TimeDelta::minutes(4));
        assert!(!sleeping.cycle_reset);

        let resumed = rule
            .observe(
                &mut state,
                start + TimeDelta::minutes(10),
                TimeDelta::zero(),
                ComputerSessionState::Active,
            )
            .unwrap();
        assert!(resumed.cycle_reset);
        assert_eq!(resumed.accumulated, TimeDelta::zero());
        assert_eq!(state.cycle().sequence(), 1);
    }

    #[test]
    fn short_lock_preserves_cycle_without_counting_locked_time() {
        let rule = rule();
        let start = at(9, 0, 0);
        let mut state = ComputerUsageState::new("usage", start);
        rule.observe(
            &mut state,
            start + TimeDelta::minutes(4),
            TimeDelta::zero(),
            ComputerSessionState::Active,
        )
        .unwrap();
        rule.observe(
            &mut state,
            start + TimeDelta::minutes(4),
            TimeDelta::zero(),
            ComputerSessionState::Locked,
        )
        .unwrap();
        let resumed = rule
            .observe(
                &mut state,
                start + TimeDelta::minutes(7),
                TimeDelta::zero(),
                ComputerSessionState::Active,
            )
            .unwrap();

        assert!(!resumed.cycle_reset);
        assert_eq!(resumed.accumulated, TimeDelta::minutes(4));
        assert_eq!(state.cycle().sequence(), 0);
    }

    #[test]
    fn threshold_emits_once_per_cycle_with_stable_key() {
        let rule = rule();
        let start = at(9, 0, 0);
        let mut state = ComputerUsageState::new("usage", start);

        let crossed = rule
            .observe(
                &mut state,
                start + TimeDelta::minutes(10),
                TimeDelta::zero(),
                ComputerSessionState::Active,
            )
            .unwrap();
        let trigger = crossed.trigger.unwrap();
        assert_eq!(
            trigger.occurrence_key.as_str(),
            "v1|activity|cycle=usage:0|crossing=1"
        );

        for minute in 11..=20 {
            let later = rule
                .observe(
                    &mut state,
                    start + TimeDelta::minutes(minute),
                    TimeDelta::zero(),
                    ComputerSessionState::Active,
                )
                .unwrap();
            assert!(later.trigger.is_none());
        }
        assert!(state.cycle().threshold_was_emitted());
    }

    #[test]
    fn completing_rest_starts_next_cycle_and_next_key() {
        let rule = rule();
        let start = at(9, 0, 0);
        let mut state = ComputerUsageState::new("usage", start);
        rule.observe(
            &mut state,
            start + TimeDelta::minutes(10),
            TimeDelta::zero(),
            ComputerSessionState::Active,
        )
        .unwrap();

        state.complete_rest(start + TimeDelta::minutes(11)).unwrap();
        let next = rule
            .observe(
                &mut state,
                start + TimeDelta::minutes(21),
                TimeDelta::zero(),
                ComputerSessionState::Active,
            )
            .unwrap()
            .trigger
            .unwrap();

        assert_eq!(state.cycle().sequence(), 1);
        assert_eq!(
            next.occurrence_key.as_str(),
            "v1|activity|cycle=usage:1|crossing=1"
        );
    }

    #[test]
    fn serialized_checkpoint_resumes_without_double_trigger() {
        let rule = rule();
        let start = at(9, 0, 0);
        let mut state = ComputerUsageState::new("usage", start);
        rule.observe(
            &mut state,
            start + TimeDelta::minutes(10),
            TimeDelta::zero(),
            ComputerSessionState::Active,
        )
        .unwrap();

        let json = serde_json::to_string(&state).unwrap();
        let mut restored: ComputerUsageState = serde_json::from_str(&json).unwrap();
        let observation = rule
            .observe(
                &mut restored,
                start + TimeDelta::minutes(11),
                TimeDelta::zero(),
                ComputerSessionState::Active,
            )
            .unwrap();

        assert!(observation.trigger.is_none());
        assert_eq!(restored.cycle().id(), "usage:0");
        assert_eq!(restored.cycle().accumulated(), TimeDelta::minutes(11));
    }

    #[test]
    fn invalid_observation_does_not_mutate_checkpoint() {
        let rule = rule();
        let start = at(9, 0, 0);
        let mut state = ComputerUsageState::new("usage", start);
        let original = state.clone();

        assert_eq!(
            rule.observe(
                &mut state,
                start - TimeDelta::seconds(1),
                TimeDelta::zero(),
                ComputerSessionState::Active,
            ),
            Err(ComputerUsageError::ObservationMovedBackwards)
        );
        assert_eq!(state, original);
    }
}
