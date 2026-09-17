//! Reads iCalendar (.ics) data into read-only events for a window of local days. Recurring
//! events are expanded one series at a time and only as far as the window reaches.

use crate::model::CalendarProblem;
use calcard::common::timezone::Tz as IcalZone;
use calcard::common::PartialDateTime;
use calcard::icalendar::dates::TimeOrDelta;
use calcard::icalendar::{
    ICalendar, ICalendarComponent, ICalendarComponentType, ICalendarProperty, ICalendarStatus,
    ICalendarTransparency, ICalendarValue,
};
use calcard::{Entry, Parser};
use chrono::{DateTime, Days, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use std::collections::HashMap;

/// The largest calendar DayPlan reads, from a file or a link.
pub const MAX_CALENDAR_BYTES: usize = 20 * 1024 * 1024;
/// Days before today whose events are kept, so recent weeks stay visible.
pub const PAST_DAYS: u64 = 42;
/// Days after today whose events are kept.
pub const FUTURE_DAYS: u64 = 400;
/// Instances generated for one series, counted from its first occurrence.
const MAX_INSTANCES_PER_SERIES: usize = 10_000;
const MAX_EVENTS: usize = 50_000;
const MAX_ENTRIES: usize = 100_000;
const MAX_TEXT_LENGTH: usize = 200;

/// The local days a calendar's events are read for, and the zone that floating times and all-day
/// dates are interpreted in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SyncWindow {
    pub zone: Tz,
    pub first_day: NaiveDate,
    /// Exclusive.
    pub end_day: NaiveDate,
}

impl SyncWindow {
    pub fn around(today: NaiveDate, zone: Tz) -> Self {
        Self {
            zone,
            first_day: today
                .checked_sub_days(Days::new(PAST_DAYS))
                .unwrap_or(today),
            end_day: today
                .checked_add_days(Days::new(FUTURE_DAYS))
                .unwrap_or(today),
        }
    }

    pub fn start_utc(&self) -> DateTime<Utc> {
        local_midnight(self.first_day, self.zone)
    }

    pub fn end_utc(&self) -> DateTime<Utc> {
        local_midnight(self.end_day, self.zone)
    }
}

/// The first instant of a local day, or the first valid instant after a skipped midnight.
pub fn local_midnight(day: NaiveDate, zone: Tz) -> DateTime<Utc> {
    let midnight = day.and_hms_opt(0, 0, 0).expect("midnight is a valid time");
    (0..=3)
        .find_map(|hour| {
            zone.from_local_datetime(&(midnight + chrono::Duration::hours(hour)))
                .earliest()
        })
        .map(|local| local.with_timezone(&Utc))
        .unwrap_or_else(|| Utc.from_utc_datetime(&midnight))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventTime {
    Timed {
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    },
    /// Local dates; `end` is exclusive.
    AllDay { start: NaiveDate, end: NaiveDate },
}

/// One occurrence read from a calendar, before it is stored.
#[derive(Debug, Clone, PartialEq)]
pub struct EventDraft {
    pub key: String,
    pub title: String,
    pub location: String,
    pub when: EventTime,
    pub busy: bool,
    pub tentative: bool,
    pub recurring: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CalendarRead {
    /// The name the calendar gives itself (X-WR-CALNAME), when it has one.
    pub name: Option<String>,
    /// Occurrences that overlap the window, earliest first.
    pub events: Vec<EventDraft>,
}

/// Parses `content` and returns the events that overlap `window`. Cancelled occurrences, excluded
/// dates, and the originals of moved occurrences are left out.
pub fn read_calendar(content: &str, window: &SyncWindow) -> Result<CalendarRead, CalendarProblem> {
    if content.len() > MAX_CALENDAR_BYTES {
        return Err(CalendarProblem::TooLarge);
    }
    let content = content.trim_start_matches('\u{feff}').trim_start();
    if !content
        .get(..15)
        .is_some_and(|start| start.eq_ignore_ascii_case("BEGIN:VCALENDAR"))
    {
        return Err(CalendarProblem::NotACalendar);
    }

    let mut parser = Parser::new(content);
    let mut components = Vec::new();
    let mut name = None;
    for _ in 0..MAX_ENTRIES {
        match parser.entry() {
            Entry::ICalendar(calendar) => {
                if name.is_none() {
                    name = calendar.components.first().and_then(calendar_name);
                }
                components.extend(calendar.components);
            }
            Entry::Eof => break,
            Entry::TooManyComponents => return Err(CalendarProblem::TooLarge),
            Entry::UnterminatedComponent(_) => break,
            _ => continue,
        }
    }
    if components.is_empty() {
        return Err(CalendarProblem::NotACalendar);
    }

    let mut timezones = Vec::new();
    let mut series: Vec<(String, Vec<ICalendarComponent>)> = Vec::new();
    let mut series_by_uid: HashMap<String, usize> = HashMap::new();
    for (index, mut component) in components.into_iter().enumerate() {
        match component.component_type {
            ICalendarComponentType::VTimezone => timezones.push(component),
            ICalendarComponentType::VEvent => {
                component.component_ids.clear();
                let uid = component
                    .uid()
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("dayplan-unnamed-{index}"));
                match series_by_uid.get(&uid) {
                    Some(position) => series[*position].1.push(component),
                    None => {
                        series_by_uid.insert(uid.clone(), series.len());
                        series.push((uid, vec![component]));
                    }
                }
            }
            _ => {}
        }
    }

    let mut events = Vec::new();
    for (uid, group) in series {
        events.extend(expand_series(&uid, group, &timezones, window));
        if events.len() > MAX_EVENTS {
            return Err(CalendarProblem::TooLarge);
        }
    }
    events.sort_by_key(|event| sort_key(&event.when, window.zone));
    Ok(CalendarRead { name, events })
}

/// Expands one UID's master event, its overrides, and its extra dates inside the window.
fn expand_series(
    uid: &str,
    mut group: Vec<ICalendarComponent>,
    timezones: &[ICalendarComponent],
    window: &SyncWindow,
) -> Vec<EventDraft> {
    let bound_day = window
        .end_day
        .checked_add_days(Days::new(2))
        .unwrap_or(window.end_day);
    let bound = bound_day.and_hms_opt(23, 59, 59).expect("valid time");
    let recurring = group.iter().any(|component| {
        component.has_property(&ICalendarProperty::Rrule)
            || component.has_property(&ICalendarProperty::Rdate)
            || component.has_property(&ICalendarProperty::RecurrenceId)
    });
    for component in &mut group {
        // Overrides are matched to their series by UID here, so their SEQUENCE must not matter.
        component
            .entries
            .retain(|entry| entry.name != ICalendarProperty::Sequence);
        if start_value(component)
            .and_then(PartialDateTime::to_date_time)
            .is_some_and(|start| start.date_time > bound)
        {
            // A series that begins after the window contributes nothing inside it.
            component.entries.retain(|entry| {
                !matches!(
                    entry.name,
                    ICalendarProperty::Rrule | ICalendarProperty::Rdate | ICalendarProperty::Exdate
                )
            });
            continue;
        }
        for entry in &mut component.entries {
            if entry.name != ICalendarProperty::Rrule {
                continue;
            }
            if let Some(ICalendarValue::RecurrenceRule(rule)) = entry.values.first_mut() {
                let open_ended = rule.until.as_ref().is_none_or(|until| {
                    until
                        .to_date_time()
                        .is_none_or(|until| until.date_time > bound)
                });
                if rule.count.is_none() && open_ended {
                    rule.until = Some(floating(bound));
                }
            }
        }
    }

    let mut components = timezones.to_vec();
    components.extend(group);
    let calendar = ICalendar { components };
    let expanded = calendar.expand_dates(IcalZone::Tz(window.zone), MAX_INSTANCES_PER_SERIES);
    let (window_start, window_end) = (window.start_utc(), window.end_utc());
    expanded
        .events
        .into_iter()
        .filter_map(|occurrence| {
            let component = calendar.components.get(occurrence.comp_id as usize)?;
            if component.status() == Some(&ICalendarStatus::Cancelled) {
                return None;
            }
            let all_day = !start_value(component)?.has_time();
            let has_length = component.has_property(&ICalendarProperty::Dtend)
                || component.has_property(&ICalendarProperty::Duration);
            let when = if all_day {
                let start = occurrence.start.naive_local().date();
                let end = match occurrence.end {
                    TimeOrDelta::Time(end) => end.naive_local().date(),
                    TimeOrDelta::Delta(length) => (occurrence.start.naive_local() + length).date(),
                };
                let end = if has_length && end > start {
                    end
                } else {
                    start.checked_add_days(Days::new(1))?
                };
                if start >= window.end_day || end <= window.first_day {
                    return None;
                }
                EventTime::AllDay { start, end }
            } else {
                let start = occurrence.start.with_timezone(&Utc);
                let end = match occurrence.end {
                    _ if !has_length => start,
                    TimeOrDelta::Time(end) => end.with_timezone(&Utc),
                    TimeOrDelta::Delta(length) => start + length,
                }
                .max(start);
                if start >= window_end || (end <= window_start && start < window_start) {
                    return None;
                }
                EventTime::Timed { start, end }
            };
            let (busy, tentative) = availability(component, all_day);
            Some(EventDraft {
                key: instance_key(uid, &when),
                title: component_text(component, &ICalendarProperty::Summary)
                    .filter(|title| !title.is_empty())
                    .unwrap_or_else(|| "Untitled event".into()),
                location: component_text(component, &ICalendarProperty::Location)
                    .unwrap_or_default(),
                when,
                busy,
                tentative,
                recurring,
            })
        })
        .collect()
}

/// The DTSTART value, or the RECURRENCE-ID of an override that leaves DTSTART out.
fn start_value(component: &ICalendarComponent) -> Option<&PartialDateTime> {
    [ICalendarProperty::Dtstart, ICalendarProperty::RecurrenceId]
        .iter()
        .find_map(
            |property| match component.property(property)?.values.first() {
                Some(ICalendarValue::PartialDateTime(value)) => Some(value.as_ref()),
                _ => None,
            },
        )
}

fn floating(value: NaiveDateTime) -> PartialDateTime {
    use chrono::{Datelike, Timelike};
    PartialDateTime {
        year: u16::try_from(value.year()).ok(),
        month: Some(value.month() as u8),
        day: Some(value.day() as u8),
        hour: Some(value.hour() as u8),
        minute: Some(value.minute() as u8),
        second: Some(value.second() as u8),
        tz_hour: None,
        tz_minute: None,
        tz_minus: false,
    }
}

/// Whether an occurrence makes its time busy, and whether it is only tentative. Outlook's busy
/// status wins over TRANSP. Timed events are busy by default; all-day events only when marked.
fn availability(component: &ICalendarComponent, all_day: bool) -> (bool, bool) {
    let tentative = component.status() == Some(&ICalendarStatus::Tentative);
    if let Some(status) = other_text(component, "X-MICROSOFT-CDO-BUSYSTATUS") {
        return match status.to_ascii_uppercase().as_str() {
            "FREE" | "WORKINGELSEWHERE" => (false, tentative),
            "TENTATIVE" => (true, true),
            _ => (true, tentative),
        };
    }
    match component.transparency() {
        Some(ICalendarTransparency::Transparent) => (false, tentative),
        Some(ICalendarTransparency::Opaque) => (true, tentative),
        None => (!all_day, tentative),
    }
}

fn calendar_name(root: &ICalendarComponent) -> Option<String> {
    other_text(root, "X-WR-CALNAME")
        .map(clean_text)
        .filter(|name| !name.is_empty())
}

fn component_text(component: &ICalendarComponent, property: &ICalendarProperty) -> Option<String> {
    component
        .property(property)
        .and_then(|entry| entry.values.first())
        .and_then(ICalendarValue::as_text)
        .map(clean_text)
}

fn other_text<'a>(component: &'a ICalendarComponent, name: &str) -> Option<&'a str> {
    component
        .entries
        .iter()
        .find_map(|entry| match &entry.name {
            ICalendarProperty::Other(other) if other.eq_ignore_ascii_case(name) => {
                entry.values.first().and_then(ICalendarValue::as_text)
            }
            _ => None,
        })
}

/// Collapses line breaks and runs of whitespace, then caps the length for display.
fn clean_text(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_TEXT_LENGTH)
        .collect()
}

fn sort_key(when: &EventTime, zone: Tz) -> DateTime<Utc> {
    match when {
        EventTime::Timed { start, .. } => *start,
        EventTime::AllDay { start, .. } => local_midnight(*start, zone),
    }
}

/// A short, stable identifier for one occurrence: FNV-1a over its UID and start.
fn instance_key(uid: &str, when: &EventTime) -> String {
    let start = match when {
        EventTime::Timed { start, .. } => start.timestamp().to_string(),
        EventTime::AllDay { start, .. } => start.to_string(),
    };
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in uid.bytes().chain([0x1f]).chain(start.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOGLE: &str = "BEGIN:VCALENDAR\r
PRODID:-//Google Inc//Google Calendar 70.9054//EN\r
VERSION:2.0\r
X-WR-CALNAME:Work\r
X-WR-TIMEZONE:America/New_York\r
BEGIN:VTIMEZONE\r
TZID:America/New_York\r
X-LIC-LOCATION:America/New_York\r
BEGIN:DAYLIGHT\r
TZOFFSETFROM:-0500\r
TZOFFSETTO:-0400\r
DTSTART:19700308T020000\r
RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=2SU\r
END:DAYLIGHT\r
BEGIN:STANDARD\r
TZOFFSETFROM:-0400\r
TZOFFSETTO:-0500\r
DTSTART:19701101T020000\r
RRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=1SU\r
END:STANDARD\r
END:VTIMEZONE\r
BEGIN:VEVENT\r
DTSTART;TZID=America/New_York:20260105T090000\r
DTEND;TZID=America/New_York:20260105T093000\r
RRULE:FREQ=WEEKLY;BYDAY=MO,WE\r
EXDATE;TZID=America/New_York:20260916T090000\r
UID:standup@google.com\r
SEQUENCE:0\r
SUMMARY:Standup\r
TRANSP:OPAQUE\r
BEGIN:VALARM\r
ACTION:DISPLAY\r
TRIGGER:-PT10M\r
END:VALARM\r
END:VEVENT\r
BEGIN:VEVENT\r
DTSTART;TZID=America/New_York:20260921T100000\r
DTEND;TZID=America/New_York:20260921T103000\r
UID:standup@google.com\r
RECURRENCE-ID;TZID=America/New_York:20260921T090000\r
SEQUENCE:2\r
SUMMARY:Standup (moved)\r
END:VEVENT\r
BEGIN:VEVENT\r
DTSTART;TZID=America/New_York:20260923T090000\r
DTEND;TZID=America/New_York:20260923T093000\r
UID:standup@google.com\r
RECURRENCE-ID;TZID=America/New_York:20260923T090000\r
STATUS:CANCELLED\r
SUMMARY:Standup\r
END:VEVENT\r
BEGIN:VEVENT\r
DTSTART;VALUE=DATE:20260918\r
DTEND;VALUE=DATE:20260920\r
UID:offsite@google.com\r
SUMMARY:Offsite\r
TRANSP:TRANSPARENT\r
END:VEVENT\r
BEGIN:VEVENT\r
DTSTART:20260917T170000Z\r
DTEND:20260917T180000Z\r
UID:call@google.com\r
SUMMARY:Call with Sam\\, re: budget\r
LOCATION:Zoom\r
STATUS:TENTATIVE\r
END:VEVENT\r
END:VCALENDAR\r
";

    const OUTLOOK: &str = "BEGIN:VCALENDAR\r
METHOD:PUBLISH\r
PRODID:Microsoft Exchange Server 2010\r
VERSION:2.0\r
BEGIN:VTIMEZONE\r
TZID:Eastern Standard Time\r
BEGIN:STANDARD\r
DTSTART:16010101T020000\r
TZOFFSETFROM:-0400\r
TZOFFSETTO:-0500\r
RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=1SU;BYMONTH=11\r
END:STANDARD\r
BEGIN:DAYLIGHT\r
DTSTART:16010101T020000\r
TZOFFSETFROM:-0500\r
TZOFFSETTO:-0400\r
RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=2SU;BYMONTH=3\r
END:DAYLIGHT\r
END:VTIMEZONE\r
BEGIN:VEVENT\r
RRULE:FREQ=DAILY;UNTIL=20260922T140000Z;INTERVAL=1\r
UID:040000008200E00074C5B7101A82E0080000000010\r
SUMMARY:Focus time with a title long enough that Outlook folds the\r
  line\r
DTSTART;TZID=Eastern Standard Time:20260901T100000\r
DTEND;TZID=Eastern Standard Time:20260901T110000\r
TRANSP:OPAQUE\r
X-MICROSOFT-CDO-BUSYSTATUS:FREE\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:quoted-zone\r
SUMMARY:Supplier call\r
DTSTART;TZID=\"(UTC+01:00) Amsterdam, Berlin, Bern, Rome, Stockholm, Vienna\":20260918T150000\r
DTEND;TZID=\"(UTC+01:00) Amsterdam, Berlin, Bern, Rome, Stockholm, Vienna\":20260918T160000\r
X-MICROSOFT-CDO-BUSYSTATUS:OOF\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:floating\r
SUMMARY:Lunch\r
DTSTART:20260918T120000\r
DTEND:20260918T130000\r
END:VEVENT\r
END:VCALENDAR\r
";

    fn day(value: &str) -> NaiveDate {
        NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
    }

    fn window() -> SyncWindow {
        SyncWindow::around(day("2026-09-17"), chrono_tz::America::Chicago)
    }

    fn timed(event: &EventDraft) -> (String, String) {
        match event.when {
            EventTime::Timed { start, end } => (
                start.format("%Y-%m-%dT%H:%MZ").to_string(),
                end.format("%H:%MZ").to_string(),
            ),
            EventTime::AllDay { .. } => panic!("{} is all-day", event.title),
        }
    }

    fn between<'a>(read: &'a CalendarRead, first: &str, last: &str) -> Vec<&'a EventDraft> {
        read.events
            .iter()
            .filter(|event| {
                let start = match event.when {
                    EventTime::Timed { start, .. } => start.format("%Y-%m-%d").to_string(),
                    EventTime::AllDay { start, .. } => start.to_string(),
                };
                start.as_str() >= first && start.as_str() <= last
            })
            .collect()
    }

    #[test]
    fn reads_moved_cancelled_and_excluded_occurrences_of_a_google_series() {
        let read = read_calendar(GOOGLE, &window()).unwrap();
        assert_eq!(read.name.as_deref(), Some("Work"));
        let standups = between(&read, "2026-09-14", "2026-09-23")
            .into_iter()
            .filter(|event| event.title.starts_with("Standup"))
            .map(|event| (event.title.as_str(), timed(event).0, event.recurring))
            .collect::<Vec<_>>();
        assert_eq!(
            standups,
            [
                ("Standup", "2026-09-14T13:00Z".to_string(), true),
                ("Standup (moved)", "2026-09-21T14:00Z".to_string(), true),
            ],
            "the 16th is excluded, the 21st moved, and the 23rd cancelled"
        );
        let call = read
            .events
            .iter()
            .find(|event| event.title == "Call with Sam, re: budget")
            .unwrap();
        assert_eq!(timed(call), ("2026-09-17T17:00Z".into(), "18:00Z".into()));
        assert_eq!(
            (call.location.as_str(), call.busy, call.tentative),
            ("Zoom", true, true)
        );
        let offsite = read
            .events
            .iter()
            .find(|event| event.title == "Offsite")
            .unwrap();
        assert_eq!(
            offsite.when,
            EventTime::AllDay {
                start: day("2026-09-18"),
                end: day("2026-09-20")
            }
        );
        assert!(!offsite.busy && !offsite.recurring);
    }

    #[test]
    fn reads_outlook_zones_busy_status_and_floating_times() {
        let read = read_calendar(OUTLOOK, &window()).unwrap();
        assert_eq!(read.name, None);
        let focus = between(&read, "2026-09-17", "2026-09-17")
            .into_iter()
            .find(|event| event.title.starts_with("Focus time"))
            .unwrap();
        assert_eq!(
            focus.title,
            "Focus time with a title long enough that Outlook folds the line"
        );
        assert_eq!(timed(focus), ("2026-09-17T14:00Z".into(), "15:00Z".into()));
        assert!(!focus.busy, "Outlook's FREE status wins over TRANSP");
        assert_eq!(
            read.events
                .iter()
                .filter(|event| event.title.starts_with("Focus time"))
                .count(),
            22,
            "September 1 through the UNTIL on the 22nd"
        );
        let supplier = read
            .events
            .iter()
            .find(|event| event.title == "Supplier call")
            .unwrap();
        assert_eq!(timed(supplier).0, "2026-09-18T13:00Z");
        assert!(supplier.busy);
        let lunch = read
            .events
            .iter()
            .find(|event| event.title == "Lunch")
            .unwrap();
        assert_eq!(
            timed(lunch).0,
            "2026-09-18T17:00Z",
            "floating times use the window's zone"
        );
    }

    #[test]
    fn expands_a_long_running_series_only_inside_the_window() {
        let content = "BEGIN:VCALENDAR\r
BEGIN:VEVENT\r
UID:gym\r
SUMMARY:Gym\r
DTSTART;TZID=America/Chicago:20100104T063000\r
DURATION:PT45M\r
RRULE:FREQ=DAILY\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:later\r
SUMMARY:Starts after the window\r
DTSTART:20300101T100000Z\r
DTEND:20300101T110000Z\r
RRULE:FREQ=DAILY\r
END:VEVENT\r
END:VCALENDAR\r
";
        let window = window();
        let started = std::time::Instant::now();
        let read = read_calendar(content, &window).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        let expected = (window.end_day - window.first_day).num_days() as usize;
        assert_eq!(read.events.len(), expected);
        assert_eq!(
            timed(&read.events[0]),
            ("2026-08-06T11:30Z".into(), "12:15Z".into())
        );
        let winter = read
            .events
            .iter()
            .find(|event| timed(event).0.starts_with("2026-12-01"))
            .unwrap();
        assert_eq!(
            timed(winter).0,
            "2026-12-01T12:30Z",
            "local time holds across DST"
        );
    }

    #[test]
    fn events_without_an_end_take_no_time_or_one_day() {
        let content = "BEGIN:VCALENDAR\r
BEGIN:VEVENT\r
UID:reminder\r
SUMMARY:Pay rent\r
DTSTART:20260917T150000Z\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:holiday\r
DTSTART;VALUE=DATE:20260919\r
END:VEVENT\r
END:VCALENDAR\r
";
        let read = read_calendar(content, &window()).unwrap();
        assert_eq!(
            timed(&read.events[0]),
            ("2026-09-17T15:00Z".into(), "15:00Z".into())
        );
        assert_eq!(
            (&read.events[1].title, read.events[1].when),
            (
                &"Untitled event".to_string(),
                EventTime::AllDay {
                    start: day("2026-09-19"),
                    end: day("2026-09-20")
                }
            )
        );
    }

    #[test]
    fn rejects_content_that_is_not_a_calendar() {
        for content in ["", "<html>Sign in</html>", "BEGIN:VCARD\r\nEND:VCARD\r\n"] {
            assert_eq!(
                read_calendar(content, &window()),
                Err(CalendarProblem::NotACalendar)
            );
        }
        let oversized = format!("BEGIN:VCALENDAR{}", " ".repeat(MAX_CALENDAR_BYTES));
        assert_eq!(
            read_calendar(&oversized, &window()),
            Err(CalendarProblem::TooLarge)
        );
    }

    #[test]
    fn instance_keys_are_stable_and_distinct() {
        let first = read_calendar(GOOGLE, &window()).unwrap();
        let second = read_calendar(GOOGLE, &window()).unwrap();
        let keys = first
            .events
            .iter()
            .map(|event| event.key.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            second
                .events
                .iter()
                .map(|event| event.key.clone())
                .collect::<Vec<_>>()
        );
        let unique = keys.iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), keys.len());
    }
}
