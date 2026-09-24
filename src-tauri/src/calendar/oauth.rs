//! Read-only OAuth for calendar accounts. Sign-in happens in the system browser with PKCE and a
//! loopback redirect; Delve Planner never sees the password and never asks for a scope that could
//! change a calendar. Tokens are handled here and stored in the system keychain.

use crate::error::{AppError, AppResult};
use crate::model::{CalendarProblem, CalendarProvider};
use chrono::{DateTime, Duration, Utc};
use reqwest::{Client, Url};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::time::Duration as StdDuration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use uuid::Uuid;

/// A build-time override, ignored when empty: GitHub passes an unset repository variable to the
/// build as an empty string, which would otherwise become a client ID no provider recognizes.
const fn overridden(value: Option<&'static str>, default: &'static str) -> &'static str {
    match value {
        Some(value) => {
            if value.is_empty() {
                default
            } else {
                value
            }
        }
        None => default,
    }
}

/// Public client identifiers: they appear in every sign-in URL, so they aren't secrets. A build
/// can point Delve Planner at different clients.
const GOOGLE_CLIENT_ID: &str = overridden(
    option_env!("DELVE_PLANNER_GOOGLE_CLIENT_ID"),
    "783077374407-nlg8h3hsa1b5kj2fhd3dmbj8nemk4aq3.apps.googleusercontent.com",
);
const MICROSOFT_CLIENT_ID: &str = overridden(
    option_env!("DELVE_PLANNER_MICROSOFT_CLIENT_ID"),
    "33ce6dba-0b7e-4fe2-8578-bd84dd1318f9",
);
/// Google lists the client secret as optional for installed apps, which can't keep one private, so
/// Delve Planner sends none by default. A build whose own client needs one passes it in; it is
/// never committed.
const GOOGLE_CLIENT_SECRET: Option<&str> = match option_env!("DELVE_PLANNER_GOOGLE_CLIENT_SECRET") {
    Some(secret) if !secret.is_empty() => Some(secret),
    _ => None,
};

/// The only scopes Delve Planner ever requests. Both are read-only; `offline_access` just keeps the
/// sign-in alive and grants no access of its own.
const GOOGLE_SCOPES: &str = "https://www.googleapis.com/auth/calendar.readonly";
const MICROSOFT_SCOPES: &str = "offline_access Calendars.Read";

/// How long the loopback page waits for the browser before giving up.
const SIGN_IN_TIMEOUT_SECONDS: u64 = 300;
const MAX_REQUEST_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub provider: CalendarProvider,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub authorize_url: String,
    pub token_url: String,
    pub revoke_url: Option<String>,
    pub scopes: String,
}

impl ProviderConfig {
    pub fn new(provider: CalendarProvider) -> Self {
        match provider {
            CalendarProvider::Google => Self {
                provider,
                client_id: GOOGLE_CLIENT_ID.into(),
                client_secret: GOOGLE_CLIENT_SECRET.map(Into::into),
                authorize_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
                token_url: "https://oauth2.googleapis.com/token".into(),
                revoke_url: Some("https://oauth2.googleapis.com/revoke".into()),
                scopes: GOOGLE_SCOPES.into(),
            },
            CalendarProvider::Microsoft => Self {
                provider,
                client_id: MICROSOFT_CLIENT_ID.into(),
                client_secret: None,
                authorize_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize"
                    .into(),
                token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token".into(),
                revoke_url: None,
                scopes: MICROSOFT_SCOPES.into(),
            },
        }
    }

    /// Where the browser sends the code back. Google takes any loopback port and recommends the IP
    /// literal, which a renamed interface can't divert. Microsoft only ignores the port when the
    /// registered reply URL is `localhost`, and its portal won't accept an `http://127.0.0.1` one,
    /// so Outlook has to use the name.
    pub fn redirect_uri(&self, port: u16) -> String {
        match self.provider {
            CalendarProvider::Google => format!("http://127.0.0.1:{port}"),
            CalendarProvider::Microsoft => format!("http://localhost:{port}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tokens {
    pub refresh_token: String,
    pub access_token: String,
    pub expires_at: DateTime<Utc>,
}

/// Runs the whole sign-in: a loopback listener, the browser, and the token exchange. `open` is
/// given the sign-in URL to hand to the system browser.
pub async fn sign_in(
    config: &ProviderConfig,
    client: &Client,
    open: &(dyn Fn(&str) -> AppResult<()> + Send + Sync),
) -> AppResult<Tokens> {
    // IPv4 loopback only, so nothing outside this machine can reach the port. A browser sending
    // Microsoft's `localhost` callback to ::1 first falls back to 127.0.0.1 when that is refused.
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| AppError::Internal("Delve Planner couldn't open a sign-in port.".into()))?;
    let port = listener
        .local_addr()
        .map_err(|_| AppError::Internal("Delve Planner couldn't open a sign-in port.".into()))?
        .port();
    let redirect_uri = config.redirect_uri(port);
    let verifier = random_secret();
    let state = random_secret();
    open(&sign_in_url(config, &redirect_uri, &verifier, &state))?;
    let code = tokio::time::timeout(
        StdDuration::from_secs(SIGN_IN_TIMEOUT_SECONDS),
        wait_for_code(listener, &state),
    )
    .await
    .map_err(|_| AppError::Validation("The sign-in timed out. Try connecting again.".into()))??;
    exchange(config, client, &redirect_uri, &verifier, &code).await
}

/// The browser URL that asks the provider for read-only calendar access.
pub fn sign_in_url(
    config: &ProviderConfig,
    redirect_uri: &str,
    verifier: &str,
    state: &str,
) -> String {
    let challenge = code_challenge(verifier);
    let mut params = vec![
        ("client_id", config.client_id.as_str()),
        ("redirect_uri", redirect_uri),
        ("response_type", "code"),
        ("scope", config.scopes.as_str()),
        ("state", state),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
    ];
    match config.provider {
        // Google returns a refresh token only when it is asked for consent for offline access.
        CalendarProvider::Google => {
            params.push(("access_type", "offline"));
            params.push(("prompt", "consent"));
        }
        CalendarProvider::Microsoft => {
            params.push(("response_mode", "query"));
            params.push(("prompt", "select_account"));
        }
    }
    Url::parse_with_params(&config.authorize_url, &params)
        .map(String::from)
        .unwrap_or_else(|_| config.authorize_url.clone())
}

/// Serves the loopback redirect until the browser delivers a code for `state`.
async fn wait_for_code(listener: TcpListener, state: &str) -> AppResult<String> {
    loop {
        let (mut socket, _) = listener
            .accept()
            .await
            .map_err(|_| AppError::Internal("The sign-in page could not be served.".into()))?;
        let mut buffer = vec![0; MAX_REQUEST_BYTES];
        let read = socket.read(&mut buffer).await.unwrap_or(0);
        let request = String::from_utf8_lossy(&buffer[..read]).to_string();
        let Some(target) = request
            .split_whitespace()
            .nth(1)
            .filter(|target| target.starts_with('/'))
        else {
            let _ = reply(&mut socket, "400 Bad Request", "Sign-in failed.").await;
            continue;
        };
        let Ok(url) = Url::parse(&format!("http://127.0.0.1{target}")) else {
            let _ = reply(&mut socket, "400 Bad Request", "Sign-in failed.").await;
            continue;
        };
        let value = |name: &str| {
            url.query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.to_string())
        };
        if let Some(error) = value("error") {
            let _ = reply(
                &mut socket,
                "200 OK",
                "Delve Planner didn't get access. You can close this window.",
            )
            .await;
            return Err(AppError::Validation(if error == "access_denied" {
                "The sign-in was declined, so nothing was connected.".into()
            } else {
                "The calendar service turned down the sign-in. Try again.".into()
            }));
        }
        let Some(code) = value("code") else {
            // The browser also asks for things like /favicon.ico; keep waiting for the callback.
            let _ = reply(&mut socket, "404 Not Found", "Waiting for sign-in.").await;
            continue;
        };
        if value("state").as_deref() != Some(state) {
            let _ = reply(&mut socket, "400 Bad Request", "Sign-in failed.").await;
            return Err(AppError::Validation(
                "That sign-in response didn't match this request. Try connecting again.".into(),
            ));
        }
        let _ = reply(
            &mut socket,
            "200 OK",
            "Delve Planner is connected. You can close this window.",
        )
        .await;
        return Ok(code);
    }
}

async fn reply(socket: &mut tokio::net::TcpStream, status: &str, message: &str) -> AppResult<()> {
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <title>Delve Planner</title></head><body style=\"font-family:system-ui;padding:3rem\">\
         <p>{message}</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    socket
        .write_all(response.as_bytes())
        .await
        .map_err(|_| AppError::Internal("The sign-in page could not be served.".into()))?;
    let _ = socket.shutdown().await;
    Ok(())
}

async fn exchange(
    config: &ProviderConfig,
    client: &Client,
    redirect_uri: &str,
    verifier: &str,
    code: &str,
) -> AppResult<Tokens> {
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", config.client_id.as_str()),
        ("code_verifier", verifier),
    ];
    if let Some(secret) = &config.client_secret {
        form.push(("client_secret", secret.as_str()));
    }
    let response = post_tokens(config, client, &form).await?;
    let refresh_token = response.refresh_token.ok_or_else(|| {
        AppError::Validation(
            "The calendar service didn't return a lasting sign-in. Try connecting again.".into(),
        )
    })?;
    Ok(Tokens {
        refresh_token,
        access_token: response.access_token,
        expires_at: expiry(response.expires_in),
    })
}

/// Trades a refresh token for a fresh access token, keeping whichever refresh token comes back.
pub async fn refresh(
    config: &ProviderConfig,
    client: &Client,
    refresh_token: &str,
) -> AppResult<Tokens> {
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", config.client_id.as_str()),
        ("scope", config.scopes.as_str()),
    ];
    if let Some(secret) = &config.client_secret {
        form.push(("client_secret", secret.as_str()));
    }
    let response = post_tokens(config, client, &form).await?;
    Ok(Tokens {
        refresh_token: response
            .refresh_token
            .unwrap_or_else(|| refresh_token.to_string()),
        access_token: response.access_token,
        expires_at: expiry(response.expires_in),
    })
}

/// Tells the provider to forget the sign-in where that is supported. Microsoft has no endpoint
/// for this, so the tokens are simply deleted.
pub async fn revoke(config: &ProviderConfig, client: &Client, refresh_token: &str) {
    let Some(url) = &config.revoke_url else {
        return;
    };
    let _ = client
        .post(url)
        .form(&[("token", refresh_token)])
        .send()
        .await;
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct TokenError {
    #[serde(default)]
    error: String,
}

async fn post_tokens(
    config: &ProviderConfig,
    client: &Client,
    form: &[(&str, &str)],
) -> AppResult<TokenResponse> {
    let response = client
        .post(&config.token_url)
        .form(form)
        .send()
        .await
        .map_err(|_| AppError::Calendar(CalendarProblem::Unreachable))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|_| AppError::Calendar(CalendarProblem::Unreachable))?;
    if status.is_success() {
        return serde_json::from_str(&body)
            .map_err(|_| AppError::Calendar(CalendarProblem::SignInExpired));
    }
    let error = serde_json::from_str::<TokenError>(&body)
        .map(|parsed| parsed.error)
        .unwrap_or_default();
    // A refused or withdrawn grant is the one case the user has to act on.
    if status.is_client_error() && matches!(error.as_str(), "invalid_grant" | "unauthorized_client")
    {
        return Err(AppError::Calendar(CalendarProblem::SignInExpired));
    }
    if status.is_server_error() {
        return Err(AppError::Calendar(CalendarProblem::ServerError));
    }
    tauri_plugin_log::log::info!(
        "oauth_token_rejected provider={} error={error}",
        config.provider.as_str()
    );
    Err(AppError::Validation(
        "The calendar service turned down the sign-in. Try connecting again.".into(),
    ))
}

fn expiry(expires_in: Option<i64>) -> DateTime<Utc> {
    Utc::now() + Duration::seconds(expires_in.unwrap_or(3600).clamp(60, 24 * 3600))
}

/// 256 bits of randomness as base64url, used for the PKCE verifier and the state value.
fn random_secret() -> String {
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(Uuid::new_v4().as_bytes());
    bytes.extend_from_slice(Uuid::new_v4().as_bytes());
    base64_url(&bytes)
}

fn code_challenge(verifier: &str) -> String {
    base64_url(&Sha256::digest(verifier.as_bytes()))
}

/// base64url without padding, as PKCE requires.
fn base64_url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let mut buffer = [0u8; 3];
        buffer[..chunk.len()].copy_from_slice(chunk);
        let value = u32::from(buffer[0]) << 16 | u32::from(buffer[1]) << 8 | u32::from(buffer[2]);
        for index in 0..chunk.len() + 1 {
            let shift = 18 - index * 6;
            encoded.push(ALPHABET[(value >> shift & 0b11_1111) as usize] as char);
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn config(token_url: &str) -> ProviderConfig {
        ProviderConfig {
            token_url: token_url.into(),
            ..ProviderConfig::new(CalendarProvider::Google)
        }
    }

    /// Serves one canned JSON response per connection and reports the form bodies it received.
    async fn token_server(
        responses: Vec<(&'static str, &'static str)>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let mut bodies = Vec::new();
            for (status, body) in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = vec![0; 8_192];
                let read = socket.read(&mut buffer).await.unwrap();
                bodies.push(String::from_utf8_lossy(&buffer[..read]).to_string());
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
                let _ = socket.shutdown().await;
            }
            bodies
        });
        (format!("http://{address}/token"), handle)
    }

    #[test]
    fn asks_only_for_read_only_scopes() {
        for provider in [CalendarProvider::Google, CalendarProvider::Microsoft] {
            let config = ProviderConfig::new(provider);
            assert!(
                !config.scopes.contains("write")
                    && !config.scopes.contains("ReadWrite")
                    && !config.scopes.contains("full"),
                "{} asks for {}",
                provider.as_str(),
                config.scopes
            );
        }
        assert_eq!(
            ProviderConfig::new(CalendarProvider::Google).scopes,
            "https://www.googleapis.com/auth/calendar.readonly"
        );
        assert_eq!(
            ProviderConfig::new(CalendarProvider::Microsoft).scopes,
            "offline_access Calendars.Read"
        );
    }

    #[test]
    fn an_empty_build_override_keeps_the_built_in_client() {
        assert_eq!(
            overridden(Some("custom-client"), "built-in"),
            "custom-client"
        );
        assert_eq!(overridden(Some(""), "built-in"), "built-in");
        assert_eq!(overridden(None, "built-in"), "built-in");
        for provider in [CalendarProvider::Google, CalendarProvider::Microsoft] {
            assert!(!ProviderConfig::new(provider).client_id.is_empty());
        }
    }

    #[test]
    fn sends_each_provider_back_to_the_loopback_it_accepts() {
        assert_eq!(
            ProviderConfig::new(CalendarProvider::Google).redirect_uri(51234),
            "http://127.0.0.1:51234"
        );
        // Entra matches this against a registered `http://localhost`, ignoring the port.
        assert_eq!(
            ProviderConfig::new(CalendarProvider::Microsoft).redirect_uri(51234),
            "http://localhost:51234"
        );
    }

    #[test]
    fn builds_a_pkce_sign_in_url() {
        let config = ProviderConfig::new(CalendarProvider::Google);
        // The verifier and challenge from RFC 7636's appendix B.
        let url = Url::parse(&sign_in_url(
            &config,
            "http://127.0.0.1:51234",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            "state-value",
        ))
        .unwrap();
        let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(
            params.get("code_challenge").map(String::as_str),
            Some("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM")
        );
        assert_eq!(
            params.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(
            params.get("redirect_uri").map(String::as_str),
            Some("http://127.0.0.1:51234")
        );
        assert_eq!(
            params.get("access_type").map(String::as_str),
            Some("offline")
        );
        assert_eq!(params.get("state").map(String::as_str), Some("state-value"));
        assert!(!params.get("client_id").unwrap().is_empty());
    }

    #[tokio::test]
    async fn signs_in_through_the_loopback_page_and_keeps_the_tokens() {
        let (token_url, server) = token_server(vec![(
            "200 OK",
            r#"{"access_token":"at-1","refresh_token":"rt-1","expires_in":3600,"scope":"https://www.googleapis.com/auth/calendar.readonly"}"#,
        )])
        .await;
        let config = config(&token_url);
        let visited: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));
        let seen = visited.clone();
        // Stands in for the system browser: follows the redirect back to Delve Planner with a code.
        let open = move |url: &str| -> AppResult<()> {
            let url = url.to_string();
            *seen.lock().unwrap() = Some(url.clone());
            tokio::spawn(async move {
                let parsed = Url::parse(&url).unwrap();
                let params: std::collections::HashMap<_, _> =
                    parsed.query_pairs().into_owned().collect();
                let redirect = params.get("redirect_uri").cloned().unwrap();
                let state = params.get("state").cloned().unwrap();
                let client = Client::new();
                let _ = client
                    .get(format!("{redirect}/?code=auth-code&state={state}"))
                    .send()
                    .await;
            });
            Ok(())
        };
        let tokens = sign_in(&config, &Client::new(), &open).await.unwrap();
        assert_eq!(
            (tokens.refresh_token.as_str(), tokens.access_token.as_str()),
            ("rt-1", "at-1")
        );
        assert!(tokens.expires_at > Utc::now());
        let body = server.await.unwrap().remove(0);
        assert!(body.contains("grant_type=authorization_code"));
        assert!(body.contains("code=auth-code"));
        assert!(body.contains("code_verifier="));
        assert!(visited
            .lock()
            .unwrap()
            .as_deref()
            .unwrap()
            .starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
    }

    #[tokio::test]
    async fn refuses_a_callback_that_doesnt_match_the_request() {
        let config = config("http://127.0.0.1:1/token");
        let open = |url: &str| -> AppResult<()> {
            let url = url.to_string();
            tokio::spawn(async move {
                let parsed = Url::parse(&url).unwrap();
                let redirect = parsed
                    .query_pairs()
                    .find(|(key, _)| key == "redirect_uri")
                    .map(|(_, value)| value.to_string())
                    .unwrap();
                let client = Client::new();
                // A stray request first, then a callback carrying someone else's state.
                let _ = client.get(format!("{redirect}/favicon.ico")).send().await;
                let _ = client
                    .get(format!("{redirect}/?code=auth-code&state=someone-else"))
                    .send()
                    .await;
            });
            Ok(())
        };
        assert!(matches!(
            sign_in(&config, &Client::new(), &open).await,
            Err(AppError::Validation(_))
        ));
    }

    #[tokio::test]
    async fn a_withdrawn_sign_in_asks_the_user_to_connect_again() {
        let (token_url, server) = token_server(vec![
            ("400 Bad Request", r#"{"error":"invalid_grant"}"#),
            ("200 OK", r#"{"access_token":"at-2","expires_in":3600}"#),
        ])
        .await;
        let config = config(&token_url);
        let client = Client::new();
        assert!(matches!(
            refresh(&config, &client, "rt-old").await,
            Err(AppError::Calendar(CalendarProblem::SignInExpired))
        ));
        let kept = refresh(&config, &client, "rt-old").await.unwrap();
        assert_eq!(
            (kept.refresh_token.as_str(), kept.access_token.as_str()),
            ("rt-old", "at-2"),
            "a provider that doesn't rotate the refresh token keeps the old one"
        );
        let bodies = server.await.unwrap();
        assert!(bodies[0].contains("grant_type=refresh_token"));
    }
}
