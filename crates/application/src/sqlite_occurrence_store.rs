use async_trait::async_trait;
use chrono::{DateTime, Utc};
use takefive_persistence_sqlite::{
    ClaimOutcome as PersistenceClaimOutcome, NewOccurrence, OccurrenceDecisionRecord,
    OccurrenceRepository,
};
use takefive_scheduler::{
    ClaimOutcome, OccurrenceStore, PlannedCandidate, PolicyDecision, StoreError, SuppressionReason,
};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct SqliteOccurrenceStore {
    repository: OccurrenceRepository,
}

impl SqliteOccurrenceStore {
    pub fn new(repository: OccurrenceRepository) -> Self {
        Self { repository }
    }

    pub fn repository(&self) -> &OccurrenceRepository {
        &self.repository
    }
}

#[async_trait]
impl OccurrenceStore for SqliteOccurrenceStore {
    async fn claim(
        &self,
        candidate: &PlannedCandidate,
        claimed_at: DateTime<Utc>,
    ) -> Result<ClaimOutcome, StoreError> {
        let occurrence = NewOccurrence {
            id: Uuid::new_v4().to_string(),
            reminder_id: candidate.reminder_id.clone(),
            reminder_revision: candidate.reminder_revision,
            occurrence_key: candidate.occurrence_key.clone(),
            scheduled_at_utc: candidate.scheduled_at_utc.timestamp_millis(),
            scheduled_local: candidate.scheduled_local.clone(),
            timezone_id: candidate.timezone_id.clone(),
            created_at_utc: claimed_at.timestamp_millis(),
        };

        match self
            .repository
            .create_and_claim(
                &occurrence,
                &Uuid::new_v4().to_string(),
                claimed_at.timestamp_millis(),
            )
            .await
            .map_err(store_error)?
        {
            PersistenceClaimOutcome::Claimed(value) => Ok(ClaimOutcome::Claimed {
                occurrence_id: value.id,
            }),
            PersistenceClaimOutcome::AlreadyClaimed(_) => Ok(ClaimOutcome::Duplicate),
        }
    }

    async fn record_decision(
        &self,
        occurrence_id: &str,
        decision: &PolicyDecision,
        decided_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let record = match decision {
            PolicyDecision::Deliver => OccurrenceDecisionRecord::Deliver,
            PolicyDecision::Defer { until, reason } => OccurrenceDecisionRecord::Defer {
                until_utc: until.timestamp_millis(),
                reason: reason_code(*reason),
            },
            PolicyDecision::Ignore { reason } => OccurrenceDecisionRecord::Ignore {
                reason: reason_code(*reason),
            },
            PolicyDecision::Missed { reason } => OccurrenceDecisionRecord::Missed {
                reason: reason_code(*reason),
            },
        };

        self.repository
            .apply_decision(occurrence_id, &record, decided_at.timestamp_millis())
            .await
            .map(|_| ())
            .map_err(store_error)
    }

    async fn mark_presented(
        &self,
        occurrence_id: &str,
        presented_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.repository
            .mark_presented(occurrence_id, presented_at.timestamp_millis())
            .await
            .map(|_| ())
            .map_err(store_error)
    }

    async fn mark_delivery_failed(
        &self,
        occurrence_id: &str,
        error_code: &str,
        failed_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.repository
            .mark_delivery_failed(occurrence_id, error_code, failed_at.timestamp_millis())
            .await
            .map(|_| ())
            .map_err(store_error)
    }
}

fn reason_code(reason: SuppressionReason) -> String {
    serde_json::to_value(reason)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_string())
}

fn store_error(error: takefive_persistence_sqlite::PersistenceError) -> StoreError {
    StoreError {
        code: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::{TimeDelta, TimeZone};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use takefive_domain::FakeClock;
    use takefive_persistence_sqlite::{NewReminder, ReminderRepository, SqliteStore};
    use takefive_scheduler::{
        CandidateKind, CandidateSource, DeliveryError, DeliveryPort, DeliveryRequest,
        PlannedCandidate, ReconcileCause, ReminderDeliveryPolicy, RuntimeContext, Scheduler,
        SchedulerError,
    };

    #[derive(Clone)]
    struct StaticSource(Vec<PlannedCandidate>);

    #[async_trait]
    impl CandidateSource for StaticSource {
        async fn due_candidates(
            &self,
            _since: DateTime<Utc>,
            _now: DateTime<Utc>,
            _cause: ReconcileCause,
        ) -> Result<Vec<PlannedCandidate>, SchedulerError> {
            Ok(self.0.clone())
        }
    }

    #[derive(Clone, Default)]
    struct RecordingDelivery(Arc<AtomicUsize>);

    #[async_trait]
    impl DeliveryPort for RecordingDelivery {
        async fn deliver(&self, _request: DeliveryRequest) -> Result<(), DeliveryError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn candidate() -> PlannedCandidate {
        PlannedCandidate {
            resume_occurrence_id: None,
            reminder_id: "reminder-1".to_string(),
            reminder_revision: 1,
            occurrence_key: "fixed|2026-07-14T10:00:00|fold=0".to_string(),
            scheduled_at_utc: Utc.with_ymd_and_hms(2026, 7, 14, 2, 0, 0).unwrap(),
            scheduled_local: "2026-07-14T10:00:00".to_string(),
            timezone_id: "Asia/Shanghai".to_string(),
            kind: CandidateKind::Cyclic,
            policy: ReminderDeliveryPolicy::default(),
        }
    }

    fn active_context() -> RuntimeContext {
        RuntimeContext {
            reminder_enabled: true,
            in_active_window: true,
            session_available: true,
            ..Default::default()
        }
    }

    async fn database() -> (tempfile::TempDir, SqliteStore) {
        let directory = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(directory.path().join("application.sqlite3"))
            .await
            .unwrap();
        ReminderRepository::new(store.clone())
            .create(&NewReminder::new("reminder-1", "Drink water", 100))
            .await
            .unwrap();
        (directory, store)
    }

    #[tokio::test]
    async fn real_sqlite_claim_prevents_duplicate_delivery() {
        let (_directory, database) = database().await;
        let repository = OccurrenceRepository::new(database);
        let store = SqliteOccurrenceStore::new(repository.clone());
        let delivery = RecordingDelivery::default();
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 2, 0, 1).unwrap();
        let scheduler = Scheduler::new(
            FakeClock::new(now),
            StaticSource(vec![candidate()]),
            store,
            delivery.clone(),
        );

        let first = scheduler
            .reconcile(
                now - TimeDelta::minutes(1),
                ReconcileCause::Timer,
                &active_context(),
            )
            .await
            .unwrap();
        let second = scheduler
            .reconcile(
                now - TimeDelta::minutes(1),
                ReconcileCause::TimeChanged,
                &active_context(),
            )
            .await
            .unwrap();

        assert_eq!(first.delivered, 1);
        assert_eq!(second.duplicates, 1);
        assert_eq!(delivery.0.load(Ordering::SeqCst), 1);
        let stored = repository
            .get_by_identity("reminder-1", &candidate().occurrence_key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.state, "presented");
    }

    #[tokio::test]
    async fn deferred_sqlite_occurrence_resumes_after_restart() {
        let (_directory, database) = database().await;
        let repository = OccurrenceRepository::new(database);
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 2, 0, 1).unwrap();
        let pause_end = now + TimeDelta::hours(1);
        let mut one_shot = candidate();
        one_shot.kind = CandidateKind::OneShot;
        let first_scheduler = Scheduler::new(
            FakeClock::new(now),
            StaticSource(vec![one_shot.clone()]),
            SqliteOccurrenceStore::new(repository.clone()),
            RecordingDelivery::default(),
        );
        let paused = RuntimeContext {
            global_paused_until: Some(pause_end),
            ..active_context()
        };
        first_scheduler
            .reconcile(now - TimeDelta::minutes(1), ReconcileCause::Timer, &paused)
            .await
            .unwrap();

        let due = repository
            .list_due_deferred(pause_end.timestamp_millis())
            .await
            .unwrap();
        assert_eq!(due.len(), 1);
        let mut resumed = one_shot;
        resumed.resume_occurrence_id = Some(due[0].id.clone());
        let delivery = RecordingDelivery::default();
        let restarted_scheduler = Scheduler::new(
            FakeClock::new(pause_end),
            StaticSource(vec![resumed]),
            SqliteOccurrenceStore::new(repository.clone()),
            delivery.clone(),
        );

        let report = restarted_scheduler
            .reconcile(now, ReconcileCause::Startup, &active_context())
            .await
            .unwrap();

        assert_eq!(report.resumed, 1);
        assert_eq!(report.delivered, 1);
        assert_eq!(delivery.0.load(Ordering::SeqCst), 1);
        assert_eq!(
            repository.get(&due[0].id).await.unwrap().unwrap().state,
            "presented"
        );
    }

    #[tokio::test]
    async fn paused_cyclic_occurrence_is_recorded_without_a_deferred_backlog() {
        let (_directory, database) = database().await;
        let repository = OccurrenceRepository::new(database);
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 2, 0, 1).unwrap();
        let pause_end = now + TimeDelta::hours(1);
        let scheduler = Scheduler::new(
            FakeClock::new(now),
            StaticSource(vec![candidate()]),
            SqliteOccurrenceStore::new(repository.clone()),
            RecordingDelivery::default(),
        );
        let paused = RuntimeContext {
            global_paused_until: Some(pause_end),
            ..active_context()
        };

        scheduler
            .reconcile(now - TimeDelta::minutes(1), ReconcileCause::Timer, &paused)
            .await
            .unwrap();

        assert!(repository
            .list_due_deferred(pause_end.timestamp_millis())
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            repository
                .get_by_identity("reminder-1", &candidate().occurrence_key)
                .await
                .unwrap()
                .unwrap()
                .state,
            "ignored"
        );
    }
}
