use sqlx::{Sqlite, Transaction};

use crate::{
    ClaimOutcome, InsertOccurrenceOutcome, NewOccurrence, NewPauseSession, NewReminder,
    NewReminderBundle, NewReminderPolicy, NewScheduleRule, Occurrence, OccurrenceDecisionRecord,
    OutstandingSurfaceDelivery, PauseScope, PauseSession, PersistenceError, Reminder,
    ReminderChanges, Result, ScheduledReminderRecord, SqliteStore,
};

#[derive(Clone, Debug)]
pub struct ReminderRepository {
    store: SqliteStore,
}

impl ReminderRepository {
    pub fn new(store: SqliteStore) -> Self {
        Self { store }
    }

    pub async fn create(&self, reminder: &NewReminder) -> Result<Reminder> {
        insert_reminder(self.store.pool(), reminder).await
    }

    pub async fn create_with_configuration(
        &self,
        reminder: &NewReminder,
        rule: Option<&NewScheduleRule>,
        policy: Option<&NewReminderPolicy>,
    ) -> Result<Reminder> {
        let mut transaction = self.store.pool().begin().await?;
        let result = create_reminder_bundle(&mut transaction, reminder, rule, policy).await;

        match result {
            Ok(created) => {
                transaction.commit().await?;
                Ok(created)
            }
            Err(error) => {
                transaction.rollback().await?;
                Err(error)
            }
        }
    }

    pub async fn create_many_with_configuration(
        &self,
        bundles: &[NewReminderBundle],
    ) -> Result<Vec<Reminder>> {
        let mut transaction = self.store.pool().begin().await?;
        let mut created = Vec::with_capacity(bundles.len());

        for bundle in bundles {
            match create_reminder_bundle(
                &mut transaction,
                &bundle.reminder,
                bundle.rule.as_ref(),
                bundle.policy.as_ref(),
            )
            .await
            {
                Ok(reminder) => created.push(reminder),
                Err(error) => {
                    transaction.rollback().await?;
                    return Err(error);
                }
            }
        }

        transaction.commit().await?;
        Ok(created)
    }

    pub async fn get(&self, id: &str) -> Result<Option<Reminder>> {
        Ok(sqlx::query_as::<_, Reminder>(
            "SELECT id, title, description, enabled, revision, created_at_utc, \
             updated_at_utc, deleted_at_utc FROM reminders WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(self.store.pool())
        .await?)
    }

    pub async fn list(&self, include_deleted: bool) -> Result<Vec<Reminder>> {
        let query = if include_deleted {
            "SELECT id, title, description, enabled, revision, created_at_utc, \
             updated_at_utc, deleted_at_utc FROM reminders ORDER BY created_at_utc, id"
        } else {
            "SELECT id, title, description, enabled, revision, created_at_utc, \
             updated_at_utc, deleted_at_utc FROM reminders \
             WHERE deleted_at_utc IS NULL ORDER BY created_at_utc, id"
        };

        Ok(sqlx::query_as::<_, Reminder>(query)
            .fetch_all(self.store.pool())
            .await?)
    }

    pub async fn list_scheduled_enabled(&self) -> Result<Vec<ScheduledReminderRecord>> {
        self.list_configured_with_filter(true).await
    }

    pub async fn list_configured(&self) -> Result<Vec<ScheduledReminderRecord>> {
        self.list_configured_with_filter(false).await
    }

    async fn list_configured_with_filter(
        &self,
        enabled_only: bool,
    ) -> Result<Vec<ScheduledReminderRecord>> {
        Ok(sqlx::query_as::<_, ScheduledReminderRecord>(
            "SELECT r.id AS reminder_id, r.title, r.description, \
             r.revision AS reminder_revision, sr.id AS rule_id, sr.rule_type, \
             sr.timezone_mode, sr.timezone_id, sr.config_json AS rule_config_json, \
             rp.id AS policy_id, rp.delivery_json, rp.sound_json, rp.snooze_json, \
             rp.missed_json, rp.dnd_json \
             FROM reminders r \
             INNER JOIN schedule_rules sr ON sr.reminder_id = r.id \
             INNER JOIN reminder_policies rp ON rp.reminder_id = r.id \
             WHERE r.deleted_at_utc IS NULL AND (? = 0 OR r.enabled = 1) \
             ORDER BY r.created_at_utc, r.id",
        )
        .bind(enabled_only)
        .fetch_all(self.store.pool())
        .await?)
    }

    pub async fn update(
        &self,
        id: &str,
        expected_revision: i64,
        changes: &ReminderChanges,
    ) -> Result<Reminder> {
        let updated = sqlx::query_as::<_, Reminder>(
            "UPDATE reminders SET title = ?, description = ?, enabled = ?, \
             updated_at_utc = ?, revision = revision + 1 \
             WHERE id = ? AND revision = ? AND deleted_at_utc IS NULL \
             RETURNING id, title, description, enabled, revision, created_at_utc, \
             updated_at_utc, deleted_at_utc",
        )
        .bind(&changes.title)
        .bind(&changes.description)
        .bind(changes.enabled)
        .bind(changes.updated_at_utc)
        .bind(id)
        .bind(expected_revision)
        .fetch_optional(self.store.pool())
        .await?;

        updated.ok_or_else(|| PersistenceError::RevisionConflict {
            id: id.to_owned(),
            expected_revision,
        })
    }

    pub async fn soft_delete(&self, id: &str, deleted_at_utc: i64) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE reminders SET enabled = 0, deleted_at_utc = ?, updated_at_utc = ?, \
             revision = revision + 1 WHERE id = ? AND deleted_at_utc IS NULL",
        )
        .bind(deleted_at_utc)
        .bind(deleted_at_utc)
        .bind(id)
        .execute(self.store.pool())
        .await?;

        Ok(result.rows_affected() == 1)
    }

    pub async fn restore(&self, id: &str, restored_at_utc: i64) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE reminders SET deleted_at_utc = NULL, updated_at_utc = ?, \
             revision = revision + 1 WHERE id = ? AND deleted_at_utc IS NOT NULL",
        )
        .bind(restored_at_utc)
        .bind(id)
        .execute(self.store.pool())
        .await?;

        Ok(result.rows_affected() == 1)
    }
}

#[derive(Clone, Debug)]
pub struct PauseRepository {
    store: SqliteStore,
}

impl PauseRepository {
    pub fn new(store: SqliteStore) -> Self {
        Self { store }
    }

    pub async fn create(&self, pause: &NewPauseSession) -> Result<PauseSession> {
        let (scope, reminder_id) = pause_scope_parts(&pause.scope);
        let row = sqlx::query_as::<_, PauseSessionRow>(
            "INSERT INTO pause_sessions (id, scope, reminder_id, starts_at_utc, ends_at_utc, \
             reason, created_at_utc) VALUES (?, ?, ?, ?, ?, ?, ?) \
             RETURNING id, scope, reminder_id, starts_at_utc, ends_at_utc, \
             cancelled_at_utc, reason, created_at_utc",
        )
        .bind(&pause.id)
        .bind(scope)
        .bind(reminder_id)
        .bind(pause.starts_at_utc)
        .bind(pause.ends_at_utc)
        .bind(&pause.reason)
        .bind(pause.created_at_utc)
        .fetch_one(self.store.pool())
        .await?;

        row.try_into()
    }

    pub async fn cancel(&self, id: &str, cancelled_at_utc: i64) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE pause_sessions SET cancelled_at_utc = ? \
             WHERE id = ? AND cancelled_at_utc IS NULL",
        )
        .bind(cancelled_at_utc)
        .bind(id)
        .execute(self.store.pool())
        .await?;

        Ok(result.rows_affected() == 1)
    }

    pub async fn list_active(&self, now_utc: i64) -> Result<Vec<PauseSession>> {
        self.fetch_active(
            "SELECT id, scope, reminder_id, starts_at_utc, ends_at_utc, cancelled_at_utc, \
             reason, created_at_utc FROM pause_sessions \
             WHERE cancelled_at_utc IS NULL AND starts_at_utc <= ? \
             AND (ends_at_utc IS NULL OR ends_at_utc > ?) \
             ORDER BY starts_at_utc, id",
            now_utc,
            None,
        )
        .await
    }

    pub async fn list_active_global(&self, now_utc: i64) -> Result<Vec<PauseSession>> {
        self.fetch_active(
            "SELECT id, scope, reminder_id, starts_at_utc, ends_at_utc, cancelled_at_utc, \
             reason, created_at_utc FROM pause_sessions \
             WHERE scope = 'global' AND cancelled_at_utc IS NULL AND starts_at_utc <= ? \
             AND (ends_at_utc IS NULL OR ends_at_utc > ?) \
             ORDER BY starts_at_utc, id",
            now_utc,
            None,
        )
        .await
    }

    pub async fn list_active_for_reminder(
        &self,
        reminder_id: &str,
        now_utc: i64,
    ) -> Result<Vec<PauseSession>> {
        self.fetch_active(
            "SELECT id, scope, reminder_id, starts_at_utc, ends_at_utc, cancelled_at_utc, \
             reason, created_at_utc FROM pause_sessions \
             WHERE scope = 'reminder' AND reminder_id = ? AND cancelled_at_utc IS NULL \
             AND starts_at_utc <= ? AND (ends_at_utc IS NULL OR ends_at_utc > ?) \
             ORDER BY starts_at_utc, id",
            now_utc,
            Some(reminder_id),
        )
        .await
    }

    pub async fn list_effective_for_reminder(
        &self,
        reminder_id: &str,
        now_utc: i64,
    ) -> Result<Vec<PauseSession>> {
        let rows = sqlx::query_as::<_, PauseSessionRow>(
            "SELECT id, scope, reminder_id, starts_at_utc, ends_at_utc, cancelled_at_utc, \
             reason, created_at_utc FROM pause_sessions \
             WHERE (scope = 'global' OR (scope = 'reminder' AND reminder_id = ?)) \
             AND cancelled_at_utc IS NULL AND starts_at_utc <= ? \
             AND (ends_at_utc IS NULL OR ends_at_utc > ?) \
             ORDER BY CASE scope WHEN 'global' THEN 0 ELSE 1 END, starts_at_utc, id",
        )
        .bind(reminder_id)
        .bind(now_utc)
        .bind(now_utc)
        .fetch_all(self.store.pool())
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn fetch_active(
        &self,
        query: &str,
        now_utc: i64,
        reminder_id: Option<&str>,
    ) -> Result<Vec<PauseSession>> {
        let mut query = sqlx::query_as::<_, PauseSessionRow>(query);
        if let Some(reminder_id) = reminder_id {
            query = query.bind(reminder_id);
        }
        let rows = query
            .bind(now_utc)
            .bind(now_utc)
            .fetch_all(self.store.pool())
            .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }
}

#[derive(Debug, sqlx::FromRow)]
struct PauseSessionRow {
    id: String,
    scope: String,
    reminder_id: Option<String>,
    starts_at_utc: i64,
    ends_at_utc: Option<i64>,
    cancelled_at_utc: Option<i64>,
    reason: Option<String>,
    created_at_utc: i64,
}

impl TryFrom<PauseSessionRow> for PauseSession {
    type Error = PersistenceError;

    fn try_from(row: PauseSessionRow) -> Result<Self> {
        let scope = match (row.scope.as_str(), row.reminder_id) {
            ("global", None) => PauseScope::Global,
            ("reminder", Some(reminder_id)) => PauseScope::Reminder { reminder_id },
            (scope, reminder_id) => {
                return Err(PersistenceError::InvariantViolation(format!(
                    "invalid pause scope {scope} with reminder {reminder_id:?}"
                )))
            }
        };

        Ok(Self {
            id: row.id,
            scope,
            starts_at_utc: row.starts_at_utc,
            ends_at_utc: row.ends_at_utc,
            cancelled_at_utc: row.cancelled_at_utc,
            reason: row.reason,
            created_at_utc: row.created_at_utc,
        })
    }
}

fn pause_scope_parts(scope: &PauseScope) -> (&'static str, Option<&str>) {
    match scope {
        PauseScope::Global => ("global", None),
        PauseScope::Reminder { reminder_id } => ("reminder", Some(reminder_id)),
    }
}

#[derive(Clone, Debug)]
pub struct OccurrenceRepository {
    store: SqliteStore,
}

impl OccurrenceRepository {
    pub const MAX_TIMELINE_LIMIT: u32 = 500;

    pub fn new(store: SqliteStore) -> Self {
        Self { store }
    }

    pub async fn insert(&self, occurrence: &NewOccurrence) -> Result<InsertOccurrenceOutcome> {
        let inserted = insert_occurrence(self.store.pool(), occurrence).await?;
        match inserted {
            Some(inserted) => Ok(InsertOccurrenceOutcome::Inserted(inserted)),
            None => {
                let existing = self
                    .get_by_identity(&occurrence.reminder_id, &occurrence.occurrence_key)
                    .await?
                    .ok_or_else(|| {
                        PersistenceError::InvariantViolation(format!(
                            "occurrence conflict disappeared for reminder {} and key {}",
                            occurrence.reminder_id, occurrence.occurrence_key
                        ))
                    })?;
                Ok(InsertOccurrenceOutcome::Existing(existing))
            }
        }
    }

    pub async fn get(&self, id: &str) -> Result<Option<Occurrence>> {
        Ok(sqlx::query_as::<_, Occurrence>(OCCURRENCE_SELECT_BY_ID)
            .bind(id)
            .fetch_optional(self.store.pool())
            .await?)
    }

    pub async fn get_by_identity(
        &self,
        reminder_id: &str,
        occurrence_key: &str,
    ) -> Result<Option<Occurrence>> {
        Ok(
            sqlx::query_as::<_, Occurrence>(OCCURRENCE_SELECT_BY_IDENTITY)
                .bind(reminder_id)
                .bind(occurrence_key)
                .fetch_optional(self.store.pool())
                .await?,
        )
    }

    pub async fn list_for_reminder(&self, reminder_id: &str) -> Result<Vec<Occurrence>> {
        Ok(sqlx::query_as::<_, Occurrence>(
            "SELECT id, reminder_id, reminder_revision, occurrence_key, scheduled_at_utc, \
             scheduled_local, timezone_id, state, result, suppression_reason, \
             deferred_until_utc, display_at_utc, snooze_due_at_utc, snooze_count, \
             presented_at_utc, handled_at_utc, merged_into_id, claim_token, claimed_at_utc, \
             created_at_utc, updated_at_utc FROM occurrences \
             WHERE reminder_id = ? ORDER BY scheduled_at_utc, id",
        )
        .bind(reminder_id)
        .fetch_all(self.store.pool())
        .await?)
    }

    pub async fn list_for_day(
        &self,
        starts_at_utc: i64,
        ends_at_utc: i64,
        limit: u32,
    ) -> Result<Vec<Occurrence>> {
        if starts_at_utc >= ends_at_utc {
            return Err(PersistenceError::InvariantViolation(
                "timeline start must be before its exclusive end".into(),
            ));
        }
        if !(1..=Self::MAX_TIMELINE_LIMIT).contains(&limit) {
            return Err(PersistenceError::InvariantViolation(format!(
                "timeline limit must be between 1 and {}",
                Self::MAX_TIMELINE_LIMIT
            )));
        }

        Ok(sqlx::query_as::<_, Occurrence>(
            "SELECT id, reminder_id, reminder_revision, occurrence_key, scheduled_at_utc, \
             scheduled_local, timezone_id, state, result, suppression_reason, \
             deferred_until_utc, display_at_utc, snooze_due_at_utc, snooze_count, \
             presented_at_utc, handled_at_utc, merged_into_id, claim_token, claimed_at_utc, \
             created_at_utc, updated_at_utc FROM occurrences \
             WHERE (scheduled_at_utc >= ? AND scheduled_at_utc < ?) \
                OR (display_at_utc >= ? AND display_at_utc < ?) \
             ORDER BY CASE WHEN display_at_utc >= ? AND display_at_utc < ? \
                           THEN display_at_utc ELSE scheduled_at_utc END DESC, \
                      scheduled_at_utc DESC, id DESC LIMIT ?",
        )
        .bind(starts_at_utc)
        .bind(ends_at_utc)
        .bind(starts_at_utc)
        .bind(ends_at_utc)
        .bind(starts_at_utc)
        .bind(ends_at_utc)
        .bind(i64::from(limit))
        .fetch_all(self.store.pool())
        .await?)
    }

    pub async fn create_and_claim(
        &self,
        occurrence: &NewOccurrence,
        claim_token: &str,
        claimed_at_utc: i64,
    ) -> Result<ClaimOutcome> {
        let mut transaction = self.store.pool().begin().await?;
        insert_occurrence(&mut *transaction, occurrence).await?;

        let claimed = claim_by_identity(
            &mut transaction,
            &occurrence.reminder_id,
            &occurrence.occurrence_key,
            claim_token,
            claimed_at_utc,
        )
        .await?;

        let outcome = match claimed {
            Some(claimed) => ClaimOutcome::Claimed(claimed),
            None => ClaimOutcome::AlreadyClaimed(
                sqlx::query_as::<_, Occurrence>(OCCURRENCE_SELECT_BY_IDENTITY)
                    .bind(&occurrence.reminder_id)
                    .bind(&occurrence.occurrence_key)
                    .fetch_one(&mut *transaction)
                    .await?,
            ),
        };

        transaction.commit().await?;
        Ok(outcome)
    }

    pub async fn claim_existing(
        &self,
        id: &str,
        claim_token: &str,
        claimed_at_utc: i64,
    ) -> Result<Option<Occurrence>> {
        let mut transaction = self.store.pool().begin().await?;
        let claimed = sqlx::query_as::<_, Occurrence>(
            "UPDATE occurrences SET state = 'claimed', claim_token = ?, claimed_at_utc = ?, \
             updated_at_utc = ? WHERE id = ? AND state = 'pending' AND claim_token IS NULL \
             RETURNING id, reminder_id, reminder_revision, occurrence_key, scheduled_at_utc, \
             scheduled_local, timezone_id, state, result, suppression_reason, \
             deferred_until_utc, display_at_utc, snooze_due_at_utc, snooze_count, \
             presented_at_utc, handled_at_utc, merged_into_id, claim_token, claimed_at_utc, \
             created_at_utc, updated_at_utc",
        )
        .bind(claim_token)
        .bind(claimed_at_utc)
        .bind(claimed_at_utc)
        .bind(id)
        .fetch_optional(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(claimed)
    }

    pub async fn apply_decision(
        &self,
        id: &str,
        decision: &OccurrenceDecisionRecord,
        decided_at_utc: i64,
    ) -> Result<Occurrence> {
        let (state, result, reason, deferred_until) = match decision {
            OccurrenceDecisionRecord::Deliver => ("delivering", None, None, None),
            OccurrenceDecisionRecord::Defer { until_utc, reason } => {
                ("suppressed", None, Some(reason.as_str()), Some(*until_utc))
            }
            OccurrenceDecisionRecord::Ignore { reason } => {
                ("ignored", Some("ignored"), Some(reason.as_str()), None)
            }
            OccurrenceDecisionRecord::Missed { reason } => {
                ("missed", Some("missed"), Some(reason.as_str()), None)
            }
        };

        sqlx::query_as::<_, Occurrence>(
            "UPDATE occurrences SET state = ?, result = ?, suppression_reason = ?, \
             deferred_until_utc = ?, handled_at_utc = CASE WHEN ? IS NULL THEN NULL ELSE ? END, \
             updated_at_utc = ? WHERE id = ? \
             AND state IN ('claimed', 'suppressed', 'snoozed') \
             RETURNING id, reminder_id, reminder_revision, occurrence_key, scheduled_at_utc, \
             scheduled_local, timezone_id, state, result, suppression_reason, \
             deferred_until_utc, display_at_utc, snooze_due_at_utc, snooze_count, \
             presented_at_utc, handled_at_utc, merged_into_id, claim_token, claimed_at_utc, \
             created_at_utc, updated_at_utc",
        )
        .bind(state)
        .bind(result)
        .bind(reason)
        .bind(deferred_until)
        .bind(result)
        .bind(decided_at_utc)
        .bind(decided_at_utc)
        .bind(id)
        .fetch_optional(self.store.pool())
        .await?
        .ok_or_else(|| {
            PersistenceError::InvariantViolation(format!(
                "occurrence {id} was not in a decision-compatible state"
            ))
        })
    }

    pub async fn mark_presented(&self, id: &str, presented_at_utc: i64) -> Result<Occurrence> {
        sqlx::query_as::<_, Occurrence>(
            "UPDATE occurrences SET state = 'presented', presented_at_utc = ?, \
             display_at_utc = ?, deferred_until_utc = NULL, updated_at_utc = ? \
             WHERE id = ? AND state = 'delivering' \
             RETURNING id, reminder_id, reminder_revision, occurrence_key, scheduled_at_utc, \
             scheduled_local, timezone_id, state, result, suppression_reason, \
             deferred_until_utc, display_at_utc, snooze_due_at_utc, snooze_count, \
             presented_at_utc, handled_at_utc, merged_into_id, claim_token, claimed_at_utc, \
             created_at_utc, updated_at_utc",
        )
        .bind(presented_at_utc)
        .bind(presented_at_utc)
        .bind(presented_at_utc)
        .bind(id)
        .fetch_optional(self.store.pool())
        .await?
        .ok_or_else(|| {
            PersistenceError::InvariantViolation(format!(
                "occurrence {id} was not delivering when presented"
            ))
        })
    }

    pub async fn complete_presented(&self, id: &str, handled_at_utc: i64) -> Result<Occurrence> {
        self.finish_presented(id, "completed", "completed", None, handled_at_utc)
            .await
    }

    pub async fn skip_presented(&self, id: &str, handled_at_utc: i64) -> Result<Occurrence> {
        self.finish_presented(id, "skipped", "skipped", None, handled_at_utc)
            .await
    }

    pub async fn snooze_presented(
        &self,
        id: &str,
        due_at_utc: i64,
        handled_at_utc: i64,
    ) -> Result<Occurrence> {
        if due_at_utc <= handled_at_utc {
            return Err(PersistenceError::InvariantViolation(format!(
                "occurrence {id} snooze deadline must be after the action time"
            )));
        }

        let updated = sqlx::query_as::<_, Occurrence>(
            "UPDATE occurrences SET state = 'snoozed', result = NULL, \
             snooze_due_at_utc = ?, snooze_count = snooze_count + 1, \
             handled_at_utc = NULL, updated_at_utc = ? \
             WHERE id = ? AND state = 'presented' AND snooze_count < 9223372036854775807 \
             RETURNING id, reminder_id, reminder_revision, occurrence_key, scheduled_at_utc, \
             scheduled_local, timezone_id, state, result, suppression_reason, \
             deferred_until_utc, display_at_utc, snooze_due_at_utc, snooze_count, \
             presented_at_utc, handled_at_utc, merged_into_id, claim_token, claimed_at_utc, \
             created_at_utc, updated_at_utc",
        )
        .bind(due_at_utc)
        .bind(handled_at_utc)
        .bind(id)
        .fetch_optional(self.store.pool())
        .await?;

        match updated {
            Some(occurrence) => Ok(occurrence),
            None => Err(self.action_invariant_violation(id, "snooze").await?),
        }
    }

    pub async fn mark_unhandled_presented(
        &self,
        id: &str,
        handled_at_utc: i64,
    ) -> Result<Occurrence> {
        self.finish_presented(
            id,
            "unhandled",
            "unhandled",
            Some("timed_out"),
            handled_at_utc,
        )
        .await
    }

    pub async fn mark_delivery_failed(
        &self,
        id: &str,
        error_code: &str,
        failed_at_utc: i64,
    ) -> Result<Occurrence> {
        sqlx::query_as::<_, Occurrence>(
            "UPDATE occurrences SET state = 'delivery_failed', suppression_reason = ?, \
             updated_at_utc = ? WHERE id = ? AND state = 'delivering' \
             RETURNING id, reminder_id, reminder_revision, occurrence_key, scheduled_at_utc, \
             scheduled_local, timezone_id, state, result, suppression_reason, \
             deferred_until_utc, display_at_utc, snooze_due_at_utc, snooze_count, \
             presented_at_utc, handled_at_utc, merged_into_id, claim_token, claimed_at_utc, \
             created_at_utc, updated_at_utc",
        )
        .bind(error_code)
        .bind(failed_at_utc)
        .bind(id)
        .fetch_optional(self.store.pool())
        .await?
        .ok_or_else(|| {
            PersistenceError::InvariantViolation(format!(
                "occurrence {id} was not delivering when delivery failed"
            ))
        })
    }

    pub async fn record_surface_delivery_accepted(
        &self,
        attempt_id: &str,
        occurrence_id: &str,
        payload_json: &str,
        attempted_at_utc: i64,
    ) -> Result<()> {
        let inserted = sqlx::query(
            "INSERT INTO delivery_attempts (
                 id, occurrence_id, channel, status, attempted_at_utc, diagnostic_json
             )
             SELECT ?, id, 'reminder_surface', 'accepted', ?, ?
             FROM occurrences WHERE id = ? AND state = 'delivering'",
        )
        .bind(attempt_id)
        .bind(attempted_at_utc)
        .bind(payload_json)
        .bind(occurrence_id)
        .execute(self.store.pool())
        .await?;

        if inserted.rows_affected() == 1 {
            Ok(())
        } else {
            Err(PersistenceError::InvariantViolation(format!(
                "occurrence {occurrence_id} was not delivering when the surface accepted it"
            )))
        }
    }

    pub async fn mark_surface_delivery_failed(
        &self,
        attempt_id: &str,
        error_code: &str,
        completed_at_utc: i64,
    ) -> Result<()> {
        let updated = sqlx::query(
            "UPDATE delivery_attempts
             SET status = 'failed', completed_at_utc = ?, error_code = ?
             WHERE id = ? AND channel = 'reminder_surface' AND status = 'accepted'",
        )
        .bind(completed_at_utc)
        .bind(error_code)
        .bind(attempt_id)
        .execute(self.store.pool())
        .await?;

        if updated.rows_affected() == 1 {
            Ok(())
        } else {
            Err(PersistenceError::InvariantViolation(format!(
                "surface delivery attempt {attempt_id} was not accepted when it failed"
            )))
        }
    }

    pub async fn list_outstanding_surface_deliveries(
        &self,
    ) -> Result<Vec<OutstandingSurfaceDelivery>> {
        Ok(sqlx::query_as::<_, OutstandingSurfaceDelivery>(
            "SELECT o.id AS occurrence_id, o.state AS occurrence_state,
                    da.diagnostic_json AS payload_json
             FROM occurrences o
             INNER JOIN delivery_attempts da ON da.id = (
                 SELECT latest.id FROM delivery_attempts latest
                 WHERE latest.occurrence_id = o.id
                   AND latest.channel = 'reminder_surface'
                 ORDER BY latest.attempted_at_utc DESC, latest.id DESC
                 LIMIT 1
             )
             WHERE o.state IN ('delivering', 'presented')
               AND da.status = 'accepted'
               AND da.diagnostic_json IS NOT NULL
             ORDER BY COALESCE(o.display_at_utc, o.scheduled_at_utc), o.id",
        )
        .fetch_all(self.store.pool())
        .await?)
    }

    pub async fn list_unattempted_deliveries(&self) -> Result<Vec<Occurrence>> {
        Ok(sqlx::query_as::<_, Occurrence>(
            "SELECT id, reminder_id, reminder_revision, occurrence_key, scheduled_at_utc,
                    scheduled_local, timezone_id, state, result, suppression_reason,
                    deferred_until_utc, display_at_utc, snooze_due_at_utc, snooze_count,
                    presented_at_utc, handled_at_utc, merged_into_id, claim_token, claimed_at_utc,
                    created_at_utc, updated_at_utc
             FROM occurrences o
             WHERE o.state = 'delivering'
               AND NOT EXISTS (
                   SELECT 1 FROM delivery_attempts da WHERE da.occurrence_id = o.id
               )
             ORDER BY o.scheduled_at_utc, o.id",
        )
        .fetch_all(self.store.pool())
        .await?)
    }

    pub async fn list_due_deferred(&self, now_utc: i64) -> Result<Vec<Occurrence>> {
        Ok(sqlx::query_as::<_, Occurrence>(
            "SELECT id, reminder_id, reminder_revision, occurrence_key, scheduled_at_utc, \
             scheduled_local, timezone_id, state, result, suppression_reason, \
             deferred_until_utc, display_at_utc, snooze_due_at_utc, snooze_count, \
             presented_at_utc, handled_at_utc, merged_into_id, claim_token, claimed_at_utc, \
             created_at_utc, updated_at_utc FROM occurrences \
             WHERE (state = 'suppressed' AND deferred_until_utc <= ?) \
                OR (state = 'snoozed' AND snooze_due_at_utc <= ?) \
             ORDER BY COALESCE(deferred_until_utc, snooze_due_at_utc), id",
        )
        .bind(now_utc)
        .bind(now_utc)
        .fetch_all(self.store.pool())
        .await?)
    }

    pub async fn list_recoverable_due(&self, now_utc: i64) -> Result<Vec<Occurrence>> {
        Ok(sqlx::query_as::<_, Occurrence>(
            "SELECT id, reminder_id, reminder_revision, occurrence_key, scheduled_at_utc, \
             scheduled_local, timezone_id, state, result, suppression_reason, \
             deferred_until_utc, display_at_utc, snooze_due_at_utc, snooze_count, \
             presented_at_utc, handled_at_utc, merged_into_id, claim_token, claimed_at_utc, \
             created_at_utc, updated_at_utc FROM occurrences \
             WHERE state = 'claimed' \
                OR (state = 'suppressed' AND deferred_until_utc <= ?) \
                OR (state = 'snoozed' AND snooze_due_at_utc <= ?) \
             ORDER BY COALESCE(deferred_until_utc, snooze_due_at_utc, claimed_at_utc), id",
        )
        .bind(now_utc)
        .bind(now_utc)
        .fetch_all(self.store.pool())
        .await?)
    }

    pub async fn next_deferred_due_at(&self, after_utc: i64) -> Result<Option<i64>> {
        Ok(sqlx::query_scalar::<_, Option<i64>>(
            "SELECT MIN(due_at) FROM (\
             SELECT deferred_until_utc AS due_at FROM occurrences \
             WHERE state = 'suppressed' AND deferred_until_utc > ? \
             UNION ALL \
             SELECT snooze_due_at_utc AS due_at FROM occurrences \
             WHERE state = 'snoozed' AND snooze_due_at_utc > ?\
             )",
        )
        .bind(after_utc)
        .bind(after_utc)
        .fetch_one(self.store.pool())
        .await?)
    }

    async fn finish_presented(
        &self,
        id: &str,
        state: &str,
        result: &str,
        reason: Option<&str>,
        handled_at_utc: i64,
    ) -> Result<Occurrence> {
        let updated = sqlx::query_as::<_, Occurrence>(
            "UPDATE occurrences SET state = ?, result = ?, \
             suppression_reason = COALESCE(?, suppression_reason), handled_at_utc = ?, \
             updated_at_utc = ? WHERE id = ? AND state = 'presented' \
             RETURNING id, reminder_id, reminder_revision, occurrence_key, scheduled_at_utc, \
             scheduled_local, timezone_id, state, result, suppression_reason, \
             deferred_until_utc, display_at_utc, snooze_due_at_utc, snooze_count, \
             presented_at_utc, handled_at_utc, merged_into_id, claim_token, claimed_at_utc, \
             created_at_utc, updated_at_utc",
        )
        .bind(state)
        .bind(result)
        .bind(reason)
        .bind(handled_at_utc)
        .bind(handled_at_utc)
        .bind(id)
        .fetch_optional(self.store.pool())
        .await?;

        match updated {
            Some(occurrence) => Ok(occurrence),
            None => Err(self.action_invariant_violation(id, state).await?),
        }
    }

    async fn action_invariant_violation(&self, id: &str, action: &str) -> Result<PersistenceError> {
        let current = sqlx::query_as::<_, (String, i64)>(
            "SELECT state, snooze_count FROM occurrences WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(self.store.pool())
        .await?;

        Ok(PersistenceError::InvariantViolation(match current {
            Some((state, snooze_count)) if action == "snooze" && state == "presented" => {
                format!("occurrence {id} snooze count cannot be incremented past {snooze_count}")
            }
            Some((state, _)) => {
                format!("occurrence {id} cannot {action} from state {state}; expected presented")
            }
            None => format!("occurrence {id} does not exist; cannot {action}"),
        }))
    }
}

const OCCURRENCE_SELECT_BY_ID: &str =
    "SELECT id, reminder_id, reminder_revision, occurrence_key, scheduled_at_utc, \
     scheduled_local, timezone_id, state, result, suppression_reason, deferred_until_utc, \
     display_at_utc, snooze_due_at_utc, snooze_count, presented_at_utc, handled_at_utc, \
     merged_into_id, claim_token, claimed_at_utc, created_at_utc, updated_at_utc \
     FROM occurrences WHERE id = ?";

const OCCURRENCE_SELECT_BY_IDENTITY: &str =
    "SELECT id, reminder_id, reminder_revision, occurrence_key, scheduled_at_utc, \
     scheduled_local, timezone_id, state, result, suppression_reason, deferred_until_utc, \
     display_at_utc, snooze_due_at_utc, snooze_count, presented_at_utc, handled_at_utc, \
     merged_into_id, claim_token, claimed_at_utc, created_at_utc, updated_at_utc FROM occurrences \
     WHERE reminder_id = ? AND occurrence_key = ?";

async fn create_reminder_bundle(
    transaction: &mut Transaction<'_, Sqlite>,
    reminder: &NewReminder,
    rule: Option<&NewScheduleRule>,
    policy: Option<&NewReminderPolicy>,
) -> Result<Reminder> {
    let created = insert_reminder(&mut **transaction, reminder).await?;

    if let Some(rule) = rule {
        sqlx::query(
            "INSERT INTO schedule_rules (id, reminder_id, rule_type, timezone_mode, timezone_id, \
             config_json, created_at_utc, updated_at_utc) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&rule.id)
        .bind(&reminder.id)
        .bind(&rule.rule_type)
        .bind(&rule.timezone_mode)
        .bind(&rule.timezone_id)
        .bind(&rule.config_json)
        .bind(reminder.created_at_utc)
        .bind(reminder.created_at_utc)
        .execute(&mut **transaction)
        .await?;
    }

    if let Some(policy) = policy {
        sqlx::query(
            "INSERT INTO reminder_policies (id, reminder_id, delivery_json, sound_json, \
             snooze_json, missed_json, dnd_json, created_at_utc, updated_at_utc) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&policy.id)
        .bind(&reminder.id)
        .bind(&policy.delivery_json)
        .bind(&policy.sound_json)
        .bind(&policy.snooze_json)
        .bind(&policy.missed_json)
        .bind(&policy.dnd_json)
        .bind(reminder.created_at_utc)
        .bind(reminder.created_at_utc)
        .execute(&mut **transaction)
        .await?;
    }

    Ok(created)
}

async fn insert_reminder<'e, E>(executor: E, reminder: &NewReminder) -> Result<Reminder>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    Ok(sqlx::query_as::<_, Reminder>(
        "INSERT INTO reminders (id, title, description, enabled, revision, created_at_utc, \
         updated_at_utc) VALUES (?, ?, ?, ?, 1, ?, ?) \
         RETURNING id, title, description, enabled, revision, created_at_utc, \
         updated_at_utc, deleted_at_utc",
    )
    .bind(&reminder.id)
    .bind(&reminder.title)
    .bind(&reminder.description)
    .bind(reminder.enabled)
    .bind(reminder.created_at_utc)
    .bind(reminder.created_at_utc)
    .fetch_one(executor)
    .await?)
}

async fn insert_occurrence<'e, E>(
    executor: E,
    occurrence: &NewOccurrence,
) -> Result<Option<Occurrence>>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    Ok(sqlx::query_as::<_, Occurrence>(
        "INSERT INTO occurrences (id, reminder_id, reminder_revision, occurrence_key, \
         scheduled_at_utc, scheduled_local, timezone_id, state, created_at_utc, updated_at_utc) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?) \
         ON CONFLICT(reminder_id, occurrence_key) DO NOTHING \
         RETURNING id, reminder_id, reminder_revision, occurrence_key, scheduled_at_utc, \
         scheduled_local, timezone_id, state, result, suppression_reason, deferred_until_utc, \
         display_at_utc, snooze_due_at_utc, snooze_count, presented_at_utc, handled_at_utc, \
         merged_into_id, claim_token, claimed_at_utc, created_at_utc, updated_at_utc",
    )
    .bind(&occurrence.id)
    .bind(&occurrence.reminder_id)
    .bind(occurrence.reminder_revision)
    .bind(&occurrence.occurrence_key)
    .bind(occurrence.scheduled_at_utc)
    .bind(&occurrence.scheduled_local)
    .bind(&occurrence.timezone_id)
    .bind(occurrence.created_at_utc)
    .bind(occurrence.created_at_utc)
    .fetch_optional(executor)
    .await?)
}

async fn claim_by_identity(
    transaction: &mut Transaction<'_, Sqlite>,
    reminder_id: &str,
    occurrence_key: &str,
    claim_token: &str,
    claimed_at_utc: i64,
) -> Result<Option<Occurrence>> {
    Ok(sqlx::query_as::<_, Occurrence>(
        "UPDATE occurrences SET state = 'claimed', claim_token = ?, claimed_at_utc = ?, \
         updated_at_utc = ? WHERE reminder_id = ? AND occurrence_key = ? \
         AND state = 'pending' AND claim_token IS NULL \
         RETURNING id, reminder_id, reminder_revision, occurrence_key, scheduled_at_utc, \
         scheduled_local, timezone_id, state, result, suppression_reason, deferred_until_utc, \
         display_at_utc, snooze_due_at_utc, snooze_count, presented_at_utc, handled_at_utc, \
         merged_into_id, claim_token, claimed_at_utc, created_at_utc, updated_at_utc",
    )
    .bind(claim_token)
    .bind(claimed_at_utc)
    .bind(claimed_at_utc)
    .bind(reminder_id)
    .bind(occurrence_key)
    .fetch_optional(&mut **transaction)
    .await?)
}
