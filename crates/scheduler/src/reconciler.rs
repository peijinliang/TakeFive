use chrono::{DateTime, Utc};
use takefive_domain::Clock;

use crate::{
    CandidateSource, ClaimOutcome, DeliveryPort, DeliveryRequest, OccurrenceStore,
    PlannedCandidate, PolicyDecision, PolicyEngine, ReconcileCause, RuntimeContext,
    RuntimeContextSource, SchedulerError,
};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReconcileReport {
    pub candidates: usize,
    pub claimed: usize,
    pub resumed: usize,
    pub duplicates: usize,
    pub delivered: usize,
    pub suppressed: usize,
    pub delivery_failed: usize,
}

pub struct Scheduler<C, S, O, D> {
    clock: C,
    source: S,
    store: O,
    delivery: D,
    policy: PolicyEngine,
}

impl<C, S, O, D> Scheduler<C, S, O, D>
where
    C: Clock,
    S: CandidateSource,
    O: OccurrenceStore,
    D: DeliveryPort,
{
    pub fn new(clock: C, source: S, store: O, delivery: D) -> Self {
        Self {
            clock,
            source,
            store,
            delivery,
            policy: PolicyEngine,
        }
    }

    pub async fn reconcile(
        &self,
        since: DateTime<Utc>,
        cause: ReconcileCause,
        context: &RuntimeContext,
    ) -> Result<ReconcileReport, SchedulerError> {
        self.reconcile_with_context_source(since, cause, &FixedRuntimeContextSource(context))
            .await
    }

    pub async fn reconcile_with_context_source<R>(
        &self,
        since: DateTime<Utc>,
        cause: ReconcileCause,
        context_source: &R,
    ) -> Result<ReconcileReport, SchedulerError>
    where
        R: RuntimeContextSource + ?Sized,
    {
        let now = self.clock.now_utc();
        let mut candidates = self.source.due_candidates(since, now, cause).await?;
        candidates.sort_by(|left, right| {
            left.scheduled_at_utc
                .cmp(&right.scheduled_at_utc)
                .then_with(|| left.reminder_id.cmp(&right.reminder_id))
                .then_with(|| left.occurrence_key.cmp(&right.occurrence_key))
        });

        let mut report = ReconcileReport {
            candidates: candidates.len(),
            ..Default::default()
        };

        for candidate in candidates {
            let context = context_source.runtime_context(&candidate, now).await?;
            let occurrence_id = match &candidate.resume_occurrence_id {
                Some(existing_id) => {
                    report.resumed += 1;
                    existing_id.clone()
                }
                None => match self.store.claim(&candidate, now).await? {
                    ClaimOutcome::Claimed { occurrence_id } => {
                        report.claimed += 1;
                        occurrence_id
                    }
                    ClaimOutcome::Duplicate => {
                        report.duplicates += 1;
                        continue;
                    }
                },
            };

            let decision = self.policy.evaluate(&candidate, &context, now);
            self.store
                .record_decision(&occurrence_id, &decision, now)
                .await?;

            if decision != PolicyDecision::Deliver {
                report.suppressed += 1;
                continue;
            }

            let request = DeliveryRequest {
                occurrence_id: occurrence_id.clone(),
                reminder_id: candidate.reminder_id,
                scheduled_at_utc: candidate.scheduled_at_utc,
            };
            match self.delivery.deliver(request).await {
                Ok(()) => {
                    self.store.mark_presented(&occurrence_id, now).await?;
                    report.delivered += 1;
                }
                Err(error) => {
                    self.store
                        .mark_delivery_failed(&occurrence_id, &error.code, now)
                        .await?;
                    report.delivery_failed += 1;
                }
            }
        }

        Ok(report)
    }
}

struct FixedRuntimeContextSource<'a>(&'a RuntimeContext);

#[async_trait::async_trait]
impl RuntimeContextSource for FixedRuntimeContextSource<'_> {
    async fn runtime_context(
        &self,
        _candidate: &PlannedCandidate,
        _now: DateTime<Utc>,
    ) -> Result<RuntimeContext, SchedulerError> {
        Ok(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::{TimeDelta, TimeZone};
    use std::{
        collections::HashSet,
        sync::{Arc, Mutex},
    };
    use takefive_domain::FakeClock;

    use crate::{
        CandidateKind, ClaimOutcome, DeliveryError, PlannedCandidate, ReminderDeliveryPolicy,
        RuntimeContextSource, StoreError, SuppressionReason,
    };

    #[derive(Clone)]
    struct FakeSource(Vec<PlannedCandidate>);

    #[async_trait]
    impl CandidateSource for FakeSource {
        async fn due_candidates(
            &self,
            _since: DateTime<Utc>,
            _now: DateTime<Utc>,
            _cause: ReconcileCause,
        ) -> Result<Vec<PlannedCandidate>, SchedulerError> {
            Ok(self.0.clone())
        }
    }

    #[derive(Default, Clone)]
    struct FakeStore {
        claimed: Arc<Mutex<HashSet<String>>>,
        decisions: Arc<Mutex<Vec<PolicyDecision>>>,
        presented: Arc<Mutex<Vec<String>>>,
        failed: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl OccurrenceStore for FakeStore {
        async fn claim(
            &self,
            candidate: &PlannedCandidate,
            _claimed_at: DateTime<Utc>,
        ) -> Result<ClaimOutcome, StoreError> {
            let key = format!("{}:{}", candidate.reminder_id, candidate.occurrence_key);
            let mut claimed = self.claimed.lock().unwrap();
            if claimed.insert(key.clone()) {
                Ok(ClaimOutcome::Claimed { occurrence_id: key })
            } else {
                Ok(ClaimOutcome::Duplicate)
            }
        }

        async fn record_decision(
            &self,
            _occurrence_id: &str,
            decision: &PolicyDecision,
            _decided_at: DateTime<Utc>,
        ) -> Result<(), StoreError> {
            self.decisions.lock().unwrap().push(decision.clone());
            Ok(())
        }

        async fn mark_presented(
            &self,
            occurrence_id: &str,
            _presented_at: DateTime<Utc>,
        ) -> Result<(), StoreError> {
            self.presented
                .lock()
                .unwrap()
                .push(occurrence_id.to_string());
            Ok(())
        }

        async fn mark_delivery_failed(
            &self,
            occurrence_id: &str,
            _error_code: &str,
            _failed_at: DateTime<Utc>,
        ) -> Result<(), StoreError> {
            self.failed.lock().unwrap().push(occurrence_id.to_string());
            Ok(())
        }
    }

    #[derive(Clone)]
    struct FakeDelivery {
        requests: Arc<Mutex<Vec<DeliveryRequest>>>,
        fail: bool,
    }

    #[async_trait]
    impl DeliveryPort for FakeDelivery {
        async fn deliver(&self, request: DeliveryRequest) -> Result<(), DeliveryError> {
            self.requests.lock().unwrap().push(request);
            if self.fail {
                Err(DeliveryError {
                    code: "test_failure".to_string(),
                })
            } else {
                Ok(())
            }
        }
    }

    fn planned() -> PlannedCandidate {
        PlannedCandidate {
            resume_occurrence_id: None,
            reminder_id: "r-1".to_string(),
            reminder_revision: 1,
            occurrence_key: "2026-07-14T10:00".to_string(),
            scheduled_at_utc: Utc.with_ymd_and_hms(2026, 7, 14, 2, 0, 0).unwrap(),
            scheduled_local: "2026-07-14T10:00:00".to_string(),
            timezone_id: "Asia/Shanghai".to_string(),
            kind: CandidateKind::Cyclic,
            policy: ReminderDeliveryPolicy::default(),
        }
    }

    fn planned_for(reminder_id: &str, occurrence_key: &str) -> PlannedCandidate {
        PlannedCandidate {
            reminder_id: reminder_id.to_string(),
            occurrence_key: occurrence_key.to_string(),
            ..planned()
        }
    }

    fn context() -> RuntimeContext {
        RuntimeContext {
            reminder_enabled: true,
            in_active_window: true,
            session_available: true,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn duplicate_reconcile_does_not_deliver_twice() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 2, 0, 1).unwrap();
        let store = FakeStore::default();
        let delivery = FakeDelivery {
            requests: Arc::default(),
            fail: false,
        };
        let scheduler = Scheduler::new(
            FakeClock::new(now),
            FakeSource(vec![planned()]),
            store,
            delivery.clone(),
        );

        let first = scheduler
            .reconcile(
                now - TimeDelta::minutes(1),
                ReconcileCause::Timer,
                &context(),
            )
            .await
            .unwrap();
        let second = scheduler
            .reconcile(
                now - TimeDelta::minutes(1),
                ReconcileCause::TimeChanged,
                &context(),
            )
            .await
            .unwrap();

        assert_eq!(first.delivered, 1);
        assert_eq!(second.duplicates, 1);
        assert_eq!(delivery.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn paused_candidate_is_claimed_and_explained_without_delivery() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 2, 0, 1).unwrap();
        let store = FakeStore::default();
        let delivery = FakeDelivery {
            requests: Arc::default(),
            fail: false,
        };
        let scheduler = Scheduler::new(
            FakeClock::new(now),
            FakeSource(vec![planned()]),
            store.clone(),
            delivery.clone(),
        );
        let paused = RuntimeContext {
            global_paused_until: Some(now + TimeDelta::hours(1)),
            ..context()
        };

        let report = scheduler
            .reconcile(now - TimeDelta::minutes(1), ReconcileCause::Timer, &paused)
            .await
            .unwrap();

        assert_eq!(report.suppressed, 1);
        assert!(delivery.requests.lock().unwrap().is_empty());
        assert!(matches!(
            store.decisions.lock().unwrap().as_slice(),
            [PolicyDecision::Ignore {
                reason: SuppressionReason::GlobalPaused
            }]
        ));
    }

    #[derive(Clone)]
    struct PerReminderContextSource {
        paused_reminder_id: String,
        paused_until: DateTime<Utc>,
    }

    #[async_trait]
    impl RuntimeContextSource for PerReminderContextSource {
        async fn runtime_context(
            &self,
            candidate: &PlannedCandidate,
            _now: DateTime<Utc>,
        ) -> Result<RuntimeContext, SchedulerError> {
            let reminder_paused_until =
                (candidate.reminder_id == self.paused_reminder_id).then_some(self.paused_until);
            Ok(RuntimeContext {
                reminder_paused_until,
                ..context()
            })
        }
    }

    struct FailingContextSource;

    #[async_trait]
    impl RuntimeContextSource for FailingContextSource {
        async fn runtime_context(
            &self,
            _candidate: &PlannedCandidate,
            _now: DateTime<Utc>,
        ) -> Result<RuntimeContext, SchedulerError> {
            Err(SchedulerError::RuntimeContextSource(
                "context unavailable".to_string(),
            ))
        }
    }

    #[tokio::test]
    async fn context_source_evaluates_pause_for_each_reminder() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 2, 0, 1).unwrap();
        let store = FakeStore::default();
        let delivery = FakeDelivery {
            requests: Arc::default(),
            fail: false,
        };
        let scheduler = Scheduler::new(
            FakeClock::new(now),
            FakeSource(vec![
                planned_for("r-paused", "paused-key"),
                planned_for("r-active", "active-key"),
            ]),
            store.clone(),
            delivery.clone(),
        );
        let contexts = PerReminderContextSource {
            paused_reminder_id: "r-paused".to_string(),
            paused_until: now + TimeDelta::hours(1),
        };

        let report = scheduler
            .reconcile_with_context_source(
                now - TimeDelta::minutes(1),
                ReconcileCause::Timer,
                &contexts,
            )
            .await
            .unwrap();

        assert_eq!(report.claimed, 2);
        assert_eq!(report.suppressed, 1);
        assert_eq!(report.delivered, 1);
        assert_eq!(delivery.requests.lock().unwrap()[0].reminder_id, "r-active");
        assert!(store.decisions.lock().unwrap().iter().any(|decision| {
            matches!(
                decision,
                PolicyDecision::Ignore {
                    reason: SuppressionReason::ReminderPaused
                }
            )
        }));
    }

    #[tokio::test]
    async fn context_failure_does_not_leave_a_claimed_occurrence() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 2, 0, 1).unwrap();
        let store = FakeStore::default();
        let scheduler = Scheduler::new(
            FakeClock::new(now),
            FakeSource(vec![planned()]),
            store.clone(),
            FakeDelivery {
                requests: Arc::default(),
                fail: false,
            },
        );

        let error = scheduler
            .reconcile_with_context_source(
                now - TimeDelta::minutes(1),
                ReconcileCause::Timer,
                &FailingContextSource,
            )
            .await
            .unwrap_err();

        assert!(matches!(error, SchedulerError::RuntimeContextSource(_)));
        assert!(store.claimed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn failed_delivery_is_persisted_and_not_reported_as_presented() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 2, 0, 1).unwrap();
        let store = FakeStore::default();
        let scheduler = Scheduler::new(
            FakeClock::new(now),
            FakeSource(vec![planned()]),
            store.clone(),
            FakeDelivery {
                requests: Arc::default(),
                fail: true,
            },
        );

        let report = scheduler
            .reconcile(
                now - TimeDelta::minutes(1),
                ReconcileCause::Timer,
                &context(),
            )
            .await
            .unwrap();

        assert_eq!(report.delivery_failed, 1);
        assert!(store.presented.lock().unwrap().is_empty());
        assert_eq!(store.failed.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn pending_occurrence_can_resume_after_restart_without_a_second_claim() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 2, 10, 0).unwrap();
        let store = FakeStore::default();
        let delivery = FakeDelivery {
            requests: Arc::default(),
            fail: false,
        };
        let mut pending = planned();
        pending.resume_occurrence_id = Some("occ-existing".to_string());
        let scheduler = Scheduler::new(
            FakeClock::new(now),
            FakeSource(vec![pending]),
            store.clone(),
            delivery.clone(),
        );

        let report = scheduler
            .reconcile(
                now - TimeDelta::minutes(10),
                ReconcileCause::Startup,
                &context(),
            )
            .await
            .unwrap();

        assert_eq!(report.resumed, 1);
        assert_eq!(report.claimed, 0);
        assert!(store.claimed.lock().unwrap().is_empty());
        assert_eq!(
            delivery.requests.lock().unwrap()[0].occurrence_id,
            "occ-existing"
        );
    }
}
