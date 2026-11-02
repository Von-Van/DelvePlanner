//! Clock-advance tests: the same planner, checked at different moments, brings up exactly the
//! heads-ups the rules promise, never repeats a dismissed one, and says nothing in a quiet week.

use super::*;
use crate::db::{HeadsUpStatus, PlannerDatabase, MAX_NEW_HEADS_UPS_PER_DAY};
use crate::error::AppResult;
use crate::model::{
    CreateEventInput, CreateTaskBlockInput, CreateTaskInput, ExternalEvent, TaskStatus,
};
use crate::tools::NoCalendars;
use tempfile::TempDir;

const ZONE: &str = "America/New_York";

fn zone() -> Tz {
    ZONE.parse().unwrap()
}

/// Thursday 5 November 2026 at `hour:minute` in New York (UTC−5 after the clocks change).
fn thursday(hour: u32, minute: u32) -> DateTime<Utc> {
    at("2026-11-05", hour, minute)
}

fn at(day: &str, hour: u32, minute: u32) -> DateTime<Utc> {
    let local = NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .unwrap()
        .and_hms_opt(hour, minute, 0)
        .unwrap();
    use chrono::TimeZone;
    zone()
        .from_local_datetime(&local)
        .single()
        .unwrap()
        .with_timezone(&Utc)
}

fn utc(instant: DateTime<Utc>) -> String {
    crate::calendar::store::timestamp(instant)
}

fn database() -> (TempDir, PlannerDatabase) {
    let dir = tempfile::tempdir().unwrap();
    let database = PlannerDatabase::open(&dir.path().join("dayplan.sqlite3")).unwrap();
    (dir, database)
}

struct NewTask<'a> {
    title: &'a str,
    due: Option<&'a str>,
    scheduled: Option<&'a str>,
    estimate: Option<i64>,
    priority: TaskPriority,
}

impl Default for NewTask<'_> {
    fn default() -> Self {
        Self {
            title: "Task",
            due: None,
            scheduled: None,
            estimate: None,
            priority: TaskPriority::Normal,
        }
    }
}

fn add_task(database: &mut PlannerDatabase, task: NewTask<'_>) -> Task {
    database
        .create_task(CreateTaskInput {
            title: task.title.into(),
            description: String::new(),
            plan_id: None,
            milestone_id: None,
            workstream_id: None,
            owner_id: None,
            due_date: task.due.map(Into::into),
            scheduled_day: task.scheduled.map(Into::into),
            planned_week: None,
            estimated_minutes: task.estimate,
            status: TaskStatus::Todo,
            priority: task.priority,
            checklist: Vec::new(),
            recurrence: None,
            waiting_on: Vec::new(),
        })
        .unwrap()
}

fn add_block(database: &mut PlannerDatabase, task: &Task, start: DateTime<Utc>, minutes: i64) {
    database
        .create_task_block(CreateTaskBlockInput {
            task_id: task.id.clone(),
            start_at_utc: utc(start),
            time_zone: ZONE.into(),
            duration_minutes: minutes,
        })
        .unwrap();
}

fn add_event(database: &mut PlannerDatabase, title: &str, start: DateTime<Utc>, minutes: i64) {
    database
        .create_event(CreateEventInput {
            title: title.into(),
            notes: String::new(),
            start_at_utc: utc(start),
            time_zone: ZONE.into(),
            duration_minutes: minutes,
            reminder_minutes_before: None,
            plan_id: None,
            location: String::new(),
            workstream_id: None,
            owner_id: None,
        })
        .unwrap();
}

/// Read-only calendars with fixed busy events.
struct Busy(Vec<ExternalEvent>);

impl BusySource for Busy {
    fn busy(&self, _range: &DayRange) -> AppResult<(Vec<ExternalEvent>, bool)> {
        Ok((self.0.clone(), false))
    }
}

fn meeting(key: &str, title: &str, start: DateTime<Utc>, minutes: i64) -> ExternalEvent {
    ExternalEvent {
        calendar_id: "work".into(),
        key: key.into(),
        title: title.into(),
        location: String::new(),
        all_day: false,
        start_at_utc: Some(utc(start)),
        end_at_utc: Some(utc(start + Duration::minutes(minutes))),
        start_date: None,
        end_date: None,
        busy: true,
        tentative: false,
        recurring: false,
    }
}

fn found(
    database: &PlannerDatabase,
    calendars: &dyn BusySource,
    now: DateTime<Utc>,
) -> Vec<NewHeadsUp> {
    detect(&collect(database, calendars, now, zone()).unwrap())
}

fn check(database: &mut PlannerDatabase, now: DateTime<Utc>) -> Vec<HeadsUp> {
    run(database, &NoCalendars, now, zone()).unwrap();
    database.active_heads_ups(now, MAX_SHOWN_HEADS_UPS).unwrap()
}

#[test]
fn a_quiet_week_brings_up_nothing() {
    let (_dir, mut database) = database();
    // Ordinary work: a far deadline, and today's task with its time already planned.
    add_task(
        &mut database,
        NewTask {
            title: "Annual report",
            due: Some("2026-11-20"),
            ..NewTask::default()
        },
    );
    let today = add_task(
        &mut database,
        NewTask {
            title: "Inbox zero",
            scheduled: Some("2026-11-05"),
            estimate: Some(30),
            ..NewTask::default()
        },
    );
    add_block(&mut database, &today, thursday(10, 0), 30);

    assert!(found(&database, &NoCalendars, thursday(9, 0)).is_empty());
    let outcome = run(&mut database, &NoCalendars, thursday(9, 0), zone()).unwrap();
    assert_eq!(
        outcome,
        HeadsUpRefresh::default(),
        "nothing stored, nothing changed"
    );
    assert!(database.all_heads_ups().unwrap().is_empty());
}

#[test]
fn a_near_deadline_with_nothing_planned_names_the_free_time_and_clears_once_planned() {
    let (_dir, mut database) = database();
    let deposit = add_task(
        &mut database,
        NewTask {
            title: "Venue deposit",
            due: Some("2026-11-06"),
            estimate: Some(60),
            ..NewTask::default()
        },
    );
    // Booked until 2 PM; the afternoon is free.
    add_event(&mut database, "Workshop", thursday(9, 0), 5 * 60);

    let shown = check(&mut database, thursday(9, 0));
    assert_eq!(shown.len(), 1, "{shown:#?}");
    let heads_up = &shown[0];
    assert_eq!(heads_up.kind, HeadsUpKind::Deadline);
    assert_eq!(heads_up.title, "“Venue deposit” is due tomorrow");
    assert!(
        heads_up
            .evidence
            .contains("You have 3 h free this afternoon (2 PM–5 PM)"),
        "{}",
        heads_up.evidence
    );
    assert_eq!(heads_up.action_label, "Plan it");
    assert!(heads_up.action_request.contains("Venue deposit"));
    assert!(heads_up.urgent);

    // The person plans the time: on the next check the heads-up has cleared by itself.
    add_block(&mut database, &deposit, thursday(14, 0), 60);
    assert!(check(&mut database, thursday(9, 30)).is_empty());
    assert_eq!(
        database.all_heads_ups().unwrap()[0].status,
        HeadsUpStatus::Cleared
    );
}

#[test]
fn a_dismissed_heads_up_does_not_come_back() {
    let (_dir, mut database) = database();
    add_task(
        &mut database,
        NewTask {
            title: "Venue deposit",
            due: Some("2026-11-06"),
            ..NewTask::default()
        },
    );
    let shown = check(&mut database, thursday(9, 0));
    assert_eq!(shown.len(), 1);
    database
        .dismiss_heads_up(&shown[0].id, thursday(9, 5))
        .unwrap();

    // Still true an hour later, the next morning, and once overdue: never shown again.
    for now in [
        thursday(10, 0),
        at("2026-11-06", 9, 0),
        at("2026-11-07", 9, 0),
    ] {
        assert!(check(&mut database, now).is_empty(), "shown again at {now}");
    }
    let stored = database.all_heads_ups().unwrap();
    assert_eq!(stored.len(), 1, "no second copy");
    assert_eq!(stored[0].status, HeadsUpStatus::Dismissed);
}

#[test]
fn a_snoozed_heads_up_returns_later_if_still_true() {
    let (_dir, mut database) = database();
    add_task(
        &mut database,
        NewTask {
            title: "Venue deposit",
            due: Some("2026-11-06"),
            ..NewTask::default()
        },
    );
    let shown = check(&mut database, thursday(9, 0));
    database
        .snooze_heads_up(&shown[0].id, thursday(13, 0), thursday(9, 5))
        .unwrap();
    assert!(check(&mut database, thursday(11, 0)).is_empty());
    let back = check(&mut database, thursday(13, 30));
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].id, shown[0].id, "the same heads-up, not a copy");
}

#[test]
fn an_unacted_deadline_turns_into_an_overdue_one() {
    let (_dir, mut database) = database();
    add_task(
        &mut database,
        NewTask {
            title: "Venue deposit",
            due: Some("2026-11-06"),
            ..NewTask::default()
        },
    );
    assert_eq!(
        check(&mut database, thursday(9, 0))[0].title,
        "“Venue deposit” is due tomorrow"
    );
    assert_eq!(
        check(&mut database, at("2026-11-06", 9, 0))[0].title,
        "“Venue deposit” is due today"
    );
    let saturday = check(&mut database, at("2026-11-07", 9, 0));
    assert_eq!(saturday[0].title, "“Venue deposit” was due yesterday");
    // A week after the due date it is no longer a deadline heads-up.
    assert!(check(&mut database, at("2026-11-14", 9, 0)).is_empty());
}

#[test]
fn an_overloaded_week_brings_up_the_worst_day() {
    let (_dir, mut database) = database();
    // Friday: 10 h of work in an 8 h day. Monday: 9 h. Today is busy too, but Today shows that.
    for (day, hours) in [("2026-11-06", 10), ("2026-11-09", 9), ("2026-11-05", 12)] {
        for index in 0..hours {
            add_task(
                &mut database,
                NewTask {
                    title: &format!("Work {day} {index}"),
                    scheduled: Some(day),
                    estimate: Some(60),
                    ..NewTask::default()
                },
            );
        }
    }
    let found = found(&database, &NoCalendars, thursday(9, 0));
    let overloaded: Vec<_> = found
        .iter()
        .filter(|heads_up| heads_up.kind == HeadsUpKind::Overloaded)
        .collect();
    assert_eq!(overloaded.len(), 1, "one heads-up for the week");
    assert_eq!(overloaded[0].title, "Tomorrow is overbooked by 2 h");
    assert_eq!(overloaded[0].key, "overloaded:2026-11-06");
    assert!(overloaded[0]
        .evidence
        .contains("1 other coming day is over too"));
}

#[test]
fn clashes_are_found_when_something_of_ours_overlaps() {
    let (_dir, mut database) = database();
    let report = add_task(
        &mut database,
        NewTask {
            title: "Write report",
            scheduled: Some("2026-11-06"),
            ..NewTask::default()
        },
    );
    // Tomorrow: an event 3–4 PM and a block 3:30–4:30 PM.
    add_event(&mut database, "Team sync", at("2026-11-06", 15, 0), 60);
    add_block(&mut database, &report, at("2026-11-06", 15, 30), 60);
    // Two overlapping calendar meetings are the calendar's business, not ours.
    let calendars = Busy(vec![
        meeting("a", "Standup", thursday(10, 0), 30),
        meeting("b", "1:1", thursday(10, 15), 30),
    ]);

    let clashes: Vec<_> = found(&database, &calendars, thursday(9, 0))
        .into_iter()
        .filter(|heads_up| heads_up.kind == HeadsUpKind::Clash)
        .collect();
    assert_eq!(clashes.len(), 1, "{clashes:#?}");
    assert_eq!(clashes[0].title, "“Team sync” and “Write report” overlap");
    assert_eq!(
        clashes[0].evidence,
        "Tomorrow: 3 PM–4 PM and 3:30 PM–4:30 PM overlap by 30 m."
    );
    assert!(!clashes[0].urgent, "tomorrow isn't urgent yet");

    // A calendar meeting over a block today is a clash, and urgent.
    add_block(&mut database, &report, thursday(11, 0), 60);
    let calendars = Busy(vec![meeting("c", "Client call", thursday(11, 30), 60)]);
    let today: Vec<_> = found(&database, &calendars, thursday(9, 0))
        .into_iter()
        .filter(|heads_up| heads_up.kind == HeadsUpKind::Clash)
        .collect();
    assert_eq!(today[0].title, "“Write report” and “Client call” overlap");
    assert!(today[0].urgent);

    // Once it's over, it isn't a clash any more.
    let later: Vec<_> = found(&database, &calendars, thursday(13, 0))
        .into_iter()
        .filter(|heads_up| heads_up.title.contains("Client call"))
        .collect();
    assert!(later.is_empty());
}

#[test]
fn free_focus_time_needs_important_unplanned_work_and_working_hours() {
    let (_dir, mut database) = database();
    // A free day with only ordinary work: nothing worth saying.
    add_task(
        &mut database,
        NewTask {
            title: "Tidy desk",
            scheduled: Some("2026-11-05"),
            ..NewTask::default()
        },
    );
    assert!(found(&database, &NoCalendars, thursday(9, 0)).is_empty());

    add_task(
        &mut database,
        NewTask {
            title: "Sponsor follow-up",
            scheduled: Some("2026-11-05"),
            priority: TaskPriority::High,
            ..NewTask::default()
        },
    );
    add_event(&mut database, "Lunch", thursday(12, 0), 60);
    let found_now = found(&database, &NoCalendars, thursday(9, 10));
    assert_eq!(found_now.len(), 1, "{found_now:#?}");
    let focus = &found_now[0];
    assert_eq!(focus.kind, HeadsUpKind::FocusWindow);
    assert_eq!(focus.title, "You have 2 h 45 m free this morning");
    assert_eq!(
        focus.evidence,
        "Nothing is booked 9:15 AM–12 PM, and “Sponsor follow-up” (high priority) has no time planned."
    );
    assert_eq!(focus.key, "focus:2026-11-05");

    // After working hours there is no focus time left to point at.
    assert!(found(&database, &NoCalendars, thursday(17, 30)).is_empty());
}

#[test]
fn pushed_back_work_and_quiet_projects_are_noticed() {
    let (_dir, database) = database();
    let mut facts = collect(&database, &NoCalendars, thursday(9, 0), zone()).unwrap();
    facts.pushed_back = vec![PushedTask {
        task_id: "t1".into(),
        title: "Update website".into(),
        moves: 4,
    }];
    facts.quiet_plans = vec![
        QuietPlan {
            plan_id: "p-busy".into(),
            title: "Busy project".into(),
            open_tasks: 3,
            last_activity: thursday(8, 0),
        },
        QuietPlan {
            plan_id: "p-quiet".into(),
            title: "Charity Week".into(),
            open_tasks: 5,
            last_activity: thursday(9, 0) - Duration::days(18),
        },
    ];
    let found = detect(&facts);
    let titles: Vec<_> = found
        .iter()
        .map(|heads_up| heads_up.title.as_str())
        .collect();
    assert_eq!(
        titles,
        [
            "“Update website” keeps getting pushed back",
            "“Charity Week” has gone quiet"
        ]
    );
    assert_eq!(
        found[1].evidence,
        "Nothing in it has changed for 18 days, and 5 tasks are still open."
    );
}

#[test]
fn quiet_projects_come_from_the_database() {
    let (_dir, mut database) = database();
    let plan = database
        .create_plan(crate::model::CreatePlanInput {
            title: "Charity Week".into(),
            description: String::new(),
            status: crate::model::PlanStatus::Active,
            start_date: None,
            target_date: None,
            color: None,
            links: Vec::new(),
        })
        .unwrap();
    database
        .create_task(CreateTaskInput {
            title: "Book venue".into(),
            description: String::new(),
            plan_id: Some(plan.id.clone()),
            milestone_id: None,
            workstream_id: None,
            owner_id: None,
            due_date: None,
            scheduled_day: None,
            planned_week: None,
            estimated_minutes: None,
            status: TaskStatus::Todo,
            priority: TaskPriority::Normal,
            checklist: Vec::new(),
            recurrence: None,
            waiting_on: Vec::new(),
        })
        .unwrap();
    // Just created: not quiet. Three weeks on with nothing touched: quiet.
    let now = Utc::now();
    assert!(found(&database, &NoCalendars, now)
        .iter()
        .all(|heads_up| heads_up.kind != HeadsUpKind::QuietProject));
    let later = found(&database, &NoCalendars, now + Duration::days(21));
    let quiet: Vec<_> = later
        .iter()
        .filter(|heads_up| heads_up.kind == HeadsUpKind::QuietProject)
        .collect();
    assert_eq!(quiet.len(), 1);
    assert_eq!(quiet[0].key, format!("quiet_project:{}", plan.id));
}

fn synthetic(index: usize, now: DateTime<Utc>) -> NewHeadsUp {
    NewHeadsUp {
        kind: HeadsUpKind::Deadline,
        key: format!("test:{index}"),
        title: format!("Heads-up {index}"),
        evidence: "Because.".into(),
        target: None,
        action_label: "Plan it".into(),
        action_request: "Plan it.".into(),
        score: 100 - index as i64,
        urgent: true,
        expires_at: now + Duration::days(5),
    }
}

#[test]
fn a_day_brings_up_a_limited_number_and_shows_at_most_three() {
    let (_dir, mut database) = database();
    let now = thursday(9, 0);
    let all: Vec<_> = (0..8).map(|index| synthetic(index, now)).collect();
    let outcome = database
        .record_heads_ups(&all, now, day_start(now, zone()))
        .unwrap();
    assert_eq!(outcome.surfaced as i64, MAX_NEW_HEADS_UPS_PER_DAY);
    assert_eq!(outcome.held_back, 8 - MAX_NEW_HEADS_UPS_PER_DAY as usize);
    let shown = database.active_heads_ups(now, 10).unwrap();
    assert_eq!(shown.len(), 3, "never more than three on screen");
    assert_eq!(shown[0].title, "Heads-up 0", "most worth saying first");

    // The next day the rest come through.
    let tomorrow = at("2026-11-06", 9, 0);
    let outcome = database
        .record_heads_ups(&all, tomorrow, day_start(tomorrow, zone()))
        .unwrap();
    assert_eq!(outcome.surfaced, 2);
    assert_eq!(outcome.held_back, 0);
}

#[test]
fn notifications_are_limited_per_day_and_never_repeat() {
    let (_dir, mut database) = database();
    let now = thursday(9, 0);
    let all: Vec<_> = (0..4).map(|index| synthetic(index, now)).collect();
    database
        .record_heads_ups(&all, now, day_start(now, zone()))
        .unwrap();
    let first = database
        .heads_ups_to_notify(now, day_start(now, zone()))
        .unwrap();
    assert_eq!(first.len(), crate::db::MAX_NOTIFICATIONS_PER_DAY as usize);
    for heads_up in &first {
        database.mark_heads_up_notified(&heads_up.id, now).unwrap();
    }
    assert!(database
        .heads_ups_to_notify(thursday(10, 0), day_start(now, zone()))
        .unwrap()
        .is_empty());
    // Tomorrow the other two may notify; the first two never again.
    let tomorrow = at("2026-11-06", 9, 0);
    let next = database
        .heads_ups_to_notify(tomorrow, day_start(tomorrow, zone()))
        .unwrap();
    assert_eq!(next.len(), 2);
    assert!(next.iter().all(|heads_up| !first.contains(heads_up)));
}

#[test]
fn quiet_hours_are_outside_working_hours_and_days_off() {
    let (_dir, database) = database();
    let quiet = |now| quiet_hours(&collect(&database, &NoCalendars, now, zone()).unwrap());
    assert!(!quiet(thursday(10, 0)));
    assert!(quiet(thursday(8, 30)));
    assert!(quiet(thursday(20, 0)));
    assert!(quiet(at("2026-11-07", 11, 0)), "Saturday");
}

#[test]
fn turning_heads_ups_off_clears_them_and_stops_checks() {
    let (_dir, mut database) = database();
    add_task(
        &mut database,
        NewTask {
            title: "Venue deposit",
            due: Some("2026-11-06"),
            ..NewTask::default()
        },
    );
    assert_eq!(check(&mut database, thursday(9, 0)).len(), 1);
    database
        .update_heads_up_settings(
            HeadsUpSettings {
                enabled: false,
                notify: true,
            },
            thursday(9, 5),
        )
        .unwrap();
    assert!(check(&mut database, thursday(9, 10)).is_empty());
    let outcome = run(&mut database, &NoCalendars, thursday(9, 15), zone()).unwrap();
    assert_eq!(outcome, HeadsUpRefresh::default());
}

#[test]
fn acting_on_a_heads_up_returns_its_request_and_retires_it() {
    let (_dir, mut database) = database();
    add_task(
        &mut database,
        NewTask {
            title: "Venue deposit",
            due: Some("2026-11-06"),
            ..NewTask::default()
        },
    );
    let shown = check(&mut database, thursday(9, 0));
    let acted = database
        .act_on_heads_up(&shown[0].id, thursday(9, 1))
        .unwrap();
    assert_eq!(acted.status, HeadsUpStatus::Acted);
    assert!(acted
        .action_request
        .starts_with("“Venue deposit” is due tomorrow"));
    assert!(check(&mut database, thursday(9, 30)).is_empty());
    assert!(database
        .act_on_heads_up(&shown[0].id, thursday(9, 31))
        .is_err());
}

#[test]
fn schema_eleven_gains_heads_ups_with_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dayplan.sqlite3");
    drop(PlannerDatabase::open(&path).unwrap());
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "DROP TABLE agent_suggestion; DROP TABLE heads_up_settings;
             PRAGMA user_version = 11;",
        )
        .unwrap();
    drop(connection);

    let migrated = PlannerDatabase::open(&path).unwrap();
    assert_eq!(
        migrated.schema_version().unwrap(),
        crate::db::CURRENT_SCHEMA_VERSION
    );
    assert_eq!(
        migrated.heads_up_settings().unwrap(),
        HeadsUpSettings {
            enabled: true,
            notify: true
        }
    );
    assert!(migrated.all_heads_ups().unwrap().is_empty());
}
