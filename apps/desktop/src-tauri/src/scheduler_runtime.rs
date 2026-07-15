use async_trait::async_trait;
use chrono::{DateTime, TimeDelta, Utc};
use serde::Deserialize;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use takefive_application::{SqliteCandidateSource, SqliteOccurrenceStore};
use takefive_domain::{AlignedIntervalRule, Clock, FixedTimeRule, OneShotRule, SystemClock};
use takefive_persistence_sqlite::{
    OccurrenceRepository, PauseRepository, PauseSession, ReminderRepository, SqliteStore,
};
use takefive_scheduler::{
    DeliveryError, DeliveryPort, DeliveryRequest, PlannedCandidate, ReconcileCause, RuntimeContext,
    RuntimeContextSource, Scheduler, SchedulerError,
};
use tauri::{AppHandle, Listener};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::reminder_surface::{ReminderSurfacePayload, ReminderSurfaceState};

const STARTUP_LOOKBACK_HOURS: i64 = 24;
const WAKE_COOLDOWN_SECONDS: i64 = 30;
const FALLBACK_RECONCILE_SECONDS: u64 = 30;
const MINIMUM_TIMER_DELAY_MS: u64 = 20;

#[derive(Clone)]
pub(crate) struct SchedulerRuntimeHandle {
    sender: mpsc::UnboundedSender<RuntimeSignal>,
}

impl SchedulerRuntimeHandle {
    pub(crate) fn configuration_changed(&self) {
        let _ = self.sender.send(RuntimeSignal::ConfigurationChanged);
    }
}

pub(crate) fn start(
    app: AppHandle,
    store: SqliteStore,
    reminder_surface: ReminderSurfaceState,
) -> SchedulerRuntimeHandle {
    let (sender, receiver) = mpsc::unbounded_channel();
    let listener_sender = sender.clone();
    app.listen("native-lifecycle-event", move |event| {
        if let Some(signal) = lifecycle_signal(event.payload()) {
            let _ = listener_sender.send(RuntimeSignal::Lifecycle(signal));
        }
    });

    tauri::async_runtime::spawn(run_scheduler(app, store, reminder_surface, receiver));
    SchedulerRuntimeHandle { sender }
}

async fn run_scheduler(
    app: AppHandle,
    store: SqliteStore,
    reminder_surface: ReminderSurfaceState,
    mut receiver: mpsc::UnboundedReceiver<RuntimeSignal>,
) {
    let reminders = ReminderRepository::new(store.clone());
    let occurrences = OccurrenceRepository::new(store.clone());
    let source = SqliteCandidateSource::new(reminders.clone(), occurrences.clone())
        .with_catch_up_limits(1, 64);
    let occurrence_store = SqliteOccurrenceStore::new(occurrences.clone());
    let delivery = TauriNotificationDelivery {
        app: app.clone(),
        reminders: reminders.clone(),
        occurrences: occurrences.clone(),
        reminder_surface: reminder_surface.clone(),
    };
    let session = SessionRuntimeState::recovering_from_startup();
    let contexts = SqliteRuntimeContextSource {
        reminders: reminders.clone(),
        pauses: PauseRepository::new(store),
        session: session.clone(),
    };
    let scheduler = Scheduler::new(SystemClock, source, occurrence_store, delivery.clone());

    let mut since = SystemClock.now_utc() - TimeDelta::hours(STARTUP_LOOKBACK_HOURS);
    let mut cause = ReconcileCause::Startup;

    loop {
        if let Err(error) =
            restore_surface_deliveries(&app, &occurrences, &reminder_surface, SystemClock.now_utc())
                .await
        {
            eprintln!("TakeFive surface queue recovery failed: {error}");
        }
        if let Err(error) =
            recover_unattempted_deliveries(&delivery, &occurrences, SystemClock.now_utc()).await
        {
            eprintln!("TakeFive interrupted delivery recovery failed: {error}");
        }

        let window_end = SystemClock.now_utc();
        if since >= window_end {
            since = window_end - TimeDelta::minutes(5);
        }

        let result = scheduler
            .reconcile_with_context_source(since, cause, &contexts)
            .await;
        let recovered = match result {
            Ok(_) => {
                since = window_end;
                session.complete_recovery(SystemClock.now_utc())
            }
            Err(error) => {
                eprintln!("TakeFive scheduler reconcile failed: {error}");
                false
            }
        };

        let delay = if recovered {
            Duration::from_millis(MINIMUM_TIMER_DELAY_MS)
        } else {
            next_wake_delay(&reminders, &occurrences, SystemClock.now_utc()).await
        };
        match tokio::time::timeout(delay, receiver.recv()).await {
            Ok(Some(signal)) => {
                signal.apply_before_reconcile(&session);
                cause = signal.reconcile_cause();
            }
            Ok(None) => break,
            Err(_) => cause = ReconcileCause::Timer,
        }
    }
}

#[derive(Clone)]
struct TauriNotificationDelivery {
    app: AppHandle,
    reminders: ReminderRepository,
    occurrences: OccurrenceRepository,
    reminder_surface: ReminderSurfaceState,
}

#[async_trait]
impl DeliveryPort for TauriNotificationDelivery {
    async fn deliver(&self, request: DeliveryRequest) -> Result<(), DeliveryError> {
        let reminder = self
            .reminders
            .get(&request.reminder_id)
            .await
            .map_err(|_| delivery_error("reminder_lookup_failed"))?
            .filter(|reminder| reminder.enabled && reminder.deleted_at_utc.is_none())
            .ok_or_else(|| delivery_error("reminder_unavailable"))?;
        let body = if reminder.description.trim().is_empty() {
            "到时间啦，别忘了照顾一下自己。".to_string()
        } else {
            reminder.description
        };

        let payload = ReminderSurfacePayload {
            title: reminder.title.clone(),
            body: body.clone(),
            occurrence_id: request.occurrence_id,
            scheduled_at: request.scheduled_at_utc.to_rfc3339(),
        };
        let attempt_id = Uuid::new_v4().to_string();
        let payload_json = serde_json::to_string(&payload)
            .map_err(|_| delivery_error("reminder_surface_payload_invalid"))?;
        self.occurrences
            .record_surface_delivery_accepted(
                &attempt_id,
                &payload.occurrence_id,
                &payload_json,
                Utc::now().timestamp_millis(),
            )
            .await
            .map_err(|_| delivery_error("reminder_surface_queue_persist_failed"))?;
        let surface_result = self
            .reminder_surface
            .present(&self.app, payload)
            .map_err(|_| delivery_error("reminder_surface_failed"));

        if let Err(error) = &surface_result {
            self.occurrences
                .mark_surface_delivery_failed(
                    &attempt_id,
                    &error.code,
                    Utc::now().timestamp_millis(),
                )
                .await
                .map_err(|_| delivery_error("reminder_surface_failure_persist_failed"))?;
        }

        deliver_surface_first(surface_result, || {
            self.app
                .notification()
                .builder()
                .title(reminder.title)
                .body(body)
                .show()
                .map_err(|_| delivery_error("system_notification_failed"))
        })
        .map(|_| ())
    }
}

async fn restore_surface_deliveries(
    app: &AppHandle,
    occurrences: &OccurrenceRepository,
    reminder_surface: &ReminderSurfaceState,
    recovered_at: DateTime<Utc>,
) -> Result<(), String> {
    for delivery in occurrences
        .list_outstanding_surface_deliveries()
        .await
        .map_err(|error| error.to_string())?
    {
        let payload: ReminderSurfacePayload = serde_json::from_str(&delivery.payload_json)
            .map_err(|_| "reminder_surface_recovery_payload_invalid".to_string())?;
        if payload.occurrence_id != delivery.occurrence_id {
            return Err("reminder_surface_recovery_identity_mismatch".to_string());
        }
        reminder_surface.present(app, payload)?;
        if delivery.occurrence_state == "delivering" {
            occurrences
                .mark_presented(&delivery.occurrence_id, recovered_at.timestamp_millis())
                .await
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

async fn recover_unattempted_deliveries(
    delivery: &TauriNotificationDelivery,
    occurrences: &OccurrenceRepository,
    recovered_at: DateTime<Utc>,
) -> Result<(), String> {
    for occurrence in occurrences
        .list_unattempted_deliveries()
        .await
        .map_err(|error| error.to_string())?
    {
        let scheduled_at_utc = DateTime::from_timestamp_millis(occurrence.scheduled_at_utc)
            .ok_or_else(|| "interrupted_delivery_timestamp_invalid".to_string())?;
        let request = DeliveryRequest {
            occurrence_id: occurrence.id.clone(),
            reminder_id: occurrence.reminder_id,
            scheduled_at_utc,
        };
        match delivery.deliver(request).await {
            Ok(()) => occurrences
                .mark_presented(&occurrence.id, recovered_at.timestamp_millis())
                .await
                .map_err(|error| error.to_string())?,
            Err(error) => occurrences
                .mark_delivery_failed(&occurrence.id, &error.code, recovered_at.timestamp_millis())
                .await
                .map_err(|error| error.to_string())?,
        };
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeliveryRoute {
    SystemNotification,
    ReminderSurface,
}

fn deliver_surface_first<F>(
    surface_result: Result<(), DeliveryError>,
    system_fallback: F,
) -> Result<DeliveryRoute, DeliveryError>
where
    F: FnOnce() -> Result<(), DeliveryError>,
{
    match surface_result {
        Ok(()) => Ok(DeliveryRoute::ReminderSurface),
        Err(_) => system_fallback()
            .map(|_| DeliveryRoute::SystemNotification)
            .map_err(|_| delivery_error("all_delivery_channels_failed")),
    }
}

fn delivery_error(code: &str) -> DeliveryError {
    DeliveryError {
        code: code.to_string(),
    }
}

#[derive(Clone)]
struct SqliteRuntimeContextSource {
    reminders: ReminderRepository,
    pauses: PauseRepository,
    session: SessionRuntimeState,
}

#[async_trait]
impl RuntimeContextSource for SqliteRuntimeContextSource {
    async fn runtime_context(
        &self,
        candidate: &PlannedCandidate,
        now: DateTime<Utc>,
    ) -> Result<RuntimeContext, SchedulerError> {
        let now_millis = now.timestamp_millis();
        let reminder = self
            .reminders
            .get(&candidate.reminder_id)
            .await
            .map_err(context_error)?;
        let global_pauses = self
            .pauses
            .list_active_global(now_millis)
            .await
            .map_err(context_error)?;
        let reminder_pauses = self
            .pauses
            .list_active_for_reminder(&candidate.reminder_id, now_millis)
            .await
            .map_err(context_error)?;
        let session = self.session.snapshot()?;

        Ok(RuntimeContext {
            reminder_enabled: reminder.is_some_and(|reminder| {
                reminder.enabled
                    && reminder.deleted_at_utc.is_none()
                    && reminder.revision == candidate.reminder_revision
            }),
            in_active_window: true,
            reminder_paused_until: active_pause_until(&reminder_pauses),
            global_paused_until: active_pause_until(&global_pauses),
            dnd_until: None,
            session_available: session.session_available,
            wake_cooldown_until: session.wake_cooldown_until,
            fullscreen: fullscreen_for_policy(crate::platform::probe().foreground_fullscreen),
        })
    }
}

fn fullscreen_for_policy(foreground_fullscreen: Option<bool>) -> bool {
    foreground_fullscreen.unwrap_or(true)
}

fn context_error(error: takefive_persistence_sqlite::PersistenceError) -> SchedulerError {
    SchedulerError::RuntimeContextSource(error.to_string())
}

fn active_pause_until(pauses: &[PauseSession]) -> Option<DateTime<Utc>> {
    pauses
        .iter()
        .filter_map(|pause| match pause.ends_at_utc {
            Some(value) => DateTime::from_timestamp_millis(value),
            None => Some(DateTime::<Utc>::MAX_UTC),
        })
        .max()
}

#[derive(Debug, Clone, Copy)]
struct SessionSnapshot {
    session_available: bool,
    wake_cooldown_until: Option<DateTime<Utc>>,
}

#[derive(Debug)]
struct SessionStateInner {
    session_available: bool,
    recovery_pending: bool,
    cooldown_after_recovery: bool,
    wake_cooldown_until: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
struct SessionRuntimeState(Arc<RwLock<SessionStateInner>>);

impl SessionRuntimeState {
    fn recovering_from_startup() -> Self {
        Self(Arc::new(RwLock::new(SessionStateInner {
            session_available: false,
            recovery_pending: true,
            cooldown_after_recovery: false,
            wake_cooldown_until: None,
        })))
    }

    fn mark_unavailable(&self) {
        if let Ok(mut state) = self.0.write() {
            state.session_available = false;
            state.recovery_pending = false;
            state.cooldown_after_recovery = false;
            state.wake_cooldown_until = None;
        }
    }

    fn begin_recovery(&self) {
        if let Ok(mut state) = self.0.write() {
            state.session_available = false;
            state.recovery_pending = true;
            state.cooldown_after_recovery = true;
            state.wake_cooldown_until = None;
        }
    }

    fn complete_recovery(&self, now: DateTime<Utc>) -> bool {
        if let Ok(mut state) = self.0.write() {
            if !state.recovery_pending {
                return false;
            }
            state.session_available = true;
            if state.cooldown_after_recovery {
                state.wake_cooldown_until = Some(now + TimeDelta::seconds(WAKE_COOLDOWN_SECONDS));
            }
            state.recovery_pending = false;
            state.cooldown_after_recovery = false;
            true
        } else {
            false
        }
    }

    fn snapshot(&self) -> Result<SessionSnapshot, SchedulerError> {
        self.0
            .read()
            .map(|state| SessionSnapshot {
                session_available: state.session_available,
                wake_cooldown_until: state.wake_cooldown_until,
            })
            .map_err(|_| SchedulerError::RuntimeContextSource("session_state_poisoned".to_string()))
    }
}

#[derive(Debug, Clone, Copy)]
enum RuntimeSignal {
    ConfigurationChanged,
    Lifecycle(LifecycleSignal),
}

impl RuntimeSignal {
    fn apply_before_reconcile(self, session: &SessionRuntimeState) {
        match self {
            Self::Lifecycle(LifecycleSignal::Sleep | LifecycleSignal::Lock) => {
                session.mark_unavailable();
            }
            Self::Lifecycle(LifecycleSignal::Wake | LifecycleSignal::Unlock) => {
                session.begin_recovery();
            }
            _ => {}
        }
    }

    fn reconcile_cause(self) -> ReconcileCause {
        match self {
            Self::ConfigurationChanged => ReconcileCause::ConfigurationChanged,
            Self::Lifecycle(LifecycleSignal::Wake) => ReconcileCause::Wake,
            Self::Lifecycle(LifecycleSignal::Unlock) => ReconcileCause::Unlock,
            Self::Lifecycle(LifecycleSignal::TimeChanged) => ReconcileCause::TimeChanged,
            Self::Lifecycle(LifecycleSignal::TimezoneChanged) => ReconcileCause::TimezoneChanged,
            Self::Lifecycle(LifecycleSignal::Sleep | LifecycleSignal::Lock) => {
                ReconcileCause::Timer
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleSignal {
    Sleep,
    Wake,
    Lock,
    Unlock,
    TimeChanged,
    TimezoneChanged,
}

#[derive(Deserialize)]
struct LifecyclePayload {
    kind: String,
}

fn lifecycle_signal(payload: &str) -> Option<LifecycleSignal> {
    let payload: LifecyclePayload = serde_json::from_str(payload).ok()?;
    match payload.kind.as_str() {
        "sleep" => Some(LifecycleSignal::Sleep),
        "wake" => Some(LifecycleSignal::Wake),
        "lock" => Some(LifecycleSignal::Lock),
        "unlock" => Some(LifecycleSignal::Unlock),
        "time_changed" => Some(LifecycleSignal::TimeChanged),
        "timezone_changed" => Some(LifecycleSignal::TimezoneChanged),
        _ => None,
    }
}

async fn next_wake_delay(
    reminders: &ReminderRepository,
    occurrences: &OccurrenceRepository,
    now: DateTime<Utc>,
) -> Duration {
    let next_rule = next_rule_due_at(reminders, now).await;
    let next_deferred = occurrences
        .next_deferred_due_at(now.timestamp_millis())
        .await
        .ok()
        .flatten()
        .and_then(DateTime::from_timestamp_millis);

    bounded_wake_delay(now, next_rule.into_iter().chain(next_deferred))
}

async fn next_rule_due_at(
    reminders: &ReminderRepository,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    reminders
        .list_scheduled_enabled()
        .await
        .ok()?
        .into_iter()
        .filter_map(|record| rule_due_at(&record.rule_type, &record.rule_config_json, now))
        .min()
}

fn rule_due_at(
    rule_type: &str,
    rule_config_json: &str,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    match rule_type {
        "fixed_times" => serde_json::from_str::<FixedTimeRule>(rule_config_json)
            .ok()?
            .next_after(now),
        "one_shot" => serde_json::from_str::<OneShotRule>(rule_config_json)
            .ok()?
            .next_after(now),
        "aligned_interval" => serde_json::from_str::<AlignedIntervalRule>(rule_config_json)
            .ok()?
            .next_after(now)
            .map(|candidate| candidate.schedule),
        _ => None,
    }
    .map(|candidate| candidate.scheduled_at_utc)
}

fn bounded_wake_delay(
    now: DateTime<Utc>,
    deadlines: impl IntoIterator<Item = DateTime<Utc>>,
) -> Duration {
    let fallback = Duration::from_secs(FALLBACK_RECONCILE_SECONDS);
    let Some(deadline) = deadlines.into_iter().min() else {
        return fallback;
    };
    let milliseconds = (deadline - now)
        .num_milliseconds()
        .max(MINIMUM_TIMER_DELAY_MS as i64) as u64;
    Duration::from_millis(milliseconds).min(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::sync::atomic::{AtomicBool, Ordering};
    use takefive_persistence_sqlite::{NewPauseSession, PauseScope};

    #[test]
    fn native_lifecycle_payload_maps_only_scheduler_relevant_events() {
        assert_eq!(
            lifecycle_signal(r#"{"kind":"wake","observedAt":"2026-07-14T08:00:00Z"}"#),
            Some(LifecycleSignal::Wake)
        );
        assert_eq!(lifecycle_signal(r#"{"kind":"display_changed"}"#), None);
        assert_eq!(lifecycle_signal("not-json"), None);
    }

    #[test]
    fn unknown_fullscreen_probe_is_suppressed_conservatively() {
        assert!(fullscreen_for_policy(None));
        assert!(fullscreen_for_policy(Some(true)));
        assert!(!fullscreen_for_policy(Some(false)));
    }

    #[test]
    fn wake_delay_uses_the_earliest_deadline_and_caps_the_fallback() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 8, 0, 0).unwrap();
        let delay = bounded_wake_delay(
            now,
            [now + TimeDelta::seconds(20), now + TimeDelta::seconds(2)],
        );
        assert_eq!(delay, Duration::from_secs(2));
        assert_eq!(
            bounded_wake_delay(now, [now + TimeDelta::minutes(5)]),
            Duration::from_secs(FALLBACK_RECONCILE_SECONDS)
        );
    }

    #[test]
    fn one_shot_rule_contributes_its_absolute_instant_to_the_next_wake() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 8, 0, 0).unwrap();
        let at = now + TimeDelta::seconds(12);
        let rule = OneShotRule::new(at, chrono_tz::Asia::Shanghai);
        let json = serde_json::to_string(&rule).unwrap();

        assert_eq!(rule_due_at("one_shot", &json, now), Some(at));
        assert_eq!(rule_due_at("one_shot", &json, at), None);
        assert_eq!(rule_due_at("unknown", &json, now), None);
    }

    #[test]
    fn aligned_interval_rule_contributes_the_next_anchor_slot_to_the_next_wake() {
        let timezone = chrono_tz::Asia::Shanghai;
        let anchor = timezone
            .with_ymd_and_hms(2026, 7, 15, 9, 0, 0)
            .unwrap()
            .naive_local();
        let rule = AlignedIntervalRule::new(timezone, anchor, 60, Vec::new()).unwrap();
        let json = serde_json::to_string(&rule).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 15, 1, 1, 0).unwrap();

        assert_eq!(
            rule_due_at("aligned_interval", &json, now),
            Some(Utc.with_ymd_and_hms(2026, 7, 15, 2, 0, 0).unwrap())
        );
    }

    #[test]
    fn overlapping_pause_uses_the_latest_end_and_supports_indefinite_pause() {
        let finite = PauseSession {
            id: "finite".to_string(),
            scope: PauseScope::Global,
            starts_at_utc: 100,
            ends_at_utc: Some(500),
            cancelled_at_utc: None,
            reason: None,
            created_at_utc: 100,
        };
        let mut later = finite.clone();
        later.id = "later".to_string();
        later.ends_at_utc = Some(900);
        assert_eq!(
            active_pause_until(&[finite.clone(), later]),
            DateTime::from_timestamp_millis(900)
        );

        let indefinite = NewPauseSession::global("forever", 100, None, 100);
        let indefinite = PauseSession {
            id: indefinite.id,
            scope: indefinite.scope,
            starts_at_utc: indefinite.starts_at_utc,
            ends_at_utc: indefinite.ends_at_utc,
            cancelled_at_utc: None,
            reason: indefinite.reason,
            created_at_utc: indefinite.created_at_utc,
        };
        assert_eq!(
            active_pause_until(&[finite, indefinite]),
            Some(DateTime::<Utc>::MAX_UTC)
        );
    }

    #[test]
    fn successful_reminder_surface_does_not_send_a_second_notification() {
        let notification_called = AtomicBool::new(false);

        let route = deliver_surface_first(Ok(()), || {
            notification_called.store(true, Ordering::SeqCst);
            Ok(())
        })
        .unwrap();

        assert_eq!(route, DeliveryRoute::ReminderSurface);
        assert!(!notification_called.load(Ordering::SeqCst));
    }

    #[test]
    fn failed_surface_uses_system_notification_as_the_last_channel() {
        let route =
            deliver_surface_first(Err(delivery_error("reminder_surface_failed")), || Ok(()))
                .unwrap();
        assert_eq!(route, DeliveryRoute::SystemNotification);

        let error = deliver_surface_first(Err(delivery_error("reminder_surface_failed")), || {
            Err(delivery_error("system_notification_failed"))
        })
        .unwrap_err();
        assert_eq!(error.code, "all_delivery_channels_failed");
    }
}
