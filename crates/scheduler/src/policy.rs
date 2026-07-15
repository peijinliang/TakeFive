use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{CandidateKind, PlannedCandidate};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeContext {
    pub reminder_enabled: bool,
    pub in_active_window: bool,
    pub reminder_paused_until: Option<DateTime<Utc>>,
    pub global_paused_until: Option<DateTime<Utc>>,
    pub dnd_until: Option<DateTime<Utc>>,
    pub session_available: bool,
    pub wake_cooldown_until: Option<DateTime<Utc>>,
    pub fullscreen: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PolicyDecision {
    Deliver,
    Defer {
        until: DateTime<Utc>,
        reason: SuppressionReason,
    },
    Ignore {
        reason: SuppressionReason,
    },
    Missed {
        reason: SuppressionReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuppressionReason {
    ReminderDisabled,
    OutsideActiveWindow,
    ReminderPaused,
    GlobalPaused,
    Dnd,
    SessionUnavailable,
    WakeCooldown,
    Fullscreen,
    OneShotCatchUpExpired,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PolicyEngine;

impl PolicyEngine {
    pub fn evaluate(
        &self,
        candidate: &PlannedCandidate,
        context: &RuntimeContext,
        now: DateTime<Utc>,
    ) -> PolicyDecision {
        if !context.reminder_enabled {
            return PolicyDecision::Ignore {
                reason: SuppressionReason::ReminderDisabled,
            };
        }
        if !context.in_active_window {
            return PolicyDecision::Ignore {
                reason: SuppressionReason::OutsideActiveWindow,
            };
        }
        if let Some(until) = future(context.reminder_paused_until, now) {
            return delayed_or_ignored(candidate, until, SuppressionReason::ReminderPaused);
        }

        let can_bypass = candidate.policy.important && candidate.policy.allow_important_bypass;
        if !can_bypass {
            if let Some(until) = future(context.global_paused_until, now) {
                return delayed_or_ignored(candidate, until, SuppressionReason::GlobalPaused);
            }
            if let Some(until) = future(context.dnd_until, now) {
                return delayed_or_ignored(candidate, until, SuppressionReason::Dnd);
            }
        }

        if !context.session_available {
            return self.missed_or_catch_up(candidate, now, SuppressionReason::SessionUnavailable);
        }
        if let Some(until) = future(context.wake_cooldown_until, now) {
            return PolicyDecision::Defer {
                until,
                reason: SuppressionReason::WakeCooldown,
            };
        }
        if context.fullscreen {
            return self.missed_or_catch_up(candidate, now, SuppressionReason::Fullscreen);
        }

        PolicyDecision::Deliver
    }

    fn missed_or_catch_up(
        &self,
        candidate: &PlannedCandidate,
        now: DateTime<Utc>,
        reason: SuppressionReason,
    ) -> PolicyDecision {
        match candidate.kind {
            CandidateKind::Cyclic => PolicyDecision::Missed { reason },
            CandidateKind::OneShot => {
                let expires_at = candidate.scheduled_at_utc
                    + chrono::TimeDelta::seconds(candidate.policy.catch_up_one_shot_within_seconds);
                if now <= expires_at {
                    PolicyDecision::Defer { until: now, reason }
                } else {
                    PolicyDecision::Missed {
                        reason: SuppressionReason::OneShotCatchUpExpired,
                    }
                }
            }
        }
    }
}

fn delayed_or_ignored(
    candidate: &PlannedCandidate,
    until: DateTime<Utc>,
    reason: SuppressionReason,
) -> PolicyDecision {
    match candidate.kind {
        CandidateKind::Cyclic => PolicyDecision::Ignore { reason },
        CandidateKind::OneShot => PolicyDecision::Defer { until, reason },
    }
}

fn future(value: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    value.filter(|until| *until > now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReminderDeliveryPolicy;
    use chrono::TimeZone;

    fn candidate(kind: CandidateKind) -> PlannedCandidate {
        PlannedCandidate {
            resume_occurrence_id: None,
            reminder_id: "r-1".to_string(),
            reminder_revision: 1,
            occurrence_key: "key".to_string(),
            scheduled_at_utc: Utc.with_ymd_and_hms(2026, 7, 14, 2, 0, 0).unwrap(),
            scheduled_local: "2026-07-14T10:00:00".to_string(),
            timezone_id: "Asia/Shanghai".to_string(),
            kind,
            policy: ReminderDeliveryPolicy::default(),
        }
    }

    fn available_context() -> RuntimeContext {
        RuntimeContext {
            reminder_enabled: true,
            in_active_window: true,
            session_available: true,
            ..Default::default()
        }
    }

    #[test]
    fn per_reminder_pause_wins_before_global_bypass() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 2, 0, 0).unwrap();
        let mut item = candidate(CandidateKind::Cyclic);
        item.policy.important = true;
        item.policy.allow_important_bypass = true;
        let context = RuntimeContext {
            reminder_paused_until: Some(now + chrono::TimeDelta::minutes(10)),
            ..available_context()
        };

        assert!(matches!(
            PolicyEngine.evaluate(&item, &context, now),
            PolicyDecision::Ignore {
                reason: SuppressionReason::ReminderPaused
            }
        ));
    }

    #[test]
    fn one_shot_pause_defers_but_cyclic_dnd_is_not_queued_for_catch_up() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 2, 0, 0).unwrap();
        let until = now + chrono::TimeDelta::minutes(30);
        let mut one_shot = candidate(CandidateKind::OneShot);
        one_shot.scheduled_at_utc = now;
        let paused = RuntimeContext {
            global_paused_until: Some(until),
            ..available_context()
        };
        assert_eq!(
            PolicyEngine.evaluate(&one_shot, &paused, now),
            PolicyDecision::Defer {
                until,
                reason: SuppressionReason::GlobalPaused,
            }
        );

        let cyclic = candidate(CandidateKind::Cyclic);
        let dnd = RuntimeContext {
            dnd_until: Some(until),
            ..available_context()
        };
        assert_eq!(
            PolicyEngine.evaluate(&cyclic, &dnd, now),
            PolicyDecision::Ignore {
                reason: SuppressionReason::Dnd,
            }
        );
    }

    #[test]
    fn explicitly_authorized_important_reminder_bypasses_dnd() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 2, 0, 0).unwrap();
        let mut item = candidate(CandidateKind::Cyclic);
        item.policy.important = true;
        item.policy.allow_important_bypass = true;
        let context = RuntimeContext {
            dnd_until: Some(now + chrono::TimeDelta::hours(1)),
            ..available_context()
        };

        assert_eq!(
            PolicyEngine.evaluate(&item, &context, now),
            PolicyDecision::Deliver
        );
    }

    #[test]
    fn sleeping_cyclic_reminder_is_marked_missed() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 3, 0, 0).unwrap();
        let context = RuntimeContext {
            session_available: false,
            ..available_context()
        };

        assert_eq!(
            PolicyEngine.evaluate(&candidate(CandidateKind::Cyclic), &context, now),
            PolicyDecision::Missed {
                reason: SuppressionReason::SessionUnavailable
            }
        );
    }
}
