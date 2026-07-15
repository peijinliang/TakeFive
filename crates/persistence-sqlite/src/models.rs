use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, FromRow)]
pub struct Reminder {
    pub id: String,
    pub title: String,
    pub description: String,
    pub enabled: bool,
    pub revision: i64,
    pub created_at_utc: i64,
    pub updated_at_utc: i64,
    pub deleted_at_utc: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NewReminder {
    pub id: String,
    pub title: String,
    pub description: String,
    pub enabled: bool,
    pub created_at_utc: i64,
}

impl NewReminder {
    pub fn new(id: impl Into<String>, title: impl Into<String>, now_utc: i64) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            description: String::new(),
            enabled: true,
            created_at_utc: now_utc,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReminderChanges {
    pub title: String,
    pub description: String,
    pub enabled: bool,
    pub updated_at_utc: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NewScheduleRule {
    pub id: String,
    pub rule_type: String,
    pub timezone_mode: String,
    pub timezone_id: Option<String>,
    pub config_json: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NewReminderBundle {
    pub reminder: NewReminder,
    pub rule: Option<NewScheduleRule>,
    pub policy: Option<NewReminderPolicy>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NewReminderPolicy {
    pub id: String,
    pub delivery_json: String,
    pub sound_json: String,
    pub snooze_json: String,
    pub missed_json: String,
    pub dnd_json: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, FromRow)]
pub struct ScheduledReminderRecord {
    pub reminder_id: String,
    pub title: String,
    pub description: String,
    pub reminder_revision: i64,
    pub rule_id: String,
    pub rule_type: String,
    pub timezone_mode: String,
    pub timezone_id: Option<String>,
    pub rule_config_json: String,
    pub policy_id: String,
    pub delivery_json: String,
    pub sound_json: String,
    pub snooze_json: String,
    pub missed_json: String,
    pub dnd_json: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PauseScope {
    Global,
    Reminder { reminder_id: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NewPauseSession {
    pub id: String,
    pub scope: PauseScope,
    pub starts_at_utc: i64,
    pub ends_at_utc: Option<i64>,
    pub reason: Option<String>,
    pub created_at_utc: i64,
}

impl NewPauseSession {
    pub fn global(
        id: impl Into<String>,
        starts_at_utc: i64,
        ends_at_utc: Option<i64>,
        created_at_utc: i64,
    ) -> Self {
        Self {
            id: id.into(),
            scope: PauseScope::Global,
            starts_at_utc,
            ends_at_utc,
            reason: None,
            created_at_utc,
        }
    }

    pub fn for_reminder(
        id: impl Into<String>,
        reminder_id: impl Into<String>,
        starts_at_utc: i64,
        ends_at_utc: Option<i64>,
        created_at_utc: i64,
    ) -> Self {
        Self {
            id: id.into(),
            scope: PauseScope::Reminder {
                reminder_id: reminder_id.into(),
            },
            starts_at_utc,
            ends_at_utc,
            reason: None,
            created_at_utc,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PauseSession {
    pub id: String,
    pub scope: PauseScope,
    pub starts_at_utc: i64,
    pub ends_at_utc: Option<i64>,
    pub cancelled_at_utc: Option<i64>,
    pub reason: Option<String>,
    pub created_at_utc: i64,
}

impl NewReminderPolicy {
    pub fn defaults(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            delivery_json: "{}".into(),
            sound_json: "{}".into(),
            snooze_json: "{}".into(),
            missed_json: "{}".into(),
            dnd_json: "{}".into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, FromRow)]
pub struct Occurrence {
    pub id: String,
    pub reminder_id: String,
    pub reminder_revision: i64,
    pub occurrence_key: String,
    pub scheduled_at_utc: i64,
    pub scheduled_local: String,
    pub timezone_id: String,
    pub state: String,
    pub result: Option<String>,
    pub suppression_reason: Option<String>,
    pub deferred_until_utc: Option<i64>,
    pub display_at_utc: Option<i64>,
    pub snooze_due_at_utc: Option<i64>,
    pub snooze_count: i64,
    pub presented_at_utc: Option<i64>,
    pub handled_at_utc: Option<i64>,
    pub merged_into_id: Option<String>,
    pub claim_token: Option<String>,
    pub claimed_at_utc: Option<i64>,
    pub created_at_utc: i64,
    pub updated_at_utc: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, FromRow)]
pub struct OutstandingSurfaceDelivery {
    pub occurrence_id: String,
    pub occurrence_state: String,
    pub payload_json: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NewOccurrence {
    pub id: String,
    pub reminder_id: String,
    pub reminder_revision: i64,
    pub occurrence_key: String,
    pub scheduled_at_utc: i64,
    pub scheduled_local: String,
    pub timezone_id: String,
    pub created_at_utc: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InsertOccurrenceOutcome {
    Inserted(Occurrence),
    Existing(Occurrence),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClaimOutcome {
    Claimed(Occurrence),
    AlreadyClaimed(Occurrence),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OccurrenceDecisionRecord {
    Deliver,
    Defer { until_utc: i64, reason: String },
    Ignore { reason: String },
    Missed { reason: String },
}
