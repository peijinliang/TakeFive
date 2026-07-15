use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RuleRevision(u64);

impl RuleRevision {
    pub const INITIAL: Self = Self(1);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

impl Default for RuleRevision {
    fn default() -> Self {
        Self::INITIAL
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Importance {
    #[default]
    Normal,
    Important,
}

/// User-owned reminder metadata. Schedule configuration is versioned and stored
/// separately so historical occurrences can retain the exact rule revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderDefinition {
    pub id: String,
    pub name: String,
    pub content: Option<String>,
    pub category: Option<String>,
    pub importance: Importance,
    pub enabled: bool,
    pub revision: RuleRevision,
    pub created_at_utc: DateTime<Utc>,
    pub updated_at_utc: DateTime<Utc>,
    pub deleted_at_utc: Option<DateTime<Utc>>,
}

impl ReminderDefinition {
    pub fn new(id: impl Into<String>, name: impl Into<String>, now: DateTime<Utc>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            content: None,
            category: None,
            importance: Importance::Normal,
            enabled: true,
            revision: RuleRevision::INITIAL,
            created_at_utc: now,
            updated_at_utc: now,
            deleted_at_utc: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn reminder_definition_has_stable_persistence_shape() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 10, 0, 0).unwrap();
        let mut reminder = ReminderDefinition::new("drink", "Drink water", now);
        reminder.importance = Importance::Important;

        let json = serde_json::to_string(&reminder).unwrap();
        let decoded: ReminderDefinition = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, reminder);
        assert!(json.contains("\"importance\":\"important\""));
        assert_eq!(reminder.revision.next(), Some(RuleRevision::new(2)));
    }
}
