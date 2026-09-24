//! Calendar links: which ones Delve Planner accepts, and fetching an iCalendar feed within size,
//! time, and redirect limits. Errors never carry the link, because the link is the secret.

use super::ics::MAX_CALENDAR_BYTES;
use crate::error::{AppError, AppResult};
use crate::model::{CalendarProblem, MAX_CALENDAR_LINK_LENGTH};
use futures_util::StreamExt;
use reqwest::{header, redirect, Client, StatusCode, Url};
use std::time::Duration;

const MAX_REDIRECTS: usize = 5;
const MAX_VALIDATOR_LENGTH: usize = 256;

/// An https calendar link. `webcal://` and `webcals://` links are read over https.
#[derive(Clone, PartialEq, Eq)]
pub struct CalendarLink {
    url: Url,
}

impl std::fmt::Debug for CalendarLink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CalendarLink")
            .field("host", &self.label())
            .finish_non_exhaustive()
    }
}

impl CalendarLink {
    pub fn parse(input: &str) -> AppResult<Self> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(AppError::Validation("Paste the calendar's link.".into()));
        }
        if trimmed.chars().count() > MAX_CALENDAR_LINK_LENGTH {
            return Err(AppError::Validation(
                "That link is longer than Delve Planner can store.".into(),
            ));
        }
        let Some((scheme, rest)) = trimmed.split_once("://") else {
            return Err(unsupported_link());
        };
        let normalized = match scheme.to_ascii_lowercase().as_str() {
            "https" | "webcal" | "webcals" => format!("https://{rest}"),
            "http" => return Err(insecure_link()),
            _ => return Err(unsupported_link()),
        };
        let url = Url::parse(&normalized).map_err(|_| unsupported_link())?;
        if url.host_str().is_none_or(str::is_empty) {
            return Err(unsupported_link());
        }
        Ok(Self { url })
    }

    pub fn as_str(&self) -> &str {
        self.url.as_str()
    }

    /// The host shown in place of the link, such as calendar.google.com.
    pub fn label(&self) -> String {
        let host = self.url.host_str().unwrap_or_default().to_ascii_lowercase();
        host.strip_prefix("www.")
            .map(str::to_string)
            .unwrap_or(host)
    }
}

fn insecure_link() -> AppError {
    AppError::Validation(
        "Use the calendar's https link. Delve Planner doesn't fetch private calendar links over \
         unencrypted connections."
            .into(),
    )
}

fn unsupported_link() -> AppError {
    AppError::Validation(
        "That isn't a calendar link. Links start with https:// or webcal://.".into(),
    )
}

pub fn http_client(app_version: &str) -> AppResult<Client> {
    Client::builder()
        .user_agent(format!("DelvePlanner/{app_version}"))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(60))
        .redirect(redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error("too many redirects")
            } else if attempt.url().scheme() != "https" {
                attempt.error("redirected away from https")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|_| AppError::Internal("Delve Planner couldn't prepare calendar requests.".into()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched {
    /// The server confirmed the copy Delve Planner already has.
    Unchanged,
    Content {
        body: String,
        etag: Option<String>,
        last_modified: Option<String>,
    },
}

/// Fetches a feed, sending the validators from the last successful fetch so an unchanged
/// calendar costs one small response.
pub async fn fetch(
    client: &Client,
    url: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> Result<Fetched, CalendarProblem> {
    let mut request = client
        .get(url)
        .header(header::ACCEPT, "text/calendar, text/plain;q=0.5, */*;q=0.1");
    if let Some(etag) = etag {
        request = request.header(header::IF_NONE_MATCH, etag);
    }
    if let Some(last_modified) = last_modified {
        request = request.header(header::IF_MODIFIED_SINCE, last_modified);
    }
    let response = request
        .send()
        .await
        .map_err(|_| CalendarProblem::Unreachable)?;
    match response.status() {
        StatusCode::NOT_MODIFIED => return Ok(Fetched::Unchanged),
        status if status.is_success() => {}
        StatusCode::NOT_FOUND | StatusCode::GONE => return Err(CalendarProblem::LinkNotFound),
        StatusCode::TOO_MANY_REQUESTS => return Err(CalendarProblem::RateLimited),
        status if status.is_server_error() => return Err(CalendarProblem::ServerError),
        _ => return Err(CalendarProblem::LinkRefused),
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_CALENDAR_BYTES as u64)
    {
        return Err(CalendarProblem::TooLarge);
    }
    let validator = |name: header::HeaderName| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty() && value.len() <= MAX_VALIDATOR_LENGTH)
            .map(str::to_string)
    };
    let etag = validator(header::ETAG);
    let last_modified = validator(header::LAST_MODIFIED);
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| CalendarProblem::Unreachable)?;
        if body.len() + chunk.len() > MAX_CALENDAR_BYTES {
            return Err(CalendarProblem::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Fetched::Content {
        body: String::from_utf8_lossy(&body).into_owned(),
        etag,
        last_modified,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn accepts_https_and_webcal_links_only() {
        let link =
            CalendarLink::parse("  webcal://p42-caldav.icloud.com/published/2/MTIzNDU2Nzg5  ")
                .unwrap();
        assert_eq!(
            link.as_str(),
            "https://p42-caldav.icloud.com/published/2/MTIzNDU2Nzg5"
        );
        assert_eq!(link.label(), "p42-caldav.icloud.com");
        let google = CalendarLink::parse(
            "HTTPS://www.Calendar.Google.com/calendar/ical/me%40gmail.com/private-abc/basic.ics",
        )
        .unwrap();
        assert_eq!(google.label(), "calendar.google.com");
        assert!(
            !format!("{google:?}").contains("private-abc"),
            "debug output hides the secret path"
        );
        for rejected in [
            "",
            "http://calendar.example/feed.ics",
            "ftp://calendar.example/feed.ics",
            "calendar.google.com/calendar/ical/x/basic.ics",
            "https://",
            "javascript:alert(1)",
        ] {
            assert!(
                matches!(CalendarLink::parse(rejected), Err(AppError::Validation(_))),
                "{rejected} should be rejected"
            );
        }
        let long = format!("https://calendar.example/{}", "a".repeat(1_200));
        assert!(CalendarLink::parse(&long).is_err());
    }

    /// Serves one canned HTTP response per connection and returns the requests it saw.
    async fn serve(responses: Vec<String>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = vec![0; 8_192];
                let read = socket.read(&mut buffer).await.unwrap();
                requests.push(String::from_utf8_lossy(&buffer[..read]).to_string());
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            }
            requests
        });
        (format!("http://{address}/calendar.ics"), handle)
    }

    fn response(status: &str, headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
            body.len()
        )
    }

    #[tokio::test]
    async fn fetches_with_validators_and_maps_failures() {
        let body = "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n";
        let (url, server) = serve(vec![
            response("200 OK", "ETag: \"v1\"\r\n", body),
            response("304 Not Modified", "", ""),
            response("404 Not Found", "", "gone"),
            response("403 Forbidden", "", ""),
            response("503 Service Unavailable", "", ""),
            response("429 Too Many Requests", "", ""),
        ])
        .await;
        let client = http_client("0.3.0").unwrap();
        assert_eq!(
            fetch(&client, &url, None, None).await,
            Ok(Fetched::Content {
                body: body.into(),
                etag: Some("\"v1\"".into()),
                last_modified: None
            })
        );
        assert_eq!(
            fetch(&client, &url, Some("\"v1\""), None).await,
            Ok(Fetched::Unchanged)
        );
        for expected in [
            CalendarProblem::LinkNotFound,
            CalendarProblem::LinkRefused,
            CalendarProblem::ServerError,
            CalendarProblem::RateLimited,
        ] {
            assert_eq!(fetch(&client, &url, None, None).await, Err(expected));
        }
        let requests = server.await.unwrap();
        assert!(requests[1]
            .to_ascii_lowercase()
            .contains("if-none-match: \"v1\""));
        assert!(requests[0].contains("DelvePlanner/0.3.0"));
    }

    #[tokio::test]
    async fn decodes_calendars_served_with_gzip() {
        use flate2::{write::GzEncoder, Compression};
        use std::io::Write;
        let body = "BEGIN:VCALENDAR\r\nX-WR-CALNAME:Holidays\r\nEND:VCALENDAR\r\n";
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(body.as_bytes()).unwrap();
        let compressed = encoder.finish().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0; 8_192];
            let _ = socket.read(&mut buffer).await.unwrap();
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                compressed.len()
            );
            socket.write_all(head.as_bytes()).await.unwrap();
            socket.write_all(&compressed).await.unwrap();
            socket.shutdown().await.unwrap();
        });
        let fetched = fetch(
            &http_client("0.3.0").unwrap(),
            &format!("http://{address}/holidays.ics"),
            None,
            None,
        )
        .await;
        server.await.unwrap();
        assert!(
            matches!(fetched, Ok(Fetched::Content { body: ref decoded, .. }) if decoded == body),
            "iCloud serves gzip even when the request doesn't ask for it"
        );
    }

    #[tokio::test]
    async fn refuses_oversized_calendars_and_unreachable_hosts() {
        let (url, _server) = serve(vec![format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            MAX_CALENDAR_BYTES + 1
        )])
        .await;
        let client = http_client("0.3.0").unwrap();
        assert_eq!(
            fetch(&client, &url, None, None).await,
            Err(CalendarProblem::TooLarge)
        );
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = closed.local_addr().unwrap();
        drop(closed);
        assert_eq!(
            fetch(&client, &format!("http://{address}/x.ics"), None, None).await,
            Err(CalendarProblem::Unreachable)
        );
    }
}
