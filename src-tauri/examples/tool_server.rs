//! Serves the DelveOps read tools over newline-delimited JSON on stdin/stdout, against a copy of
//! a planner database, so benchmarks and the Python agent can exercise the real Rust tools
//! without the desktop app. stdout carries protocol messages only; diagnostics go to stderr.
//!
//! ```text
//! cargo run --example tool_server -- [--bundle export.json | --db planner.sqlite3]
//!     [--profile planning-profile.json] [--memory memory-export.json]
//!     [--history decided-actions.json]
//!     [--now 2026-11-02T15:00:00Z] [--time-zone America/Chicago]
//!     [--allow-descriptions] [--github agent_bench/fixtures/github.v1.json]
//! cargo run --example tool_server -- --print-contract
//! ```
//!
//! With neither `--bundle` nor `--db`, the built-in sample fixture is served. `--db` copies the
//! database into a temporary folder first (a consistent SQLite snapshot, read-only on the source).
//! `--profile` sets the copy's planning profile (export bundles don't carry it), for
//! `get_preferences`: the profile's fields in camelCase, as Settings saves them, without
//! `revision`; fields left out stay unset.
//!
//! `--memory` replaces the copy's memory with a `delve.memory.v1` export, for `get_memory`.
//!
//! `--history` adds already-decided assistant changes (a JSON array in `get_recent_actions`'
//! `recentAction` shape) to the copy, for `get_recent_actions`.
//!
//! `--github` serves the GitHub tools from a saved GitHub context snapshot; without it they answer
//! `permission_denied`, as they do in the app until the user selects repositories.
//!
//! Every message is `{ "v": 1, "id": "…", "type": "…", "payload": { … } }`:
//! - `hello` → `hello_ack` with the protocol and contract versions, tool names, and limits.
//! - `run_start` `{ now?, timeZone?, permittedTools?, budget?, privacy? }` → `run_start_ack`
//!   with the settings in force. Starts a fresh budget and call log.
//! - `tool_call` `{ tool, arguments }` → `tool_result` (see `toolResult` in the contract).
//! - Anything malformed → `error` `{ code, message, retryable }`.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use delve_planner_desktop::db::{DecidedActionSeed, MemoryImport, PlannerDatabase};
use delve_planner_desktop::model::UpdatePlanningProfileInput;
use delve_planner_desktop::tools::github::GitHubContext;
use delve_planner_desktop::tools::registry::{contract, ToolName, CONTRACT_VERSION};
use delve_planner_desktop::tools::{
    open_fixture_database, NoCalendars, PrivacyPolicy, SessionConfig, ToolBudget, ToolSession,
    ToolSources, SAMPLE_FIXTURE,
};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

const PROTOCOL_VERSION: u64 = 1;
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

struct Options {
    source: Source,
    profile: Option<PathBuf>,
    memory: Option<PathBuf>,
    history: Option<PathBuf>,
    now: DateTime<Utc>,
    time_zone: Tz,
    privacy: PrivacyPolicy,
    github: Option<GitHubContext>,
}

enum Source {
    Sample,
    Bundle(PathBuf),
    Database(PathBuf),
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("tool_server: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let Some(options) = parse_options()? else {
        return Ok(());
    };
    let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
    let mut database = open_source(&options.source, &workspace.path().join("fixture.sqlite3"))?;
    if let Some(path) = &options.profile {
        set_profile(&mut database, path)?;
    }
    if let Some(path) = &options.memory {
        let text = fs::read_to_string(path)
            .map_err(|error| format!("can't read {}: {error}", path.display()))?;
        let import: MemoryImport = serde_json::from_str(&text)
            .map_err(|error| format!("{} isn't a memory export: {error}", path.display()))?;
        database.import_memory(&import).map_err(|e| e.to_string())?;
    }
    if let Some(path) = &options.history {
        let text = fs::read_to_string(path)
            .map_err(|error| format!("can't read {}: {error}", path.display()))?;
        let actions: Vec<DecidedActionSeed> = serde_json::from_str(&text).map_err(|error| {
            format!(
                "{} isn't a list of decided actions: {error}",
                path.display()
            )
        })?;
        database
            .seed_decided_actions(&actions)
            .map_err(|error| error.to_string())?;
    }
    eprintln!("tool_server: serving {CONTRACT_VERSION} from a fixture copy");
    let defaults = SessionConfig {
        privacy: options.privacy,
        github: options.github.map(Arc::new),
        ..SessionConfig::new(options.now, options.time_zone)
    };
    let mut session = ToolSession::new(defaults.clone());
    let sources = ToolSources {
        database: &database,
        calendars: &NoCalendars,
    };
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    loop {
        let line = match read_line(&mut input).map_err(|error| error.to_string())? {
            Line::End => return Ok(()),
            Line::TooLong => {
                send(
                    &mut output,
                    &Value::Null,
                    "error",
                    protocol_error("message_too_large", "Messages are limited to 1 MiB."),
                )?;
                continue;
            }
            Line::Text(text) if text.trim().is_empty() => continue,
            Line::Text(text) => text,
        };
        let (id, kind, payload) = match parse_envelope(&line) {
            Ok(parts) => parts,
            Err((id, error)) => {
                send(&mut output, &id, "error", error)?;
                continue;
            }
        };
        match kind.as_str() {
            "hello" => send(&mut output, &id, "hello_ack", hello_ack(session.config()))?,
            "run_start" => match run_config(&defaults, payload) {
                Ok(config) => {
                    session = ToolSession::new(config);
                    send(
                        &mut output,
                        &id,
                        "run_start_ack",
                        describe(session.config()),
                    )?;
                }
                Err(message) => send(
                    &mut output,
                    &id,
                    "error",
                    protocol_error("invalid_message", &message),
                )?,
            },
            "tool_call" => {
                let call: ToolCall = match serde_json::from_value(payload) {
                    Ok(call) => call,
                    Err(error) => {
                        send(
                            &mut output,
                            &id,
                            "error",
                            protocol_error("invalid_message", &format!("Bad tool_call: {error}")),
                        )?;
                        continue;
                    }
                };
                let outcome = session.call(&sources, &call.tool, call.arguments);
                let payload = serde_json::to_value(outcome).map_err(|error| error.to_string())?;
                send(&mut output, &id, "tool_result", payload)?;
            }
            // Calls run to completion before the next message is read, so there is nothing to
            // cancel.
            "cancel" => {}
            other => send(
                &mut output,
                &id,
                "error",
                protocol_error(
                    "unsupported_type",
                    &format!("The tool server doesn't handle '{other}' messages."),
                ),
            )?,
        }
    }
}

fn parse_options() -> Result<Option<Options>, String> {
    let mut options = Options {
        source: Source::Sample,
        profile: None,
        memory: None,
        history: None,
        now: Utc::now(),
        time_zone: Tz::UTC,
        privacy: PrivacyPolicy::default(),
        github: None,
    };
    let mut args = env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--bundle" => options.source = Source::Bundle(value()?.into()),
            "--db" => options.source = Source::Database(value()?.into()),
            "--profile" => options.profile = Some(value()?.into()),
            "--memory" => options.memory = Some(value()?.into()),
            "--history" => options.history = Some(value()?.into()),
            "--now" => options.now = parse_now(&value()?)?,
            "--time-zone" => options.time_zone = parse_zone(&value()?)?,
            "--allow-descriptions" => options.privacy.include_descriptions = true,
            "--github" => {
                let path = value()?;
                let text = fs::read_to_string(&path)
                    .map_err(|error| format!("can't read {path}: {error}"))?;
                let context = serde_json::from_str(&text)
                    .map_err(|error| format!("{path} is not a GitHub context: {error}"))?;
                options.github = Some(context);
            }
            "--print-contract" => {
                let text = serde_json::to_string_pretty(&contract()).map_err(|e| e.to_string())?;
                println!("{text}");
                return Ok(None);
            }
            "--help" | "-h" => {
                eprintln!(
                    "usage: tool_server [--bundle FILE | --db FILE] [--profile FILE] [--memory FILE] \
                     [--history FILE] [--now ISO] [--time-zone ZONE] [--allow-descriptions] \
                     [--github FILE] \
                     [--print-contract]"
                );
                return Ok(None);
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(Some(options))
}

/// Replaces the copy's planning profile with the one in `path`, checked like a Settings save.
fn set_profile(database: &mut PlannerDatabase, path: &std::path::Path) -> Result<(), String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("can't read {}: {error}", path.display()))?;
    let given: Value = serde_json::from_str(&text)
        .map_err(|error| format!("{} isn't JSON: {error}", path.display()))?;
    let Value::Object(given) = given else {
        return Err(format!("{} must hold a JSON object", path.display()));
    };
    let revision = database
        .planning_profile()
        .map_err(|e| e.to_string())?
        .revision;
    let mut input = json!({ "revision": revision, "noWorkDays": [], "mutedObservations": [] });
    for (key, value) in given {
        input[key] = value;
    }
    for key in [
        "preferredStartMinute",
        "preferredEndMinute",
        "maxPlannedMinutes",
        "focusMinutes",
        "breakMinutes",
        "energy",
    ] {
        if input.get(key).is_none() {
            input[key] = Value::Null;
        }
    }
    let input: UpdatePlanningProfileInput = serde_json::from_value(input)
        .map_err(|error| format!("{} isn't a planning profile: {error}", path.display()))?;
    database
        .update_planning_profile(input)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn open_source(source: &Source, target: &std::path::Path) -> Result<PlannerDatabase, String> {
    match source {
        Source::Sample => open_fixture_database(target, SAMPLE_FIXTURE).map_err(|e| e.to_string()),
        Source::Bundle(path) => {
            let text = fs::read_to_string(path)
                .map_err(|error| format!("can't read {}: {error}", path.display()))?;
            open_fixture_database(target, &text).map_err(|e| e.to_string())
        }
        Source::Database(path) => {
            // VACUUM INTO writes one consistent snapshot through SQLite itself, including changes
            // still in the write-ahead log, so a copy taken while the app runs never misses a
            // commit. The source is opened read-only.
            let source = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|error| format!("can't open {}: {error}", path.display()))?;
            let target_text = target
                .to_str()
                .ok_or_else(|| "the temporary path isn't valid UTF-8".to_string())?;
            source
                .execute("VACUUM INTO ?1", [target_text])
                .map_err(|error| format!("can't snapshot {}: {error}", path.display()))?;
            drop(source);
            PlannerDatabase::open(target).map_err(|e| e.to_string())
        }
    }
}

enum Line {
    Text(String),
    TooLong,
    End,
}

/// Reads one line of at most `MAX_MESSAGE_BYTES`, discarding the rest of a longer one.
fn read_line(input: &mut impl BufRead) -> io::Result<Line> {
    let mut buffer = Vec::new();
    let read = input
        .by_ref()
        .take(MAX_MESSAGE_BYTES as u64 + 1)
        .read_until(b'\n', &mut buffer)?;
    if read == 0 {
        return Ok(Line::End);
    }
    if buffer.len() > MAX_MESSAGE_BYTES && buffer.last() != Some(&b'\n') {
        discard_line(input)?;
        return Ok(Line::TooLong);
    }
    Ok(Line::Text(String::from_utf8_lossy(&buffer).into_owned()))
}

/// Skips the rest of a line without holding it in memory.
fn discard_line(input: &mut impl BufRead) -> io::Result<()> {
    loop {
        let chunk = input.fill_buf()?;
        if chunk.is_empty() {
            return Ok(());
        }
        match chunk.iter().position(|byte| *byte == b'\n') {
            Some(end) => {
                input.consume(end + 1);
                return Ok(());
            }
            None => {
                let length = chunk.len();
                input.consume(length);
            }
        }
    }
}

fn parse_envelope(line: &str) -> Result<(Value, String, Value), (Value, Value)> {
    let message: Value = serde_json::from_str(line).map_err(|_| {
        (
            Value::Null,
            protocol_error("invalid_message", "Each line must be one JSON object."),
        )
    })?;
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    if message.get("v").and_then(Value::as_u64) != Some(PROTOCOL_VERSION) {
        return Err((
            id,
            protocol_error(
                "unsupported_version",
                "This server speaks protocol version 1.",
            ),
        ));
    }
    let Some(kind) = message.get("type").and_then(Value::as_str) else {
        return Err((
            id,
            protocol_error("invalid_message", "A message needs a type."),
        ));
    };
    let payload = message.get("payload").cloned().unwrap_or(Value::Null);
    Ok((id, kind.to_string(), payload))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolCall {
    tool: String,
    #[serde(default)]
    arguments: Value,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RunStart {
    now: Option<String>,
    time_zone: Option<String>,
    permitted_tools: Option<Vec<String>>,
    budget: Option<PartialBudget>,
    privacy: Option<PrivacyPolicy>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PartialBudget {
    max_tool_calls: Option<u32>,
    max_result_bytes: Option<usize>,
    max_call_ms: Option<u64>,
    max_identical_failures: Option<u32>,
}

fn run_config(defaults: &SessionConfig, payload: Value) -> Result<SessionConfig, String> {
    let start: RunStart = if payload.is_null() {
        RunStart::default()
    } else {
        serde_json::from_value(payload).map_err(|error| format!("Bad run_start: {error}"))?
    };
    let mut config = defaults.clone();
    if let Some(now) = start.now {
        config.now = parse_now(&now)?;
    }
    if let Some(zone) = start.time_zone {
        config.time_zone = parse_zone(&zone)?;
    }
    config.permitted_tools = match start.permitted_tools {
        None => None,
        Some(names) => Some(
            names
                .iter()
                .map(|name| ToolName::parse(name).ok_or_else(|| format!("Unknown tool {name}.")))
                .collect::<Result<BTreeSet<_>, _>>()?,
        ),
    };
    if let Some(budget) = start.budget {
        let base = ToolBudget::default();
        config.budget = ToolBudget {
            max_tool_calls: budget.max_tool_calls.unwrap_or(base.max_tool_calls),
            max_result_bytes: budget.max_result_bytes.unwrap_or(base.max_result_bytes),
            max_call_ms: budget.max_call_ms.unwrap_or(base.max_call_ms),
            max_identical_failures: budget
                .max_identical_failures
                .unwrap_or(base.max_identical_failures),
        };
    }
    if let Some(privacy) = start.privacy {
        config.privacy = privacy;
    }
    Ok(config)
}

fn hello_ack(config: &SessionConfig) -> Value {
    json!({
        "protocol": PROTOCOL_VERSION,
        "contract": CONTRACT_VERSION,
        "tools": ToolName::ALL
            .into_iter()
            .filter(|tool| tool.implemented())
            .map(ToolName::as_str)
            .collect::<Vec<_>>(),
        "maxMessageBytes": MAX_MESSAGE_BYTES,
        "session": describe(config),
    })
}

/// The settings in force; the budget is shown after clamping to the ceiling.
fn describe(config: &SessionConfig) -> Value {
    json!({
        "now": config.now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "timeZone": config.time_zone.name(),
        "permittedTools": config.permitted_tools.as_ref().map(|tools| {
            tools.iter().map(|tool| tool.as_str()).collect::<Vec<_>>()
        }),
        "budget": config.budget.clamped(),
        "privacy": config.privacy,
    })
}

fn parse_now(value: &str) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(value)
        .map(|now| now.with_timezone(&Utc))
        .map_err(|_| format!("'{value}' is not an ISO-8601 time with an offset."))
}

fn parse_zone(value: &str) -> Result<Tz, String> {
    value
        .parse()
        .map_err(|_| format!("'{value}' is not an IANA time zone."))
}

fn protocol_error(code: &str, message: &str) -> Value {
    json!({ "code": code, "message": message, "retryable": false })
}

fn send(output: &mut impl Write, id: &Value, kind: &str, payload: Value) -> Result<(), String> {
    let message = json!({ "v": PROTOCOL_VERSION, "id": id, "type": kind, "payload": payload });
    writeln!(output, "{message}")
        .and_then(|()| output.flush())
        .map_err(|error| error.to_string())
}
