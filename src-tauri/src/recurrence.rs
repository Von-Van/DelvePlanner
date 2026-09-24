//! When a repeating task comes back. Pure date arithmetic, so every rule can be tested without a
//! database.
//!
//! A repeating task has one open occurrence at a time. Finishing it creates the next one on the
//! first date the rule allows after both the finished occurrence's day and today, so a task
//! finished late never comes back already overdue. A missed occurrence simply stays open and shows
//! up in carry-forward; skipping it moves it to its next date.

use crate::error::{AppError, AppResult};
use crate::model::{Recurrence, RecurrenceFrequency, MAX_RECURRENCE_INTERVAL};
use chrono::{Datelike, Days, Months, NaiveDate, Weekday};

/// Checks a rule and fills in what it leaves to the task's own day: the weekday of a weekly rule
/// with none chosen, and the day of the month of a monthly one. Fields that don't apply to the
/// frequency are cleared, so equal rules compare equal.
pub fn normalized(rule: &Recurrence, day: NaiveDate) -> AppResult<Recurrence> {
    if !(1..=MAX_RECURRENCE_INTERVAL).contains(&rule.interval) {
        return Err(AppError::Validation(
            "A task can repeat every 1 to 99 days, weeks, or months.".into(),
        ));
    }
    let mut normalized = Recurrence {
        frequency: rule.frequency,
        interval: rule.interval,
        weekdays: Vec::new(),
        month_day: None,
    };
    match rule.frequency {
        RecurrenceFrequency::Daily => {}
        RecurrenceFrequency::Weekdays => normalized.interval = 1,
        RecurrenceFrequency::Weekly => {
            let mut weekdays = rule.weekdays.clone();
            if weekdays.iter().any(|weekday| !(1..=7).contains(weekday)) {
                return Err(AppError::Validation(
                    "Weekdays run from 1 (Monday) to 7 (Sunday).".into(),
                ));
            }
            weekdays.sort_unstable();
            weekdays.dedup();
            if weekdays.is_empty() {
                weekdays.push(iso_weekday(day));
            }
            normalized.weekdays = weekdays;
        }
        RecurrenceFrequency::Monthly => {
            let month_day = rule.month_day.unwrap_or(day.day() as u8);
            if !(1..=31).contains(&month_day) {
                return Err(AppError::Validation(
                    "A monthly task repeats on a day from 1 to 31.".into(),
                ));
            }
            normalized.month_day = Some(month_day);
        }
    }
    Ok(normalized)
}

/// The first date the rule allows strictly after `day`.
pub fn step(rule: &Recurrence, day: NaiveDate) -> NaiveDate {
    let interval = rule.interval.clamp(1, MAX_RECURRENCE_INTERVAL);
    match rule.frequency {
        RecurrenceFrequency::Daily => add_days(day, u64::from(interval)),
        RecurrenceFrequency::Weekdays => {
            let mut next = add_days(day, 1);
            while matches!(next.weekday(), Weekday::Sat | Weekday::Sun) {
                next = add_days(next, 1);
            }
            next
        }
        RecurrenceFrequency::Weekly => {
            let current = iso_weekday(day);
            let weekdays = if rule.weekdays.is_empty() {
                vec![current]
            } else {
                rule.weekdays.clone()
            };
            if let Some(later) = weekdays.iter().copied().find(|weekday| *weekday > current) {
                return add_days(day, u64::from(later - current));
            }
            // Past the last chosen weekday: jump `interval` weeks ahead to the first one.
            let monday = day - Days::new(u64::from(current - 1));
            let first = weekdays.first().copied().unwrap_or(current);
            add_days(
                monday,
                u64::from(interval) * 7 + u64::from(first.saturating_sub(1)),
            )
        }
        RecurrenceFrequency::Monthly => {
            let month_day = rule.month_day.unwrap_or(day.day() as u8);
            let first_of_month = day.with_day(1).unwrap_or(day);
            let target = first_of_month
                .checked_add_months(Months::new(interval))
                .unwrap_or(first_of_month);
            let last = last_day_of_month(target);
            target
                .with_day(u32::from(month_day).min(last))
                .unwrap_or(target)
        }
    }
}

/// Where the next occurrence goes after one on `day`: the first date the rule allows after both
/// `day` and `today`.
pub fn next_occurrence(rule: &Recurrence, day: NaiveDate, today: NaiveDate) -> NaiveDate {
    let mut next = step(rule, day);
    // Every step moves forward, and a rule steps at most 99 months at a time, so this ends; the
    // cap only guards against a date library that stops moving at the end of its range.
    let mut guard = 0;
    while next <= today && guard < 100_000 {
        next = step(rule, next);
        guard += 1;
    }
    next
}

fn iso_weekday(day: NaiveDate) -> u8 {
    day.weekday().number_from_monday() as u8
}

fn add_days(day: NaiveDate, days: u64) -> NaiveDate {
    day.checked_add_days(Days::new(days)).unwrap_or(day)
}

fn last_day_of_month(day: NaiveDate) -> u32 {
    let first = day.with_day(1).unwrap_or(day);
    first
        .checked_add_months(Months::new(1))
        .and_then(|next| next.pred_opt())
        .map(|last| last.day())
        .unwrap_or(28)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(value: &str) -> NaiveDate {
        NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
    }

    fn rule(frequency: RecurrenceFrequency, interval: u32) -> Recurrence {
        Recurrence {
            frequency,
            interval,
            weekdays: Vec::new(),
            month_day: None,
        }
    }

    #[test]
    fn daily_rules_step_by_their_interval() {
        let every_day = rule(RecurrenceFrequency::Daily, 1);
        assert_eq!(step(&every_day, day("2026-09-30")), day("2026-10-01"));
        let every_third = rule(RecurrenceFrequency::Daily, 3);
        assert_eq!(step(&every_third, day("2026-09-30")), day("2026-10-03"));
    }

    #[test]
    fn weekday_rules_skip_the_weekend() {
        let weekdays = rule(RecurrenceFrequency::Weekdays, 1);
        // Thursday, Friday, then Monday.
        assert_eq!(step(&weekdays, day("2026-09-24")), day("2026-09-25"));
        assert_eq!(step(&weekdays, day("2026-09-25")), day("2026-09-28"));
        assert_eq!(step(&weekdays, day("2026-09-26")), day("2026-09-28"));
    }

    #[test]
    fn weekly_rules_visit_each_chosen_weekday_then_jump_the_interval() {
        let mut every_other = rule(RecurrenceFrequency::Weekly, 2);
        every_other.weekdays = vec![1, 4];
        // Monday 2026-09-21 → Thursday the same week → Monday two weeks later.
        assert_eq!(step(&every_other, day("2026-09-21")), day("2026-09-24"));
        assert_eq!(step(&every_other, day("2026-09-24")), day("2026-10-05"));
        // A task moved to a Tuesday still comes back on its chosen days.
        assert_eq!(step(&every_other, day("2026-09-22")), day("2026-09-24"));
        let weekly = normalized(&rule(RecurrenceFrequency::Weekly, 1), day("2026-09-23")).unwrap();
        assert_eq!(
            weekly.weekdays,
            [3],
            "no weekday chosen means the task's own"
        );
        assert_eq!(step(&weekly, day("2026-09-23")), day("2026-09-30"));
    }

    #[test]
    fn monthly_rules_keep_their_day_through_short_months() {
        let monthly =
            normalized(&rule(RecurrenceFrequency::Monthly, 1), day("2026-01-31")).unwrap();
        assert_eq!(monthly.month_day, Some(31));
        let february = step(&monthly, day("2026-01-31"));
        assert_eq!(february, day("2026-02-28"));
        // February's clamped day doesn't drag March back to the 28th.
        assert_eq!(step(&monthly, february), day("2026-03-31"));
        let quarterly =
            normalized(&rule(RecurrenceFrequency::Monthly, 3), day("2026-11-15")).unwrap();
        assert_eq!(step(&quarterly, day("2026-11-15")), day("2027-02-15"));
    }

    #[test]
    fn a_late_finish_never_creates_an_overdue_occurrence() {
        let every_day = rule(RecurrenceFrequency::Daily, 1);
        // Due on the 20th, finished on the 24th: the next one is tomorrow, not the 21st.
        assert_eq!(
            next_occurrence(&every_day, day("2026-09-20"), day("2026-09-24")),
            day("2026-09-25")
        );
        let mut mondays = rule(RecurrenceFrequency::Weekly, 1);
        mondays.weekdays = vec![1];
        // Finished early, the day before: the next one is the following Monday.
        assert_eq!(
            next_occurrence(&mondays, day("2026-09-28"), day("2026-09-27")),
            day("2026-10-05")
        );
    }

    #[test]
    fn rules_are_checked_and_tidied() {
        assert!(normalized(&rule(RecurrenceFrequency::Daily, 0), day("2026-09-24")).is_err());
        assert!(normalized(&rule(RecurrenceFrequency::Daily, 100), day("2026-09-24")).is_err());
        let mut bad_weekday = rule(RecurrenceFrequency::Weekly, 1);
        bad_weekday.weekdays = vec![8];
        assert!(normalized(&bad_weekday, day("2026-09-24")).is_err());
        let mut noisy = rule(RecurrenceFrequency::Daily, 2);
        noisy.weekdays = vec![3];
        noisy.month_day = Some(4);
        let tidied = normalized(&noisy, day("2026-09-24")).unwrap();
        assert_eq!((tidied.weekdays.len(), tidied.month_day), (0, None));
        let mut unsorted = rule(RecurrenceFrequency::Weekly, 1);
        unsorted.weekdays = vec![5, 1, 5];
        assert_eq!(
            normalized(&unsorted, day("2026-09-24")).unwrap().weekdays,
            [1, 5]
        );
    }
}
