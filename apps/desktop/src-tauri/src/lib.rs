mod platform;
mod reminder_settings;
mod reminder_surface;
mod scheduler_runtime;

use chrono::{DateTime, LocalResult, NaiveDateTime, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use takefive_application::OccurrenceActionService;
use takefive_domain::{
    ActiveWindow, AlignedIntervalRule, Clock, FixedTimeRule, OneShotRule, SystemClock,
};
use takefive_persistence_sqlite::{
    NewPauseSession, NewReminder, NewReminderBundle, NewReminderPolicy, NewScheduleRule,
    Occurrence, OccurrenceRepository, PauseRepository, PauseSession, Reminder as StoredReminder,
    ReminderChanges, ReminderRepository, ScheduleRuleChanges, ScheduledReminderRecord, SqliteStore,
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use uuid::Uuid;

use reminder_settings::{ReminderSettings, REMINDER_SETTINGS_KEY};
use reminder_surface::{ReminderSurfacePayload, ReminderSurfaceState, REMINDER_SURFACE_LABEL};

const ONBOARDING_SETTING_KEY: &str = "onboarding.v1";
const AUTOSTART_SETTING_KEY: &str = "autostart.v1";
const AUTOSTART_CONFIGURED_JSON: &str = r#"{"configured":true}"#;

#[derive(Clone)]
struct AppState {
    store: SqliteStore,
    reminders: ReminderRepository,
    pauses: PauseRepository,
    occurrence_actions: OccurrenceActionService,
    scheduler: scheduler_runtime::SchedulerRuntimeHandle,
    database_path: PathBuf,
}

#[derive(Default)]
struct BackgroundModeState {
    tray_available: AtomicBool,
}

impl BackgroundModeState {
    fn set_tray_available(&self, available: bool) {
        self.tray_available.store(available, Ordering::Release);
    }

    fn tray_available(&self) -> bool {
        self.tray_available.load(Ordering::Acquire)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SchedulePreviewRequest {
    timezone: String,
    local_time: String,
    weekdays: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SchedulePreview {
    scheduled_at_utc: String,
    scheduled_local: String,
    occurrence_key: String,
    dst_adjusted: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateReminderInput {
    name: String,
    description: Option<String>,
    local_time: String,
    timezone: String,
    weekdays: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateOneShotReminderInput {
    name: String,
    description: Option<String>,
    local_date_time: String,
    timezone: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateAlignedIntervalReminderInput {
    name: String,
    description: Option<String>,
    interval_minutes: u32,
    anchor_local_date_time: String,
    timezone: String,
    weekdays: Vec<String>,
    active_window_start: Option<String>,
    active_window_end: Option<String>,
    excluded_window_start: Option<String>,
    excluded_window_end: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateReminderInput {
    id: String,
    expected_revision: i64,
    name: String,
    description: Option<String>,
    kind: String,
    #[serde(default)]
    local_time: Option<String>,
    #[serde(default)]
    local_date_time: Option<String>,
    #[serde(default)]
    interval_minutes: Option<u32>,
    #[serde(default)]
    anchor_local_date_time: Option<String>,
    timezone: String,
    #[serde(default)]
    weekdays: Vec<String>,
    #[serde(default)]
    active_window_start: Option<String>,
    #[serde(default)]
    active_window_end: Option<String>,
    #[serde(default)]
    excluded_window_start: Option<String>,
    #[serde(default)]
    excluded_window_end: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StorageStatus {
    schema_version: i64,
    healthy: bool,
    reminder_count: usize,
    database_path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OnboardingStatus {
    completed: bool,
    needs_setup: bool,
    has_reminders: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitializeHealthTemplateInput {
    timezone: String,
    #[serde(default)]
    locale: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OnboardingSetting {
    completed: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReminderView {
    id: String,
    name: String,
    description: String,
    enabled: bool,
    revision: i64,
    created_at_utc: i64,
    rule_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rule: Option<ReminderRuleDetails>,
    next_trigger_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReminderRuleDetails {
    kind: String,
    timezone: String,
    weekdays: Vec<String>,
    times: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_date_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    interval_minutes: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_window_start: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_window_end: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    excluded_window_start: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    excluded_window_end: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    anchor_local_date_time: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OccurrenceActionView {
    id: String,
    state: String,
    result: Option<String>,
    snooze_due_at_utc: Option<i64>,
    snooze_count: i64,
    handled_at_utc: Option<i64>,
}

impl From<Occurrence> for OccurrenceActionView {
    fn from(value: Occurrence) -> Self {
        Self {
            id: value.id,
            state: value.state,
            result: value.result,
            snooze_due_at_utc: value.snooze_due_at_utc,
            snooze_count: value.snooze_count,
            handled_at_utc: value.handled_at_utc,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PauseStatus {
    is_paused: bool,
    paused_until: Option<String>,
    active_session_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AutostartStatus {
    available: bool,
    enabled: Option<bool>,
    error: Option<String>,
}

impl From<StoredReminder> for ReminderView {
    fn from(value: StoredReminder) -> Self {
        Self {
            id: value.id,
            name: value.title,
            description: value.description,
            enabled: value.enabled,
            revision: value.revision,
            created_at_utc: value.created_at_utc,
            rule_summary: None,
            rule: None,
            next_trigger_at: None,
        }
    }
}

struct ValidatedReminderInput {
    name: String,
    description: String,
    local_time: NaiveTime,
    timezone: Tz,
    weekdays: Vec<Weekday>,
}

struct ValidatedOneShotReminderInput {
    name: String,
    description: String,
    at_utc: DateTime<Utc>,
    source_timezone: Tz,
}

struct ValidatedAlignedIntervalReminderInput {
    name: String,
    description: String,
    rule: AlignedIntervalRule,
}

#[derive(Debug, Deserialize)]
struct StoredFixedTimeRule {
    timezone: Tz,
    weekdays: Vec<Weekday>,
    times: Vec<NaiveTime>,
}

#[derive(Debug, Deserialize)]
struct StoredOneShotRule {
    at_utc: DateTime<Utc>,
    source_timezone: Tz,
}

struct RuleView {
    summary: String,
    details: ReminderRuleDetails,
    next_trigger_at: Option<String>,
}

#[tauri::command]
fn get_reminder_surface_payload(
    state: tauri::State<'_, ReminderSurfaceState>,
) -> Result<Option<ReminderSurfacePayload>, String> {
    state.latest()
}

#[tauri::command]
async fn preview_reminder(
    id: String,
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    surface: tauri::State<'_, ReminderSurfaceState>,
) -> Result<ReminderSurfacePayload, String> {
    let reminder = state
        .reminders
        .get(&id)
        .await
        .map_err(|error| error.to_string())?
        .filter(|reminder| reminder.deleted_at_utc.is_none())
        .ok_or_else(|| format!("reminder_not_found: {id}"))?;
    let body = if reminder.description.trim().is_empty() {
        "到时间啦，别忘了照顾一下自己。".to_string()
    } else {
        reminder.description
    };
    let payload = ReminderSurfacePayload {
        title: reminder.title,
        body,
        occurrence_id: format!("preview:{}", reminder.id),
        scheduled_at: Utc::now().to_rfc3339(),
        preview: true,
    };
    surface.present(&app, payload.clone())?;
    Ok(payload)
}

#[tauri::command]
fn dismiss_reminder_preview(
    id: String,
    app: AppHandle,
    surface: tauri::State<'_, ReminderSurfaceState>,
) -> Result<(), String> {
    surface.finish_preview(&app, &id)
}

#[tauri::command]
fn probe_platform() -> platform::PlatformProbe {
    platform::probe()
}

#[tauri::command]
fn preview_schedule(request: SchedulePreviewRequest) -> Result<SchedulePreview, String> {
    let timezone = Tz::from_str(&request.timezone)
        .map_err(|_| format!("无法识别时区：{}", request.timezone))?;
    let local_time = NaiveTime::parse_from_str(&request.local_time, "%H:%M")
        .map_err(|_| "时间格式应为 HH:mm".to_string())?;
    let weekdays = request
        .weekdays
        .iter()
        .map(|day| parse_weekday(day))
        .collect::<Result<Vec<_>, _>>()?;
    let rule = FixedTimeRule::new(timezone, weekdays, vec![local_time])
        .map_err(|error| error.to_string())?;
    let now = SystemClock.now_utc();
    let candidate = rule
        .next_after(now)
        .ok_or_else(|| "未能在计算范围内找到下一次提醒".to_string())?;

    Ok(SchedulePreview {
        scheduled_at_utc: candidate.scheduled_at_utc.to_rfc3339(),
        scheduled_local: candidate
            .scheduled_at_utc
            .with_timezone(&timezone)
            .format("%Y-%m-%d %H:%M:%S %:z")
            .to_string(),
        occurrence_key: candidate.occurrence_key(),
        dst_adjusted: candidate.dst_adjusted,
    })
}

#[tauri::command]
async fn storage_status(state: tauri::State<'_, AppState>) -> Result<StorageStatus, String> {
    let store = state.store.clone();
    let reminders = state.reminders.clone();
    Ok(StorageStatus {
        schema_version: store
            .schema_version()
            .await
            .map_err(|error| error.to_string())?,
        healthy: store
            .quick_check()
            .await
            .map_err(|error| error.to_string())?,
        reminder_count: reminders
            .list(false)
            .await
            .map_err(|error| error.to_string())?
            .len(),
        database_path: state.database_path.display().to_string(),
    })
}

#[tauri::command]
async fn get_reminder_settings(
    timezone: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<ReminderSettings, String> {
    load_or_initialize_reminder_settings(&state.store, timezone).await
}

async fn load_or_initialize_reminder_settings(
    store: &SqliteStore,
    default_timezone: Option<String>,
) -> Result<ReminderSettings, String> {
    let value = store
        .get_setting_json(REMINDER_SETTINGS_KEY)
        .await
        .map_err(|error| error.to_string())?;
    let settings = ReminderSettings::load(value.as_deref(), default_timezone)?;
    if value.is_none() {
        let serialized = serde_json::to_string(&settings).map_err(|error| error.to_string())?;
        store
            .set_setting_json(
                REMINDER_SETTINGS_KEY,
                &serialized,
                Utc::now().timestamp_millis(),
            )
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(settings)
}

#[tauri::command]
async fn update_reminder_settings(
    input: ReminderSettings,
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<ReminderSettings, String> {
    let settings = input.validate()?;
    let value = serde_json::to_string(&settings).map_err(|error| error.to_string())?;
    state
        .store
        .set_setting_json(REMINDER_SETTINGS_KEY, &value, Utc::now().timestamp_millis())
        .await
        .map_err(|error| error.to_string())?;
    state.scheduler.configuration_changed();
    let _ = app.emit("settings-changed", ());
    Ok(settings)
}

#[tauri::command]
async fn get_onboarding_status(
    state: tauri::State<'_, AppState>,
) -> Result<OnboardingStatus, String> {
    let setting_json = state
        .store
        .get_setting_json(ONBOARDING_SETTING_KEY)
        .await
        .map_err(|error| error.to_string())?;
    let reminder_history = state
        .reminders
        .list(true)
        .await
        .map_err(|error| error.to_string())?;
    onboarding_status_from_parts(setting_json.as_deref(), &reminder_history)
}

fn onboarding_status_from_parts(
    setting_json: Option<&str>,
    reminder_history: &[StoredReminder],
) -> Result<OnboardingStatus, String> {
    let has_reminders = reminder_history
        .iter()
        .any(|reminder| reminder.deleted_at_utc.is_none());
    let completed = match setting_json {
        Some(value) => {
            serde_json::from_str::<OnboardingSetting>(value)
                .map_err(|error| format!("invalid_onboarding_setting: {error}"))?
                .completed
        }
        // Existing databases predate the onboarding setting. Reminder history,
        // including deleted starter templates, means setup already happened.
        None => !reminder_history.is_empty(),
    };

    Ok(OnboardingStatus {
        completed,
        needs_setup: !completed,
        has_reminders,
    })
}

#[tauri::command]
async fn complete_onboarding(
    state: tauri::State<'_, AppState>,
) -> Result<OnboardingStatus, String> {
    state
        .store
        .set_setting_json(
            ONBOARDING_SETTING_KEY,
            r#"{"completed":true}"#,
            Utc::now().timestamp_millis(),
        )
        .await
        .map_err(|error| error.to_string())?;
    get_onboarding_status(state).await
}

/// Explicitly applies the starter health template after the user has chosen
/// a locale/time zone. Startup never calls this automatically.
#[tauri::command]
async fn initialize_default_health_reminders(
    input: InitializeHealthTemplateInput,
    state: tauri::State<'_, AppState>,
) -> Result<OnboardingStatus, String> {
    let timezone =
        Tz::from_str(&input.timezone).map_err(|_| format!("无法识别时区：{}", input.timezone))?;
    let locale = input.locale.as_deref().unwrap_or("en-US");
    seed_default_health_reminders_for(&state.reminders, timezone, locale).await?;
    state
        .store
        .set_setting_json(
            ONBOARDING_SETTING_KEY,
            r#"{"completed":true}"#,
            Utc::now().timestamp_millis(),
        )
        .await
        .map_err(|error| error.to_string())?;
    state.scheduler.configuration_changed();
    get_onboarding_status(state).await
}

#[tauri::command]
async fn list_reminders(state: tauri::State<'_, AppState>) -> Result<Vec<ReminderView>, String> {
    let reminders = state
        .reminders
        .list(false)
        .await
        .map_err(|error| error.to_string())?;
    let scheduled = state
        .reminders
        .list_configured()
        .await
        .map_err(|error| error.to_string())?;

    Ok(merge_reminder_views(
        reminders,
        scheduled,
        SystemClock.now_utc(),
    ))
}

#[tauri::command]
async fn create_reminder(
    input: CreateReminderInput,
    state: tauri::State<'_, AppState>,
) -> Result<ReminderView, String> {
    let input = validate_reminder_input(input)?;
    let now = Utc::now().timestamp_millis();
    let mut reminder = NewReminder::new(Uuid::new_v4().to_string(), input.name, now);
    reminder.description = input.description;
    let weekday_label = weekday_label(&input.weekdays);
    let rule_details = fixed_rule_details(input.timezone, &input.weekdays, &[input.local_time]);
    let rule = FixedTimeRule::new(input.timezone, input.weekdays, vec![input.local_time])
        .map_err(|error| error.to_string())?;
    let stored_rule = NewScheduleRule {
        id: Uuid::new_v4().to_string(),
        rule_type: "fixed_times".to_string(),
        // The rule stores an explicit IANA zone and therefore remains stable
        // when the machine travels. A future follow-system mode must rewrite
        // the rule timezone instead of merely changing this metadata.
        timezone_mode: "named".to_string(),
        timezone_id: Some(input.timezone.to_string()),
        config_json: serde_json::to_string(&rule).map_err(|error| error.to_string())?,
    };
    let stored_policy = NewReminderPolicy::defaults(Uuid::new_v4().to_string());
    let created = state
        .reminders
        .create_with_configuration(&reminder, Some(&stored_rule), Some(&stored_policy))
        .await
        .map_err(|error| error.to_string())?;
    let next = rule.next_after(SystemClock.now_utc());
    let mut view = ReminderView::from(created);
    view.rule_summary = Some(format!(
        "{} · {}",
        weekday_label,
        input.local_time.format("%H:%M")
    ));
    view.rule = Some(rule_details);
    view.next_trigger_at = next.map(|candidate| candidate.scheduled_at_utc.to_rfc3339());
    state.scheduler.configuration_changed();
    Ok(view)
}

#[tauri::command]
async fn create_one_shot_reminder(
    input: CreateOneShotReminderInput,
    state: tauri::State<'_, AppState>,
) -> Result<ReminderView, String> {
    let now = SystemClock.now_utc();
    let input = validate_one_shot_reminder_input(input, now)?;
    let mut reminder = NewReminder::new(
        Uuid::new_v4().to_string(),
        input.name,
        now.timestamp_millis(),
    );
    reminder.description = input.description;
    let rule = OneShotRule::new(input.at_utc, input.source_timezone);
    let stored_rule = NewScheduleRule {
        id: Uuid::new_v4().to_string(),
        rule_type: "one_shot".to_string(),
        timezone_mode: "named".to_string(),
        timezone_id: Some(input.source_timezone.to_string()),
        config_json: serde_json::to_string(&rule).map_err(|error| error.to_string())?,
    };
    let stored_policy = NewReminderPolicy::defaults(Uuid::new_v4().to_string());
    let created = state
        .reminders
        .create_with_configuration(&reminder, Some(&stored_rule), Some(&stored_policy))
        .await
        .map_err(|error| error.to_string())?;
    let mut view = ReminderView::from(created);
    let rule_view = one_shot_rule_view_from_parts(input.at_utc, input.source_timezone, now);
    view.rule_summary = Some(rule_view.summary);
    view.rule = Some(rule_view.details);
    view.next_trigger_at = rule_view.next_trigger_at;
    state.scheduler.configuration_changed();
    Ok(view)
}

#[tauri::command]
async fn create_aligned_interval_reminder(
    input: CreateAlignedIntervalReminderInput,
    state: tauri::State<'_, AppState>,
) -> Result<ReminderView, String> {
    let input = validate_aligned_interval_reminder_input(input)?;
    let now = SystemClock.now_utc();
    let mut reminder = NewReminder::new(
        Uuid::new_v4().to_string(),
        input.name,
        now.timestamp_millis(),
    );
    reminder.description = input.description;
    let stored_rule = NewScheduleRule {
        id: Uuid::new_v4().to_string(),
        rule_type: "aligned_interval".to_string(),
        timezone_mode: "named".to_string(),
        timezone_id: Some(input.rule.timezone().to_string()),
        config_json: serde_json::to_string(&input.rule).map_err(|error| error.to_string())?,
    };
    let stored_policy = NewReminderPolicy::defaults(Uuid::new_v4().to_string());
    let created = state
        .reminders
        .create_with_configuration(&reminder, Some(&stored_rule), Some(&stored_policy))
        .await
        .map_err(|error| error.to_string())?;
    let mut view = ReminderView::from(created);
    let rule_view = aligned_interval_rule_view_from_rule(&input.rule, now);
    view.rule_summary = Some(rule_view.summary);
    view.rule = Some(rule_view.details);
    view.next_trigger_at = rule_view.next_trigger_at;
    state.scheduler.configuration_changed();
    Ok(view)
}

#[tauri::command(rename_all = "camelCase")]
async fn update_reminder(
    input: UpdateReminderInput,
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<ReminderView, String> {
    let existing = state
        .reminders
        .get(&input.id)
        .await
        .map_err(|error| error.to_string())?
        .filter(|reminder| reminder.deleted_at_utc.is_none())
        .ok_or_else(|| format!("reminder_not_found: {}", input.id))?;
    let (name, description, rule_changes, rule_view) =
        update_rule_parts(&input, SystemClock.now_utc())?;
    let updated = state
        .reminders
        .update_with_configuration(
            &input.id,
            input.expected_revision,
            &ReminderChanges {
                title: name,
                description,
                enabled: existing.enabled,
                updated_at_utc: Utc::now().timestamp_millis(),
            },
            &rule_changes,
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut view = ReminderView::from(updated);
    view.rule_summary = Some(rule_view.summary);
    view.rule = Some(rule_view.details);
    view.next_trigger_at = rule_view.next_trigger_at;
    state.scheduler.configuration_changed();
    let _ = app.emit("reminders-changed", ());
    Ok(view)
}

#[tauri::command(rename_all = "camelCase")]
async fn set_reminder_enabled(
    id: String,
    expected_revision: i64,
    enabled: bool,
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<ReminderView, String> {
    let existing = state
        .reminders
        .get(&id)
        .await
        .map_err(|error| error.to_string())?
        .filter(|reminder| reminder.deleted_at_utc.is_none())
        .ok_or_else(|| format!("reminder_not_found: {id}"))?;
    let updated = state
        .reminders
        .update(
            &id,
            expected_revision,
            &ReminderChanges {
                title: existing.title,
                description: existing.description,
                enabled,
                updated_at_utc: Utc::now().timestamp_millis(),
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    state.scheduler.configuration_changed();
    let _ = app.emit("reminders-changed", ());
    Ok(ReminderView::from(updated))
}

#[tauri::command]
async fn complete_occurrence(
    id: String,
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    surface: tauri::State<'_, ReminderSurfaceState>,
) -> Result<OccurrenceActionView, String> {
    let occurrence = state
        .occurrence_actions
        .complete(&id, Utc::now().timestamp_millis())
        .await
        .map_err(|error| error.to_string())?;
    Ok(finish_occurrence_action(&app, &surface, &id, occurrence))
}

#[tauri::command]
async fn skip_occurrence(
    id: String,
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    surface: tauri::State<'_, ReminderSurfaceState>,
) -> Result<OccurrenceActionView, String> {
    let occurrence = state
        .occurrence_actions
        .skip(&id, Utc::now().timestamp_millis())
        .await
        .map_err(|error| error.to_string())?;
    Ok(finish_occurrence_action(&app, &surface, &id, occurrence))
}

#[tauri::command(rename_all = "camelCase")]
async fn snooze_occurrence(
    id: String,
    minutes: i64,
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    surface: tauri::State<'_, ReminderSurfaceState>,
) -> Result<OccurrenceActionView, String> {
    let occurrence = state
        .occurrence_actions
        .snooze(&id, minutes, Utc::now().timestamp_millis())
        .await
        .map_err(|error| error.to_string())?;
    state.scheduler.configuration_changed();
    Ok(finish_occurrence_action(&app, &surface, &id, occurrence))
}

#[tauri::command]
async fn mark_occurrence_unhandled(
    id: String,
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    surface: tauri::State<'_, ReminderSurfaceState>,
) -> Result<OccurrenceActionView, String> {
    let occurrence = state
        .occurrence_actions
        .mark_unhandled(&id, Utc::now().timestamp_millis())
        .await
        .map_err(|error| error.to_string())?;
    Ok(finish_occurrence_action(&app, &surface, &id, occurrence))
}

#[tauri::command]
async fn get_pause_status(state: tauri::State<'_, AppState>) -> Result<PauseStatus, String> {
    load_pause_status(&state.pauses, Utc::now().timestamp_millis()).await
}

#[tauri::command]
async fn pause_all(
    minutes: i64,
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<PauseStatus, String> {
    if !(1..=1_440).contains(&minutes) {
        return Err("暂停时长必须为 1-1440 分钟".to_string());
    }
    let now = Utc::now().timestamp_millis();
    let ends_at = now
        .checked_add(
            minutes
                .checked_mul(60_000)
                .ok_or_else(|| "暂停时长超出范围".to_string())?,
        )
        .ok_or_else(|| "暂停结束时间超出范围".to_string())?;
    cancel_active_global_pauses(&state.pauses, now).await?;
    let mut pause = NewPauseSession::global(Uuid::new_v4().to_string(), now, Some(ends_at), now);
    pause.reason = Some("user".to_string());
    state
        .pauses
        .create(&pause)
        .await
        .map_err(|error| error.to_string())?;
    state.scheduler.configuration_changed();
    let _ = app.emit("reminders-changed", ());
    load_pause_status(&state.pauses, now).await
}

#[tauri::command]
async fn resume_all(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<PauseStatus, String> {
    let now = Utc::now().timestamp_millis();
    cancel_active_global_pauses(&state.pauses, now).await?;
    state.scheduler.configuration_changed();
    let _ = app.emit("reminders-changed", ());
    load_pause_status(&state.pauses, now).await
}

#[tauri::command]
fn get_autostart_status(app: AppHandle) -> AutostartStatus {
    query_autostart_status(&app)
}

#[tauri::command]
async fn set_autostart_enabled(
    enabled: bool,
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<AutostartStatus, String> {
    let manager = app.autolaunch();
    let result = if enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    result.map_err(|error| format!("autostart_update_unavailable: {error}"))?;

    let status = query_autostart_status(&app);
    if status.enabled == Some(enabled) {
        state
            .store
            .set_setting_json(
                AUTOSTART_SETTING_KEY,
                AUTOSTART_CONFIGURED_JSON,
                Utc::now().timestamp_millis(),
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(status)
    } else if let Some(error) = status.error {
        Err(error)
    } else {
        Err("autostart_verification_failed".to_string())
    }
}

#[tauri::command]
async fn delete_reminder(id: String, state: tauri::State<'_, AppState>) -> Result<bool, String> {
    let deleted = state
        .reminders
        .soft_delete(&id, Utc::now().timestamp_millis())
        .await
        .map_err(|error| error.to_string())?;
    if deleted {
        state.scheduler.configuration_changed();
    }
    Ok(deleted)
}

fn update_rule_parts(
    input: &UpdateReminderInput,
    now: DateTime<Utc>,
) -> Result<(String, String, ScheduleRuleChanges, RuleView), String> {
    match input.kind.as_str() {
        "fixed" => {
            let validated = validate_reminder_input(CreateReminderInput {
                name: input.name.clone(),
                description: input.description.clone(),
                local_time: input
                    .local_time
                    .clone()
                    .ok_or_else(|| "提醒时间格式应为 HH:mm".to_string())?,
                timezone: input.timezone.clone(),
                weekdays: input.weekdays.clone(),
            })?;
            let rule = FixedTimeRule::new(
                validated.timezone,
                validated.weekdays.clone(),
                vec![validated.local_time],
            )
            .map_err(|error| error.to_string())?;
            let rule_view = RuleView {
                summary: format!(
                    "{} · {}",
                    weekday_label(&validated.weekdays),
                    validated.local_time.format("%H:%M")
                ),
                details: fixed_rule_details(
                    validated.timezone,
                    &validated.weekdays,
                    &[validated.local_time],
                ),
                next_trigger_at: rule
                    .next_after(now)
                    .map(|candidate| candidate.scheduled_at_utc.to_rfc3339()),
            };
            Ok((
                validated.name,
                validated.description,
                ScheduleRuleChanges {
                    rule_type: "fixed_times".to_string(),
                    timezone_mode: "named".to_string(),
                    timezone_id: Some(validated.timezone.to_string()),
                    config_json: serde_json::to_string(&rule).map_err(|error| error.to_string())?,
                },
                rule_view,
            ))
        }
        "oneShot" => {
            let validated = validate_one_shot_reminder_input(
                CreateOneShotReminderInput {
                    name: input.name.clone(),
                    description: input.description.clone(),
                    local_date_time: input
                        .local_date_time
                        .clone()
                        .ok_or_else(|| "一次性提醒时间格式应为 YYYY-MM-DDTHH:mm".to_string())?,
                    timezone: input.timezone.clone(),
                },
                now,
            )?;
            let rule = OneShotRule::new(validated.at_utc, validated.source_timezone);
            let rule_view =
                one_shot_rule_view_from_parts(validated.at_utc, validated.source_timezone, now);
            Ok((
                validated.name,
                validated.description,
                ScheduleRuleChanges {
                    rule_type: "one_shot".to_string(),
                    timezone_mode: "named".to_string(),
                    timezone_id: Some(validated.source_timezone.to_string()),
                    config_json: serde_json::to_string(&rule).map_err(|error| error.to_string())?,
                },
                rule_view,
            ))
        }
        "interval" => {
            let validated =
                validate_aligned_interval_reminder_input(CreateAlignedIntervalReminderInput {
                    name: input.name.clone(),
                    description: input.description.clone(),
                    interval_minutes: input
                        .interval_minutes
                        .ok_or_else(|| "间隔时长必须为 1-1440 分钟".to_string())?,
                    anchor_local_date_time: input
                        .anchor_local_date_time
                        .clone()
                        .ok_or_else(|| "锚点时间格式应为 YYYY-MM-DDTHH:mm".to_string())?,
                    timezone: input.timezone.clone(),
                    weekdays: input.weekdays.clone(),
                    active_window_start: input.active_window_start.clone(),
                    active_window_end: input.active_window_end.clone(),
                    excluded_window_start: input.excluded_window_start.clone(),
                    excluded_window_end: input.excluded_window_end.clone(),
                })?;
            let rule_view = aligned_interval_rule_view_from_rule(&validated.rule, now);
            Ok((
                validated.name,
                validated.description,
                ScheduleRuleChanges {
                    rule_type: "aligned_interval".to_string(),
                    timezone_mode: "named".to_string(),
                    timezone_id: Some(validated.rule.timezone().to_string()),
                    config_json: serde_json::to_string(&validated.rule)
                        .map_err(|error| error.to_string())?,
                },
                rule_view,
            ))
        }
        _ => Err("invalid_reminder_kind".to_string()),
    }
}

fn validate_reminder_input(input: CreateReminderInput) -> Result<ValidatedReminderInput, String> {
    let (name, description) = validate_reminder_text(input.name, input.description)?;
    let local_time = NaiveTime::parse_from_str(&input.local_time, "%H:%M")
        .map_err(|_| "提醒时间格式应为 HH:mm".to_string())?;
    let timezone =
        Tz::from_str(&input.timezone).map_err(|_| format!("无法识别时区：{}", input.timezone))?;
    let weekdays = input
        .weekdays
        .iter()
        .map(|day| parse_weekday(day))
        .collect::<Result<Vec<_>, _>>()?;
    if weekdays.is_empty() {
        return Err("至少选择一个生效星期".to_string());
    }
    Ok(ValidatedReminderInput {
        name,
        description,
        local_time,
        timezone,
        weekdays,
    })
}

fn validate_one_shot_reminder_input(
    input: CreateOneShotReminderInput,
    now: DateTime<Utc>,
) -> Result<ValidatedOneShotReminderInput, String> {
    let (name, description) = validate_reminder_text(input.name, input.description)?;
    let source_timezone =
        Tz::from_str(&input.timezone).map_err(|_| format!("无法识别时区：{}", input.timezone))?;
    let local_date_time = NaiveDateTime::parse_from_str(&input.local_date_time, "%Y-%m-%dT%H:%M")
        .map_err(|_| "一次性提醒时间格式应为 YYYY-MM-DDTHH:mm".to_string())?;
    if local_date_time.format("%Y-%m-%dT%H:%M").to_string() != input.local_date_time {
        return Err("一次性提醒时间格式应为 YYYY-MM-DDTHH:mm".to_string());
    }
    let at_utc = match source_timezone.from_local_datetime(&local_date_time) {
        LocalResult::Single(value) => value.with_timezone(&Utc),
        LocalResult::Ambiguous(first, second) => {
            first.with_timezone(&Utc).min(second.with_timezone(&Utc))
        }
        LocalResult::None => {
            return Err("该当地时间因夏令时切换而不存在，请选择其他时间".to_string())
        }
    };
    if at_utc <= now {
        return Err("一次性提醒时间必须晚于当前时间".to_string());
    }

    Ok(ValidatedOneShotReminderInput {
        name,
        description,
        at_utc,
        source_timezone,
    })
}

fn validate_aligned_interval_reminder_input(
    input: CreateAlignedIntervalReminderInput,
) -> Result<ValidatedAlignedIntervalReminderInput, String> {
    let (name, description) = validate_reminder_text(input.name, input.description)?;
    if !(1..=1_440).contains(&input.interval_minutes) {
        return Err("间隔时长必须为 1-1440 分钟".to_string());
    }
    let timezone =
        Tz::from_str(&input.timezone).map_err(|_| format!("无法识别时区：{}", input.timezone))?;
    let anchor_local = parse_canonical_local_date_time(
        &input.anchor_local_date_time,
        "锚点时间格式应为 YYYY-MM-DDTHH:mm",
    )?;
    let weekdays = input
        .weekdays
        .iter()
        .map(|day| parse_weekday(day))
        .collect::<Result<Vec<_>, _>>()?;
    if weekdays.is_empty() {
        return Err("至少选择一个生效星期".to_string());
    }
    let active_window = match (input.active_window_start, input.active_window_end) {
        (None, None) => Vec::new(),
        (Some(start), Some(end)) => {
            let start = parse_canonical_local_time(&start, "活动窗口时间格式应为 HH:mm")?;
            let end = parse_canonical_local_time(&end, "活动窗口时间格式应为 HH:mm")?;
            vec![(start, end)]
        }
        _ => return Err("活动窗口开始和结束时间必须同时填写".to_string()),
    };
    let active_windows = match (
        active_window.as_slice(),
        input.excluded_window_start,
        input.excluded_window_end,
    ) {
        ([], None, None) => Vec::new(),
        ([(start, end)], None, None) => {
            vec![ActiveWindow::new(*start, *end).map_err(|_| "活动窗口起止时间不能相同")?]
        }
        ([(start, end)], Some(excluded_start), Some(excluded_end)) => {
            let excluded_start =
                parse_canonical_local_time(&excluded_start, "午休时间格式应为 HH:mm")?;
            let excluded_end = parse_canonical_local_time(&excluded_end, "午休时间格式应为 HH:mm")?;
            if !(*start < excluded_start && excluded_start < excluded_end && excluded_end < *end) {
                return Err("午休时间必须完整包含在当天生效时段内".to_string());
            }
            vec![
                ActiveWindow::new(*start, excluded_start)
                    .map_err(|_| "午休开始时间不能等于生效开始时间")?,
                ActiveWindow::new(excluded_end, *end)
                    .map_err(|_| "午休结束时间不能等于生效结束时间")?,
            ]
        }
        ([], Some(_), Some(_)) => return Err("设置午休前请先限定生效时段".to_string()),
        (_, None, Some(_)) | (_, Some(_), None) => {
            return Err("午休开始和结束时间必须同时填写".to_string())
        }
        _ => return Err("活动时段配置无效".to_string()),
    };
    let rule = AlignedIntervalRule::new_with_weekdays(
        timezone,
        anchor_local,
        input.interval_minutes,
        active_windows,
        weekdays,
    )
    .map_err(|error| error.to_string())?;

    Ok(ValidatedAlignedIntervalReminderInput {
        name,
        description,
        rule,
    })
}

fn parse_canonical_local_date_time(value: &str, message: &str) -> Result<NaiveDateTime, String> {
    let parsed =
        NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M").map_err(|_| message.to_string())?;
    if parsed.format("%Y-%m-%dT%H:%M").to_string() != value {
        return Err(message.to_string());
    }
    Ok(parsed)
}

fn parse_canonical_local_time(value: &str, message: &str) -> Result<NaiveTime, String> {
    let parsed = NaiveTime::parse_from_str(value, "%H:%M").map_err(|_| message.to_string())?;
    if parsed.format("%H:%M").to_string() != value {
        return Err(message.to_string());
    }
    Ok(parsed)
}

fn validate_reminder_text(
    name: String,
    description: Option<String>,
) -> Result<(String, String), String> {
    let name = name.trim().to_string();
    let description = description.unwrap_or_default().trim().to_string();
    if name.is_empty() || name.chars().count() > 30 {
        return Err("提醒名称需为 1-30 个字符".to_string());
    }
    if description.chars().count() > 100 {
        return Err("提醒内容不能超过 100 个字符".to_string());
    }
    Ok((name, description))
}

fn finish_occurrence_action(
    app: &AppHandle,
    surface: &ReminderSurfaceState,
    occurrence_id: &str,
    occurrence: Occurrence,
) -> OccurrenceActionView {
    let _ = surface.finish(app, occurrence_id);
    let _ = app.emit("reminders-changed", ());
    occurrence.into()
}

async fn dismiss_surface_as_unhandled(app: AppHandle, occurrence_id: String) -> bool {
    let occurrence_actions = app.state::<AppState>().occurrence_actions.clone();
    match occurrence_actions
        .mark_unhandled(&occurrence_id, Utc::now().timestamp_millis())
        .await
    {
        Ok(occurrence) => {
            let surface = app.state::<ReminderSurfaceState>();
            let _ = finish_occurrence_action(&app, &surface, &occurrence_id, occurrence);
            true
        }
        Err(_) => false,
    }
}

async fn cancel_active_global_pauses(pauses: &PauseRepository, now_utc: i64) -> Result<(), String> {
    for pause in pauses
        .list_active_global(now_utc)
        .await
        .map_err(|error| error.to_string())?
    {
        pauses
            .cancel(&pause.id, now_utc)
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

async fn load_pause_status(pauses: &PauseRepository, now_utc: i64) -> Result<PauseStatus, String> {
    let sessions = pauses
        .list_active_global(now_utc)
        .await
        .map_err(|error| error.to_string())?;
    Ok(pause_status_from_sessions(sessions))
}

fn pause_status_from_sessions(sessions: Vec<PauseSession>) -> PauseStatus {
    let is_paused = !sessions.is_empty();
    let has_indefinite = sessions.iter().any(|session| session.ends_at_utc.is_none());
    let paused_until = (!has_indefinite)
        .then(|| {
            sessions
                .iter()
                .filter_map(|session| session.ends_at_utc)
                .max()
        })
        .flatten()
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .map(|value| value.to_rfc3339());
    PauseStatus {
        is_paused,
        paused_until,
        active_session_ids: sessions.into_iter().map(|session| session.id).collect(),
    }
}

fn query_autostart_status(app: &AppHandle) -> AutostartStatus {
    match app.autolaunch().is_enabled() {
        Ok(enabled) => AutostartStatus {
            available: true,
            enabled: Some(enabled),
            error: None,
        },
        Err(error) => AutostartStatus {
            available: false,
            enabled: None,
            error: Some(format!("autostart_query_unavailable: {error}")),
        },
    }
}

fn should_initialize_default_autostart(configured: bool, has_existing_setup: bool) -> bool {
    !configured && !has_existing_setup
}

async fn initialize_default_autostart(
    app: &AppHandle,
    store: &SqliteStore,
    reminders: &ReminderRepository,
) -> Result<(), String> {
    let configured = store
        .get_setting_json(AUTOSTART_SETTING_KEY)
        .await
        .map_err(|error| error.to_string())?
        .is_some();
    let has_onboarding_setting = store
        .get_setting_json(ONBOARDING_SETTING_KEY)
        .await
        .map_err(|error| error.to_string())?
        .is_some();
    let has_reminders = !reminders
        .list(true)
        .await
        .map_err(|error| error.to_string())?
        .is_empty();

    if !should_initialize_default_autostart(configured, has_onboarding_setting || has_reminders) {
        if !configured {
            store
                .set_setting_json(
                    AUTOSTART_SETTING_KEY,
                    AUTOSTART_CONFIGURED_JSON,
                    Utc::now().timestamp_millis(),
                )
                .await
                .map_err(|error| error.to_string())?;
        }
        return Ok(());
    }

    let current = query_autostart_status(app);
    if !current.available {
        return Err(current
            .error
            .unwrap_or_else(|| "autostart_unavailable".to_string()));
    }
    if current.enabled != Some(true) {
        app.autolaunch()
            .enable()
            .map_err(|error| format!("autostart_update_unavailable: {error}"))?;
    }

    let verified = query_autostart_status(app);
    if verified.enabled != Some(true) {
        return Err(verified
            .error
            .unwrap_or_else(|| "autostart_verification_failed".to_string()));
    }
    store
        .set_setting_json(
            AUTOSTART_SETTING_KEY,
            AUTOSTART_CONFIGURED_JSON,
            Utc::now().timestamp_millis(),
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn has_minimized_startup_argument(args: &[String]) -> bool {
    args.iter().any(|argument| argument == "--minimized")
}

fn parse_weekday(value: &str) -> Result<Weekday, String> {
    match value.to_ascii_lowercase().as_str() {
        "mon" => Ok(Weekday::Mon),
        "tue" => Ok(Weekday::Tue),
        "wed" => Ok(Weekday::Wed),
        "thu" => Ok(Weekday::Thu),
        "fri" => Ok(Weekday::Fri),
        "sat" => Ok(Weekday::Sat),
        "sun" => Ok(Weekday::Sun),
        _ => Err(format!("无法识别星期：{value}")),
    }
}

fn merge_reminder_views(
    reminders: Vec<StoredReminder>,
    scheduled: Vec<ScheduledReminderRecord>,
    now: DateTime<Utc>,
) -> Vec<ReminderView> {
    let rule_views = scheduled
        .into_iter()
        .filter_map(|record| rule_view(&record, now).map(|view| (record.reminder_id, view)))
        .collect::<HashMap<_, _>>();

    reminders
        .into_iter()
        .map(|reminder| {
            let rule_view = rule_views.get(&reminder.id);
            let mut view = ReminderView::from(reminder);
            if let Some(rule_view) = rule_view {
                view.rule_summary = Some(rule_view.summary.clone());
                view.rule = Some(rule_view.details.clone());
                view.next_trigger_at = rule_view.next_trigger_at.clone();
            }
            view
        })
        .collect()
}

fn rule_view(record: &ScheduledReminderRecord, now: DateTime<Utc>) -> Option<RuleView> {
    match record.rule_type.as_str() {
        "fixed_times" => fixed_time_rule_view(record, now),
        "one_shot" => one_shot_rule_view(record, now),
        "aligned_interval" => aligned_interval_rule_view(record, now),
        _ => None,
    }
}

fn fixed_time_rule_view(record: &ScheduledReminderRecord, now: DateTime<Utc>) -> Option<RuleView> {
    let stored: StoredFixedTimeRule = serde_json::from_str(&record.rule_config_json).ok()?;
    let summary = format!(
        "{} · {}",
        weekday_label(&stored.weekdays),
        stored
            .times
            .iter()
            .map(|time| time.format("%H:%M").to_string())
            .collect::<Vec<_>>()
            .join("、")
    );
    let details = fixed_rule_details(stored.timezone, &stored.weekdays, &stored.times);
    let rule = FixedTimeRule::new(stored.timezone, stored.weekdays, stored.times).ok()?;
    let next_trigger_at = rule
        .next_after(now)
        .map(|candidate| candidate.scheduled_at_utc.to_rfc3339());

    Some(RuleView {
        summary,
        details,
        next_trigger_at,
    })
}

fn fixed_rule_details(
    timezone: Tz,
    weekdays: &[Weekday],
    times: &[NaiveTime],
) -> ReminderRuleDetails {
    ReminderRuleDetails {
        kind: "fixed".to_string(),
        timezone: timezone.to_string(),
        weekdays: weekday_codes(weekdays),
        times: times
            .iter()
            .map(|time| time.format("%H:%M").to_string())
            .collect(),
        local_date_time: None,
        interval_minutes: None,
        active_window_start: None,
        active_window_end: None,
        excluded_window_start: None,
        excluded_window_end: None,
        anchor_local_date_time: None,
    }
}

fn one_shot_rule_view(record: &ScheduledReminderRecord, now: DateTime<Utc>) -> Option<RuleView> {
    let stored: StoredOneShotRule = serde_json::from_str(&record.rule_config_json).ok()?;
    Some(one_shot_rule_view_from_parts(
        stored.at_utc,
        stored.source_timezone,
        now,
    ))
}

fn one_shot_rule_view_from_parts(
    at_utc: DateTime<Utc>,
    source_timezone: Tz,
    now: DateTime<Utc>,
) -> RuleView {
    let rule = OneShotRule::new(at_utc, source_timezone);
    RuleView {
        summary: format!(
            "一次性 · {}",
            at_utc
                .with_timezone(&source_timezone)
                .format("%Y-%m-%d %H:%M")
        ),
        details: ReminderRuleDetails {
            kind: "oneShot".to_string(),
            timezone: source_timezone.to_string(),
            weekdays: Vec::new(),
            times: Vec::new(),
            local_date_time: Some(
                at_utc
                    .with_timezone(&source_timezone)
                    .format("%Y-%m-%dT%H:%M")
                    .to_string(),
            ),
            interval_minutes: None,
            active_window_start: None,
            active_window_end: None,
            excluded_window_start: None,
            excluded_window_end: None,
            anchor_local_date_time: None,
        },
        next_trigger_at: rule
            .next_after(now)
            .map(|candidate| candidate.scheduled_at_utc.to_rfc3339()),
    }
}

fn aligned_interval_rule_view(
    record: &ScheduledReminderRecord,
    now: DateTime<Utc>,
) -> Option<RuleView> {
    let rule: AlignedIntervalRule = serde_json::from_str(&record.rule_config_json).ok()?;
    Some(aligned_interval_rule_view_from_rule(&rule, now))
}

fn aligned_interval_rule_view_from_rule(
    rule: &AlignedIntervalRule,
    now: DateTime<Utc>,
) -> RuleView {
    let windows = rule.active_windows();
    let window = if windows.is_empty() {
        "全天".to_string()
    } else if let [morning, afternoon] = windows {
        if morning.start() < morning.end()
            && morning.end() < afternoon.start()
            && afternoon.start() < afternoon.end()
        {
            format!(
                "{}-{} · 午休 {}-{}",
                morning.start().format("%H:%M"),
                afternoon.end().format("%H:%M"),
                morning.end().format("%H:%M"),
                afternoon.start().format("%H:%M")
            )
        } else {
            windows
                .iter()
                .map(format_active_window)
                .collect::<Vec<_>>()
                .join("、")
        }
    } else {
        windows
            .iter()
            .map(format_active_window)
            .collect::<Vec<_>>()
            .join("、")
    };
    let (active_window_start, active_window_end, excluded_window_start, excluded_window_end) =
        interval_window_details(windows);
    RuleView {
        summary: format!(
            "{} · 每 {} 分钟 · {}",
            weekday_label(rule.active_weekdays()),
            rule.interval_minutes(),
            window
        ),
        details: ReminderRuleDetails {
            kind: "interval".to_string(),
            timezone: rule.timezone().to_string(),
            weekdays: weekday_codes(rule.active_weekdays()),
            times: Vec::new(),
            local_date_time: None,
            interval_minutes: Some(rule.interval_minutes()),
            active_window_start,
            active_window_end,
            excluded_window_start,
            excluded_window_end,
            anchor_local_date_time: Some(rule.anchor_local().format("%Y-%m-%dT%H:%M").to_string()),
        },
        next_trigger_at: rule
            .next_after(now)
            .map(|candidate| candidate.schedule.scheduled_at_utc.to_rfc3339()),
    }
}

fn interval_window_details(
    windows: &[ActiveWindow],
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    let Some(first) = windows.first() else {
        return (None, None, None, None);
    };
    let Some(last) = windows.last() else {
        return (None, None, None, None);
    };
    let active_start = Some(first.start().format("%H:%M").to_string());
    let active_end = Some(last.end().format("%H:%M").to_string());
    if windows.len() == 2 {
        (
            active_start,
            active_end,
            Some(first.end().format("%H:%M").to_string()),
            Some(last.start().format("%H:%M").to_string()),
        )
    } else {
        (active_start, active_end, None, None)
    }
}

fn format_active_window(window: &ActiveWindow) -> String {
    format!(
        "{}-{}",
        window.start().format("%H:%M"),
        window.end().format("%H:%M")
    )
}

#[cfg(test)]
async fn seed_default_health_reminders(reminders: &ReminderRepository) -> Result<bool, String> {
    seed_default_health_reminders_for(reminders, Tz::Asia__Shanghai, "zh-CN").await
}

async fn seed_default_health_reminders_for(
    reminders: &ReminderRepository,
    timezone: Tz,
    locale: &str,
) -> Result<bool, String> {
    if !reminders
        .list(true)
        .await
        .map_err(|error| error.to_string())?
        .is_empty()
    {
        return Ok(false);
    }

    let workdays = vec![
        Weekday::Mon,
        Weekday::Tue,
        Weekday::Wed,
        Weekday::Thu,
        Weekday::Fri,
    ];
    let morning_start = NaiveTime::from_hms_opt(9, 0, 0)
        .ok_or_else(|| "invalid_default_seed_time: morning_start".to_string())?;
    let lunch_start = NaiveTime::from_hms_opt(12, 0, 0)
        .ok_or_else(|| "invalid_default_seed_time: lunch_start".to_string())?;
    let lunch_end = NaiveTime::from_hms_opt(13, 30, 0)
        .ok_or_else(|| "invalid_default_seed_time: lunch_end".to_string())?;
    let work_end = NaiveTime::from_hms_opt(18, 0, 0)
        .ok_or_else(|| "invalid_default_seed_time: work_end".to_string())?;
    let anchor_date = chrono::NaiveDate::from_ymd_opt(2026, 1, 5)
        .ok_or_else(|| "invalid_default_seed_date".to_string())?;
    let now = Utc::now().timestamp_millis();
    let locale = locale.to_ascii_lowercase();
    let templates = if locale.starts_with("zh") {
        [
            (
                "eye-break",
                "远望放松",
                "看向远处至少 20 秒，让眼睛放松一下",
                20,
                0,
            ),
            ("drink-water", "喝口水", "小口补水，不要等到口渴", 45, 7),
            (
                "neck-shoulder",
                "活动颈肩",
                "放松肩膀，缓慢活动颈部和肩部",
                60,
                10,
            ),
            ("stand-walk", "起身走动", "离开座位活动 2-3 分钟", 90, 15),
            (
                "posture-reset",
                "调整坐姿",
                "让背部有支撑，放松手腕和肩膀",
                120,
                30,
            ),
        ]
    } else if locale.starts_with("ja") {
        [
            (
                "eye-break",
                "目を休める",
                "遠くを20秒以上見て目を休めましょう",
                20,
                0,
            ),
            (
                "drink-water",
                "水分補給",
                "喉が渇く前に少しずつ水分を取りましょう",
                45,
                7,
            ),
            (
                "neck-shoulder",
                "首と肩を動かす",
                "肩の力を抜いて首と肩をゆっくり動かしましょう",
                60,
                10,
            ),
            (
                "stand-walk",
                "立って歩く",
                "席を離れて2〜3分歩きましょう",
                90,
                15,
            ),
            (
                "posture-reset",
                "姿勢を整える",
                "背中を支え、手首と肩の力を抜きましょう",
                120,
                30,
            ),
        ]
    } else if locale.starts_with("es") {
        [
            (
                "eye-break",
                "Descansa la vista",
                "Mira a lo lejos durante al menos 20 segundos",
                20,
                0,
            ),
            (
                "drink-water",
                "Bebe agua",
                "Bebe pequeños sorbos antes de tener sed",
                45,
                7,
            ),
            (
                "neck-shoulder",
                "Mueve cuello y hombros",
                "Relaja los hombros y mueve el cuello lentamente",
                60,
                10,
            ),
            (
                "stand-walk",
                "Levántate y camina",
                "Aléjate del escritorio y camina 2-3 minutos",
                90,
                15,
            ),
            (
                "posture-reset",
                "Ajusta tu postura",
                "Apoya la espalda y relaja muñecas y hombros",
                120,
                30,
            ),
        ]
    } else {
        [
            (
                "eye-break",
                "Rest your eyes",
                "Look into the distance for at least 20 seconds",
                20,
                0,
            ),
            (
                "drink-water",
                "Drink water",
                "Take small sips before you feel thirsty",
                45,
                7,
            ),
            (
                "neck-shoulder",
                "Move your neck and shoulders",
                "Relax your shoulders and move your neck slowly",
                60,
                10,
            ),
            (
                "stand-walk",
                "Stand and walk",
                "Leave your desk and walk for 2-3 minutes",
                90,
                15,
            ),
            (
                "posture-reset",
                "Reset your posture",
                "Support your back and relax your wrists and shoulders",
                120,
                30,
            ),
        ]
    };
    let mut bundles = templates
        .into_iter()
        .enumerate()
        .map(
            |(index, (key, title, description, interval_minutes, anchor_minute))| -> Result<
                NewReminderBundle,
                String,
            > {
                let id = format!("default-health-{key}");
                let mut reminder = NewReminder::new(&id, title, now.saturating_add(index as i64));
                reminder.description = description.to_string();
                let active_windows = vec![
                    ActiveWindow::new(morning_start, lunch_start)
                        .map_err(|error| format!("invalid_default_seed_window: {error:?}"))?,
                    ActiveWindow::new(lunch_end, work_end)
                        .map_err(|error| format!("invalid_default_seed_window: {error:?}"))?,
                ];
                let anchor_local = anchor_date
                    .and_hms_opt(9, anchor_minute, 0)
                    .ok_or_else(|| format!("invalid_default_seed_anchor: {key}"))?;
                let rule = AlignedIntervalRule::new_with_weekdays(
                    timezone,
                    anchor_local,
                    interval_minutes,
                    active_windows,
                    workdays.clone(),
                )
                .map_err(|error| format!("invalid_default_reminder_rule: {error:?}"))?;
                let config_json = serde_json::to_string(&rule)
                    .map_err(|error| format!("default_reminder_rule_serialize_failed: {error}"))?;
                Ok(NewReminderBundle {
                    reminder,
                    rule: Some(NewScheduleRule {
                        id: format!("default-health-rule-{key}"),
                        rule_type: "aligned_interval".to_string(),
                        timezone_mode: "named".to_string(),
                        timezone_id: Some(timezone.to_string()),
                        config_json,
                    }),
                    policy: Some(NewReminderPolicy::defaults(format!(
                        "default-health-policy-{key}"
                    ))),
                })
            },
        )
        .collect::<Result<Vec<_>, _>>()?;

    let (lunch_title, lunch_description) = if locale.starts_with("zh") {
        ("点外卖", "提前点好午餐，留出午休时间")
    } else if locale.starts_with("ja") {
        (
            "昼食を注文",
            "昼食を早めに注文して、休憩時間を確保しましょう",
        )
    } else if locale.starts_with("es") {
        (
            "Pide el almuerzo",
            "Pide el almuerzo con antelación y reserva tiempo para tu descanso",
        )
    } else {
        (
            "Order lunch",
            "Order lunch early and leave time for your break",
        )
    };
    let lunch_order_time = NaiveTime::from_hms_opt(11, 0, 0)
        .ok_or_else(|| "invalid_default_seed_time: lunch_order".to_string())?;
    let lunch_rule = FixedTimeRule::new(timezone, workdays.clone(), vec![lunch_order_time])
        .map_err(|error| format!("invalid_default_lunch_rule: {error:?}"))?;
    let lunch_config_json = serde_json::to_string(&lunch_rule)
        .map_err(|error| format!("default_lunch_rule_serialize_failed: {error}"))?;
    let mut lunch_reminder = NewReminder::new(
        "default-lunch-order",
        lunch_title,
        now.saturating_add(bundles.len() as i64),
    );
    lunch_reminder.description = lunch_description.to_string();
    bundles.push(NewReminderBundle {
        reminder: lunch_reminder,
        rule: Some(NewScheduleRule {
            id: "default-lunch-order-rule".to_string(),
            rule_type: "fixed_times".to_string(),
            timezone_mode: "named".to_string(),
            timezone_id: Some(timezone.to_string()),
            config_json: lunch_config_json,
        }),
        policy: Some(NewReminderPolicy::defaults("default-lunch-order-policy")),
    });

    reminders
        .create_many_with_configuration(&bundles)
        .await
        .map_err(|error| error.to_string())?;
    Ok(true)
}

fn weekday_label(weekdays: &[Weekday]) -> &'static str {
    let mut weekdays = weekdays.to_vec();
    weekdays.sort_by_key(|day| day.num_days_from_monday());
    weekdays.dedup();

    if weekdays.len() == 7 {
        "每天"
    } else if weekdays
        == [
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
        ]
    {
        "工作日"
    } else {
        "指定星期"
    }
}

fn weekday_codes(weekdays: &[Weekday]) -> Vec<String> {
    let mut sorted = weekdays.to_vec();
    sorted.sort_by_key(|day| day.num_days_from_monday());
    sorted.dedup();
    sorted
        .into_iter()
        .map(|day| match day {
            Weekday::Mon => "mon",
            Weekday::Tue => "tue",
            Weekday::Wed => "wed",
            Weekday::Thu => "thu",
            Weekday::Fri => "fri",
            Weekday::Sat => "sat",
            Weekday::Sun => "sun",
        })
        .map(str::to_string)
        .collect()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() -> Result<(), tauri::Error> {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .setup(|app| {
            app.manage(BackgroundModeState::default());
            let data_dir = app.path().app_local_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let database_path = data_dir.join("takefive.db");
            let store = tauri::async_runtime::block_on(SqliteStore::open(&database_path))
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            tauri::async_runtime::block_on(load_or_initialize_reminder_settings(
                &store,
                iana_time_zone::get_timezone().ok(),
            ))
            .map_err(std::io::Error::other)?;
            let reminders = ReminderRepository::new(store.clone());
            if let Err(error) = tauri::async_runtime::block_on(initialize_default_autostart(
                app.handle(),
                &store,
                &reminders,
            )) {
                eprintln!("TakeFive autostart initialization unavailable: {error}");
            }
            let reminder_surface = ReminderSurfaceState::default();
            app.manage(reminder_surface.clone());
            let scheduler =
                scheduler_runtime::start(app.handle().clone(), store.clone(), reminder_surface);
            let occurrences = OccurrenceRepository::new(store.clone());
            let pauses = PauseRepository::new(store.clone());
            app.manage(AppState {
                reminders: reminders.clone(),
                pauses,
                occurrence_actions: OccurrenceActionService::new(occurrences, reminders),
                store,
                scheduler,
                database_path,
            });
            if let Err(error) = platform::start_lifecycle_monitor(app.handle().clone()) {
                eprintln!("TakeFive lifecycle monitor unavailable: {error}");
            }

            let tray_available = match build_tray(app) {
                Ok(()) => true,
                Err(error) => {
                    eprintln!("TakeFive tray unavailable; using foreground mode: {error}");
                    false
                }
            };
            app.state::<BackgroundModeState>()
                .set_tray_available(tray_available);

            let startup_args = std::env::args().collect::<Vec<_>>();
            if tray_available && has_minimized_startup_argument(&startup_args) {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.destroy();
                }
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            let WindowEvent::CloseRequested { api, .. } = event else {
                return;
            };
            match window.label() {
                "main" => {
                    if window
                        .app_handle()
                        .try_state::<BackgroundModeState>()
                        .is_some_and(|state| state.tray_available())
                    {
                        api.prevent_close();
                        let _ = window.destroy();
                    }
                }
                REMINDER_SURFACE_LABEL => {
                    api.prevent_close();
                    let app = window.app_handle().clone();
                    match app.state::<ReminderSurfaceState>().latest().ok().flatten() {
                        Some(payload) if payload.preview => {
                            let surface = app.state::<ReminderSurfaceState>();
                            let _ = surface.finish_preview(&app, &payload.occurrence_id);
                        }
                        Some(payload) => {
                            tauri::async_runtime::spawn(dismiss_surface_as_unhandled(
                                app,
                                payload.occurrence_id,
                            ));
                        }
                        None => {
                            let _ = window.hide();
                        }
                    }
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            probe_platform,
            preview_schedule,
            storage_status,
            get_reminder_settings,
            update_reminder_settings,
            get_onboarding_status,
            complete_onboarding,
            initialize_default_health_reminders,
            list_reminders,
            create_reminder,
            create_one_shot_reminder,
            create_aligned_interval_reminder,
            update_reminder,
            set_reminder_enabled,
            delete_reminder,
            complete_occurrence,
            skip_occurrence,
            snooze_occurrence,
            mark_occurrence_unhandled,
            get_pause_status,
            pause_all,
            resume_all,
            get_autostart_status,
            set_autostart_enabled,
            get_reminder_surface_payload,
            preview_reminder,
            dismiss_reminder_preview
        ])
        .build(tauri::generate_context!())?;

    app.run(|app, event| {
        if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
            if code.is_none()
                && app
                    .try_state::<BackgroundModeState>()
                    .is_some_and(|state| state.tray_available())
            {
                api.prevent_exit();
            }
        }
    });
    Ok(())
}

fn build_tray(app: &tauri::App) -> Result<(), tauri::Error> {
    let open = MenuItem::with_id(app, "open", "Open TakeFive", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit TakeFive", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let mut builder = TrayIconBuilder::with_id("main-tray")
        .tooltip("TakeFive")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app).map(|_| ())
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }

    if let Ok(window) = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("TakeFive")
        .inner_size(1080.0, 760.0)
        .min_inner_size(900.0, 620.0)
        .center()
        .build()
    {
        let _ = window.set_focus();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored_reminder(id: &str) -> StoredReminder {
        StoredReminder {
            id: id.to_string(),
            title: "喝水".to_string(),
            description: "休息一下".to_string(),
            enabled: true,
            revision: 1,
            created_at_utc: 100,
            updated_at_utc: 100,
            deleted_at_utc: None,
        }
    }

    fn scheduled_reminder(id: &str, rule_config_json: String) -> ScheduledReminderRecord {
        scheduled_reminder_with_type(id, "fixed_times", rule_config_json)
    }

    fn scheduled_reminder_with_type(
        id: &str,
        rule_type: &str,
        rule_config_json: String,
    ) -> ScheduledReminderRecord {
        ScheduledReminderRecord {
            reminder_id: id.to_string(),
            title: "喝水".to_string(),
            description: "休息一下".to_string(),
            reminder_revision: 1,
            rule_id: format!("rule-{id}"),
            rule_type: rule_type.to_string(),
            timezone_mode: if rule_type == "one_shot" {
                "named".to_string()
            } else {
                "follow_system".to_string()
            },
            timezone_id: Some("Asia/Shanghai".to_string()),
            rule_config_json,
            policy_id: format!("policy-{id}"),
            delivery_json: "{}".to_string(),
            sound_json: "{}".to_string(),
            snooze_json: "{}".to_string(),
            missed_json: "{}".to_string(),
            dnd_json: "{}".to_string(),
        }
    }

    #[test]
    fn weekday_parser_rejects_unknown_values() {
        assert_eq!(parse_weekday("fri").unwrap(), Weekday::Fri);
        assert!(parse_weekday("workday").is_err());
    }

    #[test]
    fn only_the_explicit_autostart_argument_selects_tray_only_startup() {
        assert!(has_minimized_startup_argument(&[
            "takefive.exe".to_string(),
            "--minimized".to_string(),
        ]));
        assert!(!has_minimized_startup_argument(&[
            "takefive.exe".to_string(),
            "--minimized=false".to_string(),
        ]));
    }

    #[test]
    fn default_autostart_only_applies_to_a_fresh_install() {
        assert!(should_initialize_default_autostart(false, false));
        assert!(!should_initialize_default_autostart(true, false));
        assert!(!should_initialize_default_autostart(false, true));
    }

    #[test]
    fn background_mode_defaults_to_foreground_when_tray_is_unavailable() {
        let state = BackgroundModeState::default();
        assert!(!state.tray_available());
        state.set_tray_available(true);
        assert!(state.tray_available());
    }

    #[test]
    fn schedule_preview_uses_the_domain_rule_calculator() {
        let preview = preview_schedule(SchedulePreviewRequest {
            timezone: "Asia/Shanghai".to_string(),
            local_time: "10:30".to_string(),
            weekdays: vec!["mon".to_string(), "fri".to_string()],
        })
        .unwrap();

        assert!(preview.occurrence_key.starts_with("Asia/Shanghai|"));
        assert!(preview.scheduled_local.ends_with("+08:00"));
    }

    #[test]
    fn reminder_input_is_trimmed_and_validated() {
        let validated = validate_reminder_input(CreateReminderInput {
            name: "  喝水  ".to_string(),
            description: Some("  休息一下  ".to_string()),
            local_time: "10:30".to_string(),
            timezone: "Asia/Shanghai".to_string(),
            weekdays: vec!["mon".to_string(), "fri".to_string()],
        })
        .unwrap();
        assert_eq!(validated.name, "喝水");
        assert_eq!(validated.description, "休息一下");
        assert_eq!(
            validated.local_time,
            NaiveTime::from_hms_opt(10, 30, 0).unwrap()
        );

        assert!(validate_reminder_input(CreateReminderInput {
            name: " ".to_string(),
            description: None,
            local_time: "10:30".to_string(),
            timezone: "Asia/Shanghai".to_string(),
            weekdays: vec!["mon".to_string()],
        })
        .is_err());
    }

    #[test]
    fn one_shot_input_is_trimmed_and_converted_from_its_source_timezone() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 0, 0, 0).unwrap();
        let validated = validate_one_shot_reminder_input(
            CreateOneShotReminderInput {
                name: "  吃药  ".to_string(),
                description: Some("  饭后服用  ".to_string()),
                local_date_time: "2026-07-15T09:30".to_string(),
                timezone: "Asia/Shanghai".to_string(),
            },
            now,
        )
        .unwrap();

        assert_eq!(validated.name, "吃药");
        assert_eq!(validated.description, "饭后服用");
        assert_eq!(
            validated.at_utc,
            Utc.with_ymd_and_hms(2026, 7, 15, 1, 30, 0).unwrap()
        );
        assert_eq!(validated.source_timezone, Tz::Asia__Shanghai);
    }

    #[test]
    fn one_shot_input_rejects_noncanonical_invalid_and_elapsed_times() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 1, 0, 0).unwrap();
        let input = |local_date_time: &str, timezone: &str| CreateOneShotReminderInput {
            name: "吃药".to_string(),
            description: None,
            local_date_time: local_date_time.to_string(),
            timezone: timezone.to_string(),
        };

        assert!(
            validate_one_shot_reminder_input(input("2026-7-15T09:30", "Asia/Shanghai"), now)
                .is_err()
        );
        assert!(
            validate_one_shot_reminder_input(input("2026-02-30T09:30", "Asia/Shanghai"), now)
                .is_err()
        );
        assert!(validate_one_shot_reminder_input(
            input("2026-07-15T09:30", "Invalid/Timezone"),
            now
        )
        .is_err());
        assert!(
            validate_one_shot_reminder_input(input("2026-07-14T09:00", "Asia/Shanghai"), now)
                .is_err()
        );

        let before_dst_gap = Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap();
        assert!(validate_one_shot_reminder_input(
            input("2026-03-08T02:30", "America/New_York"),
            before_dst_gap
        )
        .is_err());
    }

    #[test]
    fn one_shot_input_uses_the_first_dst_fold_instant() {
        let now = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        let validated = validate_one_shot_reminder_input(
            CreateOneShotReminderInput {
                name: "切换检查".to_string(),
                description: None,
                local_date_time: "2026-11-01T01:30".to_string(),
                timezone: "America/New_York".to_string(),
            },
            now,
        )
        .unwrap();

        assert_eq!(
            validated.at_utc,
            Utc.with_ymd_and_hms(2026, 11, 1, 5, 30, 0).unwrap()
        );
    }

    fn aligned_input(
        interval_minutes: u32,
        active_window_start: Option<&str>,
        active_window_end: Option<&str>,
    ) -> CreateAlignedIntervalReminderInput {
        CreateAlignedIntervalReminderInput {
            name: "  活动一下  ".to_string(),
            description: Some("  离开座位  ".to_string()),
            interval_minutes,
            anchor_local_date_time: "2026-07-15T09:00".to_string(),
            timezone: "Asia/Shanghai".to_string(),
            weekdays: vec![
                "mon".to_string(),
                "tue".to_string(),
                "wed".to_string(),
                "thu".to_string(),
                "fri".to_string(),
            ],
            active_window_start: active_window_start.map(str::to_string),
            active_window_end: active_window_end.map(str::to_string),
            excluded_window_start: None,
            excluded_window_end: None,
        }
    }

    #[test]
    fn aligned_interval_input_builds_a_domain_rule_with_an_optional_window() {
        let validated = validate_aligned_interval_reminder_input(aligned_input(
            60,
            Some("09:00"),
            Some("18:00"),
        ))
        .unwrap();

        assert_eq!(validated.name, "活动一下");
        assert_eq!(validated.description, "离开座位");
        assert_eq!(validated.rule.timezone(), Tz::Asia__Shanghai);
        assert_eq!(validated.rule.interval_minutes(), 60);
        assert_eq!(validated.rule.active_windows().len(), 1);
        assert_eq!(validated.rule.active_weekdays().len(), 5);
        assert_eq!(
            validated.rule.anchor_local(),
            NaiveDateTime::parse_from_str("2026-07-15T09:00", "%Y-%m-%dT%H:%M").unwrap()
        );
    }

    #[test]
    fn aligned_interval_input_splits_the_active_window_around_lunch() {
        let mut input = aligned_input(60, Some("09:00"), Some("18:00"));
        input.excluded_window_start = Some("12:00".to_string());
        input.excluded_window_end = Some("13:30".to_string());

        let validated = validate_aligned_interval_reminder_input(input).unwrap();

        assert_eq!(validated.rule.active_windows().len(), 2);
        assert_eq!(
            format_active_window(&validated.rule.active_windows()[0]),
            "09:00-12:00"
        );
        assert_eq!(
            format_active_window(&validated.rule.active_windows()[1]),
            "13:30-18:00"
        );
        assert_eq!(
            aligned_interval_rule_view_from_rule(
                &validated.rule,
                Utc.with_ymd_and_hms(2026, 7, 15, 1, 1, 0).unwrap(),
            )
            .summary,
            "工作日 · 每 60 分钟 · 09:00-18:00 · 午休 12:00-13:30"
        );
    }

    #[tokio::test]
    async fn default_health_reminders_are_seeded_once_and_never_resurrected() {
        let directory = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(directory.path().join("seed.db"))
            .await
            .unwrap();
        let reminders = ReminderRepository::new(store);

        assert!(seed_default_health_reminders(&reminders).await.unwrap());
        let seeded = reminders.list(false).await.unwrap();
        assert_eq!(seeded.len(), 6);
        assert_eq!(
            seeded
                .iter()
                .map(|reminder| reminder.title.as_str())
                .collect::<Vec<_>>(),
            [
                "远望放松",
                "喝口水",
                "活动颈肩",
                "起身走动",
                "调整坐姿",
                "点外卖"
            ]
        );
        let configured = reminders.list_configured().await.unwrap();
        assert_eq!(configured.len(), 6);
        for record in configured
            .iter()
            .filter(|record| record.reminder_id.starts_with("default-health-"))
        {
            assert_eq!(record.rule_type, "aligned_interval");
            let rule: AlignedIntervalRule = serde_json::from_str(&record.rule_config_json).unwrap();
            assert_eq!(rule.active_weekdays().len(), 5);
            assert_eq!(rule.active_windows().len(), 2);
        }
        let lunch_record = configured
            .iter()
            .find(|record| record.reminder_id == "default-lunch-order")
            .unwrap();
        assert_eq!(lunch_record.rule_type, "fixed_times");
        assert_eq!(lunch_record.timezone_mode, "named");
        assert_eq!(lunch_record.timezone_id.as_deref(), Some("Asia/Shanghai"));
        let lunch_rule: FixedTimeRule =
            serde_json::from_str(&lunch_record.rule_config_json).unwrap();
        let lunch_candidate = lunch_rule
            .next_after(Utc.with_ymd_and_hms(2026, 1, 5, 2, 0, 0).unwrap())
            .unwrap();
        assert_eq!(
            lunch_candidate.planned_local.time(),
            NaiveTime::from_hms_opt(11, 0, 0).unwrap()
        );
        assert_eq!(lunch_candidate.timezone, Tz::Asia__Shanghai);
        let monday_candidate = lunch_rule
            .next_after(Utc.with_ymd_and_hms(2026, 1, 9, 3, 1, 0).unwrap())
            .unwrap();
        assert_eq!(
            monday_candidate.planned_local,
            NaiveDateTime::parse_from_str("2026-01-12T11:00", "%Y-%m-%dT%H:%M").unwrap()
        );

        assert!(!seed_default_health_reminders(&reminders).await.unwrap());
        assert_eq!(reminders.list(false).await.unwrap().len(), 6);

        for reminder in reminders.list(false).await.unwrap() {
            reminders
                .soft_delete(&reminder.id, Utc::now().timestamp_millis())
                .await
                .unwrap();
        }
        assert!(!seed_default_health_reminders(&reminders).await.unwrap());
        assert!(reminders.list(false).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn explicit_health_template_uses_the_selected_locale_and_named_timezone() {
        let directory = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(directory.path().join("localized-seed.db"))
            .await
            .unwrap();
        let reminders = ReminderRepository::new(store);

        assert!(
            seed_default_health_reminders_for(&reminders, Tz::America__New_York, "en-US",)
                .await
                .unwrap()
        );

        let created = reminders.list(false).await.unwrap();
        assert_eq!(created.len(), 6);
        assert_eq!(created[0].title, "Rest your eyes");
        assert_eq!(created[5].title, "Order lunch");
        for record in reminders.list_configured().await.unwrap() {
            assert_eq!(record.timezone_mode, "named");
            assert_eq!(record.timezone_id.as_deref(), Some("America/New_York"));
            if record.rule_type == "aligned_interval" {
                let rule: AlignedIntervalRule =
                    serde_json::from_str(&record.rule_config_json).unwrap();
                assert_eq!(rule.timezone(), Tz::America__New_York);
            } else {
                assert_eq!(record.rule_type, "fixed_times");
                let rule: FixedTimeRule = serde_json::from_str(&record.rule_config_json).unwrap();
                let candidate = rule
                    .next_after(Utc.with_ymd_and_hms(2026, 1, 5, 13, 0, 0).unwrap())
                    .unwrap();
                assert_eq!(candidate.timezone, Tz::America__New_York);
                assert_eq!(
                    candidate.planned_local.time(),
                    NaiveTime::from_hms_opt(11, 0, 0).unwrap()
                );
            }
        }
    }

    #[tokio::test]
    async fn onboarding_completion_setting_persists_without_creating_a_reminder() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("onboarding.db");
        let store = SqliteStore::open(&path).await.unwrap();

        assert!(store
            .get_setting_json(ONBOARDING_SETTING_KEY)
            .await
            .unwrap()
            .is_none());
        store
            .set_setting_json(ONBOARDING_SETTING_KEY, r#"{"completed":true}"#, 100)
            .await
            .unwrap();
        assert!(store
            .set_setting_json("invalid", "not-json", 100)
            .await
            .is_err());
        store.close().await;

        let reopened = SqliteStore::open(path).await.unwrap();
        assert_eq!(
            reopened
                .get_setting_json(ONBOARDING_SETTING_KEY)
                .await
                .unwrap()
                .as_deref(),
            Some(r#"{"completed":true}"#)
        );
        assert!(ReminderRepository::new(reopened)
            .list(false)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn reminder_settings_are_initialized_once_with_the_system_timezone() {
        let directory = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(directory.path().join("reminder-settings.db"))
            .await
            .unwrap();

        let first =
            load_or_initialize_reminder_settings(&store, Some("America/New_York".to_string()))
                .await
                .unwrap();
        assert_eq!(
            first.quiet_hours.timezone.as_deref(),
            Some("America/New_York")
        );

        let second =
            load_or_initialize_reminder_settings(&store, Some("Asia/Shanghai".to_string()))
                .await
                .unwrap();
        assert_eq!(second, first);
    }

    #[test]
    fn legacy_reminder_history_is_treated_as_completed_onboarding() {
        let active = stored_reminder("active");
        let active_status = onboarding_status_from_parts(None, &[active]).unwrap();
        assert!(active_status.completed);
        assert!(!active_status.needs_setup);
        assert!(active_status.has_reminders);

        let mut deleted = stored_reminder("deleted");
        deleted.deleted_at_utc = Some(200);
        let deleted_status = onboarding_status_from_parts(None, &[deleted]).unwrap();
        assert!(deleted_status.completed);
        assert!(!deleted_status.needs_setup);
        assert!(!deleted_status.has_reminders);

        let fresh_status = onboarding_status_from_parts(None, &[]).unwrap();
        assert!(!fresh_status.completed);
        assert!(fresh_status.needs_setup);
        assert!(!fresh_status.has_reminders);
    }

    #[test]
    fn aligned_interval_input_rejects_invalid_intervals_and_partial_or_empty_windows() {
        assert!(validate_aligned_interval_reminder_input(aligned_input(0, None, None)).is_err());
        assert!(
            validate_aligned_interval_reminder_input(aligned_input(1_441, None, None)).is_err()
        );
        assert!(
            validate_aligned_interval_reminder_input(aligned_input(30, Some("09:00"), None,))
                .is_err()
        );
        assert!(validate_aligned_interval_reminder_input(aligned_input(
            30,
            Some("09:00"),
            Some("09:00"),
        ))
        .is_err());
    }

    #[test]
    fn reminder_views_restore_aligned_interval_details_and_next_trigger() {
        let rule = AlignedIntervalRule::new(
            Tz::Asia__Shanghai,
            NaiveDateTime::parse_from_str("2026-07-15T09:00", "%Y-%m-%dT%H:%M").unwrap(),
            60,
            vec![ActiveWindow::new(
                NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                NaiveTime::from_hms_opt(18, 0, 0).unwrap(),
            )
            .unwrap()],
        )
        .unwrap();
        let scheduled = scheduled_reminder_with_type(
            "reminder-1",
            "aligned_interval",
            serde_json::to_string(&rule).unwrap(),
        );
        let now = Utc.with_ymd_and_hms(2026, 7, 15, 1, 1, 0).unwrap();

        let views = merge_reminder_views(vec![stored_reminder("reminder-1")], vec![scheduled], now);

        assert_eq!(
            views[0].rule_summary.as_deref(),
            Some("每天 · 每 60 分钟 · 09:00-18:00")
        );
        assert_eq!(
            views[0].next_trigger_at.as_deref(),
            Some("2026-07-15T02:00:00+00:00")
        );
        let details = views[0].rule.as_ref().expect("structured rule details");
        assert_eq!(details.kind, "interval");
        assert_eq!(details.timezone, "Asia/Shanghai");
        assert_eq!(
            details.weekdays,
            ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]
        );
        assert_eq!(details.interval_minutes, Some(60));
        assert_eq!(details.active_window_start.as_deref(), Some("09:00"));
        assert_eq!(details.active_window_end.as_deref(), Some("18:00"));
    }

    #[test]
    fn pause_status_uses_latest_finite_end_and_marks_indefinite_pause_explicitly() {
        let finite = |id: &str, end: i64| PauseSession {
            id: id.to_string(),
            scope: takefive_persistence_sqlite::PauseScope::Global,
            starts_at_utc: 100,
            ends_at_utc: Some(end),
            cancelled_at_utc: None,
            reason: None,
            created_at_utc: 100,
        };
        let status = pause_status_from_sessions(vec![finite("a", 500), finite("b", 900)]);
        assert!(status.is_paused);
        assert_eq!(
            status.paused_until.as_deref(),
            Some("1970-01-01T00:00:00.900+00:00")
        );

        let mut indefinite = finite("forever", 900);
        indefinite.ends_at_utc = None;
        let status = pause_status_from_sessions(vec![indefinite]);
        assert!(status.is_paused);
        assert!(status.paused_until.is_none());
        assert_eq!(status.active_session_ids, ["forever"]);
    }

    #[test]
    fn reminder_views_restore_fixed_time_details_from_stored_rules() {
        let timezone = Tz::Asia__Shanghai;
        let rule = FixedTimeRule::new(
            timezone,
            vec![
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
            ],
            vec![NaiveTime::from_hms_opt(10, 30, 0).unwrap()],
        )
        .unwrap();
        let scheduled = scheduled_reminder("reminder-1", serde_json::to_string(&rule).unwrap());
        let now = DateTime::parse_from_rfc3339("2026-07-17T03:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let views = merge_reminder_views(vec![stored_reminder("reminder-1")], vec![scheduled], now);

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].rule_summary.as_deref(), Some("工作日 · 10:30"));
        let details = views[0].rule.as_ref().expect("structured rule details");
        assert_eq!(details.kind, "fixed");
        assert_eq!(details.weekdays, ["mon", "tue", "wed", "thu", "fri"]);
        assert_eq!(details.times, ["10:30"]);
        assert_eq!(
            views[0].next_trigger_at.as_deref(),
            Some("2026-07-20T02:30:00+00:00")
        );
    }

    #[test]
    fn reminder_views_restore_one_shot_details_before_and_after_expiry() {
        let at = Utc.with_ymd_and_hms(2026, 7, 15, 1, 30, 0).unwrap();
        let rule = OneShotRule::new(at, Tz::Asia__Shanghai);
        let scheduled = scheduled_reminder_with_type(
            "reminder-1",
            "one_shot",
            serde_json::to_string(&rule).unwrap(),
        );
        let before = Utc.with_ymd_and_hms(2026, 7, 14, 0, 0, 0).unwrap();

        let views = merge_reminder_views(
            vec![stored_reminder("reminder-1")],
            vec![scheduled.clone()],
            before,
        );
        assert_eq!(
            views[0].rule_summary.as_deref(),
            Some("一次性 · 2026-07-15 09:30")
        );
        assert_eq!(
            views[0].next_trigger_at.as_deref(),
            Some("2026-07-15T01:30:00+00:00")
        );
        let details = views[0].rule.as_ref().expect("structured rule details");
        assert_eq!(details.kind, "oneShot");
        assert_eq!(details.timezone, "Asia/Shanghai");
        assert_eq!(details.local_date_time.as_deref(), Some("2026-07-15T09:30"));

        let expired =
            merge_reminder_views(vec![stored_reminder("reminder-1")], vec![scheduled], at);
        assert_eq!(
            expired[0].rule_summary.as_deref(),
            Some("一次性 · 2026-07-15 09:30")
        );
        assert!(expired[0].next_trigger_at.is_none());
    }

    #[test]
    fn malformed_rules_do_not_hide_reminders() {
        let scheduled = scheduled_reminder("reminder-1", "{}".to_string());

        let views = merge_reminder_views(
            vec![stored_reminder("reminder-1")],
            vec![scheduled],
            Utc::now(),
        );

        assert_eq!(views.len(), 1);
        assert!(views[0].rule_summary.is_none());
        assert!(views[0].next_trigger_at.is_none());
    }
}
