use std::{error::Error, fmt};

use serde_json::Value;
use takefive_persistence_sqlite::{
    Occurrence, OccurrenceRepository, PersistenceError, ReminderRepository,
};

const DEFAULT_MAX_SNOOZE_COUNT: u32 = 3;
const MAX_CONFIGURED_SNOOZE_COUNT: u32 = 10;
const MILLIS_PER_MINUTE: i64 = 60_000;

#[derive(Clone, Debug)]
pub struct OccurrenceActionService {
    occurrences: OccurrenceRepository,
    reminders: ReminderRepository,
}

impl OccurrenceActionService {
    pub fn new(occurrences: OccurrenceRepository, reminders: ReminderRepository) -> Self {
        Self {
            occurrences,
            reminders,
        }
    }

    pub async fn complete(
        &self,
        occurrence_id: &str,
        now_utc: i64,
    ) -> Result<Occurrence, ActionError> {
        let result = self
            .occurrences
            .complete_presented(occurrence_id, now_utc)
            .await;
        self.map_action_result(occurrence_id, result).await
    }

    pub async fn skip(&self, occurrence_id: &str, now_utc: i64) -> Result<Occurrence, ActionError> {
        let result = self
            .occurrences
            .skip_presented(occurrence_id, now_utc)
            .await;
        self.map_action_result(occurrence_id, result).await
    }

    pub async fn mark_unhandled(
        &self,
        occurrence_id: &str,
        now_utc: i64,
    ) -> Result<Occurrence, ActionError> {
        let result = self
            .occurrences
            .mark_unhandled_presented(occurrence_id, now_utc)
            .await;
        self.map_action_result(occurrence_id, result).await
    }

    pub async fn snooze(
        &self,
        occurrence_id: &str,
        delay_minutes: i64,
        now_utc: i64,
    ) -> Result<Occurrence, ActionError> {
        if !(1..=1_440).contains(&delay_minutes) {
            return Err(ActionError::InvalidDelay { delay_minutes });
        }

        let occurrence =
            self.occurrences
                .get(occurrence_id)
                .await?
                .ok_or_else(|| ActionError::NotFound {
                    occurrence_id: occurrence_id.to_owned(),
                })?;
        let max_count = self
            .max_snooze_count_for_reminder(&occurrence.reminder_id)
            .await?;

        if occurrence.snooze_count >= i64::from(max_count) {
            return Err(ActionError::SnoozeLimit {
                occurrence_id: occurrence_id.to_owned(),
                max_count,
            });
        }

        let delay_millis =
            delay_minutes
                .checked_mul(MILLIS_PER_MINUTE)
                .ok_or(ActionError::TimeOverflow {
                    now_utc,
                    delay_minutes,
                })?;
        let due_at_utc = now_utc
            .checked_add(delay_millis)
            .ok_or(ActionError::TimeOverflow {
                now_utc,
                delay_minutes,
            })?;

        let result = self
            .occurrences
            .snooze_presented(occurrence_id, due_at_utc, now_utc)
            .await;
        self.map_action_result(occurrence_id, result).await
    }

    pub async fn list_for_day(
        &self,
        starts_at_utc: i64,
        ends_at_utc: i64,
        limit: u32,
    ) -> Result<Vec<Occurrence>, ActionError> {
        self.occurrences
            .list_for_day(starts_at_utc, ends_at_utc, limit)
            .await
            .map_err(ActionError::from)
    }

    async fn max_snooze_count_for_reminder(&self, reminder_id: &str) -> Result<u32, ActionError> {
        let snooze_json = self
            .reminders
            .list_scheduled_enabled()
            .await?
            .into_iter()
            .find(|reminder| reminder.reminder_id == reminder_id)
            .map(|reminder| reminder.snooze_json);

        match snooze_json {
            Some(json) => parse_max_snooze_count(&json, reminder_id),
            None => Ok(DEFAULT_MAX_SNOOZE_COUNT),
        }
    }

    async fn map_action_result(
        &self,
        occurrence_id: &str,
        result: Result<Occurrence, PersistenceError>,
    ) -> Result<Occurrence, ActionError> {
        match result {
            Ok(occurrence) => Ok(occurrence),
            Err(action_error) => match self.occurrences.get(occurrence_id).await {
                Ok(None) => Err(ActionError::NotFound {
                    occurrence_id: occurrence_id.to_owned(),
                }),
                Ok(Some(_)) => Err(ActionError::Persistence(action_error)),
                Err(lookup_error) => Err(ActionError::Persistence(lookup_error)),
            },
        }
    }
}

fn parse_max_snooze_count(json: &str, reminder_id: &str) -> Result<u32, ActionError> {
    let value: Value = serde_json::from_str(json)
        .map_err(|error| invalid_snooze_policy(reminder_id, format!("invalid JSON: {error}")))?;

    for key in ["max_count", "maxCount", "max_snooze_count"] {
        let Some(configured) = value.get(key) else {
            continue;
        };
        let Some(configured) = configured.as_u64() else {
            return Err(invalid_snooze_policy(
                reminder_id,
                format!("{key} must be an integer between 0 and 10"),
            ));
        };
        if configured > u64::from(MAX_CONFIGURED_SNOOZE_COUNT) {
            return Err(invalid_snooze_policy(
                reminder_id,
                format!("{key} must be between 0 and 10"),
            ));
        }
        return Ok(configured as u32);
    }

    Ok(DEFAULT_MAX_SNOOZE_COUNT)
}

fn invalid_snooze_policy(reminder_id: &str, reason: String) -> ActionError {
    ActionError::Persistence(PersistenceError::InvariantViolation(format!(
        "reminder {reminder_id} has invalid snooze policy: {reason}"
    )))
}

#[derive(Debug)]
pub enum ActionError {
    NotFound {
        occurrence_id: String,
    },
    InvalidDelay {
        delay_minutes: i64,
    },
    SnoozeLimit {
        occurrence_id: String,
        max_count: u32,
    },
    TimeOverflow {
        now_utc: i64,
        delay_minutes: i64,
    },
    Persistence(PersistenceError),
}

impl fmt::Display for ActionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { occurrence_id } => {
                write!(formatter, "occurrence {occurrence_id} does not exist")
            }
            Self::InvalidDelay { delay_minutes } => write!(
                formatter,
                "snooze delay must be between 1 and 1440 minutes; got {delay_minutes}"
            ),
            Self::SnoozeLimit {
                occurrence_id,
                max_count,
            } => write!(
                formatter,
                "occurrence {occurrence_id} reached its snooze limit of {max_count}"
            ),
            Self::TimeOverflow {
                now_utc,
                delay_minutes,
            } => write!(
                formatter,
                "snooze deadline overflows UTC milliseconds for {now_utc} + {delay_minutes} minutes"
            ),
            Self::Persistence(error) => error.fmt(formatter),
        }
    }
}

impl Error for ActionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Persistence(error) => Some(error),
            _ => None,
        }
    }
}

impl From<PersistenceError> for ActionError {
    fn from(error: PersistenceError) -> Self {
        Self::Persistence(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use takefive_persistence_sqlite::{
        NewOccurrence, NewReminder, NewReminderPolicy, NewScheduleRule, OccurrenceDecisionRecord,
        SqliteStore,
    };
    use tempfile::TempDir;

    struct TestDatabase {
        _directory: TempDir,
        store: SqliteStore,
    }

    impl TestDatabase {
        async fn new() -> Self {
            let directory = tempfile::tempdir().expect("create temporary database directory");
            let store = SqliteStore::open(directory.path().join("takefive-test.sqlite3"))
                .await
                .expect("open temporary database");
            Self {
                _directory: directory,
                store,
            }
        }

        fn service(&self) -> OccurrenceActionService {
            OccurrenceActionService::new(
                OccurrenceRepository::new(self.store.clone()),
                ReminderRepository::new(self.store.clone()),
            )
        }
    }

    async fn create_presented(
        store: &SqliteStore,
        reminder_id: &str,
        occurrence_id: &str,
        snooze_json: &str,
        scheduled_at_utc: i64,
    ) {
        let reminder = NewReminder::new(reminder_id, format!("Reminder {reminder_id}"), 100);
        let rule = NewScheduleRule {
            id: format!("rule-{reminder_id}"),
            rule_type: "fixed_times".into(),
            timezone_mode: "named".into(),
            timezone_id: Some("Asia/Shanghai".into()),
            config_json: r#"{"times":["10:00"]}"#.into(),
        };
        let mut policy = NewReminderPolicy::defaults(format!("policy-{reminder_id}"));
        policy.snooze_json = snooze_json.into();
        ReminderRepository::new(store.clone())
            .create_with_configuration(&reminder, Some(&rule), Some(&policy))
            .await
            .unwrap();

        let occurrence = NewOccurrence {
            id: occurrence_id.into(),
            reminder_id: reminder_id.into(),
            reminder_revision: 1,
            occurrence_key: format!("fixed:{occurrence_id}"),
            scheduled_at_utc,
            scheduled_local: "2026-07-15T10:00:00".into(),
            timezone_id: "Asia/Shanghai".into(),
            created_at_utc: 100,
        };
        let repository = OccurrenceRepository::new(store.clone());
        repository
            .create_and_claim(&occurrence, "scheduler-test", 200)
            .await
            .unwrap();
        repository
            .apply_decision(occurrence_id, &OccurrenceDecisionRecord::Deliver, 210)
            .await
            .unwrap();
        repository.mark_presented(occurrence_id, 220).await.unwrap();
    }

    async fn present_after_snooze(
        repository: &OccurrenceRepository,
        occurrence_id: &str,
        due_at_utc: i64,
    ) -> i64 {
        repository
            .apply_decision(
                occurrence_id,
                &OccurrenceDecisionRecord::Deliver,
                due_at_utc,
            )
            .await
            .unwrap();
        let presented_at = due_at_utc + 1;
        repository
            .mark_presented(occurrence_id, presented_at)
            .await
            .unwrap();
        presented_at + 1
    }

    async fn consume_snoozes(
        service: &OccurrenceActionService,
        repository: &OccurrenceRepository,
        occurrence_id: &str,
        count: u32,
    ) -> i64 {
        let mut now = 1_000;
        for expected_count in 1..=count {
            let snoozed = service.snooze(occurrence_id, 1, now).await.unwrap();
            assert_eq!(snoozed.snooze_count, i64::from(expected_count));
            now = present_after_snooze(repository, occurrence_id, now + MILLIS_PER_MINUTE).await;
        }
        now
    }

    #[tokio::test]
    async fn default_policy_allows_three_snoozes_and_rejects_the_fourth() {
        let database = TestDatabase::new().await;
        create_presented(
            &database.store,
            "reminder-default",
            "occurrence-default",
            "{}",
            1_000,
        )
        .await;
        let service = database.service();
        let repository = OccurrenceRepository::new(database.store.clone());

        let now = consume_snoozes(&service, &repository, "occurrence-default", 3).await;
        let error = service
            .snooze("occurrence-default", 1, now)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            ActionError::SnoozeLimit { max_count: 3, .. }
        ));
        let stored = repository.get("occurrence-default").await.unwrap().unwrap();
        assert_eq!(stored.state, "presented");
        assert_eq!(stored.snooze_count, 3);
    }

    #[tokio::test]
    async fn configured_zero_and_ten_snooze_limits_are_enforced() {
        let database = TestDatabase::new().await;
        create_presented(
            &database.store,
            "reminder-zero",
            "occurrence-zero",
            r#"{"maxCount":0}"#,
            1_000,
        )
        .await;
        create_presented(
            &database.store,
            "reminder-ten",
            "occurrence-ten",
            r#"{"max_snooze_count":10}"#,
            2_000,
        )
        .await;
        let service = database.service();
        let repository = OccurrenceRepository::new(database.store.clone());

        assert!(matches!(
            service.snooze("occurrence-zero", 1, 1_000).await,
            Err(ActionError::SnoozeLimit { max_count: 0, .. })
        ));

        let now = consume_snoozes(&service, &repository, "occurrence-ten", 10).await;
        assert!(matches!(
            service.snooze("occurrence-ten", 1, now).await,
            Err(ActionError::SnoozeLimit { max_count: 10, .. })
        ));
    }

    #[tokio::test]
    async fn snake_case_snooze_limit_alias_is_recognized() {
        let database = TestDatabase::new().await;
        create_presented(
            &database.store,
            "reminder-snake",
            "occurrence-snake",
            r#"{"max_count":1}"#,
            1_000,
        )
        .await;
        let service = database.service();
        let repository = OccurrenceRepository::new(database.store.clone());

        let now = consume_snoozes(&service, &repository, "occurrence-snake", 1).await;
        assert!(matches!(
            service.snooze("occurrence-snake", 1, now).await,
            Err(ActionError::SnoozeLimit { max_count: 1, .. })
        ));
    }

    #[tokio::test]
    async fn illegal_or_overflowing_delay_does_not_mutate_occurrence() {
        let database = TestDatabase::new().await;
        create_presented(
            &database.store,
            "reminder-delay",
            "occurrence-delay",
            "{}",
            1_000,
        )
        .await;
        let service = database.service();

        for delay_minutes in [i64::MIN, -1, 0, 1_441, i64::MAX] {
            assert!(matches!(
                service
                    .snooze("occurrence-delay", delay_minutes, 1_000)
                    .await,
                Err(ActionError::InvalidDelay { .. })
            ));
        }
        assert!(matches!(
            service.snooze("occurrence-delay", 1, i64::MAX - 1).await,
            Err(ActionError::TimeOverflow { .. })
        ));

        let stored = OccurrenceRepository::new(database.store)
            .get("occurrence-delay")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.state, "presented");
        assert_eq!(stored.snooze_count, 0);
    }

    #[tokio::test]
    async fn complete_skip_and_unhandled_actions_persist_terminal_results() {
        let database = TestDatabase::new().await;
        for (reminder_id, occurrence_id, scheduled_at) in [
            ("reminder-complete", "occurrence-complete", 1_000),
            ("reminder-skip", "occurrence-skip", 2_000),
            ("reminder-unhandled", "occurrence-unhandled", 3_000),
        ] {
            create_presented(
                &database.store,
                reminder_id,
                occurrence_id,
                "{}",
                scheduled_at,
            )
            .await;
        }
        let service = database.service();

        let completed = service
            .complete("occurrence-complete", 10_000)
            .await
            .unwrap();
        assert_eq!(completed.state, "completed");
        assert_eq!(completed.result.as_deref(), Some("completed"));

        let skipped = service.skip("occurrence-skip", 10_001).await.unwrap();
        assert_eq!(skipped.state, "skipped");
        assert_eq!(skipped.result.as_deref(), Some("skipped"));

        let unhandled = service
            .mark_unhandled("occurrence-unhandled", 10_002)
            .await
            .unwrap();
        assert_eq!(unhandled.state, "unhandled");
        assert_eq!(unhandled.result.as_deref(), Some("unhandled"));
        assert_eq!(unhandled.suppression_reason.as_deref(), Some("timed_out"));

        assert!(matches!(
            service.complete("missing", 10_003).await,
            Err(ActionError::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn list_for_day_is_a_thin_bounded_repository_query() {
        let database = TestDatabase::new().await;
        create_presented(
            &database.store,
            "reminder-day-earlier",
            "occurrence-day-earlier",
            "{}",
            1_000,
        )
        .await;
        create_presented(
            &database.store,
            "reminder-day-later",
            "occurrence-day-later",
            "{}",
            2_000,
        )
        .await;
        create_presented(
            &database.store,
            "reminder-outside",
            "occurrence-outside",
            "{}",
            4_000,
        )
        .await;
        let service = database.service();

        let values = service.list_for_day(500, 3_000, 10).await.unwrap();
        assert_eq!(
            values
                .iter()
                .map(|occurrence| occurrence.id.as_str())
                .collect::<Vec<_>>(),
            ["occurrence-day-later", "occurrence-day-earlier"]
        );
        assert!(matches!(
            service.list_for_day(500, 3_000, 0).await,
            Err(ActionError::Persistence(
                PersistenceError::InvariantViolation(_)
            ))
        ));
    }
}
