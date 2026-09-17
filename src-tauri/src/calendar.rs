//! Read-only calendars from other services. Every provider normalizes into one external-event
//! shape stored in a cache kept apart from planner data. Today that provider is iCalendar: a link
//! DayPlan refreshes in the background, or a file imported once. Google and Microsoft accounts
//! will produce the same events through OAuth.

pub mod ics;
pub mod link;
pub mod secrets;
pub mod store;

use crate::error::{AppError, AppResult};
use crate::model::{
    Calendar, CalendarAgenda, CalendarKind, CalendarProblem, ExternalEvent, PlanColor,
    ReplaceCalendarLinkInput, SubscribeCalendarInput, UpdateCalendarInput,
};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use chrono_tz::Tz;
use ics::{local_midnight, read_calendar, CalendarRead, SyncWindow};
use link::{fetch, http_client, CalendarLink, Fetched};
use secrets::{CalendarLinks, SecretVault};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use store::{timestamp, CalendarStore, NewCalendar, RefreshTarget, Snapshot};
use uuid::Uuid;

/// How often a link calendar refreshes while DayPlan runs.
const REFRESH_INTERVAL_MINUTES: i64 = 30;
const MAX_AGENDA_DAYS: u32 = 14;

/// The local days `[first_day, end_day)` of a request and the UTC instants they span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayRange {
    pub first_day: String,
    pub end_day: String,
    pub start_utc: String,
    pub end_utc: String,
}

impl DayRange {
    pub fn new(start_day: &str, days: u32, time_zone: &str) -> AppResult<Self> {
        if !(1..=MAX_AGENDA_DAYS).contains(&days) {
            return Err(AppError::Validation(
                "Calendar ranges cover between 1 and 14 days.".into(),
            ));
        }
        let zone = parse_zone(time_zone)?;
        let first = NaiveDate::parse_from_str(start_day, "%Y-%m-%d")
            .map_err(|_| AppError::Validation("Dates must use YYYY-MM-DD.".into()))?;
        let end = first
            .checked_add_days(chrono::Days::new(u64::from(days)))
            .ok_or_else(|| AppError::Validation("That date is out of range.".into()))?;
        Ok(Self {
            first_day: first.to_string(),
            end_day: end.to_string(),
            start_utc: timestamp(local_midnight(first, zone)),
            end_utc: timestamp(local_midnight(end, zone)),
        })
    }
}

pub fn parse_zone(time_zone: &str) -> AppResult<Tz> {
    time_zone
        .parse::<Tz>()
        .map_err(|_| AppError::Validation("A valid IANA time zone is required.".into()))
}

/// The device's zone, which floating times and all-day dates in calendars are read in.
fn system_zone() -> Tz {
    iana_time_zone::get_timezone()
        .ok()
        .and_then(|name| name.parse().ok())
        .unwrap_or(Tz::UTC)
}

fn current_window() -> SyncWindow {
    let zone = system_zone();
    SyncWindow::around(Utc::now().with_timezone(&zone).date_naive(), zone)
}

/// When to try a calendar again after a failed refresh: soon for passing trouble, later for a
/// link that needs the user's attention.
fn retry_after(problem: CalendarProblem, now: DateTime<Utc>) -> DateTime<Utc> {
    now + match problem {
        CalendarProblem::Unreachable | CalendarProblem::ServerError => Duration::minutes(15),
        CalendarProblem::RateLimited => Duration::hours(2),
        _ => Duration::hours(6),
    }
}

async fn read_in_background(
    content: String,
    window: SyncWindow,
) -> AppResult<(String, CalendarRead)> {
    tokio::task::spawn_blocking(move || {
        read_calendar(&content, &window)
            .map(|read| (content, read))
            .map_err(AppError::Calendar)
    })
    .await
    .map_err(|_| AppError::Internal("The calendar could not be read.".into()))?
}

struct StoreRuntime {
    store: Option<CalendarStore>,
    startup_error: Option<String>,
}

pub struct CalendarService {
    runtime: Mutex<StoreRuntime>,
    links: CalendarLinks,
    client: reqwest::Client,
    /// Calendars with a refresh in flight, so the worker and a manual refresh never overlap.
    refreshing: Mutex<HashSet<String>>,
    /// The window events are read for; fixed in tests so they don't depend on today's date.
    window: fn() -> SyncWindow,
}

impl CalendarService {
    pub fn new(path: PathBuf, vault: Box<dyn SecretVault>, app_version: &str) -> AppResult<Self> {
        let runtime = match CalendarStore::open(&path) {
            Ok(store) => StoreRuntime {
                store: Some(store),
                startup_error: None,
            },
            Err(error) => {
                tauri_plugin_log::log::error!("calendar_store_unavailable");
                StoreRuntime {
                    store: None,
                    startup_error: Some(error.to_string()),
                }
            }
        };
        Ok(Self {
            runtime: Mutex::new(runtime),
            links: CalendarLinks::new(vault),
            client: http_client(app_version)?,
            refreshing: Mutex::new(HashSet::new()),
            window: current_window,
        })
    }

    #[cfg(test)]
    fn with_store(
        store: CalendarStore,
        vault: Box<dyn SecretVault>,
        window: fn() -> SyncWindow,
    ) -> Self {
        Self {
            runtime: Mutex::new(StoreRuntime {
                store: Some(store),
                startup_error: None,
            }),
            links: CalendarLinks::new(vault),
            client: http_client("test").unwrap(),
            refreshing: Mutex::new(HashSet::new()),
            window,
        }
    }

    fn store<T>(&self, operation: impl FnOnce(&mut CalendarStore) -> AppResult<T>) -> AppResult<T> {
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| AppError::Internal("The calendar cache is unavailable.".into()))?;
        match runtime.store.as_mut() {
            Some(store) => operation(store),
            None => Err(AppError::Internal(format!(
                "The calendar cache could not be opened: {}",
                runtime.startup_error.as_deref().unwrap_or("unknown error")
            ))),
        }
    }

    pub fn list(&self) -> AppResult<Vec<Calendar>> {
        self.store(|store| store.list())
    }

    /// Every calendar, and the events of visible calendars in the range.
    pub fn agenda(&self, range: &DayRange) -> AppResult<CalendarAgenda> {
        self.store(|store| {
            Ok(CalendarAgenda {
                calendars: store.list()?,
                events: store.events(
                    &range.first_day,
                    &range.end_day,
                    &range.start_utc,
                    &range.end_utc,
                )?,
            })
        })
    }

    /// Busy events of visible calendars in the range, and whether a visible calendar has a
    /// problem that may hide some of them.
    pub fn busy(&self, range: &DayRange) -> AppResult<(Vec<ExternalEvent>, bool)> {
        self.store(|store| {
            let incomplete = store
                .list()?
                .iter()
                .any(|calendar| calendar.visible && calendar.problem.is_some());
            let events = store
                .events(
                    &range.first_day,
                    &range.end_day,
                    &range.start_utc,
                    &range.end_utc,
                )?
                .into_iter()
                .filter(|event| event.busy)
                .collect();
            Ok((events, incomplete))
        })
    }

    pub async fn subscribe(&self, input: SubscribeCalendarInput) -> AppResult<Calendar> {
        let link = CalendarLink::parse(&input.link)?;
        self.ensure_room()?;
        let body = match fetch(&self.client, link.as_str(), None, None).await {
            Ok(Fetched::Content { body, .. }) => body,
            Ok(Fetched::Unchanged) => {
                return Err(AppError::Calendar(CalendarProblem::NotACalendar))
            }
            Err(problem) => return Err(AppError::Calendar(problem)),
        };
        let window = (self.window)();
        let (content, read) = read_in_background(body, window).await?;
        let id = Uuid::new_v4().to_string();
        let name = display_name(&input.name, &read, &link.label());
        self.links.set(&id, link.as_str())?;
        let created = self.store(|store| {
            store.create(
                NewCalendar {
                    id: &id,
                    kind: CalendarKind::IcsLink,
                    name: &name,
                    color: input.color,
                    source_label: &link.label(),
                },
                &Snapshot {
                    content: &content,
                    read: &read,
                    window: &window,
                    etag: None,
                    last_modified: None,
                },
                Some(Utc::now() + Duration::minutes(REFRESH_INTERVAL_MINUTES)),
            )
        });
        if created.is_err() {
            let _ = self.links.remove(&id);
        }
        created
    }

    /// Adds an imported .ics file as a calendar that changes only when a newer file replaces it.
    pub async fn import_file(
        &self,
        file_name: &str,
        content: String,
        color: PlanColor,
    ) -> AppResult<Calendar> {
        self.ensure_room()?;
        let window = (self.window)();
        let (content, read) = read_in_background(content, window).await?;
        let label = file_label(file_name);
        let name = display_name("", &read, file_stem(&label));
        let id = Uuid::new_v4().to_string();
        self.store(|store| {
            store.create(
                NewCalendar {
                    id: &id,
                    kind: CalendarKind::IcsFile,
                    name: &name,
                    color,
                    source_label: &label,
                },
                &Snapshot {
                    content: &content,
                    read: &read,
                    window: &window,
                    etag: None,
                    last_modified: None,
                },
                None,
            )
        })
    }

    pub async fn replace_file(
        &self,
        id: &str,
        revision: i64,
        file_name: &str,
        content: String,
    ) -> AppResult<Calendar> {
        self.expect_kind(id, CalendarKind::IcsFile)?;
        let window = (self.window)();
        let (content, read) = read_in_background(content, window).await?;
        self.store(|store| {
            store.replace_source(
                id,
                revision,
                &file_label(file_name),
                &Snapshot {
                    content: &content,
                    read: &read,
                    window: &window,
                    etag: None,
                    last_modified: None,
                },
                None,
            )
        })
    }

    /// Points a link calendar at a new link, such as one issued after the old link was reset.
    pub async fn replace_link(&self, input: ReplaceCalendarLinkInput) -> AppResult<Calendar> {
        let link = CalendarLink::parse(&input.link)?;
        self.expect_kind(&input.id, CalendarKind::IcsLink)?;
        let body = match fetch(&self.client, link.as_str(), None, None).await {
            Ok(Fetched::Content { body, .. }) => body,
            Ok(Fetched::Unchanged) => {
                return Err(AppError::Calendar(CalendarProblem::NotACalendar))
            }
            Err(problem) => return Err(AppError::Calendar(problem)),
        };
        let window = (self.window)();
        let (content, read) = read_in_background(body, window).await?;
        let current = self
            .store(|store| store.calendar(&input.id))?
            .ok_or(AppError::NotFound)?;
        if current.revision != input.revision {
            return Err(AppError::Conflict);
        }
        // If the calendar changes before its new content is stored, the old link goes back.
        let previous = self.links.get(&input.id).ok().flatten();
        self.links.set(&input.id, link.as_str())?;
        let replaced = self.store(|store| {
            store.replace_source(
                &input.id,
                input.revision,
                &link.label(),
                &Snapshot {
                    content: &content,
                    read: &read,
                    window: &window,
                    etag: None,
                    last_modified: None,
                },
                Some(Utc::now() + Duration::minutes(REFRESH_INTERVAL_MINUTES)),
            )
        });
        if replaced.is_err() {
            if let Some(previous) = previous {
                let _ = self.links.set(&input.id, &previous);
            }
        }
        replaced
    }

    pub fn update(&self, input: UpdateCalendarInput) -> AppResult<Calendar> {
        self.store(|store| store.update(&input))
    }

    /// Removes a calendar, everything cached for it, and its link. A keychain that refuses the
    /// deletion doesn't keep the calendar around; the orphaned item is logged instead.
    pub fn remove(&self, id: &str, revision: i64) -> AppResult<()> {
        let calendar = self
            .store(|store| store.calendar(id))?
            .ok_or(AppError::NotFound)?;
        if calendar.revision != revision {
            return Err(AppError::Conflict);
        }
        if calendar.kind == CalendarKind::IcsLink && self.links.remove(id).is_err() {
            tauri_plugin_log::log::warn!("calendar_link_not_deleted");
        }
        self.store(|store| store.remove(id, revision))
    }

    /// Refreshes one calendar now. A link is fetched again; a file is re-read from its stored
    /// content for the current window.
    pub async fn refresh(&self, id: &str) -> AppResult<Calendar> {
        let calendar = self
            .store(|store| store.calendar(id))?
            .ok_or(AppError::NotFound)?;
        match calendar.kind {
            CalendarKind::IcsLink => {
                let target = self
                    .store(|store| store.refresh_target(id))?
                    .ok_or(AppError::NotFound)?;
                if let Err(AppError::Keychain) = self.refresh_link(target).await {
                    return Err(AppError::Keychain);
                }
            }
            CalendarKind::IcsFile => self.rewindow(id, &(self.window)()).await?,
        }
        self.store(|store| store.calendar(id))?
            .ok_or(AppError::NotFound)
    }

    /// Refreshes due link calendars and re-reads calendars whose window moved. Returns whether
    /// anything the renderer shows may have changed.
    pub async fn run_due(&self) -> AppResult<bool> {
        let targets = self.store(|store| store.due_refreshes(Utc::now()))?;
        let mut changed = !targets.is_empty();
        for target in targets {
            let _ = self.refresh_link(target).await;
        }
        let window = (self.window)();
        let stale = self.store(|store| store.stale_windows(&window))?;
        changed |= !stale.is_empty();
        for id in stale {
            let _ = self.rewindow(&id, &window).await;
        }
        Ok(changed)
    }

    /// Fetches a link calendar and records the outcome on the calendar itself.
    async fn refresh_link(&self, target: RefreshTarget) -> AppResult<()> {
        let Some(_guard) = self.claim(&target.id)? else {
            return Ok(());
        };
        let link = match self.links.get(&target.id) {
            Ok(Some(link)) => link,
            Ok(None) => return self.record(&target.id, CalendarProblem::LinkUnavailable),
            Err(error) => {
                self.record(&target.id, CalendarProblem::LinkUnavailable)?;
                return Err(error);
            }
        };
        let fetched = fetch(
            &self.client,
            &link,
            target.etag.as_deref(),
            target.last_modified.as_deref(),
        )
        .await;
        let next_refresh = Utc::now() + Duration::minutes(REFRESH_INTERVAL_MINUTES);
        match fetched {
            Ok(Fetched::Unchanged) => {
                self.store(|store| store.record_unchanged(&target.id, next_refresh))?;
                let window = (self.window)();
                if self
                    .store(|store| store.stale_windows(&window))?
                    .contains(&target.id)
                {
                    self.rewindow(&target.id, &window).await?;
                }
                Ok(())
            }
            Ok(Fetched::Content {
                body,
                etag,
                last_modified,
            }) => {
                let window = (self.window)();
                match read_in_background(body, window).await {
                    Ok((content, read)) => self.store(|store| {
                        store.store_refresh(
                            &target.id,
                            &Snapshot {
                                content: &content,
                                read: &read,
                                window: &window,
                                etag: etag.as_deref(),
                                last_modified: last_modified.as_deref(),
                            },
                            Some(next_refresh),
                        )
                    }),
                    Err(AppError::Calendar(problem)) => self.record(&target.id, problem),
                    Err(error) => Err(error),
                }
            }
            Err(problem) => {
                tauri_plugin_log::log::info!("calendar_refresh_failed code={}", problem.as_str());
                self.record(&target.id, problem)
            }
        }
    }

    async fn rewindow(&self, id: &str, window: &SyncWindow) -> AppResult<()> {
        let Some(content) = self.store(|store| store.content(id))? else {
            return Ok(());
        };
        let (_, read) = read_in_background(content, *window).await?;
        self.store(|store| store.store_window(id, &read, window))
    }

    fn record(&self, id: &str, problem: CalendarProblem) -> AppResult<()> {
        self.store(|store| {
            store.record_problem(id, problem, Some(retry_after(problem, Utc::now())))
        })
    }

    fn ensure_room(&self) -> AppResult<()> {
        if self.list()?.len() >= crate::model::MAX_CALENDARS {
            return Err(AppError::Validation(format!(
                "DayPlan can show up to {} calendars. Remove one to add another.",
                crate::model::MAX_CALENDARS
            )));
        }
        Ok(())
    }

    fn expect_kind(&self, id: &str, kind: CalendarKind) -> AppResult<()> {
        let calendar = self
            .store(|store| store.calendar(id))?
            .ok_or(AppError::NotFound)?;
        if calendar.kind != kind {
            return Err(AppError::Validation(
                "That change doesn't apply to this kind of calendar.".into(),
            ));
        }
        Ok(())
    }

    /// Marks a calendar as refreshing until the returned guard drops, or returns `None` when a
    /// refresh is already in flight.
    fn claim(&self, id: &str) -> AppResult<Option<RefreshClaim<'_>>> {
        let mut refreshing = self.refreshing_set()?;
        if !refreshing.insert(id.into()) {
            return Ok(None);
        }
        Ok(Some(RefreshClaim {
            service: self,
            id: id.into(),
        }))
    }

    fn refreshing_set(&self) -> AppResult<MutexGuard<'_, HashSet<String>>> {
        self.refreshing
            .lock()
            .map_err(|_| AppError::Internal("Calendar refreshes are unavailable.".into()))
    }
}

struct RefreshClaim<'a> {
    service: &'a CalendarService,
    id: String,
}

impl Drop for RefreshClaim<'_> {
    fn drop(&mut self) {
        if let Ok(mut refreshing) = self.service.refreshing_set() {
            refreshing.remove(&self.id);
        }
    }
}

/// The name a user typed, else the calendar's own name, else where it came from.
fn display_name(typed: &str, read: &CalendarRead, fallback: &str) -> String {
    let typed = typed.trim();
    if !typed.is_empty() {
        return typed.into();
    }
    read.name
        .clone()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| fallback.into())
        .chars()
        .take(crate::model::MAX_CALENDAR_NAME_LENGTH)
        .collect()
}

fn file_label(file_name: &str) -> String {
    let name = file_name.trim();
    if name.is_empty() {
        "Imported calendar.ics".into()
    } else {
        name.chars().take(120).collect()
    }
}

fn file_stem(label: &str) -> &str {
    label
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .filter(|stem| !stem.is_empty())
        .unwrap_or(label)
}

#[cfg(test)]
mod tests {
    use super::secrets::MemoryVault;
    use super::*;
    use std::sync::Arc;

    const FEED: &str = "BEGIN:VCALENDAR\r
X-WR-CALNAME:Family\r
BEGIN:VEVENT\r
UID:dinner\r
SUMMARY:Dinner\r
DTSTART:20260917T230000Z\r
DTEND:20260918T000000Z\r
END:VEVENT\r
END:VCALENDAR\r
";

    fn fixed_window() -> SyncWindow {
        SyncWindow::around(
            NaiveDate::from_ymd_opt(2026, 9, 17).unwrap(),
            chrono_tz::America::New_York,
        )
    }

    fn service() -> (CalendarService, Arc<MemoryVault>) {
        let vault = Arc::new(MemoryVault::default());
        let service = CalendarService::with_store(
            CalendarStore::open_in_memory().unwrap(),
            Box::new(vault.clone()),
            fixed_window,
        );
        (service, vault)
    }

    #[test]
    fn day_ranges_follow_local_midnight() {
        let range = DayRange::new("2026-11-01", 1, "America/New_York").unwrap();
        assert_eq!(
            range,
            DayRange {
                first_day: "2026-11-01".into(),
                end_day: "2026-11-02".into(),
                start_utc: "2026-11-01T04:00:00.000Z".into(),
                end_utc: "2026-11-02T05:00:00.000Z".into(),
            }
        );
        assert!(DayRange::new("2026-11-01", 15, "America/New_York").is_err());
        assert!(DayRange::new("2026-11-01", 1, "Mars/Olympus").is_err());
    }

    #[tokio::test]
    async fn imported_files_are_named_listed_replaced_and_removed() {
        let (service, _) = service();
        let calendar = service
            .import_file("family.ics", FEED.into(), PlanColor::Clay)
            .await
            .unwrap();
        assert_eq!(
            (
                calendar.kind,
                calendar.name.as_str(),
                calendar.source_label.as_str()
            ),
            (CalendarKind::IcsFile, "Family", "family.ics")
        );
        let unnamed = service
            .import_file(
                "Holidays 2027.ics",
                FEED.replace("X-WR-CALNAME:Family\r\n", ""),
                PlanColor::Sage,
            )
            .await
            .unwrap();
        assert_eq!(unnamed.name, "Holidays 2027");
        assert!(matches!(
            service
                .import_file("notes.ics", "just text".into(), PlanColor::Sage)
                .await,
            Err(AppError::Calendar(CalendarProblem::NotACalendar))
        ));

        let replaced = service
            .replace_file(
                &calendar.id,
                calendar.revision,
                "family-2.ics",
                FEED.replace("Dinner", "Late dinner"),
            )
            .await
            .unwrap();
        assert_eq!(
            (replaced.revision, replaced.source_label.as_str()),
            (2, "family-2.ics")
        );
        let range = DayRange::new("2026-09-17", 1, "America/New_York").unwrap();
        let titles = service
            .agenda(&range)
            .unwrap()
            .events
            .into_iter()
            .map(|event| event.title)
            .collect::<Vec<_>>();
        assert_eq!(titles, ["Dinner", "Late dinner"]);
        assert!(matches!(
            service.remove(&calendar.id, 1),
            Err(AppError::Conflict)
        ));
        service.remove(&calendar.id, 2).unwrap();
        assert_eq!(service.list().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_link_calendar_without_its_link_reports_a_problem() {
        let (service, vault) = service();
        let window = fixed_window();
        let read = read_calendar(FEED, &window).unwrap();
        service
            .store(|store| {
                store.create(
                    NewCalendar {
                        id: "family",
                        kind: CalendarKind::IcsLink,
                        name: "Family",
                        color: PlanColor::Lake,
                        source_label: "calendar.google.com",
                    },
                    &Snapshot {
                        content: FEED,
                        read: &read,
                        window: &window,
                        etag: None,
                        last_modified: None,
                    },
                    None,
                )
            })
            .unwrap();
        assert!(vault.secrets.lock().unwrap().is_empty());
        let refreshed = service.refresh("family").await.unwrap();
        assert_eq!(
            refreshed.problem.map(|issue| issue.code),
            Some(CalendarProblem::LinkUnavailable)
        );
        let range = DayRange::new("2026-09-17", 1, "America/New_York").unwrap();
        let (busy, incomplete) = service.busy(&range).unwrap();
        assert!(
            incomplete,
            "a visible calendar with a problem may be missing busy time"
        );
        assert_eq!(busy.len(), 1);
        service.remove("family", refreshed.revision).unwrap();
        assert!(service.list().unwrap().is_empty());
    }

    #[test]
    fn names_fall_back_to_the_calendar_then_its_source() {
        let read = CalendarRead {
            name: Some("Work".into()),
            events: Vec::new(),
        };
        assert_eq!(display_name("  Mine ", &read, "host"), "Mine");
        assert_eq!(display_name("", &read, "host"), "Work");
        let unnamed = CalendarRead {
            name: None,
            events: Vec::new(),
        };
        assert_eq!(
            display_name(" ", &unnamed, "calendar.google.com"),
            "calendar.google.com"
        );
        assert_eq!(file_stem("work.ics"), "work");
        assert_eq!(file_stem(".ics"), ".ics");
    }
}
