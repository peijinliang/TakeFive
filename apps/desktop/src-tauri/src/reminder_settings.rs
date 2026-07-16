use chrono::{
    DateTime, Days, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, TimeZone, Utc,
};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

pub(crate) const REMINDER_SETTINGS_KEY: &str = "reminder_settings.v1";
pub(crate) const DEFAULT_AUTO_DISMISS_SECONDS: u32 = 7;
pub(crate) const DEFAULT_APP_DISPLAY_NAME: &str = "摸个鱼 TakeFive";
const DEFAULT_QUIET_START: &str = "12:00";
const DEFAULT_QUIET_END: &str = "13:30";
const MAX_AUTO_DISMISS_SECONDS: u32 = 60;
const MAX_APP_DISPLAY_NAME_CHARS: usize = 30;
const MAX_DST_GAP_MINUTES: i64 = 180;

fn default_app_display_name() -> String {
    DEFAULT_APP_DISPLAY_NAME.to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReminderSettings {
    #[serde(default = "default_app_display_name")]
    pub(crate) app_display_name: String,
    pub(crate) auto_dismiss_seconds: u32,
    pub(crate) quiet_hours: QuietHoursSettings,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QuietHoursSettings {
    pub(crate) enabled: bool,
    pub(crate) start_local: String,
    pub(crate) end_local: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) timezone: Option<String>,
}

impl ReminderSettings {
    pub(crate) fn defaults(timezone: Option<String>) -> Self {
        Self {
            app_display_name: default_app_display_name(),
            auto_dismiss_seconds: DEFAULT_AUTO_DISMISS_SECONDS,
            quiet_hours: QuietHoursSettings {
                enabled: true,
                start_local: DEFAULT_QUIET_START.to_string(),
                end_local: DEFAULT_QUIET_END.to_string(),
                timezone,
            },
        }
    }

    pub(crate) fn load(
        value_json: Option<&str>,
        default_timezone: Option<String>,
    ) -> Result<Self, String> {
        let settings = match value_json {
            Some(value) => serde_json::from_str(value)
                .map_err(|error| format!("invalid_reminder_settings: {error}"))?,
            None => Self::defaults(default_timezone),
        };
        settings.validate()
    }

    pub(crate) fn validate(mut self) -> Result<Self, String> {
        self.app_display_name = self.app_display_name.trim().to_string();
        if self.app_display_name.is_empty()
            || self.app_display_name.chars().count() > MAX_APP_DISPLAY_NAME_CHARS
        {
            return Err("app_display_name_out_of_range".to_string());
        }

        if !(1..=MAX_AUTO_DISMISS_SECONDS).contains(&self.auto_dismiss_seconds) {
            return Err("auto_dismiss_seconds_out_of_range".to_string());
        }

        let start = parse_time(&self.quiet_hours.start_local)?;
        let end = parse_time(&self.quiet_hours.end_local)?;
        if start == end {
            return Err("quiet_hours_start_equals_end".to_string());
        }
        if let Some(timezone) = &self.quiet_hours.timezone {
            Tz::from_str(timezone).map_err(|_| "invalid_quiet_hours_timezone".to_string())?;
        }
        Ok(self)
    }

    pub(crate) fn quiet_until(
        &self,
        now: DateTime<Utc>,
        fallback_timezone: &str,
    ) -> Result<Option<DateTime<Utc>>, String> {
        if !self.quiet_hours.enabled {
            return Ok(None);
        }

        let timezone_name = self
            .quiet_hours
            .timezone
            .as_deref()
            .unwrap_or(fallback_timezone);
        let timezone =
            Tz::from_str(timezone_name).map_err(|_| "invalid_quiet_hours_timezone".to_string())?;
        let start = parse_time(&self.quiet_hours.start_local)?;
        let end = parse_time(&self.quiet_hours.end_local)?;
        let local_now = now.with_timezone(&timezone);
        let date = local_now.date_naive();
        let time = local_now.time();

        let end_date = if start < end {
            (start <= time && time < end).then_some(date)
        } else if time >= start {
            date.checked_add_days(Days::new(1))
        } else if time < end {
            Some(date)
        } else {
            None
        };

        let Some(end_date) = end_date else {
            return Ok(None);
        };
        let resolved = resolve_local_end(timezone, end_date, end)
            .ok_or_else(|| "quiet_hours_end_unresolvable".to_string())?;
        let until = resolved.with_timezone(&Utc);
        Ok((until > now).then_some(until))
    }
}

fn parse_time(value: &str) -> Result<NaiveTime, String> {
    let parsed = NaiveTime::parse_from_str(value, "%H:%M")
        .map_err(|_| "invalid_quiet_hours_time".to_string())?;
    if parsed.format("%H:%M").to_string() != value {
        return Err("invalid_quiet_hours_time".to_string());
    }
    Ok(parsed)
}

fn resolve_local_end(timezone: Tz, date: NaiveDate, time: NaiveTime) -> Option<DateTime<Tz>> {
    let planned = NaiveDateTime::new(date, time);
    for minute in 0..=MAX_DST_GAP_MINUTES {
        let shifted = planned.checked_add_signed(TimeDelta::minutes(minute))?;
        match timezone.from_local_datetime(&shifted) {
            LocalResult::Single(value) => return Some(value),
            LocalResult::Ambiguous(first, second) => {
                return Some(if first.with_timezone(&Utc) >= second.with_timezone(&Utc) {
                    first
                } else {
                    second
                });
            }
            LocalResult::None => continue,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 15, hour, minute, 0).unwrap()
    }

    #[test]
    fn defaults_are_seven_seconds_and_lunch_quiet_hours() {
        let settings = ReminderSettings::defaults(Some("UTC".to_string()));
        assert_eq!(settings.app_display_name, DEFAULT_APP_DISPLAY_NAME);
        assert_eq!(settings.auto_dismiss_seconds, 7);
        assert!(settings.quiet_hours.enabled);
        assert_eq!(settings.quiet_hours.start_local, "12:00");
        assert_eq!(settings.quiet_hours.end_local, "13:30");
        assert_eq!(
            settings.quiet_until(at(12, 15), "UTC").unwrap(),
            Some(at(13, 30))
        );
        assert_eq!(settings.quiet_until(at(13, 30), "UTC").unwrap(), None);
    }

    #[test]
    fn crossing_midnight_quiet_hours_use_the_next_local_end() {
        let settings = ReminderSettings {
            app_display_name: DEFAULT_APP_DISPLAY_NAME.to_string(),
            auto_dismiss_seconds: 7,
            quiet_hours: QuietHoursSettings {
                enabled: true,
                start_local: "22:00".to_string(),
                end_local: "07:00".to_string(),
                timezone: Some("UTC".to_string()),
            },
        };
        let expected = Utc.with_ymd_and_hms(2026, 7, 16, 7, 0, 0).unwrap();
        assert_eq!(
            settings.quiet_until(at(23, 30), "UTC").unwrap(),
            Some(expected)
        );
        assert_eq!(
            settings.quiet_until(at(6, 30), "UTC").unwrap(),
            Some(at(7, 0))
        );
        assert_eq!(settings.quiet_until(at(12, 0), "UTC").unwrap(), None);
    }

    #[test]
    fn spring_dst_gap_moves_a_nonexistent_quiet_end_to_the_next_valid_instant() {
        let settings = ReminderSettings {
            app_display_name: DEFAULT_APP_DISPLAY_NAME.to_string(),
            auto_dismiss_seconds: 7,
            quiet_hours: QuietHoursSettings {
                enabled: true,
                start_local: "01:30".to_string(),
                end_local: "02:30".to_string(),
                timezone: Some("America/New_York".to_string()),
            },
        };
        let now = Utc.with_ymd_and_hms(2026, 3, 8, 6, 45, 0).unwrap();
        let first_valid_after_gap = Utc.with_ymd_and_hms(2026, 3, 8, 7, 0, 0).unwrap();

        assert_eq!(
            settings.quiet_until(now, "UTC").unwrap(),
            Some(first_valid_after_gap)
        );
    }

    #[test]
    fn fall_dst_overlap_uses_the_later_repeated_quiet_end() {
        let settings = ReminderSettings {
            app_display_name: DEFAULT_APP_DISPLAY_NAME.to_string(),
            auto_dismiss_seconds: 7,
            quiet_hours: QuietHoursSettings {
                enabled: true,
                start_local: "00:30".to_string(),
                end_local: "01:30".to_string(),
                timezone: Some("America/New_York".to_string()),
            },
        };
        let second_one_fifteen = Utc.with_ymd_and_hms(2026, 11, 1, 6, 15, 0).unwrap();
        let later_one_thirty = Utc.with_ymd_and_hms(2026, 11, 1, 6, 30, 0).unwrap();

        assert_eq!(
            settings.quiet_until(second_one_fifteen, "UTC").unwrap(),
            Some(later_one_thirty)
        );
    }

    #[test]
    fn validates_duration_time_and_timezone() {
        let mut settings = ReminderSettings::defaults(Some("UTC".to_string()));
        settings.auto_dismiss_seconds = 0;
        assert_eq!(
            settings.validate().unwrap_err(),
            "auto_dismiss_seconds_out_of_range"
        );

        let mut settings = ReminderSettings::defaults(Some("UTC".to_string()));
        settings.quiet_hours.end_local = "12:00".to_string();
        assert_eq!(
            settings.validate().unwrap_err(),
            "quiet_hours_start_equals_end"
        );

        let settings = ReminderSettings::defaults(Some("Not/AZone".to_string()));
        assert_eq!(
            settings.validate().unwrap_err(),
            "invalid_quiet_hours_timezone"
        );
    }

    #[test]
    fn legacy_settings_receive_the_default_display_name() {
        let settings = ReminderSettings::load(
            Some(
                r#"{"autoDismissSeconds":7,"quietHours":{"enabled":true,"startLocal":"12:00","endLocal":"13:30","timezone":"UTC"}}"#,
            ),
            None,
        )
        .unwrap();

        assert_eq!(settings.app_display_name, DEFAULT_APP_DISPLAY_NAME);
    }

    #[test]
    fn display_name_is_trimmed_and_validated_by_character_count() {
        let mut settings = ReminderSettings::defaults(Some("UTC".to_string()));
        settings.app_display_name = "  Project Notes  ".to_string();
        assert_eq!(
            settings.validate().unwrap().app_display_name,
            "Project Notes"
        );

        let mut settings = ReminderSettings::defaults(Some("UTC".to_string()));
        settings.app_display_name = "   ".to_string();
        assert_eq!(
            settings.validate().unwrap_err(),
            "app_display_name_out_of_range"
        );

        let mut settings = ReminderSettings::defaults(Some("UTC".to_string()));
        settings.app_display_name = "名".repeat(MAX_APP_DISPLAY_NAME_CHARS + 1);
        assert_eq!(
            settings.validate().unwrap_err(),
            "app_display_name_out_of_range"
        );
    }
}
