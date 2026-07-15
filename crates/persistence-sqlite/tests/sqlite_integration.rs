use std::sync::Arc;

use sqlx::Row;
use takefive_persistence_sqlite::{
    ClaimOutcome, InsertOccurrenceOutcome, NewOccurrence, NewPauseSession, NewReminder,
    NewReminderBundle, NewReminderPolicy, NewScheduleRule, OccurrenceDecisionRecord,
    OccurrenceRepository, PauseRepository, PauseScope, PersistenceError, ReminderChanges,
    ReminderRepository, ScheduleRuleChanges, SqliteStore,
};
use tempfile::TempDir;
use tokio::sync::Barrier;

struct TestDatabase {
    _directory: TempDir,
    store: SqliteStore,
}

impl TestDatabase {
    async fn new() -> Self {
        let directory = tempfile::tempdir().expect("create temporary database directory");
        let database_path = directory.path().join("takefive-test.sqlite3");
        let store = SqliteStore::open(database_path)
            .await
            .expect("open temporary database");
        Self {
            _directory: directory,
            store,
        }
    }
}

fn new_occurrence(id: &str, reminder_id: &str, occurrence_key: &str) -> NewOccurrence {
    NewOccurrence {
        id: id.into(),
        reminder_id: reminder_id.into(),
        reminder_revision: 1,
        occurrence_key: occurrence_key.into(),
        scheduled_at_utc: 1_721_000_000_000,
        scheduled_local: "2026-07-14T10:00:00".into(),
        timezone_id: "Asia/Shanghai".into(),
        created_at_utc: 1_721_000_000_000,
    }
}

async fn create_configured_reminder(
    repository: &ReminderRepository,
    id: &str,
    enabled: bool,
    now_utc: i64,
) {
    let mut reminder = NewReminder::new(id, format!("Reminder {id}"), now_utc);
    reminder.enabled = enabled;
    let rule = NewScheduleRule {
        id: format!("rule-{id}"),
        rule_type: "fixed_times".into(),
        timezone_mode: "named".into(),
        timezone_id: Some("Asia/Shanghai".into()),
        config_json: r#"{"times":["10:00"]}"#.into(),
    };
    let policy = NewReminderPolicy::defaults(format!("policy-{id}"));
    repository
        .create_with_configuration(&reminder, Some(&rule), Some(&policy))
        .await
        .unwrap();
}

async fn create_presented_occurrence(
    store: &SqliteStore,
    reminder_id: &str,
    occurrence_id: &str,
) -> OccurrenceRepository {
    ReminderRepository::new(store.clone())
        .create(&NewReminder::new(reminder_id, "Action test", 100))
        .await
        .unwrap();
    let repository = OccurrenceRepository::new(store.clone());
    repository
        .create_and_claim(
            &new_occurrence(occurrence_id, reminder_id, "fixed:action-test"),
            "scheduler-action-test",
            200,
        )
        .await
        .unwrap();
    repository
        .apply_decision(occurrence_id, &OccurrenceDecisionRecord::Deliver, 210)
        .await
        .unwrap();
    repository.mark_presented(occurrence_id, 220).await.unwrap();
    repository
}

#[tokio::test]
async fn migrates_schema_and_initializes_connection_pragmas() {
    let database = TestDatabase::new().await;
    assert_eq!(database.store.schema_version().await.unwrap(), 2);
    assert!(database.store.quick_check().await.unwrap());

    let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(database.store.pool())
        .await
        .unwrap();
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(database.store.pool())
        .await
        .unwrap();
    let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
        .fetch_one(database.store.pool())
        .await
        .unwrap();

    assert_eq!(journal_mode, "wal");
    assert_eq!(foreign_keys, 1);
    assert_eq!(busy_timeout, 5_000);

    let required_tables = [
        "reminders",
        "schedule_rules",
        "reminder_policies",
        "occurrences",
        "delivery_attempts",
        "pause_sessions",
        "settings",
        "schema_meta",
    ];
    for table in required_tables {
        let exists: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
        )
        .bind(table)
        .fetch_one(database.store.pool())
        .await
        .unwrap();
        assert_eq!(exists, 1, "missing table {table}");
    }
}

#[tokio::test]
async fn occurrence_identity_is_unique_and_returns_original_event() {
    let database = TestDatabase::new().await;
    let reminders = ReminderRepository::new(database.store.clone());
    let occurrences = OccurrenceRepository::new(database.store.clone());
    reminders
        .create(&NewReminder::new("reminder-1", "Drink water", 100))
        .await
        .unwrap();

    let first = new_occurrence("occurrence-1", "reminder-1", "2026-07-14T10:00+08:00#0");
    let duplicate = new_occurrence("occurrence-2", "reminder-1", "2026-07-14T10:00+08:00#0");

    assert!(matches!(
        occurrences.insert(&first).await.unwrap(),
        InsertOccurrenceOutcome::Inserted(_)
    ));
    match occurrences.insert(&duplicate).await.unwrap() {
        InsertOccurrenceOutcome::Existing(existing) => assert_eq!(existing.id, first.id),
        InsertOccurrenceOutcome::Inserted(_) => panic!("duplicate occurrence was inserted"),
    }

    assert_eq!(
        occurrences
            .list_for_reminder("reminder-1")
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_transactional_claim_succeeds_exactly_once() {
    let database = TestDatabase::new().await;
    ReminderRepository::new(database.store.clone())
        .create(&NewReminder::new("reminder-1", "Stand up", 100))
        .await
        .unwrap();

    let repository = OccurrenceRepository::new(database.store.clone());
    let occurrence = new_occurrence("occurrence-1", "reminder-1", "fixed:2026-07-14:10:00:0");
    let barrier = Arc::new(Barrier::new(3));

    let mut tasks = Vec::new();
    for token in ["scheduler-a", "scheduler-b"] {
        let repository = repository.clone();
        let occurrence = occurrence.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            repository
                .create_and_claim(&occurrence, token, 200)
                .await
                .unwrap()
        }));
    }

    barrier.wait().await;
    let first = tasks.remove(0).await.unwrap();
    let second = tasks.remove(0).await.unwrap();
    let claimed_count = [&first, &second]
        .into_iter()
        .filter(|outcome| matches!(outcome, ClaimOutcome::Claimed(_)))
        .count();
    let duplicate_count = [&first, &second]
        .into_iter()
        .filter(|outcome| matches!(outcome, ClaimOutcome::AlreadyClaimed(_)))
        .count();

    assert_eq!(claimed_count, 1);
    assert_eq!(duplicate_count, 1);
    let stored = repository.get("occurrence-1").await.unwrap().unwrap();
    assert_eq!(stored.state, "claimed");
    assert!(matches!(
        stored.claim_token.as_deref(),
        Some("scheduler-a" | "scheduler-b")
    ));
    let recoverable = repository.list_recoverable_due(200).await.unwrap();
    assert_eq!(recoverable.len(), 1);
    assert_eq!(recoverable[0].id, "occurrence-1");
}

#[tokio::test]
async fn failed_configuration_write_rolls_back_the_reminder() {
    let database = TestDatabase::new().await;
    let repository = ReminderRepository::new(database.store.clone());
    let reminder = NewReminder::new("reminder-rollback", "Rollback test", 100);
    let invalid_rule = NewScheduleRule {
        id: "rule-invalid".into(),
        rule_type: "fixed_times".into(),
        timezone_mode: "named".into(),
        timezone_id: Some("Asia/Shanghai".into()),
        config_json: "not-json".into(),
    };

    let result = repository
        .create_with_configuration(
            &reminder,
            Some(&invalid_rule),
            Some(&NewReminderPolicy::defaults("policy-rollback")),
        )
        .await;

    assert!(result.is_err());
    assert_eq!(repository.get(&reminder.id).await.unwrap(), None);
}

#[tokio::test]
async fn configuration_update_changes_reminder_and_rule_atomically() {
    let database = TestDatabase::new().await;
    let repository = ReminderRepository::new(database.store.clone());
    create_configured_reminder(&repository, "reminder-update", true, 100).await;

    let updated = repository
        .update_with_configuration(
            "reminder-update",
            1,
            &ReminderChanges {
                title: "Updated reminder".into(),
                description: "New description".into(),
                enabled: true,
                updated_at_utc: 200,
            },
            &ScheduleRuleChanges {
                rule_type: "fixed_times".into(),
                timezone_mode: "named".into(),
                timezone_id: Some("Asia/Shanghai".into()),
                config_json: r#"{"times":["11:30"]}"#.into(),
            },
        )
        .await
        .unwrap();

    assert_eq!(updated.title, "Updated reminder");
    assert_eq!(updated.revision, 2);
    let configured = repository.list_configured().await.unwrap();
    assert_eq!(configured[0].title, "Updated reminder");
    assert_eq!(configured[0].rule_config_json, r#"{"times":["11:30"]}"#);

    let conflict = repository
        .update_with_configuration(
            "reminder-update",
            1,
            &ReminderChanges {
                title: "Should not win".into(),
                description: String::new(),
                enabled: false,
                updated_at_utc: 300,
            },
            &ScheduleRuleChanges {
                rule_type: "fixed_times".into(),
                timezone_mode: "named".into(),
                timezone_id: Some("Asia/Shanghai".into()),
                config_json: r#"{"times":["12:00"]}"#.into(),
            },
        )
        .await;
    assert!(matches!(
        conflict,
        Err(PersistenceError::RevisionConflict { .. })
    ));
    assert_eq!(
        repository
            .get("reminder-update")
            .await
            .unwrap()
            .unwrap()
            .revision,
        2
    );
}

#[tokio::test]
async fn batch_configuration_write_is_atomic() {
    let database = TestDatabase::new().await;
    let repository = ReminderRepository::new(database.store.clone());
    let bundle = |id: &str, config_json: &str| NewReminderBundle {
        reminder: NewReminder::new(id, format!("Reminder {id}"), 100),
        rule: Some(NewScheduleRule {
            id: format!("rule-{id}"),
            rule_type: "fixed_times".into(),
            timezone_mode: "named".into(),
            timezone_id: Some("Asia/Shanghai".into()),
            config_json: config_json.into(),
        }),
        policy: Some(NewReminderPolicy::defaults(format!("policy-{id}"))),
    };

    let result = repository
        .create_many_with_configuration(&[
            bundle("batch-valid", r#"{"times":["10:00"]}"#),
            bundle("batch-invalid", "not-json"),
        ])
        .await;

    assert!(result.is_err());
    assert!(repository.list(true).await.unwrap().is_empty());
}

#[tokio::test]
async fn soft_deleting_reminder_preserves_occurrence_history() {
    let database = TestDatabase::new().await;
    let reminders = ReminderRepository::new(database.store.clone());
    let occurrences = OccurrenceRepository::new(database.store.clone());
    reminders
        .create(&NewReminder::new("reminder-1", "Eye break", 100))
        .await
        .unwrap();
    occurrences
        .insert(&new_occurrence(
            "occurrence-1",
            "reminder-1",
            "fixed:2026-07-14:10:00:0",
        ))
        .await
        .unwrap();

    assert!(reminders.soft_delete("reminder-1", 300).await.unwrap());

    let deleted = reminders.get("reminder-1").await.unwrap().unwrap();
    assert_eq!(deleted.deleted_at_utc, Some(300));
    assert!(!deleted.enabled);
    assert_eq!(reminders.list(false).await.unwrap(), Vec::new());
    assert_eq!(
        occurrences
            .list_for_reminder("reminder-1")
            .await
            .unwrap()
            .len(),
        1
    );

    let foreign_key = sqlx::query("PRAGMA foreign_key_check")
        .fetch_optional(database.store.pool())
        .await
        .unwrap();
    assert!(foreign_key.is_none());
}

#[tokio::test]
async fn migrations_are_idempotent_across_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("reopen.sqlite3");
    let first = SqliteStore::open(&database_path).await.unwrap();
    ReminderRepository::new(first.clone())
        .create(&NewReminder::new("reminder-1", "Persist me", 100))
        .await
        .unwrap();
    first.close().await;

    let reopened = SqliteStore::open(&database_path).await.unwrap();
    assert_eq!(reopened.schema_version().await.unwrap(), 2);
    assert!(ReminderRepository::new(reopened)
        .get("reminder-1")
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn core_table_columns_are_strictly_typed() {
    let database = TestDatabase::new().await;
    let row = sqlx::query("PRAGMA table_list('occurrences')")
        .fetch_one(database.store.pool())
        .await
        .unwrap();
    let strict: i64 = row.try_get("strict").unwrap();
    assert_eq!(strict, 1);
}

#[tokio::test]
async fn scheduler_query_excludes_disabled_and_soft_deleted_reminders() {
    let database = TestDatabase::new().await;
    let repository = ReminderRepository::new(database.store.clone());
    create_configured_reminder(&repository, "active", true, 100).await;
    create_configured_reminder(&repository, "disabled", false, 101).await;
    create_configured_reminder(&repository, "deleted", true, 102).await;
    repository.soft_delete("deleted", 200).await.unwrap();

    let scheduled = repository.list_scheduled_enabled().await.unwrap();

    assert_eq!(scheduled.len(), 1);
    assert_eq!(scheduled[0].reminder_id, "active");
    assert_eq!(scheduled[0].rule_type, "fixed_times");
    assert_eq!(scheduled[0].timezone_id.as_deref(), Some("Asia/Shanghai"));
    assert_eq!(scheduled[0].rule_config_json, r#"{"times":["10:00"]}"#);
    assert_eq!(scheduled[0].delivery_json, "{}");
    assert_eq!(
        repository
            .list_configured()
            .await
            .unwrap()
            .into_iter()
            .map(|record| record.reminder_id)
            .collect::<Vec<_>>(),
        ["active", "disabled"]
    );

    repository
        .update(
            "active",
            1,
            &ReminderChanges {
                title: "Disabled later".into(),
                description: String::new(),
                enabled: false,
                updated_at_utc: 300,
            },
        )
        .await
        .unwrap();
    assert!(repository
        .list_scheduled_enabled()
        .await
        .unwrap()
        .is_empty());
    assert_eq!(repository.list_configured().await.unwrap().len(), 2);
}

#[tokio::test]
async fn pause_active_window_uses_an_exclusive_end_boundary_and_honors_cancellation() {
    let database = TestDatabase::new().await;
    let repository = PauseRepository::new(database.store.clone());
    let mut pause = NewPauseSession::global("pause-global", 100, Some(200), 90);
    pause.reason = Some("meeting".into());
    let created = repository.create(&pause).await.unwrap();
    assert_eq!(created.scope, PauseScope::Global);

    assert!(repository.list_active(99).await.unwrap().is_empty());
    assert_eq!(repository.list_active(100).await.unwrap().len(), 1);
    assert_eq!(repository.list_active(199).await.unwrap().len(), 1);
    assert!(repository.list_active(200).await.unwrap().is_empty());

    assert!(repository.cancel("pause-global", 150).await.unwrap());
    assert!(!repository.cancel("pause-global", 151).await.unwrap());
    assert!(repository.list_active(150).await.unwrap().is_empty());
}

#[tokio::test]
async fn global_and_reminder_pauses_can_coexist_and_are_scoped_correctly() {
    let database = TestDatabase::new().await;
    let reminders = ReminderRepository::new(database.store.clone());
    reminders
        .create(&NewReminder::new("reminder-a", "A", 10))
        .await
        .unwrap();
    reminders
        .create(&NewReminder::new("reminder-b", "B", 11))
        .await
        .unwrap();
    let pauses = PauseRepository::new(database.store.clone());
    pauses
        .create(&NewPauseSession::global("global", 100, None, 90))
        .await
        .unwrap();
    pauses
        .create(&NewPauseSession::for_reminder(
            "single-a",
            "reminder-a",
            100,
            Some(300),
            91,
        ))
        .await
        .unwrap();

    assert_eq!(pauses.list_active_global(150).await.unwrap().len(), 1);
    assert_eq!(
        pauses
            .list_active_for_reminder("reminder-a", 150)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(pauses
        .list_active_for_reminder("reminder-b", 150)
        .await
        .unwrap()
        .is_empty());

    let effective_a = pauses
        .list_effective_for_reminder("reminder-a", 150)
        .await
        .unwrap();
    assert_eq!(effective_a.len(), 2);
    assert_eq!(effective_a[0].scope, PauseScope::Global);
    assert!(matches!(
        effective_a[1].scope,
        PauseScope::Reminder { ref reminder_id } if reminder_id == "reminder-a"
    ));
    assert_eq!(
        pauses
            .list_effective_for_reminder("reminder-b", 150)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn scheduler_decision_and_delivery_result_are_persisted() {
    let database = TestDatabase::new().await;
    ReminderRepository::new(database.store.clone())
        .create(&NewReminder::new("reminder-1", "Hydrate", 100))
        .await
        .unwrap();
    let repository = OccurrenceRepository::new(database.store.clone());
    let occurrence = new_occurrence("occurrence-1", "reminder-1", "fixed:10:00");
    let claimed = repository
        .create_and_claim(&occurrence, "scheduler-a", 200)
        .await
        .unwrap();
    assert!(matches!(claimed, ClaimOutcome::Claimed(_)));

    let delivering = repository
        .apply_decision("occurrence-1", &OccurrenceDecisionRecord::Deliver, 210)
        .await
        .unwrap();
    assert_eq!(delivering.state, "delivering");

    let presented = repository
        .mark_presented("occurrence-1", 220)
        .await
        .unwrap();
    assert_eq!(presented.state, "presented");
    assert_eq!(presented.presented_at_utc, Some(220));
}

#[tokio::test]
async fn accepted_surface_deliveries_are_recoverable_until_the_occurrence_is_handled() {
    let database = TestDatabase::new().await;
    ReminderRepository::new(database.store.clone())
        .create(&NewReminder::new("reminder-surface", "Hydrate", 100))
        .await
        .unwrap();
    let repository = OccurrenceRepository::new(database.store.clone());
    repository
        .create_and_claim(
            &new_occurrence("occurrence-surface", "reminder-surface", "fixed:surface"),
            "scheduler-surface",
            200,
        )
        .await
        .unwrap();
    repository
        .apply_decision(
            "occurrence-surface",
            &OccurrenceDecisionRecord::Deliver,
            210,
        )
        .await
        .unwrap();

    assert_eq!(
        repository
            .list_unattempted_deliveries()
            .await
            .unwrap()
            .len(),
        1
    );
    let payload = r#"{"title":"Hydrate","body":"Drink","occurrenceId":"occurrence-surface","scheduledAt":"2026-07-14T08:00:00Z"}"#;
    repository
        .record_surface_delivery_accepted("surface-attempt-1", "occurrence-surface", payload, 220)
        .await
        .unwrap();

    assert!(repository
        .list_unattempted_deliveries()
        .await
        .unwrap()
        .is_empty());
    let delivering = repository
        .list_outstanding_surface_deliveries()
        .await
        .unwrap();
    assert_eq!(delivering.len(), 1);
    assert_eq!(delivering[0].occurrence_id, "occurrence-surface");
    assert_eq!(delivering[0].occurrence_state, "delivering");
    assert_eq!(delivering[0].payload_json, payload);

    repository
        .mark_presented("occurrence-surface", 230)
        .await
        .unwrap();
    assert_eq!(
        repository
            .list_outstanding_surface_deliveries()
            .await
            .unwrap()[0]
            .occurrence_state,
        "presented"
    );

    repository
        .complete_presented("occurrence-surface", 240)
        .await
        .unwrap();
    assert!(repository
        .list_outstanding_surface_deliveries()
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn failed_surface_attempt_is_not_restored() {
    let database = TestDatabase::new().await;
    ReminderRepository::new(database.store.clone())
        .create(&NewReminder::new("reminder-surface-failed", "Hydrate", 100))
        .await
        .unwrap();
    let repository = OccurrenceRepository::new(database.store.clone());
    repository
        .create_and_claim(
            &new_occurrence(
                "occurrence-surface-failed",
                "reminder-surface-failed",
                "fixed:surface-failed",
            ),
            "scheduler-surface-failed",
            200,
        )
        .await
        .unwrap();
    repository
        .apply_decision(
            "occurrence-surface-failed",
            &OccurrenceDecisionRecord::Deliver,
            210,
        )
        .await
        .unwrap();
    repository
        .record_surface_delivery_accepted(
            "surface-attempt-failed",
            "occurrence-surface-failed",
            r#"{"occurrenceId":"occurrence-surface-failed"}"#,
            220,
        )
        .await
        .unwrap();
    repository
        .mark_surface_delivery_failed("surface-attempt-failed", "window_unavailable", 230)
        .await
        .unwrap();

    assert!(repository
        .list_outstanding_surface_deliveries()
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn deferred_occurrence_becomes_due_without_losing_reason() {
    let database = TestDatabase::new().await;
    ReminderRepository::new(database.store.clone())
        .create(&NewReminder::new("reminder-1", "Stand", 100))
        .await
        .unwrap();
    let repository = OccurrenceRepository::new(database.store.clone());
    let occurrence = new_occurrence("occurrence-1", "reminder-1", "fixed:11:00");
    repository
        .create_and_claim(&occurrence, "scheduler-a", 200)
        .await
        .unwrap();
    repository
        .apply_decision(
            "occurrence-1",
            &OccurrenceDecisionRecord::Defer {
                until_utc: 500,
                reason: "global_paused".to_string(),
            },
            210,
        )
        .await
        .unwrap();

    assert!(repository.list_due_deferred(499).await.unwrap().is_empty());
    assert_eq!(
        repository.next_deferred_due_at(499).await.unwrap(),
        Some(500)
    );
    assert_eq!(repository.next_deferred_due_at(500).await.unwrap(), None);
    let due = repository.list_due_deferred(500).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].suppression_reason.as_deref(), Some("global_paused"));
    assert_eq!(due[0].deferred_until_utc, Some(500));
}

#[tokio::test]
async fn presented_occurrences_can_be_completed_or_skipped_exactly_once() {
    let completed_database = TestDatabase::new().await;
    let completed_repository = create_presented_occurrence(
        &completed_database.store,
        "reminder-complete",
        "occurrence-complete",
    )
    .await;

    let completed = completed_repository
        .complete_presented("occurrence-complete", 300)
        .await
        .unwrap();
    assert_eq!(completed.state, "completed");
    assert_eq!(completed.result.as_deref(), Some("completed"));
    assert_eq!(completed.handled_at_utc, Some(300));

    let repeated = completed_repository
        .complete_presented("occurrence-complete", 301)
        .await;
    assert!(matches!(
        repeated,
        Err(PersistenceError::InvariantViolation(message))
            if message.contains("state completed") && message.contains("expected presented")
    ));

    let skipped_database = TestDatabase::new().await;
    let skipped_repository =
        create_presented_occurrence(&skipped_database.store, "reminder-skip", "occurrence-skip")
            .await;
    let skipped = skipped_repository
        .skip_presented("occurrence-skip", 400)
        .await
        .unwrap();
    assert_eq!(skipped.state, "skipped");
    assert_eq!(skipped.result.as_deref(), Some("skipped"));
    assert_eq!(skipped.handled_at_utc, Some(400));

    let unhandled_database = TestDatabase::new().await;
    let unhandled_repository = create_presented_occurrence(
        &unhandled_database.store,
        "reminder-unhandled",
        "occurrence-unhandled",
    )
    .await;
    let unhandled = unhandled_repository
        .mark_unhandled_presented("occurrence-unhandled", 500)
        .await
        .unwrap();
    assert_eq!(unhandled.state, "unhandled");
    assert_eq!(unhandled.result.as_deref(), Some("unhandled"));
    assert_eq!(unhandled.suppression_reason.as_deref(), Some("timed_out"));
    assert_eq!(unhandled.handled_at_utc, Some(500));
}

#[tokio::test]
async fn snoozed_occurrence_can_be_redelivered_and_snoozed_again_without_identity_drift() {
    let database = TestDatabase::new().await;
    let repository =
        create_presented_occurrence(&database.store, "reminder-snooze", "occurrence-snooze").await;
    let original = repository.get("occurrence-snooze").await.unwrap().unwrap();

    let first = repository
        .snooze_presented("occurrence-snooze", 500, 300)
        .await
        .unwrap();
    assert_eq!(first.state, "snoozed");
    assert_eq!(first.snooze_due_at_utc, Some(500));
    assert_eq!(first.snooze_count, 1);
    assert_eq!(first.handled_at_utc, None);
    assert!(repository.list_due_deferred(499).await.unwrap().is_empty());
    assert_eq!(repository.list_due_deferred(500).await.unwrap().len(), 1);

    repository
        .apply_decision("occurrence-snooze", &OccurrenceDecisionRecord::Deliver, 500)
        .await
        .unwrap();
    repository
        .mark_presented("occurrence-snooze", 510)
        .await
        .unwrap();
    let second = repository
        .snooze_presented("occurrence-snooze", 700, 520)
        .await
        .unwrap();

    assert_eq!(second.state, "snoozed");
    assert_eq!(second.snooze_due_at_utc, Some(700));
    assert_eq!(second.snooze_count, 2);
    assert_eq!(second.occurrence_key, original.occurrence_key);
    assert_eq!(second.scheduled_at_utc, original.scheduled_at_utc);
    assert_eq!(second.scheduled_local, original.scheduled_local);
}

#[tokio::test]
async fn illegal_actions_return_invariant_errors_without_mutating_the_occurrence() {
    let database = TestDatabase::new().await;
    ReminderRepository::new(database.store.clone())
        .create(&NewReminder::new("reminder-illegal", "Illegal action", 100))
        .await
        .unwrap();
    let repository = OccurrenceRepository::new(database.store.clone());
    repository
        .insert(&new_occurrence(
            "occurrence-illegal",
            "reminder-illegal",
            "fixed:illegal",
        ))
        .await
        .unwrap();
    let pending = repository.get("occurrence-illegal").await.unwrap().unwrap();

    let completion = repository
        .complete_presented("occurrence-illegal", 300)
        .await;
    assert!(matches!(
        completion,
        Err(PersistenceError::InvariantViolation(message)) if message.contains("state pending")
    ));
    assert_eq!(
        repository.get("occurrence-illegal").await.unwrap().unwrap(),
        pending
    );

    let presented_repository = create_presented_occurrence(
        &database.store,
        "reminder-invalid-due",
        "occurrence-invalid-due",
    )
    .await;
    let presented = presented_repository
        .get("occurrence-invalid-due")
        .await
        .unwrap()
        .unwrap();
    let invalid_due = presented_repository
        .snooze_presented("occurrence-invalid-due", 300, 300)
        .await;
    assert!(matches!(
        invalid_due,
        Err(PersistenceError::InvariantViolation(message))
            if message.contains("deadline must be after")
    ));
    assert_eq!(
        presented_repository
            .get("occurrence-invalid-due")
            .await
            .unwrap()
            .unwrap(),
        presented
    );
}

#[tokio::test]
async fn terminal_occurrence_cannot_return_to_an_actionable_state() {
    let database = TestDatabase::new().await;
    let repository =
        create_presented_occurrence(&database.store, "reminder-terminal", "occurrence-terminal")
            .await;
    let terminal = repository
        .complete_presented("occurrence-terminal", 300)
        .await
        .unwrap();

    assert!(repository
        .skip_presented("occurrence-terminal", 301)
        .await
        .is_err());
    assert!(repository
        .snooze_presented("occurrence-terminal", 500, 302)
        .await
        .is_err());
    assert!(repository
        .mark_unhandled_presented("occurrence-terminal", 303)
        .await
        .is_err());
    assert!(repository
        .apply_decision(
            "occurrence-terminal",
            &OccurrenceDecisionRecord::Deliver,
            304,
        )
        .await
        .is_err());
    assert!(repository
        .mark_presented("occurrence-terminal", 305)
        .await
        .is_err());
    assert_eq!(
        repository
            .get("occurrence-terminal")
            .await
            .unwrap()
            .unwrap(),
        terminal
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_presented_actions_have_exactly_one_winner() {
    let database = TestDatabase::new().await;
    let repository =
        create_presented_occurrence(&database.store, "reminder-race", "occurrence-race").await;
    let barrier = Arc::new(Barrier::new(3));

    let complete_task = {
        let repository = repository.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            repository.complete_presented("occurrence-race", 300).await
        })
    };
    let skip_task = {
        let repository = repository.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            repository.skip_presented("occurrence-race", 300).await
        })
    };

    barrier.wait().await;
    let outcomes = [complete_task.await.unwrap(), skip_task.await.unwrap()];
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    assert_eq!(
        outcomes.iter().filter(|outcome| outcome.is_err()).count(),
        1
    );
    assert!(outcomes.iter().filter_map(|outcome| outcome.as_ref().err()).all(
        |error| matches!(error, PersistenceError::InvariantViolation(message) if message.contains("expected presented"))
    ));

    let stored = repository.get("occurrence-race").await.unwrap().unwrap();
    assert!(matches!(stored.state.as_str(), "completed" | "skipped"));
    assert_eq!(stored.result.as_deref(), Some(stored.state.as_str()));
}

#[tokio::test]
async fn daily_timeline_is_bounded_sorted_and_keeps_soft_deleted_reminder_history() {
    let database = TestDatabase::new().await;
    let reminders = ReminderRepository::new(database.store.clone());
    reminders
        .create(&NewReminder::new("reminder-history", "History", 100))
        .await
        .unwrap();
    let repository = OccurrenceRepository::new(database.store.clone());

    let mut earlier = new_occurrence("occurrence-earlier", "reminder-history", "fixed:earlier");
    earlier.scheduled_at_utc = 1_000;
    earlier.created_at_utc = 900;
    repository.insert(&earlier).await.unwrap();

    let mut displayed = new_occurrence("occurrence-displayed", "reminder-history", "fixed:shown");
    displayed.scheduled_at_utc = 1_100;
    displayed.created_at_utc = 900;
    repository
        .create_and_claim(&displayed, "timeline-scheduler", 1_200)
        .await
        .unwrap();
    repository
        .apply_decision(
            "occurrence-displayed",
            &OccurrenceDecisionRecord::Deliver,
            1_300,
        )
        .await
        .unwrap();
    repository
        .mark_presented("occurrence-displayed", 1_800)
        .await
        .unwrap();
    repository
        .complete_presented("occurrence-displayed", 1_810)
        .await
        .unwrap();
    reminders
        .soft_delete("reminder-history", 2_000)
        .await
        .unwrap();

    let timeline = repository.list_for_day(900, 1_900, 10).await.unwrap();
    assert_eq!(timeline.len(), 2);
    assert_eq!(timeline[0].id, "occurrence-displayed");
    assert_eq!(timeline[1].id, "occurrence-earlier");
    assert_eq!(
        repository.list_for_day(900, 1_900, 1).await.unwrap().len(),
        1
    );
    assert!(matches!(
        repository.list_for_day(900, 1_900, 0).await,
        Err(PersistenceError::InvariantViolation(message)) if message.contains("timeline limit")
    ));
    assert!(matches!(
        repository
            .list_for_day(900, 1_900, OccurrenceRepository::MAX_TIMELINE_LIMIT + 1)
            .await,
        Err(PersistenceError::InvariantViolation(message)) if message.contains("timeline limit")
    ));
    assert!(matches!(
        repository.list_for_day(1_900, 900, 10).await,
        Err(PersistenceError::InvariantViolation(message)) if message.contains("timeline start")
    ));
}
