use crate::{schedule::resolve_local, OccurrenceKey, ScheduleCandidate};
use chrono::{DateTime, Datelike, NaiveDateTime, NaiveTime, TimeDelta, Utc, Weekday};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::fmt;

const MILLIS_PER_MINUTE: i64 = 60_000;
const MAX_INTERVAL_MINUTES: u32 = 1_440;
// The combined minute-of-day and weekday alignment pattern repeats within one
// 7-day cycle, which takes at most 10,080 slots for a one-minute interval.
const MAX_ACTIVE_WINDOW_SEARCH_SLOTS: u64 = 10_080;

fn every_day() -> Vec<Weekday> {
    vec![
        Weekday::Mon,
        Weekday::Tue,
        Weekday::Wed,
        Weekday::Thu,
        Weekday::Fri,
        Weekday::Sat,
        Weekday::Sun,
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveWindow {
    start: NaiveTime,
    end: NaiveTime,
}

impl ActiveWindow {
    pub fn new(start: NaiveTime, end: NaiveTime) -> Result<Self, IntervalError> {
        if start == end {
            return Err(IntervalError::EmptyActiveWindow);
        }
        Ok(Self { start, end })
    }

    pub const fn start(self) -> NaiveTime {
        self.start
    }

    pub const fn end(self) -> NaiveTime {
        self.end
    }

    /// Active windows are start-inclusive and end-exclusive. A window whose
    /// start is later than its end crosses midnight.
    pub fn contains(self, time: NaiveTime) -> bool {
        if self.start < self.end {
            self.start <= time && time < self.end
        } else {
            time >= self.start || time < self.end
        }
    }
}

fn is_active(windows: &[ActiveWindow], time: NaiveTime) -> bool {
    windows.is_empty() || windows.iter().any(|window| window.contains(time))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlignedIntervalRule {
    timezone: Tz,
    anchor_local: NaiveDateTime,
    interval_minutes: u32,
    active_windows: Vec<ActiveWindow>,
    #[serde(default = "every_day")]
    active_weekdays: Vec<Weekday>,
}

impl AlignedIntervalRule {
    pub fn new(
        timezone: Tz,
        anchor_local: NaiveDateTime,
        interval_minutes: u32,
        active_windows: Vec<ActiveWindow>,
    ) -> Result<Self, IntervalError> {
        Self::new_with_weekdays(
            timezone,
            anchor_local,
            interval_minutes,
            active_windows,
            every_day(),
        )
    }

    pub fn new_with_weekdays(
        timezone: Tz,
        anchor_local: NaiveDateTime,
        interval_minutes: u32,
        active_windows: Vec<ActiveWindow>,
        mut active_weekdays: Vec<Weekday>,
    ) -> Result<Self, IntervalError> {
        validate_interval_minutes(interval_minutes)?;
        active_weekdays.sort_by_key(|day| day.num_days_from_monday());
        active_weekdays.dedup();
        if active_weekdays.is_empty() {
            return Err(IntervalError::NoActiveWeekdays);
        }
        Ok(Self {
            timezone,
            anchor_local,
            interval_minutes,
            active_windows,
            active_weekdays,
        })
    }

    pub fn next_after(&self, after: DateTime<Utc>) -> Option<AlignedIntervalCandidate> {
        let after_local = after.with_timezone(&self.timezone).naive_local();
        let interval_millis = i64::from(self.interval_minutes) * MILLIS_PER_MINUTE;
        let elapsed_millis = after_local
            .signed_duration_since(self.anchor_local)
            .num_milliseconds();
        let first_index = if elapsed_millis < 0 {
            0
        } else {
            u64::try_from(elapsed_millis.div_euclid(interval_millis))
                .ok()?
                .checked_add(1)?
        };
        let slots = if self.active_windows.is_empty() && self.active_weekdays.len() == 7 {
            1
        } else {
            MAX_ACTIVE_WINDOW_SEARCH_SLOTS
        };

        for offset in 0..slots {
            let interval_index = first_index.checked_add(offset)?;
            let offset_minutes = i64::from(self.interval_minutes)
                .checked_mul(i64::try_from(interval_index).ok()?)?;
            let planned_local = self
                .anchor_local
                .checked_add_signed(TimeDelta::minutes(offset_minutes))?;
            if !self.active_weekdays.contains(&planned_local.weekday())
                || !is_active(&self.active_windows, planned_local.time())
            {
                continue;
            }

            let (resolved, dst_adjusted) = resolve_local(self.timezone, planned_local)?;
            let scheduled_at_utc = resolved.with_timezone(&Utc);
            if scheduled_at_utc <= after {
                continue;
            }

            return Some(AlignedIntervalCandidate {
                schedule: ScheduleCandidate {
                    scheduled_at_utc,
                    planned_local,
                    resolved_local: resolved.naive_local(),
                    timezone: self.timezone,
                    dst_adjusted,
                },
                interval_index,
                occurrence_key: OccurrenceKey::aligned(
                    self.timezone,
                    self.anchor_local,
                    self.interval_minutes,
                    interval_index,
                ),
            });
        }
        None
    }

    pub fn is_active_at(&self, local_time: NaiveTime) -> bool {
        is_active(&self.active_windows, local_time)
    }

    pub const fn timezone(&self) -> Tz {
        self.timezone
    }

    pub const fn anchor_local(&self) -> NaiveDateTime {
        self.anchor_local
    }

    pub const fn interval_minutes(&self) -> u32 {
        self.interval_minutes
    }

    pub fn active_windows(&self) -> &[ActiveWindow] {
        &self.active_windows
    }

    pub fn active_weekdays(&self) -> &[Weekday] {
        &self.active_weekdays
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlignedIntervalCandidate {
    pub schedule: ScheduleCandidate,
    pub interval_index: u64,
    pub occurrence_key: OccurrenceKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionIntervalRule {
    timezone: Tz,
    interval_millis: i64,
    active_windows: Vec<ActiveWindow>,
}

impl SessionIntervalRule {
    pub fn new(
        timezone: Tz,
        interval: TimeDelta,
        active_windows: Vec<ActiveWindow>,
    ) -> Result<Self, IntervalError> {
        let interval_millis = interval.num_milliseconds();
        if !(1..=TimeDelta::hours(24).num_milliseconds()).contains(&interval_millis) {
            return Err(IntervalError::IntervalOutOfRange);
        }
        Ok(Self {
            timezone,
            interval_millis,
            active_windows,
        })
    }

    pub fn start_session(
        &self,
        session_id: impl Into<String>,
        now_utc: DateTime<Utc>,
    ) -> SessionIntervalState {
        let active = self.is_active_at(now_utc);
        SessionIntervalState {
            session_id: session_id.into(),
            cycle: 0,
            interval_millis: self.interval_millis,
            remaining_millis: self.interval_millis,
            running_since_utc: active.then_some(now_utc),
        }
    }

    pub fn is_active_at(&self, at_utc: DateTime<Utc>) -> bool {
        is_active(
            &self.active_windows,
            at_utc.with_timezone(&self.timezone).time(),
        )
    }

    pub const fn timezone(&self) -> Tz {
        self.timezone
    }

    pub fn interval(&self) -> TimeDelta {
        TimeDelta::milliseconds(self.interval_millis)
    }

    pub fn active_windows(&self) -> &[ActiveWindow] {
        &self.active_windows
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionIntervalState {
    session_id: String,
    cycle: u64,
    interval_millis: i64,
    remaining_millis: i64,
    running_since_utc: Option<DateTime<Utc>>,
}

impl SessionIntervalState {
    pub fn pause(&mut self, now_utc: DateTime<Utc>) {
        self.remaining_millis = self.remaining_millis_at(now_utc);
        self.running_since_utc = None;
    }

    pub fn resume(&mut self, now_utc: DateTime<Utc>) {
        if self.running_since_utc.is_none() && self.remaining_millis > 0 {
            self.running_since_utc = Some(now_utc);
        }
    }

    /// Reconciles the timer with the current active-window state. Call this at
    /// window boundaries as well as on lock, sleep, pause and resume events.
    pub fn reconcile(&mut self, rule: &SessionIntervalRule, now_utc: DateTime<Utc>) {
        if rule.is_active_at(now_utc) {
            self.resume(now_utc);
        } else {
            self.pause(now_utc);
        }
    }

    pub fn remaining_at(&self, now_utc: DateTime<Utc>) -> TimeDelta {
        TimeDelta::milliseconds(self.remaining_millis_at(now_utc))
    }

    pub fn due_at_utc(&self) -> Option<DateTime<Utc>> {
        self.running_since_utc?
            .checked_add_signed(TimeDelta::milliseconds(self.remaining_millis))
    }

    pub fn is_due_at(&self, now_utc: DateTime<Utc>) -> bool {
        self.remaining_millis_at(now_utc) == 0
    }

    pub fn begin_next_cycle(
        &mut self,
        now_utc: DateTime<Utc>,
        active: bool,
    ) -> Result<(), IntervalError> {
        self.cycle = self
            .cycle
            .checked_add(1)
            .ok_or(IntervalError::CycleOverflow)?;
        self.remaining_millis = self.interval_millis;
        self.running_since_utc = active.then_some(now_utc);
        Ok(())
    }

    pub fn occurrence_key(&self) -> OccurrenceKey {
        OccurrenceKey::session(&self.session_id, self.cycle)
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub const fn cycle(&self) -> u64 {
        self.cycle
    }

    pub const fn is_running(&self) -> bool {
        self.running_since_utc.is_some()
    }

    fn remaining_millis_at(&self, now_utc: DateTime<Utc>) -> i64 {
        let elapsed = self
            .running_since_utc
            .map(|started| now_utc.signed_duration_since(started).num_milliseconds())
            .unwrap_or(0)
            .max(0);
        self.remaining_millis.saturating_sub(elapsed).max(0)
    }
}

fn validate_interval_minutes(interval_minutes: u32) -> Result<(), IntervalError> {
    if !(1..=MAX_INTERVAL_MINUTES).contains(&interval_minutes) {
        return Err(IntervalError::IntervalOutOfRange);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntervalError {
    IntervalOutOfRange,
    EmptyActiveWindow,
    NoActiveWeekdays,
    CycleOverflow,
}

impl fmt::Display for IntervalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IntervalOutOfRange => {
                formatter.write_str("interval must be between zero and 24 hours")
            }
            Self::EmptyActiveWindow => {
                formatter.write_str("active window start and end must differ")
            }
            Self::NoActiveWeekdays => formatter.write_str("at least one weekday is required"),
            Self::CycleOverflow => formatter.write_str("session interval cycle overflow"),
        }
    }
}

impl std::error::Error for IntervalError {}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};
    use pretty_assertions::assert_eq;

    fn local_date_time(hour: u32, minute: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 7, 14)
            .unwrap()
            .and_hms_opt(hour, minute, 0)
            .unwrap()
    }

    fn shanghai_utc(hour: u32, minute: u32) -> DateTime<Utc> {
        chrono_tz::Asia::Shanghai
            .with_ymd_and_hms(2026, 7, 14, hour, minute, 0)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn aligned_interval_is_computed_from_anchor_not_last_delivery() {
        let rule = AlignedIntervalRule::new(
            chrono_tz::Asia::Shanghai,
            local_date_time(9, 0),
            60,
            Vec::new(),
        )
        .unwrap();

        let first = rule.next_after(shanghai_utc(9, 1)).unwrap();
        let after_late_delivery = first.schedule.scheduled_at_utc + TimeDelta::minutes(7);
        let second = rule.next_after(after_late_delivery).unwrap();

        assert_eq!(first.schedule.planned_local, local_date_time(10, 0));
        assert_eq!(second.schedule.planned_local, local_date_time(11, 0));
        assert_eq!(first.interval_index, 1);
        assert_eq!(second.interval_index, 2);
    }

    #[test]
    fn aligned_interval_skips_inactive_gap_without_changing_anchor() {
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
        let rule = AlignedIntervalRule::new(
            chrono_tz::Asia::Shanghai,
            local_date_time(9, 0),
            60,
            windows,
        )
        .unwrap();

        let next = rule.next_after(shanghai_utc(11, 1)).unwrap();

        assert_eq!(next.schedule.planned_local, local_date_time(13, 0));
        assert_eq!(next.interval_index, 4);
        assert_eq!(rule.anchor_local(), local_date_time(9, 0));
    }

    #[test]
    fn aligned_interval_skips_lunch_and_weekends() {
        let windows = vec![
            ActiveWindow::new(
                NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
            )
            .unwrap(),
            ActiveWindow::new(
                NaiveTime::from_hms_opt(13, 30, 0).unwrap(),
                NaiveTime::from_hms_opt(18, 0, 0).unwrap(),
            )
            .unwrap(),
        ];
        let workdays = vec![
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
        ];
        let rule = AlignedIntervalRule::new_with_weekdays(
            chrono_tz::Asia::Shanghai,
            local_date_time(9, 0),
            60,
            windows,
            workdays,
        )
        .unwrap();

        let friday_after_work = chrono_tz::Asia::Shanghai
            .with_ymd_and_hms(2026, 7, 17, 18, 1, 0)
            .unwrap()
            .with_timezone(&Utc);
        let next_workday = rule.next_after(friday_after_work).unwrap();
        assert_eq!(
            next_workday.schedule.planned_local,
            NaiveDate::from_ymd_opt(2026, 7, 20)
                .unwrap()
                .and_hms_opt(9, 0, 0)
                .unwrap()
        );

        let before_lunch = chrono_tz::Asia::Shanghai
            .with_ymd_and_hms(2026, 7, 20, 11, 1, 0)
            .unwrap()
            .with_timezone(&Utc);
        let after_lunch = rule.next_after(before_lunch).unwrap();
        assert_eq!(
            after_lunch.schedule.planned_local,
            NaiveDate::from_ymd_opt(2026, 7, 20)
                .unwrap()
                .and_hms_opt(14, 0, 0)
                .unwrap()
        );
    }

    #[test]
    fn active_window_supports_cross_midnight() {
        let window = ActiveWindow::new(
            NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
            NaiveTime::from_hms_opt(2, 0, 0).unwrap(),
        )
        .unwrap();

        assert!(window.contains(NaiveTime::from_hms_opt(23, 59, 0).unwrap()));
        assert!(window.contains(NaiveTime::from_hms_opt(1, 59, 0).unwrap()));
        assert!(!window.contains(NaiveTime::from_hms_opt(2, 0, 0).unwrap()));
        assert!(!window.contains(NaiveTime::from_hms_opt(12, 0, 0).unwrap()));
    }

    #[test]
    fn snooze_or_late_display_cannot_change_aligned_occurrence_key() {
        let rule = AlignedIntervalRule::new(
            chrono_tz::Asia::Shanghai,
            local_date_time(9, 0),
            60,
            Vec::new(),
        )
        .unwrap();
        let candidate = rule.next_after(shanghai_utc(9, 30)).unwrap();

        let same_slot = OccurrenceKey::aligned(
            rule.timezone(),
            rule.anchor_local(),
            rule.interval_minutes(),
            candidate.interval_index,
        );

        assert_eq!(candidate.occurrence_key, same_slot);
        assert_eq!(
            candidate.occurrence_key.as_str(),
            "v1|aligned|tz=Asia/Shanghai|anchor=2026-07-14T09:00:00|every=60m|index=1"
        );
    }

    #[test]
    fn session_interval_freezes_remaining_time_and_resumes() {
        let rule = SessionIntervalRule::new(
            chrono_tz::Asia::Shanghai,
            TimeDelta::minutes(60),
            Vec::new(),
        )
        .unwrap();
        let start = shanghai_utc(9, 23);
        let mut state = rule.start_session("session-a", start);

        state.pause(start + TimeDelta::minutes(20));
        assert_eq!(
            state.remaining_at(start + TimeDelta::hours(2)),
            TimeDelta::minutes(40)
        );
        assert!(!state.is_running());

        let resumed = start + TimeDelta::hours(2);
        state.resume(resumed);
        assert_eq!(state.due_at_utc(), Some(resumed + TimeDelta::minutes(40)));
        assert!(!state.is_due_at(resumed + TimeDelta::minutes(39)));
        assert!(state.is_due_at(resumed + TimeDelta::minutes(40)));
    }

    #[test]
    fn session_starts_frozen_outside_active_window() {
        let rule = SessionIntervalRule::new(
            chrono_tz::Asia::Shanghai,
            TimeDelta::minutes(60),
            vec![ActiveWindow::new(
                NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                NaiveTime::from_hms_opt(18, 0, 0).unwrap(),
            )
            .unwrap()],
        )
        .unwrap();

        let mut state = rule.start_session("session-b", shanghai_utc(8, 30));
        assert!(!state.is_running());
        state.reconcile(&rule, shanghai_utc(9, 0));
        assert!(state.is_running());
        assert_eq!(state.due_at_utc(), Some(shanghai_utc(10, 0)));
    }

    #[test]
    fn interval_models_round_trip_through_json() {
        let rule = AlignedIntervalRule::new(
            chrono_tz::Asia::Shanghai,
            local_date_time(9, 0),
            30,
            Vec::new(),
        )
        .unwrap();
        let json = serde_json::to_string(&rule).unwrap();
        let decoded: AlignedIntervalRule = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, rule);
    }

    #[test]
    fn legacy_aligned_interval_json_defaults_to_every_day() {
        let rule = AlignedIntervalRule::new(
            chrono_tz::Asia::Shanghai,
            local_date_time(9, 0),
            30,
            Vec::new(),
        )
        .unwrap();
        let mut json = serde_json::to_value(rule).unwrap();
        json.as_object_mut().unwrap().remove("active_weekdays");

        let decoded: AlignedIntervalRule = serde_json::from_value(json).unwrap();

        assert_eq!(decoded.active_weekdays(), every_day());
    }
}
