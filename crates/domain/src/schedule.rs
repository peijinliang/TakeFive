use chrono::{
    DateTime, Datelike, Days, LocalResult, NaiveDateTime, NaiveTime, TimeDelta, TimeZone, Utc,
    Weekday,
};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::fmt;

const MAX_LOOKAHEAD_DAYS: u64 = 3660;
const MAX_DST_GAP_MINUTES: i64 = 180;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleCandidate {
    pub scheduled_at_utc: DateTime<Utc>,
    pub planned_local: NaiveDateTime,
    pub resolved_local: NaiveDateTime,
    pub timezone: Tz,
    pub dst_adjusted: bool,
}

impl ScheduleCandidate {
    pub fn occurrence_key(&self) -> String {
        format!(
            "{}|{}|fold=0",
            self.timezone,
            self.planned_local.format("%Y-%m-%dT%H:%M:%S")
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixedTimeRule {
    timezone: Tz,
    weekdays: Vec<Weekday>,
    times: Vec<NaiveTime>,
}

impl FixedTimeRule {
    pub fn new(
        timezone: Tz,
        mut weekdays: Vec<Weekday>,
        mut times: Vec<NaiveTime>,
    ) -> Result<Self, ScheduleError> {
        if weekdays.is_empty() {
            return Err(ScheduleError::NoWeekdays);
        }
        if times.is_empty() {
            return Err(ScheduleError::NoTimes);
        }

        weekdays.sort_by_key(|day| day.num_days_from_monday());
        weekdays.dedup();
        times.sort();
        times.dedup();

        Ok(Self {
            timezone,
            weekdays,
            times,
        })
    }

    pub fn next_after(&self, after: DateTime<Utc>) -> Option<ScheduleCandidate> {
        let start_date = after.with_timezone(&self.timezone).date_naive();

        for offset in 0..=MAX_LOOKAHEAD_DAYS {
            let date = start_date.checked_add_days(Days::new(offset))?;
            if !self.weekdays.contains(&date.weekday()) {
                continue;
            }

            for time in &self.times {
                let planned_local = date.and_time(*time);
                let (resolved, dst_adjusted) = resolve_local(self.timezone, planned_local)?;
                let scheduled_at_utc = resolved.with_timezone(&Utc);

                if scheduled_at_utc > after {
                    return Some(ScheduleCandidate {
                        scheduled_at_utc,
                        planned_local,
                        resolved_local: resolved.naive_local(),
                        timezone: self.timezone,
                        dst_adjusted,
                    });
                }
            }
        }

        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OneShotRule {
    at_utc: DateTime<Utc>,
    source_timezone: Tz,
}

impl OneShotRule {
    pub fn new(at_utc: DateTime<Utc>, source_timezone: Tz) -> Self {
        Self {
            at_utc,
            source_timezone,
        }
    }

    pub fn next_after(&self, after: DateTime<Utc>) -> Option<ScheduleCandidate> {
        if self.at_utc <= after {
            return None;
        }

        let local = self
            .at_utc
            .with_timezone(&self.source_timezone)
            .naive_local();
        Some(ScheduleCandidate {
            scheduled_at_utc: self.at_utc,
            planned_local: local,
            resolved_local: local,
            timezone: self.source_timezone,
            dst_adjusted: false,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "config", rename_all = "snake_case")]
pub enum ScheduleRule {
    FixedTimes(FixedTimeRule),
    OneShot(OneShotRule),
}

impl ScheduleRule {
    pub fn next_after(&self, after: DateTime<Utc>) -> Option<ScheduleCandidate> {
        match self {
            Self::FixedTimes(rule) => rule.next_after(after),
            Self::OneShot(rule) => rule.next_after(after),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleError {
    NoWeekdays,
    NoTimes,
}

impl fmt::Display for ScheduleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoWeekdays => formatter.write_str("at least one weekday is required"),
            Self::NoTimes => formatter.write_str("at least one local time is required"),
        }
    }
}

impl std::error::Error for ScheduleError {}

pub(crate) fn resolve_local(timezone: Tz, planned: NaiveDateTime) -> Option<(DateTime<Tz>, bool)> {
    match timezone.from_local_datetime(&planned) {
        LocalResult::Single(value) => Some((value, false)),
        LocalResult::Ambiguous(first, second) => {
            let first_utc = first.with_timezone(&Utc);
            let second_utc = second.with_timezone(&Utc);
            Some((
                if first_utc <= second_utc {
                    first
                } else {
                    second
                },
                false,
            ))
        }
        LocalResult::None => {
            for minute in 1..=MAX_DST_GAP_MINUTES {
                let shifted = planned.checked_add_signed(TimeDelta::minutes(minute))?;
                match timezone.from_local_datetime(&shifted) {
                    LocalResult::Single(value) => return Some((value, true)),
                    LocalResult::Ambiguous(first, second) => {
                        let first_utc = first.with_timezone(&Utc);
                        let second_utc = second.with_timezone(&Utc);
                        return Some((
                            if first_utc <= second_utc {
                                first
                            } else {
                                second
                            },
                            true,
                        ));
                    }
                    LocalResult::None => continue,
                }
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, NaiveTime, TimeZone};
    use pretty_assertions::assert_eq;

    #[test]
    fn fixed_time_skips_weekend_and_keeps_wall_clock_time() {
        let rule = FixedTimeRule::new(
            chrono_tz::Asia::Shanghai,
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
        let after = Utc.with_ymd_and_hms(2026, 7, 17, 2, 31, 0).unwrap();

        let next = rule.next_after(after).unwrap();

        assert_eq!(
            next.scheduled_at_utc,
            Utc.with_ymd_and_hms(2026, 7, 20, 2, 30, 0).unwrap()
        );
        assert_eq!(
            next.occurrence_key(),
            "Asia/Shanghai|2026-07-20T10:30:00|fold=0"
        );
    }

    #[test]
    fn named_timezone_keeps_local_wall_clock_time_across_travel_and_dst() {
        let rule = FixedTimeRule::new(
            chrono_tz::America::New_York,
            vec![Weekday::Mon],
            vec![NaiveTime::from_hms_opt(9, 0, 0).unwrap()],
        )
        .unwrap();
        let before_us_dst_change = Utc.with_ymd_and_hms(2026, 3, 6, 14, 1, 0).unwrap();

        let next = rule.next_after(before_us_dst_change).unwrap();

        assert_eq!(next.planned_local.weekday(), Weekday::Mon);
        assert_eq!(
            next.planned_local.time(),
            NaiveTime::from_hms_opt(9, 0, 0).unwrap()
        );
        assert_eq!(
            next.scheduled_at_utc,
            Utc.with_ymd_and_hms(2026, 3, 9, 13, 0, 0).unwrap()
        );
        assert_eq!(next.timezone, chrono_tz::America::New_York);
    }

    #[test]
    fn weekday_is_evaluated_in_the_rule_timezone_near_utc_midnight() {
        let rule = FixedTimeRule::new(
            chrono_tz::Asia::Tokyo,
            vec![Weekday::Mon],
            vec![NaiveTime::from_hms_opt(0, 30, 0).unwrap()],
        )
        .unwrap();
        let sunday_utc = Utc.with_ymd_and_hms(2026, 7, 19, 14, 0, 0).unwrap();

        let next = rule.next_after(sunday_utc).unwrap();

        assert_eq!(next.planned_local.weekday(), Weekday::Mon);
        assert_eq!(
            next.scheduled_at_utc,
            Utc.with_ymd_and_hms(2026, 7, 19, 15, 30, 0).unwrap()
        );
    }

    #[test]
    fn spring_dst_gap_moves_to_first_valid_instant() {
        let rule = FixedTimeRule::new(
            chrono_tz::America::New_York,
            vec![Weekday::Sun],
            vec![NaiveTime::from_hms_opt(2, 30, 0).unwrap()],
        )
        .unwrap();
        let after = Utc.with_ymd_and_hms(2026, 3, 7, 12, 0, 0).unwrap();

        let next = rule.next_after(after).unwrap();

        assert!(next.dst_adjusted);
        assert_eq!(
            next.resolved_local.time(),
            NaiveTime::from_hms_opt(3, 0, 0).unwrap()
        );
    }

    #[test]
    fn autumn_dst_fold_does_not_emit_the_second_copy() {
        let rule = FixedTimeRule::new(
            chrono_tz::America::New_York,
            vec![Weekday::Sun],
            vec![NaiveTime::from_hms_opt(1, 30, 0).unwrap()],
        )
        .unwrap();
        let after_first_copy = Utc.with_ymd_and_hms(2026, 11, 1, 5, 31, 0).unwrap();

        let next = rule.next_after(after_first_copy).unwrap();

        assert_eq!(next.planned_local.date().day(), 8);
    }

    #[test]
    fn one_shot_is_not_returned_after_its_absolute_instant() {
        let at = Utc.with_ymd_and_hms(2026, 7, 14, 10, 0, 0).unwrap();
        let rule = OneShotRule::new(at, chrono_tz::Asia::Shanghai);

        assert!(rule.next_after(at - TimeDelta::seconds(1)).is_some());
        assert!(rule.next_after(at).is_none());
    }
}
