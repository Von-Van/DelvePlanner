//! Reading calendars and events from connected accounts. Every request here is a GET to a
//! read-only endpoint, and occurrences arrive already expanded, so they take the same shape as
//! the ones read from an iCalendar file.

use super::ics::{clean_text, instance_key, EventDraft, EventTime, SyncWindow};
use crate::error::{AppError, AppResult};
use crate::model::{CalendarProblem, CalendarProvider, RemoteCalendar};
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use reqwest::{Client, RequestBuilder, Response, StatusCode, Url};
use serde::Deserialize;

const GOOGLE_API: &str = "https://www.googleapis.com/calendar/v3";
const GRAPH_API: &str = "https://graph.microsoft.com/v1.0";
/// Enough pages for a busy year; a calendar past this is truncated rather than fetched forever.
const MAX_PAGES: usize = 20;
const GOOGLE_PAGE_SIZE: &str = "2500";
const GRAPH_PAGE_SIZE: &str = "500";

/// The calendars an account offers, and the address that account signs in as.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountCalendars {
    pub label: Option<String>,
    pub calendars: Vec<RemoteCalendar>,
}

/// Read-only calls to one provider's API.
#[derive(Debug, Clone)]
pub struct ProviderApi {
    pub provider: CalendarProvider,
    pub base_url: String,
}

impl ProviderApi {
    pub fn new(provider: CalendarProvider) -> Self {
        Self {
            provider,
            base_url: match provider {
                CalendarProvider::Google => GOOGLE_API.into(),
                CalendarProvider::Microsoft => GRAPH_API.into(),
            },
        }
    }

    /// Every calendar the account can read.
    pub async fn calendars(&self, client: &Client, token: &str) -> AppResult<AccountCalendars> {
        match self.provider {
            CalendarProvider::Google => {
                let mut url = self.url(&["users", "me", "calendarList"])?;
                url.query_pairs_mut()
                    .append_pair("maxResults", "250")
                    .append_pair("minAccessRole", "reader")
                    .append_pair("showDeleted", "false")
                    .append_pair("showHidden", "true");
                let list: GoogleCalendarList = self.read(client, token, url).await?;
                let mut label = None;
                let calendars = list
                    .items
                    .into_iter()
                    .filter(|item| !item.deleted)
                    .map(|item| {
                        if item.primary {
                            label = Some(item.id.clone());
                        }
                        RemoteCalendar {
                            name: clean_text(
                                item.summary_override.as_deref().unwrap_or(&item.summary),
                            ),
                            id: item.id,
                            primary: item.primary,
                            already_added: false,
                        }
                    })
                    .collect();
                Ok(AccountCalendars { label, calendars })
            }
            CalendarProvider::Microsoft => {
                let mut url = self.url(&["me", "calendars"])?;
                url.query_pairs_mut()
                    .append_pair("$select", "id,name,isDefaultCalendar,owner")
                    .append_pair("$top", "100");
                let list: GraphList<GraphCalendar> = self.read(client, token, url).await?;
                let mut label = None;
                let calendars = list
                    .value
                    .into_iter()
                    .map(|item| {
                        if item.is_default_calendar {
                            label = item.owner.and_then(|owner| owner.address);
                        }
                        RemoteCalendar {
                            id: item.id,
                            name: clean_text(&item.name),
                            primary: item.is_default_calendar,
                            already_added: false,
                        }
                    })
                    .collect();
                Ok(AccountCalendars { label, calendars })
            }
        }
    }

    /// The occurrences in `window`, already expanded by the provider.
    pub async fn events(
        &self,
        client: &Client,
        token: &str,
        remote_id: &str,
        window: &SyncWindow,
    ) -> AppResult<Vec<EventDraft>> {
        let start = instant(window.start_utc());
        let end = instant(window.end_utc());
        let mut events = Vec::new();
        match self.provider {
            CalendarProvider::Google => {
                let mut page: Option<String> = None;
                for _ in 0..MAX_PAGES {
                    let mut url = self.url(&["calendars", remote_id, "events"])?;
                    url.query_pairs_mut()
                        .append_pair("singleEvents", "true")
                        .append_pair("orderBy", "startTime")
                        .append_pair("showDeleted", "false")
                        .append_pair("maxResults", GOOGLE_PAGE_SIZE)
                        .append_pair("timeMin", &start)
                        .append_pair("timeMax", &end);
                    if let Some(token) = &page {
                        url.query_pairs_mut().append_pair("pageToken", token);
                    }
                    let list: GoogleEvents = self.read(client, token, url).await?;
                    events.extend(list.items.iter().filter_map(google_event));
                    page = list.next_page_token;
                    if page.is_none() {
                        break;
                    }
                }
            }
            CalendarProvider::Microsoft => {
                let mut url = self.url(&["me", "calendars", remote_id, "calendarView"])?;
                url.query_pairs_mut()
                    .append_pair("startDateTime", &start)
                    .append_pair("endDateTime", &end)
                    .append_pair(
                        "$select",
                        "id,subject,start,end,isAllDay,showAs,type,isCancelled,location",
                    )
                    .append_pair("$orderby", "start/dateTime")
                    .append_pair("$top", GRAPH_PAGE_SIZE);
                let mut next = Some(url);
                for _ in 0..MAX_PAGES {
                    let Some(url) = next.take() else { break };
                    let list: GraphList<GraphEvent> = self.read(client, token, url).await?;
                    events.extend(list.value.iter().filter_map(graph_event));
                    next = list
                        .next_link
                        .as_deref()
                        .and_then(|link| Url::parse(link).ok());
                }
            }
        }
        Ok(events)
    }

    fn url(&self, segments: &[&str]) -> AppResult<Url> {
        let mut url = Url::parse(&self.base_url)
            .map_err(|_| AppError::Internal("A calendar service address is invalid.".into()))?;
        url.path_segments_mut()
            .map_err(|_| AppError::Internal("A calendar service address is invalid.".into()))?
            .extend(segments);
        Ok(url)
    }

    /// One authorized GET, with the provider's failures mapped onto calendar problems.
    async fn read<T: for<'de> Deserialize<'de>>(
        &self,
        client: &Client,
        token: &str,
        url: Url,
    ) -> AppResult<T> {
        let request = client.get(url).bearer_auth(token);
        let response = self
            .send(request)
            .await
            .map_err(|_| AppError::Calendar(CalendarProblem::Unreachable))?;
        match response.status() {
            status if status.is_success() => response
                .json()
                .await
                .map_err(|_| AppError::Calendar(CalendarProblem::ServerError)),
            // A sign-in that expired, was revoked, or lost the calendar permission.
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                Err(AppError::Calendar(CalendarProblem::SignInExpired))
            }
            StatusCode::NOT_FOUND | StatusCode::GONE => {
                Err(AppError::Calendar(CalendarProblem::LinkNotFound))
            }
            StatusCode::TOO_MANY_REQUESTS => Err(AppError::Calendar(CalendarProblem::RateLimited)),
            status if status.is_server_error() => {
                Err(AppError::Calendar(CalendarProblem::ServerError))
            }
            _ => Err(AppError::Calendar(CalendarProblem::ServerError)),
        }
    }

    async fn send(&self, request: RequestBuilder) -> Result<Response, reqwest::Error> {
        match self.provider {
            // Graph returns times in the zone the client asks for.
            CalendarProvider::Microsoft => {
                request
                    .header("Prefer", "outlook.timezone=\"UTC\"")
                    .send()
                    .await
            }
            CalendarProvider::Google => request.send().await,
        }
    }
}

fn instant(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleCalendarList {
    #[serde(default)]
    items: Vec<GoogleCalendar>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleCalendar {
    id: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    summary_override: Option<String>,
    #[serde(default)]
    primary: bool,
    #[serde(default)]
    deleted: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleEvents {
    #[serde(default)]
    items: Vec<GoogleEvent>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleEvent {
    #[serde(default)]
    id: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    transparency: Option<String>,
    #[serde(default)]
    recurring_event_id: Option<String>,
    start: Option<GoogleTime>,
    end: Option<GoogleTime>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleTime {
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    date_time: Option<String>,
}

fn google_event(event: &GoogleEvent) -> Option<EventDraft> {
    if event.status.as_deref() == Some("cancelled") {
        return None;
    }
    let (start, end) = (event.start.as_ref()?, event.end.as_ref()?);
    let when = match (&start.date, &end.date) {
        (Some(start), Some(end)) => EventTime::AllDay {
            start: day(start)?,
            end: day(end)?,
        },
        _ => {
            let start = moment(start.date_time.as_deref()?)?;
            EventTime::Timed {
                start,
                end: moment(end.date_time.as_deref()?)?.max(start),
            }
        }
    };
    let all_day = matches!(when, EventTime::AllDay { .. });
    let busy = match event.transparency.as_deref() {
        Some("transparent") => false,
        Some("opaque") => true,
        // Timed events take the time by default; all-day events only when they say so, so
        // holiday and birthday calendars don't erase a day.
        _ => !all_day,
    };
    Some(EventDraft {
        key: instance_key(&event.id, &when),
        title: title(event.summary.as_deref()),
        location: event
            .location
            .as_deref()
            .map(clean_text)
            .unwrap_or_default(),
        when,
        busy,
        tentative: event.status.as_deref() == Some("tentative"),
        recurring: event.recurring_event_id.is_some(),
    })
}

#[derive(Debug, Deserialize)]
struct GraphList<T> {
    #[serde(default = "Vec::new")]
    value: Vec<T>,
    #[serde(rename = "@odata.nextLink", default)]
    next_link: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphCalendar {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    is_default_calendar: bool,
    #[serde(default)]
    owner: Option<GraphOwner>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphOwner {
    #[serde(default)]
    address: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphEvent {
    #[serde(default)]
    id: String,
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    is_all_day: bool,
    #[serde(default)]
    is_cancelled: bool,
    #[serde(default)]
    show_as: Option<String>,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    location: Option<GraphLocation>,
    start: Option<GraphTime>,
    end: Option<GraphTime>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphLocation {
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphTime {
    #[serde(default)]
    date_time: Option<String>,
}

fn graph_event(event: &GraphEvent) -> Option<EventDraft> {
    if event.is_cancelled {
        return None;
    }
    let (start, end) = (
        event.start.as_ref()?.date_time.as_deref()?,
        event.end.as_ref()?.date_time.as_deref()?,
    );
    let when = if event.is_all_day {
        EventTime::AllDay {
            start: day(&start[..10.min(start.len())])?,
            end: day(&end[..10.min(end.len())])?,
        }
    } else {
        // Graph answers in UTC because every request asks for it.
        let start = graph_moment(start)?;
        EventTime::Timed {
            start,
            end: graph_moment(end)?.max(start),
        }
    };
    let show_as = event.show_as.as_deref().unwrap_or("busy");
    Some(EventDraft {
        key: instance_key(&event.id, &when),
        title: title(event.subject.as_deref()),
        location: event
            .location
            .as_ref()
            .and_then(|location| location.display_name.as_deref())
            .map(clean_text)
            .unwrap_or_default(),
        when,
        busy: !matches!(show_as, "free" | "workingElsewhere"),
        tentative: show_as == "tentative",
        recurring: event
            .kind
            .as_deref()
            .is_some_and(|kind| kind != "singleInstance"),
    })
}

fn title(value: Option<&str>) -> String {
    value
        .map(clean_text)
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| "Untitled event".into())
}

fn day(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()
}

fn moment(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

/// Graph writes local times without an offset, such as `2026-09-17T13:00:00.0000000`.
fn graph_moment(value: &str) -> Option<DateTime<Utc>> {
    moment(value).or_else(|| {
        let trimmed = value.split('.').next().unwrap_or(value);
        chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S")
            .ok()
            .map(|naive| naive.and_utc())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::America::New_York;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Serves canned JSON and records the request lines and headers it saw.
    async fn api(
        responses: Vec<(&'static str, String)>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
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
        (format!("http://{address}/api"), handle)
    }

    fn window() -> SyncWindow {
        SyncWindow::around(NaiveDate::from_ymd_opt(2026, 9, 17).unwrap(), New_York)
    }

    #[tokio::test]
    async fn reads_google_calendars_and_events_without_asking_to_change_anything() {
        let calendars = r#"{"items":[
            {"id":"me@gmail.com","summary":"me@gmail.com","primary":true},
            {"id":"family@group.calendar.google.com","summary":"Family","summaryOverride":"Home"},
            {"id":"old@group.calendar.google.com","summary":"Old","deleted":true}
        ]}"#
        .to_string();
        let first_page = r#"{"nextPageToken":"page-2","items":[
            {"id":"a1","status":"confirmed","summary":"Design review","location":"Room 4B",
             "start":{"dateTime":"2026-09-17T11:00:00-04:00"},"end":{"dateTime":"2026-09-17T12:00:00-04:00"}},
            {"id":"a2","status":"cancelled","summary":"Dropped",
             "start":{"dateTime":"2026-09-17T15:00:00-04:00"},"end":{"dateTime":"2026-09-17T15:30:00-04:00"}},
            {"id":"a3","summary":"Focus time","transparency":"transparent","recurringEventId":"series-1",
             "start":{"dateTime":"2026-09-17T13:00:00-04:00"},"end":{"dateTime":"2026-09-17T14:00:00-04:00"}}
        ]}"#
        .to_string();
        let second_page = r#"{"items":[
            {"id":"a4","summary":"Offsite","start":{"date":"2026-09-18"},"end":{"date":"2026-09-20"}},
            {"id":"a5","start":{"date":"2026-09-21"},"end":{"date":"2026-09-22"},"transparency":"opaque"}
        ]}"#
        .to_string();
        let (base, server) = api(vec![
            ("200 OK", calendars),
            ("200 OK", first_page),
            ("200 OK", second_page),
        ])
        .await;
        let api = ProviderApi {
            provider: CalendarProvider::Google,
            base_url: base,
        };
        let client = Client::new();

        let listed = api.calendars(&client, "at-1").await.unwrap();
        assert_eq!(listed.label.as_deref(), Some("me@gmail.com"));
        assert_eq!(
            listed
                .calendars
                .iter()
                .map(|calendar| (calendar.name.as_str(), calendar.primary))
                .collect::<Vec<_>>(),
            [("me@gmail.com", true), ("Home", false)],
            "deleted calendars are left out and an override wins over the summary"
        );

        let events = api
            .events(&client, "at-1", "me@gmail.com", &window())
            .await
            .unwrap();
        let described = events
            .iter()
            .map(|event| (event.title.as_str(), event.busy, event.recurring))
            .collect::<Vec<_>>();
        assert_eq!(
            described,
            [
                ("Design review", true, false),
                ("Focus time", false, true),
                ("Offsite", false, false),
                ("Untitled event", true, false),
            ],
            "cancelled events are skipped, transparent events are free, and all-day events are \
             free unless marked busy"
        );
        assert_eq!(
            events[0].when,
            EventTime::Timed {
                start: "2026-09-17T15:00:00Z".parse().unwrap(),
                end: "2026-09-17T16:00:00Z".parse().unwrap()
            }
        );
        assert_eq!(
            events[2].when,
            EventTime::AllDay {
                start: NaiveDate::from_ymd_opt(2026, 9, 18).unwrap(),
                end: NaiveDate::from_ymd_opt(2026, 9, 20).unwrap()
            }
        );
        assert_eq!(events[0].location, "Room 4B");

        let requests = server.await.unwrap();
        for request in &requests {
            assert!(request.starts_with("GET "), "only reads: {request}");
            assert!(request.contains("authorization: Bearer at-1"));
        }
        assert!(requests[1].contains("singleEvents=true"));
        assert!(requests[1].contains("timeMin=2026-08-06"));
        assert!(requests[2].contains("pageToken=page-2"));
    }

    #[tokio::test]
    async fn reads_outlook_calendars_and_events_in_utc() {
        let calendars = r#"{"value":[
            {"id":"AAA","name":"Calendar","isDefaultCalendar":true,"owner":{"name":"Sam","address":"sam@outlook.com"}},
            {"id":"BBB","name":"Team"}
        ]}"#
        .to_string();
        let events = r#"{"value":[
            {"id":"e1","subject":"Standup","showAs":"busy","type":"occurrence",
             "start":{"dateTime":"2026-09-17T13:15:00.0000000","timeZone":"UTC"},
             "end":{"dateTime":"2026-09-17T13:30:00.0000000","timeZone":"UTC"},
             "location":{"displayName":"Teams"}},
            {"id":"e2","subject":"Cancelled thing","isCancelled":true,
             "start":{"dateTime":"2026-09-17T14:00:00.0000000"},"end":{"dateTime":"2026-09-17T15:00:00.0000000"}},
            {"id":"e3","subject":"Holiday","isAllDay":true,"showAs":"free","type":"singleInstance",
             "start":{"dateTime":"2026-09-19T00:00:00.0000000"},"end":{"dateTime":"2026-09-20T00:00:00.0000000"}},
            {"id":"e4","subject":"Maybe","showAs":"tentative",
             "start":{"dateTime":"2026-09-18T16:00:00.0000000"},"end":{"dateTime":"2026-09-18T17:00:00.0000000"}}
        ]}"#
        .to_string();
        let (base, server) = api(vec![("200 OK", calendars), ("200 OK", events)]).await;
        let api = ProviderApi {
            provider: CalendarProvider::Microsoft,
            base_url: base,
        };
        let client = Client::new();

        let listed = api.calendars(&client, "at-2").await.unwrap();
        assert_eq!(listed.label.as_deref(), Some("sam@outlook.com"));
        assert!(listed.calendars[0].primary);
        assert_eq!(listed.calendars[1].name, "Team");

        let events = api.events(&client, "at-2", "AAA", &window()).await.unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| (
                    event.title.as_str(),
                    event.busy,
                    event.tentative,
                    event.recurring
                ))
                .collect::<Vec<_>>(),
            [
                ("Standup", true, false, true),
                ("Holiday", false, false, false),
                ("Maybe", true, true, false),
            ]
        );
        assert_eq!(
            events[0].when,
            EventTime::Timed {
                start: "2026-09-17T13:15:00Z".parse().unwrap(),
                end: "2026-09-17T13:30:00Z".parse().unwrap()
            }
        );
        assert_eq!(
            events[1].when,
            EventTime::AllDay {
                start: NaiveDate::from_ymd_opt(2026, 9, 19).unwrap(),
                end: NaiveDate::from_ymd_opt(2026, 9, 20).unwrap()
            }
        );
        let requests = server.await.unwrap();
        for request in &requests {
            assert!(request.starts_with("GET "), "only reads: {request}");
        }
        assert!(requests[1].contains("prefer: outlook.timezone=\"UTC\""));
        assert!(requests[1].contains("calendarView"));
    }

    #[tokio::test]
    async fn a_revoked_sign_in_becomes_a_reconnect_prompt() {
        let (base, _server) = api(vec![
            (
                "401 Unauthorized",
                r#"{"error":{"code":"InvalidAuthenticationToken"}}"#.to_string(),
            ),
            ("429 Too Many Requests", "{}".to_string()),
        ])
        .await;
        let api = ProviderApi {
            provider: CalendarProvider::Microsoft,
            base_url: base,
        };
        let client = Client::new();
        assert!(matches!(
            api.calendars(&client, "stale").await,
            Err(AppError::Calendar(CalendarProblem::SignInExpired))
        ));
        assert!(matches!(
            api.calendars(&client, "stale").await,
            Err(AppError::Calendar(CalendarProblem::RateLimited))
        ));
    }
}
