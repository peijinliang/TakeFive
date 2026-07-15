use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::{HashMap, HashSet, VecDeque};
use takefive_domain::{
    AlignedIntervalCandidate, AlignedIntervalRule, FixedTimeRule, OneShotRule, ScheduleCandidate,
};
use takefive_persistence_sqlite::{
    Occurrence, OccurrenceRepository, ReminderRepository, ScheduledReminderRecord,
};
use takefive_scheduler::{
    CandidateKind, CandidateSource, PlannedCandidate, ReconcileCause, ReminderDeliveryPolicy,
    SchedulerError,
};

const DEFAULT_CATCH_UP_PER_REMINDER: usize = 8;
const DEFAULT_CATCH_UP_TOTAL: usize = 64;

#[derive(Clone, Debug)]
pub struct SqliteCandidateSource {
    reminders: ReminderRepository,
    occurrences: OccurrenceRepository,
    catch_up_per_reminder: usize,
    catch_up_total: usize,
}

impl SqliteCandidateSource {
    pub fn new(reminders: ReminderRepository, occurrences: OccurrenceRepository) -> Self {
        Self {
            reminders,
            occurrences,
            catch_up_per_reminder: DEFAULT_CATCH_UP_PER_REMINDER,
            catch_up_total: DEFAULT_CATCH_UP_TOTAL,
        }
    }

    /// Limits newly planned schedule catch-up candidates. Due deferred occurrences are never
    /// discarded because they already represent persisted user-visible work.
    pub fn with_catch_up_limits(mut self, per_reminder: usize, total: usize) -> Self {
        self.catch_up_per_reminder = per_reminder.max(1);
        self.catch_up_total = total.max(1);
        self
    }
}

#[async_trait]
impl CandidateSource for SqliteCandidateSource {
    async fn due_candidates(
        &self,
        since: DateTime<Utc>,
        now: DateTime<Utc>,
        _cause: ReconcileCause,
    ) -> Result<Vec<PlannedCandidate>, SchedulerError> {
        let records = self
            .reminders
            .list_scheduled_enabled()
            .await
            .map_err(candidate_source_error)?;
        let configured = records
            .iter()
            .map(parse_configured_reminder)
            .collect::<Result<Vec<_>, _>>()?;
        let configuration_by_reminder = configured
            .iter()
            .map(|item| (item.reminder_id.as_str(), item))
            .collect::<HashMap<_, _>>();

        let due_deferred = self
            .occurrences
            .list_recoverable_due(now.timestamp_millis())
            .await
            .map_err(candidate_source_error)?;
        let resumed_identities = due_deferred
            .iter()
            .map(|item| (item.reminder_id.as_str(), item.occurrence_key.as_str()))
            .collect::<HashSet<_>>();

        let mut scheduled = Vec::new();
        if since < now {
            for item in &configured {
                if let Some(rule) = &item.fixed_time_rule {
                    let mut latest = VecDeque::with_capacity(self.catch_up_per_reminder);
                    let mut cursor = since;
                    while let Some(candidate) = rule.next_after(cursor) {
                        if candidate.scheduled_at_utc > now {
                            break;
                        }
                        cursor = candidate.scheduled_at_utc;
                        let occurrence_key = candidate.occurrence_key();
                        if resumed_identities
                            .contains(&(item.reminder_id.as_str(), occurrence_key.as_str()))
                        {
                            continue;
                        }
                        if latest.len() == self.catch_up_per_reminder {
                            latest.pop_front();
                        }
                        latest.push_back(planned_candidate(item, candidate));
                    }
                    scheduled.extend(latest);
                }

                if let Some(rule) = &item.aligned_interval_rule {
                    let mut latest = VecDeque::with_capacity(self.catch_up_per_reminder);
                    let mut cursor = since;
                    while let Some(candidate) = rule.next_after(cursor) {
                        if candidate.schedule.scheduled_at_utc > now {
                            break;
                        }
                        cursor = candidate.schedule.scheduled_at_utc;
                        if resumed_identities.contains(&(
                            item.reminder_id.as_str(),
                            candidate.occurrence_key.as_str(),
                        )) {
                            continue;
                        }
                        if latest.len() == self.catch_up_per_reminder {
                            latest.pop_front();
                        }
                        latest.push_back(planned_aligned_candidate(item, candidate));
                    }
                    scheduled.extend(latest);
                }

                // A one-shot rule has exactly one plan, so the cyclic per-reminder catch-up
                // limit must not filter it. It still participates in the global safety limit.
                if let Some(candidate) = item
                    .one_shot_rule
                    .as_ref()
                    .and_then(|rule| rule.next_after(since))
                    .filter(|candidate| candidate.scheduled_at_utc <= now)
                {
                    let occurrence_key = candidate.occurrence_key();
                    if !resumed_identities
                        .contains(&(item.reminder_id.as_str(), occurrence_key.as_str()))
                    {
                        scheduled.push(planned_candidate(item, candidate));
                    }
                }
            }
        }

        // When many reminders were missed, retain the most recent plans globally. SQLite's
        // occurrence identity still prevents duplicates on the next reconciliation.
        scheduled.sort_by(|left, right| {
            right
                .scheduled_at_utc
                .cmp(&left.scheduled_at_utc)
                .then_with(|| left.reminder_id.cmp(&right.reminder_id))
                .then_with(|| left.occurrence_key.cmp(&right.occurrence_key))
        });
        scheduled.truncate(self.catch_up_total);

        let mut candidates = due_deferred
            .into_iter()
            .filter_map(|occurrence| {
                configuration_by_reminder
                    .get(occurrence.reminder_id.as_str())
                    .copied()
                    .map(|configuration| resume_candidate(occurrence, configuration))
            })
            .collect::<Result<Vec<_>, _>>()?;
        candidates.extend(scheduled);
        candidates.sort_by(|left, right| {
            left.scheduled_at_utc
                .cmp(&right.scheduled_at_utc)
                .then_with(|| left.reminder_id.cmp(&right.reminder_id))
                .then_with(|| left.occurrence_key.cmp(&right.occurrence_key))
        });
        Ok(candidates)
    }
}

#[derive(Debug)]
struct ConfiguredReminder {
    reminder_id: String,
    reminder_revision: i64,
    fixed_time_rule: Option<FixedTimeRule>,
    aligned_interval_rule: Option<AlignedIntervalRule>,
    one_shot_rule: Option<OneShotRule>,
    kind: CandidateKind,
    policy: ReminderDeliveryPolicy,
}

fn parse_configured_reminder(
    record: &ScheduledReminderRecord,
) -> Result<ConfiguredReminder, SchedulerError> {
    let (fixed_time_rule, aligned_interval_rule, one_shot_rule, kind) =
        match record.rule_type.as_str() {
            "fixed_times" => (
                Some(
                    serde_json::from_str(&record.rule_config_json).map_err(|error| {
                        SchedulerError::CandidateSource(format!(
                            "invalid fixed-time rule for reminder {}: {error}",
                            record.reminder_id
                        ))
                    })?,
                ),
                None,
                None,
                CandidateKind::Cyclic,
            ),
            "aligned_interval" => (
                None,
                Some(
                    serde_json::from_str(&record.rule_config_json).map_err(|error| {
                        SchedulerError::CandidateSource(format!(
                            "invalid aligned-interval rule for reminder {}: {error}",
                            record.reminder_id
                        ))
                    })?,
                ),
                None,
                CandidateKind::Cyclic,
            ),
            "one_shot" => (
                None,
                None,
                Some(
                    serde_json::from_str(&record.rule_config_json).map_err(|error| {
                        SchedulerError::CandidateSource(format!(
                            "invalid one-shot rule for reminder {}: {error}",
                            record.reminder_id
                        ))
                    })?,
                ),
                CandidateKind::OneShot,
            ),
            _ => (None, None, None, CandidateKind::Cyclic),
        };
    Ok(ConfiguredReminder {
        reminder_id: record.reminder_id.clone(),
        reminder_revision: record.reminder_revision,
        fixed_time_rule,
        aligned_interval_rule,
        one_shot_rule,
        kind,
        policy: parse_policy(record)?,
    })
}

fn planned_aligned_candidate(
    configuration: &ConfiguredReminder,
    candidate: AlignedIntervalCandidate,
) -> PlannedCandidate {
    PlannedCandidate {
        resume_occurrence_id: None,
        reminder_id: configuration.reminder_id.clone(),
        reminder_revision: configuration.reminder_revision,
        occurrence_key: candidate.occurrence_key.to_string(),
        scheduled_at_utc: candidate.schedule.scheduled_at_utc,
        scheduled_local: candidate
            .schedule
            .planned_local
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string(),
        timezone_id: candidate.schedule.timezone.to_string(),
        kind: configuration.kind,
        policy: configuration.policy.clone(),
    }
}

fn planned_candidate(
    configuration: &ConfiguredReminder,
    candidate: ScheduleCandidate,
) -> PlannedCandidate {
    PlannedCandidate {
        resume_occurrence_id: None,
        reminder_id: configuration.reminder_id.clone(),
        reminder_revision: configuration.reminder_revision,
        occurrence_key: candidate.occurrence_key(),
        scheduled_at_utc: candidate.scheduled_at_utc,
        scheduled_local: candidate
            .planned_local
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string(),
        timezone_id: candidate.timezone.to_string(),
        kind: configuration.kind,
        policy: configuration.policy.clone(),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PolicyPatch {
    important: Option<bool>,
    #[serde(alias = "allowImportantBypass")]
    allow_important_bypass: Option<bool>,
    #[serde(alias = "catchUpOneShotWithinSeconds")]
    catch_up_one_shot_within_seconds: Option<i64>,
}

fn parse_policy(
    record: &ScheduledReminderRecord,
) -> Result<ReminderDeliveryPolicy, SchedulerError> {
    let mut policy = ReminderDeliveryPolicy::default();
    for (name, json) in [
        ("delivery", record.delivery_json.as_str()),
        ("missed", record.missed_json.as_str()),
        ("dnd", record.dnd_json.as_str()),
    ] {
        let patch: PolicyPatch = serde_json::from_str(json).map_err(|error| {
            SchedulerError::CandidateSource(format!(
                "invalid {name} policy for reminder {}: {error}",
                record.reminder_id
            ))
        })?;
        if let Some(value) = patch.important {
            policy.important = value;
        }
        if let Some(value) = patch.allow_important_bypass {
            policy.allow_important_bypass = value;
        }
        if let Some(value) = patch.catch_up_one_shot_within_seconds {
            policy.catch_up_one_shot_within_seconds = value.max(0);
        }
    }
    Ok(policy)
}

fn resume_candidate(
    occurrence: Occurrence,
    configuration: &ConfiguredReminder,
) -> Result<PlannedCandidate, SchedulerError> {
    let scheduled_at_utc = DateTime::from_timestamp_millis(occurrence.scheduled_at_utc)
        .ok_or_else(|| {
            SchedulerError::CandidateSource(format!(
                "occurrence {} has an invalid scheduled timestamp",
                occurrence.id
            ))
        })?;
    Ok(PlannedCandidate {
        resume_occurrence_id: Some(occurrence.id),
        reminder_id: occurrence.reminder_id,
        reminder_revision: configuration.reminder_revision,
        occurrence_key: occurrence.occurrence_key,
        scheduled_at_utc,
        scheduled_local: occurrence.scheduled_local,
        timezone_id: occurrence.timezone_id,
        kind: configuration.kind,
        policy: configuration.policy.clone(),
    })
}

fn candidate_source_error(error: impl std::fmt::Display) -> SchedulerError {
    SchedulerError::CandidateSource(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteOccurrenceStore;
    use async_trait::async_trait;
    use chrono::{NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Timelike, Weekday};
    use std::sync::{Arc, Mutex};
    use takefive_domain::{ActiveWindow, FakeClock, OneShotRule};
    use takefive_persistence_sqlite::{
        NewOccurrence, NewReminder, NewReminderPolicy, NewScheduleRule, OccurrenceDecisionRecord,
        ReminderChanges, SqliteStore,
    };
    use takefive_scheduler::{
        DeliveryError, DeliveryPort, DeliveryRequest, RuntimeContext, Scheduler,
    };

    const EVERY_DAY: [Weekday; 7] = [
        Weekday::Mon,
        Weekday::Tue,
        Weekday::Wed,
        Weekday::Thu,
        Weekday::Fri,
        Weekday::Sat,
        Weekday::Sun,
    ];

    async fn database() -> (tempfile::TempDir, SqliteStore) {
        let directory = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(directory.path().join("candidate-source.sqlite3"))
            .await
            .unwrap();
        (directory, store)
    }

    async fn create_configured_reminder(
        store: &SqliteStore,
        id: &str,
        rule: &FixedTimeRule,
        policy: NewReminderPolicy,
    ) {
        let reminder = NewReminder::new(id, format!("Reminder {id}"), 100);
        let stored_rule = NewScheduleRule {
            id: format!("rule-{id}"),
            rule_type: "fixed_times".into(),
            timezone_mode: "named".into(),
            timezone_id: Some("UTC".into()),
            config_json: serde_json::to_string(rule).unwrap(),
        };
        ReminderRepository::new(store.clone())
            .create_with_configuration(&reminder, Some(&stored_rule), Some(&policy))
            .await
            .unwrap();
    }

    async fn create_one_shot_reminder(
        store: &SqliteStore,
        id: &str,
        rule: &OneShotRule,
        policy: NewReminderPolicy,
    ) {
        let reminder = NewReminder::new(id, format!("Reminder {id}"), 100);
        let stored_rule = NewScheduleRule {
            id: format!("rule-{id}"),
            rule_type: "one_shot".into(),
            timezone_mode: "named".into(),
            timezone_id: Some("Asia/Shanghai".into()),
            config_json: serde_json::to_string(rule).unwrap(),
        };
        ReminderRepository::new(store.clone())
            .create_with_configuration(&reminder, Some(&stored_rule), Some(&policy))
            .await
            .unwrap();
    }

    async fn create_aligned_interval_reminder(
        store: &SqliteStore,
        id: &str,
        rule: &AlignedIntervalRule,
        policy: NewReminderPolicy,
    ) {
        let reminder = NewReminder::new(id, format!("Reminder {id}"), 100);
        let stored_rule = NewScheduleRule {
            id: format!("rule-{id}"),
            rule_type: "aligned_interval".into(),
            timezone_mode: "named".into(),
            timezone_id: Some(rule.timezone().to_string()),
            config_json: serde_json::to_string(rule).unwrap(),
        };
        ReminderRepository::new(store.clone())
            .create_with_configuration(&reminder, Some(&stored_rule), Some(&policy))
            .await
            .unwrap();
    }

    fn local_date_time(day: u32, hour: u32, minute: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 7, day)
            .unwrap()
            .and_hms_opt(hour, minute, 0)
            .unwrap()
    }

    fn aligned_rule(
        timezone: &str,
        anchor_local: NaiveDateTime,
        interval_minutes: u32,
        active_windows: Vec<ActiveWindow>,
    ) -> AlignedIntervalRule {
        AlignedIntervalRule::new(
            timezone.parse().unwrap(),
            anchor_local,
            interval_minutes,
            active_windows,
        )
        .unwrap()
    }

    fn fixed_rule(times: &[(u32, u32)]) -> FixedTimeRule {
        FixedTimeRule::new(
            "UTC".parse().unwrap(),
            EVERY_DAY.to_vec(),
            times
                .iter()
                .map(|(hour, minute)| NaiveTime::from_hms_opt(*hour, *minute, 0).unwrap())
                .collect(),
        )
        .unwrap()
    }

    fn source(store: &SqliteStore) -> SqliteCandidateSource {
        SqliteCandidateSource::new(
            ReminderRepository::new(store.clone()),
            OccurrenceRepository::new(store.clone()),
        )
    }

    #[derive(Clone, Default)]
    struct RecordingDelivery(Arc<Mutex<Vec<DeliveryRequest>>>);

    #[async_trait]
    impl DeliveryPort for RecordingDelivery {
        async fn deliver(&self, request: DeliveryRequest) -> Result<(), DeliveryError> {
            self.0.lock().unwrap().push(request);
            Ok(())
        }
    }

    #[tokio::test]
    async fn fixed_time_candidates_use_exclusive_since_and_inclusive_now_boundaries() {
        let (_directory, store) = database().await;
        let mut policy = NewReminderPolicy::defaults("policy-boundary");
        policy.delivery_json = r#"{"important":true}"#.into();
        policy.dnd_json = r#"{"allowImportantBypass":true}"#.into();
        policy.missed_json = r#"{"catch_up_one_shot_within_seconds":3600}"#.into();
        create_configured_reminder(&store, "boundary", &fixed_rule(&[(10, 0)]), policy).await;
        let since = Utc.with_ymd_and_hms(2026, 7, 14, 10, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 15, 10, 0, 0).unwrap();

        let candidates = source(&store)
            .due_candidates(since, now, ReconcileCause::Timer)
            .await
            .unwrap();

        assert_eq!(candidates.len(), 1);
        let candidate = &candidates[0];
        assert_eq!(candidate.resume_occurrence_id, None);
        assert_eq!(candidate.reminder_id, "boundary");
        assert_eq!(candidate.reminder_revision, 1);
        assert_eq!(candidate.occurrence_key, "UTC|2026-07-15T10:00:00|fold=0");
        assert_eq!(candidate.scheduled_at_utc, now);
        assert_eq!(candidate.scheduled_local, "2026-07-15T10:00:00");
        assert_eq!(candidate.timezone_id, "UTC");
        assert_eq!(candidate.kind, CandidateKind::Cyclic);
        assert_eq!(
            candidate.policy,
            ReminderDeliveryPolicy {
                important: true,
                allow_important_bypass: true,
                catch_up_one_shot_within_seconds: 3600,
            }
        );
    }

    #[tokio::test]
    async fn one_shot_candidates_use_exclusive_since_and_inclusive_now_boundaries() {
        let (_directory, store) = database().await;
        let since = Utc.with_ymd_and_hms(2026, 7, 14, 9, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 10, 0, 0).unwrap();
        for (id, at) in [
            ("at-since", since),
            ("inside", since + chrono::TimeDelta::minutes(30)),
            ("at-now", now),
            ("after-now", now + chrono::TimeDelta::seconds(1)),
        ] {
            create_one_shot_reminder(
                &store,
                id,
                &OneShotRule::new(at, "Asia/Shanghai".parse().unwrap()),
                NewReminderPolicy::defaults(format!("policy-{id}")),
            )
            .await;
        }
        ReminderRepository::new(store.clone())
            .update(
                "at-now",
                1,
                &ReminderChanges {
                    title: "Updated one-shot".into(),
                    description: String::new(),
                    enabled: true,
                    updated_at_utc: now.timestamp_millis(),
                },
            )
            .await
            .unwrap();

        let candidates = source(&store)
            .due_candidates(since, now, ReconcileCause::Timer)
            .await
            .unwrap();

        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.reminder_id.as_str())
                .collect::<Vec<_>>(),
            ["inside", "at-now"]
        );
        let at_now = &candidates[1];
        assert_eq!(at_now.resume_occurrence_id, None);
        assert_eq!(at_now.reminder_revision, 2);
        assert_eq!(
            at_now.occurrence_key,
            "Asia/Shanghai|2026-07-14T18:00:00|fold=0"
        );
        assert_eq!(at_now.scheduled_at_utc, now);
        assert_eq!(at_now.scheduled_local, "2026-07-14T18:00:00");
        assert_eq!(at_now.timezone_id, "Asia/Shanghai");
        assert_eq!(at_now.kind, CandidateKind::OneShot);
    }

    #[tokio::test]
    async fn one_shot_within_catch_up_window_is_delivered_from_real_sqlite() {
        let (_directory, store) = database().await;
        let now = Utc.with_ymd_and_hms(2026, 7, 15, 10, 0, 0).unwrap();
        let due = now - chrono::TimeDelta::hours(23);
        create_one_shot_reminder(
            &store,
            "one-shot-catch-up",
            &OneShotRule::new(due, "Asia/Shanghai".parse().unwrap()),
            NewReminderPolicy::defaults("policy-one-shot-catch-up"),
        )
        .await;
        let delivery = RecordingDelivery::default();
        let scheduler = Scheduler::new(
            FakeClock::new(now),
            source(&store).with_catch_up_limits(1, 1),
            SqliteOccurrenceStore::new(OccurrenceRepository::new(store.clone())),
            delivery.clone(),
        );

        let deferred = scheduler
            .reconcile(
                now - chrono::TimeDelta::hours(24),
                ReconcileCause::Startup,
                &RuntimeContext {
                    reminder_enabled: true,
                    in_active_window: true,
                    session_available: false,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(deferred.suppressed, 1);
        assert_eq!(deferred.delivered, 0);

        let restarted = Scheduler::new(
            FakeClock::new(now + chrono::TimeDelta::seconds(1)),
            source(&store).with_catch_up_limits(1, 1),
            SqliteOccurrenceStore::new(OccurrenceRepository::new(store.clone())),
            delivery.clone(),
        );
        let report = restarted
            .reconcile(
                now,
                ReconcileCause::Startup,
                &RuntimeContext {
                    reminder_enabled: true,
                    in_active_window: true,
                    session_available: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(report.resumed, 1);
        assert_eq!(report.delivered, 1);
        assert_eq!(delivery.0.lock().unwrap().len(), 1);
        let occurrence = OccurrenceRepository::new(store)
            .list_for_reminder("one-shot-catch-up")
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(occurrence.state, "presented");
    }

    #[tokio::test]
    async fn one_shot_plans_ignore_per_reminder_limit_but_obey_global_limit() {
        let (_directory, store) = database().await;
        let since = Utc.with_ymd_and_hms(2026, 7, 14, 9, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 12, 0, 0).unwrap();
        for (id, hour) in [("first-shot", 10), ("second-shot", 11), ("third-shot", 12)] {
            create_one_shot_reminder(
                &store,
                id,
                &OneShotRule::new(
                    Utc.with_ymd_and_hms(2026, 7, 14, hour, 0, 0).unwrap(),
                    "Asia/Shanghai".parse().unwrap(),
                ),
                NewReminderPolicy::defaults(format!("policy-{id}")),
            )
            .await;
        }

        let candidates = source(&store)
            .with_catch_up_limits(1, 2)
            .due_candidates(since, now, ReconcileCause::Startup)
            .await
            .unwrap();

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].reminder_id, "second-shot");
        assert_eq!(candidates[1].reminder_id, "third-shot");
        assert!(candidates
            .iter()
            .all(|candidate| candidate.kind == CandidateKind::OneShot));
    }

    #[tokio::test]
    async fn one_shot_outside_policy_catch_up_window_is_marked_missed() {
        let (_directory, store) = database().await;
        let now = Utc.with_ymd_and_hms(2026, 7, 15, 10, 0, 0).unwrap();
        let due = now - chrono::TimeDelta::hours(25);
        create_one_shot_reminder(
            &store,
            "expired-one-shot",
            &OneShotRule::new(due, "Asia/Shanghai".parse().unwrap()),
            NewReminderPolicy::defaults("policy-expired-one-shot"),
        )
        .await;
        let scheduler = Scheduler::new(
            FakeClock::new(now),
            source(&store),
            SqliteOccurrenceStore::new(OccurrenceRepository::new(store.clone())),
            RecordingDelivery::default(),
        );

        let report = scheduler
            .reconcile(
                now - chrono::TimeDelta::hours(26),
                ReconcileCause::Startup,
                &RuntimeContext {
                    reminder_enabled: true,
                    in_active_window: true,
                    session_available: false,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(report.claimed, 1);
        assert_eq!(report.suppressed, 1);
        assert_eq!(report.delivered, 0);
        let occurrence = OccurrenceRepository::new(store)
            .list_for_reminder("expired-one-shot")
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(occurrence.state, "missed");
        assert_eq!(
            occurrence.suppression_reason.as_deref(),
            Some("one_shot_catch_up_expired")
        );
    }

    #[tokio::test]
    async fn claimed_one_shot_is_resumed_with_current_configuration() {
        let (_directory, store) = database().await;
        let due = Utc.with_ymd_and_hms(2026, 7, 14, 10, 0, 0).unwrap();
        let mut policy = NewReminderPolicy::defaults("policy-resumed-one-shot");
        policy.delivery_json = r#"{"important":true}"#.into();
        create_one_shot_reminder(
            &store,
            "resumed-one-shot",
            &OneShotRule::new(due, "Asia/Shanghai".parse().unwrap()),
            policy,
        )
        .await;
        let occurrence = NewOccurrence {
            id: "claimed-one-shot".into(),
            reminder_id: "resumed-one-shot".into(),
            reminder_revision: 1,
            occurrence_key: "Asia/Shanghai|2026-07-14T18:00:00|fold=0".into(),
            scheduled_at_utc: due.timestamp_millis(),
            scheduled_local: "2026-07-14T18:00:00".into(),
            timezone_id: "Asia/Shanghai".into(),
            created_at_utc: due.timestamp_millis(),
        };
        OccurrenceRepository::new(store.clone())
            .create_and_claim(&occurrence, "claim-before-restart", due.timestamp_millis())
            .await
            .unwrap();
        ReminderRepository::new(store.clone())
            .update(
                "resumed-one-shot",
                1,
                &ReminderChanges {
                    title: "Updated after claim".into(),
                    description: String::new(),
                    enabled: true,
                    updated_at_utc: due.timestamp_millis(),
                },
            )
            .await
            .unwrap();

        let candidates = source(&store)
            .due_candidates(
                due,
                due + chrono::TimeDelta::seconds(1),
                ReconcileCause::Startup,
            )
            .await
            .unwrap();

        assert_eq!(candidates.len(), 1);
        let resumed = &candidates[0];
        assert_eq!(
            resumed.resume_occurrence_id.as_deref(),
            Some("claimed-one-shot")
        );
        assert_eq!(resumed.reminder_revision, 2);
        assert_eq!(resumed.kind, CandidateKind::OneShot);
        assert!(resumed.policy.important);
    }

    #[tokio::test]
    async fn real_sqlite_one_shot_delivers_once_across_reconcile_and_restart() {
        let (_directory, store) = database().await;
        let due = Utc.with_ymd_and_hms(2026, 7, 14, 10, 0, 0).unwrap();
        create_one_shot_reminder(
            &store,
            "one-shot-exactly-once",
            &OneShotRule::new(due, "Asia/Shanghai".parse().unwrap()),
            NewReminderPolicy::defaults("policy-one-shot-exactly-once"),
        )
        .await;
        let delivery = RecordingDelivery::default();
        let context = RuntimeContext {
            reminder_enabled: true,
            in_active_window: true,
            session_available: true,
            ..Default::default()
        };

        let scheduler = Scheduler::new(
            FakeClock::new(due),
            source(&store),
            SqliteOccurrenceStore::new(OccurrenceRepository::new(store.clone())),
            delivery.clone(),
        );
        let first = scheduler
            .reconcile(
                due - chrono::TimeDelta::minutes(1),
                ReconcileCause::Timer,
                &context,
            )
            .await
            .unwrap();
        assert_eq!(first.delivered, 1);

        let restarted = Scheduler::new(
            FakeClock::new(due + chrono::TimeDelta::seconds(1)),
            source(&store),
            SqliteOccurrenceStore::new(OccurrenceRepository::new(store.clone())),
            delivery.clone(),
        );
        let second = restarted
            .reconcile(
                due - chrono::TimeDelta::minutes(1),
                ReconcileCause::Startup,
                &context,
            )
            .await
            .unwrap();

        assert_eq!(second.duplicates, 1);
        assert_eq!(delivery.0.lock().unwrap().len(), 1);
        let stored = OccurrenceRepository::new(store)
            .get_by_identity(
                "one-shot-exactly-once",
                "Asia/Shanghai|2026-07-14T18:00:00|fold=0",
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.state, "presented");
    }

    #[tokio::test]
    async fn catch_up_limit_retains_the_latest_candidates_in_the_window() {
        let (_directory, store) = database().await;
        create_configured_reminder(
            &store,
            "limited",
            &fixed_rule(&[(8, 0), (9, 0), (10, 0), (11, 0)]),
            NewReminderPolicy::defaults("policy-limited"),
        )
        .await;
        let since = Utc.with_ymd_and_hms(2026, 7, 14, 7, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 11, 0, 0).unwrap();

        let candidates = source(&store)
            .with_catch_up_limits(2, 2)
            .due_candidates(since, now, ReconcileCause::Startup)
            .await
            .unwrap();

        assert_eq!(candidates.len(), 2);
        assert_eq!(
            candidates
                .iter()
                .map(|item| item.scheduled_at_utc)
                .collect::<Vec<_>>(),
            [
                Utc.with_ymd_and_hms(2026, 7, 14, 10, 0, 0).unwrap(),
                Utc.with_ymd_and_hms(2026, 7, 14, 11, 0, 0).unwrap(),
            ]
        );
    }

    #[tokio::test]
    async fn total_catch_up_limit_retains_the_latest_candidates_across_reminders() {
        let (_directory, store) = database().await;
        create_configured_reminder(
            &store,
            "first",
            &fixed_rule(&[(10, 0), (11, 0)]),
            NewReminderPolicy::defaults("policy-first"),
        )
        .await;
        create_configured_reminder(
            &store,
            "second",
            &fixed_rule(&[(10, 30), (11, 30)]),
            NewReminderPolicy::defaults("policy-second"),
        )
        .await;
        let since = Utc.with_ymd_and_hms(2026, 7, 14, 9, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 12, 0, 0).unwrap();

        let candidates = source(&store)
            .with_catch_up_limits(2, 2)
            .due_candidates(since, now, ReconcileCause::Startup)
            .await
            .unwrap();

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].scheduled_at_utc.hour(), 11);
        assert_eq!(candidates[0].scheduled_at_utc.minute(), 0);
        assert_eq!(candidates[1].scheduled_at_utc.hour(), 11);
        assert_eq!(candidates[1].scheduled_at_utc.minute(), 30);
    }

    #[tokio::test]
    async fn due_deferred_occurrences_resume_with_original_identity_outside_plan_limits() {
        let (_directory, store) = database().await;
        let mut policy = NewReminderPolicy::defaults("policy-resume");
        policy.delivery_json = r#"{"important":true}"#.into();
        create_configured_reminder(&store, "resume", &fixed_rule(&[(12, 0)]), policy).await;
        let repository = OccurrenceRepository::new(store.clone());
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 12, 0, 0).unwrap();

        for index in 1..=2 {
            let scheduled = now - chrono::TimeDelta::hours(index);
            let occurrence = NewOccurrence {
                id: format!("occurrence-{index}"),
                reminder_id: "resume".into(),
                reminder_revision: 1,
                occurrence_key: format!("deferred-{index}"),
                scheduled_at_utc: scheduled.timestamp_millis(),
                scheduled_local: scheduled.format("%Y-%m-%dT%H:%M:%S").to_string(),
                timezone_id: "UTC".into(),
                created_at_utc: scheduled.timestamp_millis(),
            };
            repository
                .create_and_claim(
                    &occurrence,
                    &format!("claim-{index}"),
                    scheduled.timestamp_millis(),
                )
                .await
                .unwrap();
            repository
                .apply_decision(
                    &occurrence.id,
                    &OccurrenceDecisionRecord::Defer {
                        until_utc: now.timestamp_millis(),
                        reason: "global_paused".into(),
                    },
                    scheduled.timestamp_millis(),
                )
                .await
                .unwrap();
        }
        ReminderRepository::new(store.clone())
            .update(
                "resume",
                1,
                &ReminderChanges {
                    title: "Updated reminder".into(),
                    description: String::new(),
                    enabled: true,
                    updated_at_utc: now.timestamp_millis(),
                },
            )
            .await
            .unwrap();

        let candidates = source(&store)
            .with_catch_up_limits(1, 1)
            .due_candidates(
                now - chrono::TimeDelta::hours(5),
                now,
                ReconcileCause::Startup,
            )
            .await
            .unwrap();

        assert_eq!(candidates.len(), 3);
        let resumed = candidates
            .iter()
            .filter(|item| item.resume_occurrence_id.is_some())
            .collect::<Vec<_>>();
        assert_eq!(resumed.len(), 2);
        assert!(resumed.iter().all(|item| item.policy.important));
        assert_eq!(resumed[0].reminder_revision, 2);
        assert_eq!(resumed[0].timezone_id, "UTC");
        assert_eq!(resumed[0].kind, CandidateKind::Cyclic);
    }

    #[tokio::test]
    async fn resumed_identity_suppresses_the_equivalent_new_plan() {
        let (_directory, store) = database().await;
        let rule = fixed_rule(&[(10, 0)]);
        create_configured_reminder(
            &store,
            "deduplicated",
            &rule,
            NewReminderPolicy::defaults("policy-deduplicated"),
        )
        .await;
        let repository = OccurrenceRepository::new(store.clone());
        let due = Utc.with_ymd_and_hms(2026, 7, 14, 10, 0, 0).unwrap();
        let occurrence = NewOccurrence {
            id: "existing-due".into(),
            reminder_id: "deduplicated".into(),
            reminder_revision: 1,
            occurrence_key: "UTC|2026-07-14T10:00:00|fold=0".into(),
            scheduled_at_utc: due.timestamp_millis(),
            scheduled_local: "2026-07-14T10:00:00".into(),
            timezone_id: "UTC".into(),
            created_at_utc: due.timestamp_millis(),
        };
        repository
            .create_and_claim(&occurrence, "claim", due.timestamp_millis())
            .await
            .unwrap();
        repository
            .apply_decision(
                &occurrence.id,
                &OccurrenceDecisionRecord::Defer {
                    until_utc: due.timestamp_millis(),
                    reason: "dnd".into(),
                },
                due.timestamp_millis(),
            )
            .await
            .unwrap();

        let candidates = source(&store)
            .due_candidates(
                due - chrono::TimeDelta::minutes(1),
                due,
                ReconcileCause::Timer,
            )
            .await
            .unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].resume_occurrence_id.as_deref(),
            Some("existing-due")
        );
    }

    #[tokio::test]
    async fn claimed_occurrence_is_resumed_after_a_crash_before_policy_decision() {
        let (_directory, store) = database().await;
        create_configured_reminder(
            &store,
            "crash-recovery",
            &fixed_rule(&[(10, 0)]),
            NewReminderPolicy::defaults("policy-crash-recovery"),
        )
        .await;
        let due = Utc.with_ymd_and_hms(2026, 7, 14, 10, 0, 0).unwrap();
        let occurrence = NewOccurrence {
            id: "claimed-before-crash".into(),
            reminder_id: "crash-recovery".into(),
            reminder_revision: 1,
            occurrence_key: "UTC|2026-07-14T10:00:00|fold=0".into(),
            scheduled_at_utc: due.timestamp_millis(),
            scheduled_local: "2026-07-14T10:00:00".into(),
            timezone_id: "UTC".into(),
            created_at_utc: due.timestamp_millis(),
        };
        OccurrenceRepository::new(store.clone())
            .create_and_claim(&occurrence, "claim-before-crash", due.timestamp_millis())
            .await
            .unwrap();

        let candidates = source(&store)
            .due_candidates(
                due,
                due + chrono::TimeDelta::seconds(1),
                ReconcileCause::Startup,
            )
            .await
            .unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].resume_occurrence_id.as_deref(),
            Some("claimed-before-crash")
        );
    }

    #[tokio::test]
    async fn due_deferred_occurrence_is_not_resumed_after_reminder_is_disabled_or_deleted() {
        let (_directory, store) = database().await;
        create_configured_reminder(
            &store,
            "disabled",
            &fixed_rule(&[(10, 0)]),
            NewReminderPolicy::defaults("policy-disabled"),
        )
        .await;
        let repository = OccurrenceRepository::new(store.clone());
        let due = Utc.with_ymd_and_hms(2026, 7, 14, 10, 0, 0).unwrap();
        let occurrence = NewOccurrence {
            id: "disabled-due".into(),
            reminder_id: "disabled".into(),
            reminder_revision: 1,
            occurrence_key: "UTC|2026-07-14T10:00:00|fold=0".into(),
            scheduled_at_utc: due.timestamp_millis(),
            scheduled_local: "2026-07-14T10:00:00".into(),
            timezone_id: "UTC".into(),
            created_at_utc: due.timestamp_millis(),
        };
        repository
            .create_and_claim(&occurrence, "claim", due.timestamp_millis())
            .await
            .unwrap();
        repository
            .apply_decision(
                &occurrence.id,
                &OccurrenceDecisionRecord::Defer {
                    until_utc: due.timestamp_millis(),
                    reason: "global_paused".into(),
                },
                due.timestamp_millis(),
            )
            .await
            .unwrap();
        ReminderRepository::new(store.clone())
            .update(
                "disabled",
                1,
                &ReminderChanges {
                    title: "Disabled".into(),
                    description: String::new(),
                    enabled: false,
                    updated_at_utc: due.timestamp_millis(),
                },
            )
            .await
            .unwrap();

        create_configured_reminder(
            &store,
            "deleted",
            &fixed_rule(&[(10, 0)]),
            NewReminderPolicy::defaults("policy-deleted"),
        )
        .await;
        let deleted_occurrence = NewOccurrence {
            id: "deleted-due".into(),
            reminder_id: "deleted".into(),
            reminder_revision: 1,
            occurrence_key: "UTC|2026-07-14T10:00:00|fold=0".into(),
            scheduled_at_utc: due.timestamp_millis(),
            scheduled_local: "2026-07-14T10:00:00".into(),
            timezone_id: "UTC".into(),
            created_at_utc: due.timestamp_millis(),
        };
        repository
            .create_and_claim(&deleted_occurrence, "deleted-claim", due.timestamp_millis())
            .await
            .unwrap();
        repository
            .apply_decision(
                &deleted_occurrence.id,
                &OccurrenceDecisionRecord::Defer {
                    until_utc: due.timestamp_millis(),
                    reason: "global_paused".into(),
                },
                due.timestamp_millis(),
            )
            .await
            .unwrap();
        ReminderRepository::new(store.clone())
            .soft_delete("deleted", due.timestamp_millis())
            .await
            .unwrap();

        let candidates = source(&store)
            .due_candidates(
                due - chrono::TimeDelta::minutes(1),
                due,
                ReconcileCause::Timer,
            )
            .await
            .unwrap();

        assert!(candidates.is_empty());
    }

    #[tokio::test]
    async fn malformed_fixed_time_configuration_is_reported() {
        let (_directory, store) = database().await;
        let reminder = NewReminder::new("invalid", "Invalid", 100);
        let stored_rule = NewScheduleRule {
            id: "rule-invalid".into(),
            rule_type: "fixed_times".into(),
            timezone_mode: "named".into(),
            timezone_id: Some("UTC".into()),
            config_json: "{}".into(),
        };
        ReminderRepository::new(store.clone())
            .create_with_configuration(
                &reminder,
                Some(&stored_rule),
                Some(&NewReminderPolicy::defaults("policy-invalid")),
            )
            .await
            .unwrap();

        let error = source(&store)
            .due_candidates(
                Utc.with_ymd_and_hms(2026, 7, 14, 9, 0, 0).unwrap(),
                Utc.with_ymd_and_hms(2026, 7, 14, 10, 0, 0).unwrap(),
                ReconcileCause::Timer,
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("invalid fixed-time rule"));
        assert!(error.to_string().contains("invalid"));
    }

    #[tokio::test]
    async fn real_sqlite_schedule_delivers_once_across_reconcile_and_restart() {
        let (_directory, store) = database().await;
        create_configured_reminder(
            &store,
            "exactly-once",
            &fixed_rule(&[(10, 0)]),
            NewReminderPolicy::defaults("policy-exactly-once"),
        )
        .await;
        let due = Utc.with_ymd_and_hms(2026, 7, 14, 10, 0, 0).unwrap();
        let delivery = RecordingDelivery::default();
        let context = RuntimeContext {
            reminder_enabled: true,
            in_active_window: true,
            session_available: true,
            ..Default::default()
        };

        let scheduler = Scheduler::new(
            FakeClock::new(due),
            source(&store),
            SqliteOccurrenceStore::new(OccurrenceRepository::new(store.clone())),
            delivery.clone(),
        );
        let first = scheduler
            .reconcile(
                due - chrono::TimeDelta::minutes(1),
                ReconcileCause::Timer,
                &context,
            )
            .await
            .unwrap();
        assert_eq!(first.delivered, 1);

        let restarted = Scheduler::new(
            FakeClock::new(due + chrono::TimeDelta::seconds(1)),
            source(&store),
            SqliteOccurrenceStore::new(OccurrenceRepository::new(store.clone())),
            delivery.clone(),
        );
        let second = restarted
            .reconcile(
                due - chrono::TimeDelta::minutes(1),
                ReconcileCause::Startup,
                &context,
            )
            .await
            .unwrap();

        assert_eq!(second.duplicates, 1);
        assert_eq!(delivery.0.lock().unwrap().len(), 1);
        let stored = OccurrenceRepository::new(store)
            .get_by_identity("exactly-once", "UTC|2026-07-14T10:00:00|fold=0")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.state, "presented");
    }

    #[tokio::test]
    async fn aligned_interval_uses_exclusive_since_inclusive_now_and_domain_key() {
        let (_directory, store) = database().await;
        let rule = aligned_rule("UTC", local_date_time(14, 9, 0), 60, Vec::new());
        create_aligned_interval_reminder(
            &store,
            "aligned-boundary",
            &rule,
            NewReminderPolicy::defaults("policy-aligned-boundary"),
        )
        .await;
        let since = Utc.with_ymd_and_hms(2026, 7, 14, 10, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 11, 0, 0).unwrap();

        let candidates = source(&store)
            .due_candidates(since, now, ReconcileCause::Timer)
            .await
            .unwrap();

        assert_eq!(candidates.len(), 1);
        let candidate = &candidates[0];
        assert_eq!(candidate.scheduled_at_utc, now);
        assert_eq!(candidate.scheduled_local, "2026-07-14T11:00:00");
        assert_eq!(candidate.timezone_id, "UTC");
        assert_eq!(candidate.kind, CandidateKind::Cyclic);
        assert_eq!(
            candidate.occurrence_key,
            "v1|aligned|tz=UTC|anchor=2026-07-14T09:00:00|every=60m|index=2"
        );
    }

    #[tokio::test]
    async fn aligned_interval_retains_latest_candidates_across_many_cycles_and_total_limit() {
        let (_directory, store) = database().await;
        let rule = aligned_rule("UTC", local_date_time(14, 9, 0), 60, Vec::new());
        create_aligned_interval_reminder(
            &store,
            "aligned-limited",
            &rule,
            NewReminderPolicy::defaults("policy-aligned-limited"),
        )
        .await;

        let candidates = source(&store)
            .with_catch_up_limits(3, 2)
            .due_candidates(
                Utc.with_ymd_and_hms(2026, 7, 14, 9, 0, 0).unwrap(),
                Utc.with_ymd_and_hms(2026, 7, 14, 15, 0, 0).unwrap(),
                ReconcileCause::Startup,
            )
            .await
            .unwrap();

        assert_eq!(candidates.len(), 2);
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.scheduled_local.as_str())
                .collect::<Vec<_>>(),
            ["2026-07-14T14:00:00", "2026-07-14T15:00:00"]
        );
        assert!(candidates[0].occurrence_key.ends_with("index=5"));
        assert!(candidates[1].occurrence_key.ends_with("index=6"));
    }

    #[tokio::test]
    async fn aligned_interval_skips_active_window_gap_without_realigning() {
        let (_directory, store) = database().await;
        let windows = vec![
            ActiveWindow::new(
                NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
            )
            .unwrap(),
            ActiveWindow::new(
                NaiveTime::from_hms_opt(13, 0, 0).unwrap(),
                NaiveTime::from_hms_opt(18, 0, 0).unwrap(),
            )
            .unwrap(),
        ];
        let rule = aligned_rule("UTC", local_date_time(14, 9, 0), 60, windows);
        create_aligned_interval_reminder(
            &store,
            "aligned-gap",
            &rule,
            NewReminderPolicy::defaults("policy-aligned-gap"),
        )
        .await;

        let candidates = source(&store)
            .due_candidates(
                Utc.with_ymd_and_hms(2026, 7, 14, 11, 0, 0).unwrap(),
                Utc.with_ymd_and_hms(2026, 7, 14, 14, 0, 0).unwrap(),
                ReconcileCause::Timer,
            )
            .await
            .unwrap();

        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.scheduled_local.as_str())
                .collect::<Vec<_>>(),
            ["2026-07-14T13:00:00", "2026-07-14T14:00:00"]
        );
        assert!(candidates[0].occurrence_key.ends_with("index=4"));
        assert!(candidates[1].occurrence_key.ends_with("index=5"));
    }

    #[tokio::test]
    async fn aligned_interval_delegates_cross_midnight_windows_to_domain_rule() {
        let (_directory, store) = database().await;
        let window = ActiveWindow::new(
            NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
            NaiveTime::from_hms_opt(2, 0, 0).unwrap(),
        )
        .unwrap();
        let rule = aligned_rule("UTC", local_date_time(14, 21, 0), 60, vec![window]);
        create_aligned_interval_reminder(
            &store,
            "aligned-midnight",
            &rule,
            NewReminderPolicy::defaults("policy-aligned-midnight"),
        )
        .await;

        let candidates = source(&store)
            .due_candidates(
                Utc.with_ymd_and_hms(2026, 7, 14, 21, 0, 0).unwrap(),
                Utc.with_ymd_and_hms(2026, 7, 15, 3, 0, 0).unwrap(),
                ReconcileCause::Timer,
            )
            .await
            .unwrap();

        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.scheduled_local.as_str())
                .collect::<Vec<_>>(),
            [
                "2026-07-14T22:00:00",
                "2026-07-14T23:00:00",
                "2026-07-15T00:00:00",
                "2026-07-15T01:00:00",
            ]
        );
    }

    #[tokio::test]
    async fn aligned_recoverable_states_use_current_revision_policy_and_kind() {
        let (_directory, store) = database().await;
        let rule = aligned_rule("UTC", local_date_time(14, 9, 0), 60, Vec::new());
        let mut policy = NewReminderPolicy::defaults("policy-aligned-recovery");
        policy.delivery_json = r#"{"important":true}"#.into();
        create_aligned_interval_reminder(&store, "aligned-recovery", &rule, policy).await;
        let repository = OccurrenceRepository::new(store.clone());
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 13, 0, 0).unwrap();

        for (id, index, hour) in [
            ("aligned-claimed", 1_u64, 10_u32),
            ("aligned-suppressed", 2, 11),
            ("aligned-snoozed", 3, 12),
        ] {
            let scheduled = Utc.with_ymd_and_hms(2026, 7, 14, hour, 0, 0).unwrap();
            let occurrence = NewOccurrence {
                id: id.into(),
                reminder_id: "aligned-recovery".into(),
                reminder_revision: 1,
                occurrence_key: format!(
                    "v1|aligned|tz=UTC|anchor=2026-07-14T09:00:00|every=60m|index={index}"
                ),
                scheduled_at_utc: scheduled.timestamp_millis(),
                scheduled_local: scheduled.format("%Y-%m-%dT%H:%M:%S").to_string(),
                timezone_id: "UTC".into(),
                created_at_utc: scheduled.timestamp_millis(),
            };
            repository
                .create_and_claim(
                    &occurrence,
                    &format!("claim-{id}"),
                    scheduled.timestamp_millis(),
                )
                .await
                .unwrap();
        }
        repository
            .apply_decision(
                "aligned-suppressed",
                &OccurrenceDecisionRecord::Defer {
                    until_utc: now.timestamp_millis(),
                    reason: "global_paused".into(),
                },
                now.timestamp_millis() - 2,
            )
            .await
            .unwrap();
        repository
            .apply_decision(
                "aligned-snoozed",
                &OccurrenceDecisionRecord::Deliver,
                now.timestamp_millis() - 3,
            )
            .await
            .unwrap();
        repository
            .mark_presented("aligned-snoozed", now.timestamp_millis() - 2)
            .await
            .unwrap();
        repository
            .snooze_presented(
                "aligned-snoozed",
                now.timestamp_millis(),
                now.timestamp_millis() - 1,
            )
            .await
            .unwrap();
        ReminderRepository::new(store.clone())
            .update(
                "aligned-recovery",
                1,
                &ReminderChanges {
                    title: "Updated aligned reminder".into(),
                    description: String::new(),
                    enabled: true,
                    updated_at_utc: now.timestamp_millis(),
                },
            )
            .await
            .unwrap();

        let candidates = source(&store)
            .due_candidates(now, now, ReconcileCause::Startup)
            .await
            .unwrap();

        assert_eq!(candidates.len(), 3);
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.resume_occurrence_id.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["aligned-claimed", "aligned-suppressed", "aligned-snoozed"]
        );
        assert!(candidates
            .iter()
            .all(|candidate| candidate.reminder_revision == 2));
        assert!(candidates
            .iter()
            .all(|candidate| candidate.policy.important));
        assert!(candidates
            .iter()
            .all(|candidate| candidate.kind == CandidateKind::Cyclic));
    }

    #[tokio::test]
    async fn real_sqlite_aligned_interval_delivers_once_across_reconcile_and_restart() {
        let (_directory, store) = database().await;
        let rule = aligned_rule("UTC", local_date_time(14, 9, 0), 60, Vec::new());
        create_aligned_interval_reminder(
            &store,
            "aligned-exactly-once",
            &rule,
            NewReminderPolicy::defaults("policy-aligned-exactly-once"),
        )
        .await;
        let due = Utc.with_ymd_and_hms(2026, 7, 14, 10, 0, 0).unwrap();
        let delivery = RecordingDelivery::default();
        let context = RuntimeContext {
            reminder_enabled: true,
            in_active_window: true,
            session_available: true,
            ..Default::default()
        };

        let scheduler = Scheduler::new(
            FakeClock::new(due),
            source(&store),
            SqliteOccurrenceStore::new(OccurrenceRepository::new(store.clone())),
            delivery.clone(),
        );
        let first = scheduler
            .reconcile(
                due - chrono::TimeDelta::minutes(1),
                ReconcileCause::Timer,
                &context,
            )
            .await
            .unwrap();
        assert_eq!(first.delivered, 1);

        let restarted = Scheduler::new(
            FakeClock::new(due + chrono::TimeDelta::seconds(1)),
            source(&store),
            SqliteOccurrenceStore::new(OccurrenceRepository::new(store.clone())),
            delivery.clone(),
        );
        let second = restarted
            .reconcile(
                due - chrono::TimeDelta::minutes(1),
                ReconcileCause::Startup,
                &context,
            )
            .await
            .unwrap();

        assert_eq!(second.duplicates, 1);
        assert_eq!(delivery.0.lock().unwrap().len(), 1);
        let key = "v1|aligned|tz=UTC|anchor=2026-07-14T09:00:00|every=60m|index=1";
        let stored = OccurrenceRepository::new(store)
            .get_by_identity("aligned-exactly-once", key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.state, "presented");
    }

    #[tokio::test]
    async fn aligned_interval_time_rollback_reuses_identity_without_second_delivery() {
        let (_directory, store) = database().await;
        let rule = aligned_rule("UTC", local_date_time(14, 9, 0), 60, Vec::new());
        create_aligned_interval_reminder(
            &store,
            "aligned-rollback",
            &rule,
            NewReminderPolicy::defaults("policy-aligned-rollback"),
        )
        .await;
        let due = Utc.with_ymd_and_hms(2026, 7, 14, 10, 0, 0).unwrap();
        let delivery = RecordingDelivery::default();
        let context = RuntimeContext {
            reminder_enabled: true,
            in_active_window: true,
            session_available: true,
            ..Default::default()
        };

        let first_scheduler = Scheduler::new(
            FakeClock::new(due + chrono::TimeDelta::minutes(30)),
            source(&store),
            SqliteOccurrenceStore::new(OccurrenceRepository::new(store.clone())),
            delivery.clone(),
        );
        let first = first_scheduler
            .reconcile(
                due - chrono::TimeDelta::minutes(30),
                ReconcileCause::Timer,
                &context,
            )
            .await
            .unwrap();
        assert_eq!(first.delivered, 1);

        let rolled_back_scheduler = Scheduler::new(
            FakeClock::new(due),
            source(&store),
            SqliteOccurrenceStore::new(OccurrenceRepository::new(store.clone())),
            delivery.clone(),
        );
        let after_rollback = rolled_back_scheduler
            .reconcile(
                due - chrono::TimeDelta::minutes(30),
                ReconcileCause::TimeChanged,
                &context,
            )
            .await
            .unwrap();

        assert_eq!(after_rollback.duplicates, 1);
        assert_eq!(delivery.0.lock().unwrap().len(), 1);
    }
}
