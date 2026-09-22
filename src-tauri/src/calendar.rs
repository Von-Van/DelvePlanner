//! Read-only calendars from other services. Every source normalizes into one external-event
//! shape stored in a cache kept apart from planner data: an iCalendar link DayPlan refreshes in
//! the background, an iCalendar file imported once, or a Google or Microsoft account read through
//! OAuth with read-only scopes.

pub mod ics;
pub mod link;
pub mod oauth;
pub mod providers;
pub mod secrets;
pub mod store;

use crate::error::{AppError, AppResult};
use crate::model::{
    Calendar, CalendarAccount, CalendarAgenda, CalendarKind, CalendarProblem, CalendarProvider,
    ConnectedAccount, ExternalEvent, PlanColor, RemoteCalendar, ReplaceCalendarLinkInput,
    SubscribeCalendarInput, UpdateCalendarInput,
};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use chrono_tz::Tz;
use ics::{local_midnight, read_calendar, CalendarRead, SyncWindow};
use link::{fetch, http_client, CalendarLink, Fetched};
use oauth::ProviderConfig;
use providers::ProviderApi;
use secrets::{CalendarSecrets, SecretVault};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use store::{timestamp, CalendarSource, CalendarStore, NewAccount, NewCalendar, Snapshot};
use uuid::Uuid;

/// An access token is refreshed a minute before it runs out, so a slow request can't use a
/// token that expires mid-flight.
const TOKEN_MARGIN_SECONDS: i64 = 60;

/// Access tokens by account, with the moment each one stops working.
type AccessTokens = HashMap<String, (String, DateTime<Utc>)>;

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
    secrets: CalendarSecrets,
    client: reqwest::Client,
    /// Calendars with a refresh in flight, so the worker and a manual refresh never overlap.
    refreshing: Mutex<HashSet<String>>,
    /// Access tokens by account, kept only in memory and only until they expire.
    access_tokens: Mutex<AccessTokens>,
    /// The window events are read for; fixed in tests so they don't depend on today's date.
    window: fn() -> SyncWindow,
    /// Where accounts sign in and where their calendars are read from; tests point these at a
    /// local server.
    sign_in: fn(CalendarProvider) -> ProviderConfig,
    api: fn(CalendarProvider) -> ProviderApi,
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
            secrets: CalendarSecrets::new(vault),
            client: http_client(app_version)?,
            refreshing: Mutex::new(HashSet::new()),
            access_tokens: Mutex::new(HashMap::new()),
            window: current_window,
            sign_in: ProviderConfig::new,
            api: ProviderApi::new,
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
            secrets: CalendarSecrets::new(vault),
            client: http_client("test").unwrap(),
            refreshing: Mutex::new(HashSet::new()),
            access_tokens: Mutex::new(HashMap::new()),
            window,
            sign_in: ProviderConfig::new,
            api: ProviderApi::new,
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
        self.secrets.set_link(&id, link.as_str())?;
        let created = self.store(|store| {
            store.create(
                NewCalendar {
                    id: &id,
                    kind: CalendarKind::IcsLink,
                    name: &name,
                    color: input.color,
                    source_label: &link.label(),
                    account_id: None,
                    remote_id: None,
                },
                &Snapshot {
                    content: Some(&content),
                    read: &read,
                    window: &window,
                    etag: None,
                    last_modified: None,
                },
                Some(Utc::now() + Duration::minutes(REFRESH_INTERVAL_MINUTES)),
            )
        });
        if created.is_err() {
            let _ = self.secrets.remove_link(&id);
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
                    account_id: None,
                    remote_id: None,
                },
                &Snapshot {
                    content: Some(&content),
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
                    content: Some(&content),
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
        let previous = self.secrets.link(&input.id).ok().flatten();
        self.secrets.set_link(&input.id, link.as_str())?;
        let replaced = self.store(|store| {
            store.replace_source(
                &input.id,
                input.revision,
                &link.label(),
                &Snapshot {
                    content: Some(&content),
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
                let _ = self.secrets.set_link(&input.id, &previous);
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
        if calendar.kind == CalendarKind::IcsLink && self.secrets.remove_link(id).is_err() {
            tauri_plugin_log::log::warn!("calendar_link_not_deleted");
        }
        self.store(|store| store.remove(id, revision))
    }

    /// Refreshes one calendar now: a link and an account calendar are fetched again, and a file
    /// is re-read from its stored content for the current window.
    pub async fn refresh(&self, id: &str) -> AppResult<Calendar> {
        let source = self
            .store(|store| store.source(id))?
            .ok_or(AppError::NotFound)?;
        match source.kind {
            CalendarKind::IcsFile => self.rewindow(id, &(self.window)()).await?,
            _ => {
                if let Err(error @ (AppError::Keychain | AppError::Calendar(_))) =
                    self.refresh_source(source).await
                {
                    // The calendar keeps the problem; the caller hears why the refresh failed.
                    if matches!(error, AppError::Keychain) {
                        return Err(error);
                    }
                }
            }
        }
        self.store(|store| store.calendar(id))?
            .ok_or(AppError::NotFound)
    }

    /// Refreshes every calendar that is due and re-reads the ones whose window moved. Returns
    /// whether anything the renderer shows may have changed.
    pub async fn run_due(&self) -> AppResult<bool> {
        let due = self.store(|store| store.due_refreshes(Utc::now()))?;
        let mut changed = !due.is_empty();
        for source in due {
            let _ = self.refresh_source(source).await;
        }
        let window = (self.window)();
        let stale = self.store(|store| store.stale_windows(&window))?;
        changed |= !stale.is_empty();
        for id in stale {
            let _ = self.rewindow(&id, &window).await;
        }
        Ok(changed)
    }

    /// Signs in to an account in the system browser and lists the calendars it offers. Signing in
    /// again to an account DayPlan already knows refreshes that account instead of doubling it.
    pub async fn connect(
        &self,
        provider: CalendarProvider,
        open: &(dyn Fn(&str) -> AppResult<()> + Send + Sync),
    ) -> AppResult<ConnectedAccount> {
        let config = (self.sign_in)(provider);
        let tokens = oauth::sign_in(&config, &self.client, open).await?;
        let api = (self.api)(provider);
        let listed = api.calendars(&self.client, &tokens.access_token).await?;
        let label = listed.label.unwrap_or_else(|| provider.label().to_string());
        let existing = self
            .store(|store| store.list_accounts())?
            .into_iter()
            .find(|account| account.provider == provider && account.label == label);
        let account = match existing {
            Some(account) => {
                self.store(|store| store.record_account_problem(&account.id, None))?;
                account
            }
            None => {
                let id = Uuid::new_v4().to_string();
                self.store(|store| {
                    store.create_account(NewAccount {
                        id: &id,
                        provider,
                        label: &label,
                    })
                })?
            }
        };
        self.secrets
            .set_refresh_token(&account.id, &tokens.refresh_token)?;
        self.remember_token(&account.id, &tokens.access_token, tokens.expires_at)?;
        let added = self.store(|store| store.account_remote_ids(&account.id))?;
        Ok(ConnectedAccount {
            calendars: mark_added(listed.calendars, &added),
            account,
        })
    }

    pub fn accounts(&self) -> AppResult<Vec<CalendarAccount>> {
        self.store(|store| store.list_accounts())
    }

    /// The calendars an account offers, for choosing which ones DayPlan shows.
    pub async fn account_calendars(&self, account_id: &str) -> AppResult<Vec<RemoteCalendar>> {
        let account = self
            .store(|store| store.account(account_id))?
            .ok_or(AppError::NotFound)?;
        let token = self
            .access_token(&account)
            .await
            .inspect_err(|error| self.note_account_problem(&account, error))?;
        let listed = (self.api)(account.provider)
            .calendars(&self.client, &token)
            .await
            .inspect_err(|error| self.note_account_problem(&account, error))?;
        let added = self.store(|store| store.account_remote_ids(account_id))?;
        Ok(mark_added(listed.calendars, &added))
    }

    /// Adds calendars from a connected account and reads each one for the first time.
    pub async fn add_account_calendars(
        &self,
        account_id: &str,
        remote_ids: Vec<String>,
    ) -> AppResult<Vec<Calendar>> {
        if remote_ids.is_empty() {
            return Ok(Vec::new());
        }
        let account = self
            .store(|store| store.account(account_id))?
            .ok_or(AppError::NotFound)?;
        let token = self
            .access_token(&account)
            .await
            .inspect_err(|error| self.note_account_problem(&account, error))?;
        let api = (self.api)(account.provider);
        let offered = api
            .calendars(&self.client, &token)
            .await
            .inspect_err(|error| self.note_account_problem(&account, error))?;
        let window = (self.window)();
        let mut added = Vec::new();
        for remote_id in remote_ids {
            let Some(offer) = offered
                .calendars
                .iter()
                .find(|calendar| calendar.id == remote_id)
            else {
                continue;
            };
            let events = api
                .events(&self.client, &token, &remote_id, &window)
                .await
                .inspect_err(|error| self.note_account_problem(&account, error))?;
            let id = Uuid::new_v4().to_string();
            let color = self.next_color()?;
            let read = CalendarRead {
                name: Some(offer.name.clone()),
                events,
            };
            added.push(self.store(|store| {
                store.create(
                    NewCalendar {
                        id: &id,
                        kind: account.provider.calendar_kind(),
                        name: &offer.name,
                        color,
                        source_label: &account.label,
                        account_id: Some(account_id),
                        remote_id: Some(&remote_id),
                    },
                    &Snapshot {
                        content: None,
                        read: &read,
                        window: &window,
                        etag: None,
                        last_modified: None,
                    },
                    Some(Utc::now() + Duration::minutes(REFRESH_INTERVAL_MINUTES)),
                )
            })?);
        }
        Ok(added)
    }

    /// Disconnects an account: the sign-in is withdrawn where the provider allows it, and every
    /// calendar and event cached from it is deleted.
    pub async fn disconnect(&self, account_id: &str, revision: i64) -> AppResult<()> {
        let account = self
            .store(|store| store.account(account_id))?
            .ok_or(AppError::NotFound)?;
        if account.revision != revision {
            return Err(AppError::Conflict);
        }
        if let Ok(Some(refresh_token)) = self.secrets.refresh_token(account_id) {
            oauth::revoke(
                &(self.sign_in)(account.provider),
                &self.client,
                &refresh_token,
            )
            .await;
        }
        if self.secrets.remove_refresh_token(account_id).is_err() {
            tauri_plugin_log::log::warn!("calendar_tokens_not_deleted");
        }
        self.forget_token(account_id)?;
        self.store(|store| store.remove_account(account_id, revision))
    }

    /// Fetches one calendar and records the outcome on the calendar itself.
    async fn refresh_source(&self, source: CalendarSource) -> AppResult<()> {
        let Some(_guard) = self.claim(&source.id)? else {
            return Ok(());
        };
        match source.kind {
            CalendarKind::IcsFile => Ok(()),
            CalendarKind::IcsLink => self.refresh_link(source).await,
            CalendarKind::Google | CalendarKind::Microsoft => {
                self.refresh_account_calendar(source).await
            }
        }
    }

    /// Reads one account calendar again. A sign-in that has ended marks the whole account, since
    /// every calendar from it is affected.
    async fn refresh_account_calendar(&self, source: CalendarSource) -> AppResult<()> {
        // Nothing here can be fixed by pasting a link, so every failure asks for a sign-in: the
        // account row or its remote ID is gone, or the keychain won't give up the refresh token.
        let (Some(account_id), Some(remote_id)) = (&source.account_id, &source.remote_id) else {
            return self.record(&source.id, CalendarProblem::SignInExpired);
        };
        let Some(account) = self.store(|store| store.account(account_id))? else {
            return self.record(&source.id, CalendarProblem::SignInExpired);
        };
        let token = match self.access_token(&account).await {
            Ok(token) => token,
            Err(AppError::Calendar(problem)) => {
                self.note_problem(&account, problem)?;
                return Ok(());
            }
            Err(AppError::Keychain) => {
                self.record(&source.id, CalendarProblem::SignInExpired)?;
                return Err(AppError::Keychain);
            }
            Err(error) => return Err(error),
        };
        let window = (self.window)();
        match (self.api)(account.provider)
            .events(&self.client, &token, remote_id, &window)
            .await
        {
            Ok(events) => {
                let read = CalendarRead { name: None, events };
                self.store(|store| {
                    store.store_refresh(
                        &source.id,
                        &Snapshot {
                            content: None,
                            read: &read,
                            window: &window,
                            etag: None,
                            last_modified: None,
                        },
                        Some(Utc::now() + Duration::minutes(REFRESH_INTERVAL_MINUTES)),
                    )
                })
            }
            Err(AppError::Calendar(problem)) => {
                tauri_plugin_log::log::info!(
                    "calendar_refresh_failed provider={} code={}",
                    account.provider.as_str(),
                    problem.as_str()
                );
                if problem == CalendarProblem::SignInExpired {
                    self.note_problem(&account, problem)
                } else {
                    self.record(&source.id, problem)
                }
            }
            Err(error) => Err(error),
        }
    }

    /// A usable access token for an account, refreshed from the keychain when it has run out.
    async fn access_token(&self, account: &CalendarAccount) -> AppResult<String> {
        if let Some((token, expires_at)) = self.access_tokens()?.get(&account.id) {
            if *expires_at > Utc::now() + Duration::seconds(TOKEN_MARGIN_SECONDS) {
                return Ok(token.clone());
            }
        }
        let refresh_token = self
            .secrets
            .refresh_token(&account.id)?
            .ok_or(AppError::Calendar(CalendarProblem::SignInExpired))?;
        let tokens = oauth::refresh(
            &(self.sign_in)(account.provider),
            &self.client,
            &refresh_token,
        )
        .await?;
        // Microsoft hands back a new refresh token each time; Google keeps the same one.
        if tokens.refresh_token != refresh_token {
            self.secrets
                .set_refresh_token(&account.id, &tokens.refresh_token)?;
        }
        self.remember_token(&account.id, &tokens.access_token, tokens.expires_at)?;
        Ok(tokens.access_token)
    }

    fn remember_token(
        &self,
        account_id: &str,
        token: &str,
        expires_at: DateTime<Utc>,
    ) -> AppResult<()> {
        self.access_tokens()?
            .insert(account_id.into(), (token.into(), expires_at));
        Ok(())
    }

    fn forget_token(&self, account_id: &str) -> AppResult<()> {
        self.access_tokens()?.remove(account_id);
        Ok(())
    }

    fn access_tokens(&self) -> AppResult<MutexGuard<'_, AccessTokens>> {
        self.access_tokens
            .lock()
            .map_err(|_| AppError::Internal("Calendar sign-ins are unavailable.".into()))
    }

    /// Marks an account, and every calendar from it, as needing the user.
    fn note_problem(&self, account: &CalendarAccount, problem: CalendarProblem) -> AppResult<()> {
        self.store(|store| store.record_account_problem(&account.id, Some(problem)))
    }

    fn note_account_problem(&self, account: &CalendarAccount, error: &AppError) {
        if let AppError::Calendar(problem) = error {
            let _ = self.note_problem(account, *problem);
        }
    }

    /// A color no other calendar is using yet, so a new one stands apart.
    fn next_color(&self) -> AppResult<PlanColor> {
        let calendars = self.store(|store| store.list())?;
        Ok(PlanColor::ALL
            .iter()
            .copied()
            .find(|color| !calendars.iter().any(|calendar| calendar.color == *color))
            .unwrap_or(PlanColor::ALL[calendars.len() % PlanColor::ALL.len()]))
    }

    /// Fetches a link calendar and records the outcome on the calendar itself.
    async fn refresh_link(&self, target: CalendarSource) -> AppResult<()> {
        let link = match self.secrets.link(&target.id) {
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
                                content: Some(&content),
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

/// Flags the calendars an account already contributes, so they can't be added twice.
fn mark_added(calendars: Vec<RemoteCalendar>, added: &[String]) -> Vec<RemoteCalendar> {
    calendars
        .into_iter()
        .map(|calendar| RemoteCalendar {
            already_added: added.contains(&calendar.id),
            ..calendar
        })
        .collect()
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
                        account_id: None,
                        remote_id: None,
                    },
                    &Snapshot {
                        content: Some(FEED),
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

    /// One scripted HTTP endpoint standing in for a provider's sign-in and calendar APIs.
    static TEST_ENDPOINT: std::sync::OnceLock<String> = std::sync::OnceLock::new();

    fn test_sign_in(provider: CalendarProvider) -> ProviderConfig {
        let base = TEST_ENDPOINT.get().expect("endpoint");
        ProviderConfig {
            authorize_url: format!("{base}/authorize"),
            token_url: format!("{base}/token"),
            revoke_url: None,
            ..ProviderConfig::new(provider)
        }
    }

    fn test_api(provider: CalendarProvider) -> ProviderApi {
        ProviderApi {
            provider,
            base_url: TEST_ENDPOINT.get().expect("endpoint").clone(),
        }
    }

    /// Answers each request in turn and reports what it was asked for.
    async fn provider_server(
        responses: Vec<(&'static str, String)>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let mut requests = Vec::new();
            for (status, body) in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = vec![0; 16_384];
                let read = socket.read(&mut buffer).await.unwrap();
                requests.push(String::from_utf8_lossy(&buffer[..read]).to_string());
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
                let _ = socket.shutdown().await;
            }
            requests
        });
        (format!("http://{address}"), handle)
    }

    /// Stands in for the system browser by following the sign-in URL back to DayPlan.
    fn browser() -> impl Fn(&str) -> AppResult<()> + Send + Sync {
        |url: &str| {
            let url = url.to_string();
            tokio::spawn(async move {
                let parsed = reqwest::Url::parse(&url).unwrap();
                let params: std::collections::HashMap<_, _> =
                    parsed.query_pairs().into_owned().collect();
                let redirect = params.get("redirect_uri").cloned().unwrap();
                let state = params.get("state").cloned().unwrap();
                let _ = reqwest::Client::new()
                    .get(format!("{redirect}/?code=auth-code&state={state}"))
                    .send()
                    .await;
            });
            Ok(())
        }
    }

    #[tokio::test]
    async fn connects_an_account_adds_its_calendars_and_gives_everything_back_on_disconnect() {
        let calendars = r#"{"items":[
            {"id":"me@gmail.com","summary":"me@gmail.com","primary":true},
            {"id":"team@group.calendar.google.com","summary":"Team"}
        ]}"#;
        let events = r#"{"items":[{"id":"a1","summary":"Design review",
            "start":{"dateTime":"2026-09-17T11:00:00-04:00"},
            "end":{"dateTime":"2026-09-17T12:00:00-04:00"}}]}"#;
        let (endpoint, server) = provider_server(vec![
            (
                "200 OK",
                r#"{"access_token":"at-1","refresh_token":"rt-1","expires_in":3600}"#.into(),
            ),
            ("200 OK", calendars.into()),
            ("200 OK", calendars.into()),
            ("200 OK", events.into()),
            ("401 Unauthorized", "{}".into()),
        ])
        .await;
        TEST_ENDPOINT.set(endpoint).ok();
        let vault = Arc::new(MemoryVault::default());
        let mut service = CalendarService::with_store(
            CalendarStore::open_in_memory().unwrap(),
            Box::new(vault.clone()),
            fixed_window,
        );
        service.sign_in = test_sign_in;
        service.api = test_api;

        let connected = service
            .connect(CalendarProvider::Google, &browser())
            .await
            .unwrap();
        assert_eq!(connected.account.label, "me@gmail.com");
        assert_eq!(
            connected
                .calendars
                .iter()
                .map(|calendar| (
                    calendar.name.as_str(),
                    calendar.primary,
                    calendar.already_added
                ))
                .collect::<Vec<_>>(),
            [("me@gmail.com", true, false), ("Team", false, false)]
        );
        assert_eq!(
            vault
                .secrets
                .lock()
                .unwrap()
                .values()
                .next()
                .map(String::as_str),
            Some("rt-1"),
            "the refresh token is the only thing kept, and only in the keychain"
        );

        let added = service
            .add_account_calendars(&connected.account.id, vec!["me@gmail.com".into()])
            .await
            .unwrap();
        assert_eq!(
            (added[0].kind, added[0].name.as_str(), added[0].event_count),
            (CalendarKind::Google, "me@gmail.com", 1)
        );
        assert_eq!(
            added[0].account_id.as_deref(),
            Some(connected.account.id.as_str())
        );
        let range = DayRange::new("2026-09-17", 1, "America/New_York").unwrap();
        assert_eq!(
            service
                .agenda(&range)
                .unwrap()
                .events
                .iter()
                .map(|event| event.title.as_str())
                .collect::<Vec<_>>(),
            ["Design review"]
        );

        // The next read finds the sign-in gone, which marks the account and its calendars.
        let refreshed = service.refresh(&added[0].id).await.unwrap();
        assert_eq!(
            refreshed.problem.map(|issue| issue.code),
            Some(CalendarProblem::SignInExpired)
        );
        let account = &service.accounts().unwrap()[0];
        assert_eq!(
            account.problem.as_ref().map(|issue| issue.code),
            Some(CalendarProblem::SignInExpired)
        );
        assert_eq!(account.calendar_count, 1);
        assert_eq!(
            service.agenda(&range).unwrap().events.len(),
            1,
            "the last copy stays while the sign-in is broken"
        );

        service
            .disconnect(&account.id, account.revision)
            .await
            .unwrap();
        assert!(service.accounts().unwrap().is_empty());
        assert!(
            service.list().unwrap().is_empty(),
            "its calendars go with it"
        );
        assert!(vault.secrets.lock().unwrap().is_empty(), "so do its tokens");
        assert!(service.agenda(&range).unwrap().events.is_empty());
        let requests = server.await.unwrap();
        assert!(
            requests[0].starts_with("POST "),
            "only the token exchange posts"
        );
        for request in &requests[1..] {
            assert!(
                request.starts_with("GET "),
                "calendars are only read: {request}"
            );
        }
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
