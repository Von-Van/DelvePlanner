//! Runs the Python agent as a child process and keeps it on a short leash: started on demand,
//! handshaken before use, one run at a time, every call timed, cancellable, killed when it breaks
//! the protocol, and restarted on the next request after a crash, unless it keeps crashing.

use super::protocol::{
    decode_from_agent, encode, read_bounded_line, result_payload, Envelope, ErrorCode, LineRead,
    LinkError, MessageType, MAX_MESSAGE_BYTES, PROTOCOL_VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::ffi::OsString;
use std::future::Future;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, Mutex};
use tokio::time::{sleep_until, timeout, Instant};
use tokio_util::sync::CancellationToken;

/// How many lines of the child's stderr are kept for diagnostics.
const STDERR_LINES: usize = 64;
/// Longest stderr line kept; the rest is cut.
const STDERR_LINE_BYTES: usize = 4 * 1024;
/// How long a write to the child may block before the child is treated as stuck.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a closed or killed child gets to exit before it is left to `kill_on_drop`.
const EXIT_WAIT: Duration = Duration::from_secs(2);
/// The statuses a `run_result` may report (shared contract §4).
const RUN_STATUSES: [&str; 4] = [
    "answered",
    "drafted",
    "needs_clarification",
    "cannot_complete",
];

/// The program that is the agent, and nothing else from the app's environment.
#[derive(Debug, Clone)]
pub struct SidecarCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    /// Added to the minimal environment the child gets. The app's own environment, which can hold
    /// tokens and proxy settings, is never passed through.
    pub env: Vec<(OsString, OsString)>,
}

impl SidecarCommand {
    /// A development setup: a Python interpreter running the agent package from source.
    /// `agent_dir` is the `agent-python` folder and `runner` a `module:function` the agent package
    /// can import, or `diagnostic` for the protocol's own test runner.
    pub fn python_source(interpreter: PathBuf, agent_dir: PathBuf, runner: &str) -> Self {
        Self {
            program: interpreter,
            args: vec![
                // No user site-packages: only what the interpreter and the agent folder provide.
                "-s".into(),
                "-m".into(),
                "delveops_agent.protocol".into(),
                "--runner".into(),
                runner.into(),
            ],
            // `-m` puts the working folder first on the import path, which is what finds the
            // package without a PYTHONPATH.
            cwd: Some(agent_dir),
            env: Vec::new(),
        }
    }

    /// A packaged agent: one frozen executable with the protocol entry point built in.
    pub fn frozen(executable: PathBuf, runner: &str) -> Self {
        Self {
            program: executable,
            args: vec!["--runner".into(), runner.into()],
            cwd: None,
            env: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SidecarLimits {
    pub handshake_timeout: Duration,
    pub tool_timeout: Duration,
    pub model_timeout: Duration,
    /// The longest any run may take, whatever its budget asks for.
    pub run_timeout: Duration,
    /// How long a cancelled run gets to report back before the child is killed.
    pub cancel_grace: Duration,
    /// Crashes allowed within `restart_window` before the app stops restarting the child.
    pub max_restarts: usize,
    pub restart_window: Duration,
}

impl Default for SidecarLimits {
    fn default() -> Self {
        Self {
            // A cold Python start on Windows with antivirus scanning every file can take seconds.
            handshake_timeout: Duration::from_secs(15),
            tool_timeout: Duration::from_secs(15),
            // Matches the planner's own model request timeout: a cold load plus a long reply.
            model_timeout: Duration::from_secs(180),
            run_timeout: Duration::from_secs(600),
            cancel_grace: Duration::from_secs(3),
            max_restarts: 3,
            restart_window: Duration::from_secs(300),
        }
    }
}

/// What a run may spend (shared contract §4). The app enforces the call counts itself as well as
/// asking the agent to.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunBudget {
    pub max_model_calls: u32,
    pub max_tool_calls: u32,
    pub max_wall_seconds: u64,
    pub max_context_tokens: u32,
}

impl Default for RunBudget {
    fn default() -> Self {
        Self {
            max_model_calls: 8,
            max_tool_calls: 12,
            max_wall_seconds: 120,
            max_context_tokens: 8_192,
        }
    }
}

/// One task for the agent (shared contract §4 `AgentTask`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRequest {
    pub goal: String,
    /// The frozen "now" for this run, ISO 8601 with offset.
    pub now: String,
    pub time_zone: String,
    pub permitted_tools: Vec<String>,
    /// Which agent architecture to use when the runner offers more than one, e.g. `A1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    pub budget: RunBudget,
}

#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub run_id: String,
    /// The agent's `run_result` payload, status checked.
    pub result: Value,
    pub tool_calls: u32,
    pub model_calls: u32,
    pub duration: Duration,
    /// Set when the run ended because it was cancelled or ran out of time, even though the agent
    /// still reported back in time.
    pub stopped: Option<StopReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    Cancelled,
    TimedOut,
}

/// What the agent's identity looked like at the handshake.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentInfo {
    pub protocol_version: u32,
    pub agent: Value,
    pub runtime: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum SidecarError {
    #[error("The AI helper could not be started: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("The AI helper did not finish starting up: {0}")]
    Handshake(String),
    #[error("The AI helper stopped unexpectedly.")]
    Crashed { diagnostics: Vec<String> },
    #[error("The AI helper keeps stopping, so DelvePlanner has paused it for a few minutes.")]
    CrashLoop,
    #[error("The AI helper sent something the app could not accept, so it was stopped: {0}")]
    Protocol(String),
    #[error("The AI helper took too long and was stopped.")]
    TimedOut,
    #[error("The request was cancelled.")]
    Cancelled,
    #[error("The AI helper reported a problem: {}", .0.message)]
    Agent(LinkError),
}

impl From<SidecarError> for crate::error::AppError {
    fn from(error: SidecarError) -> Self {
        match error {
            SidecarError::Cancelled => Self::RequestCancelled,
            other => Self::Internal(other.to_string()),
        }
    }
}

/// What the app does for the agent during a run. The supervisor bounds every call with a timeout
/// and the run's budget before it gets here, and answers cancelled calls itself.
pub trait SidecarHost: Sync {
    /// Runs one read tool. `Ok` is the whole `tool_result` payload as the tool broker builds it
    /// (`callId`, `tool`, `ok`, `result` or `error`, `durationMs`, ...), tool errors included;
    /// `Err` is for a broker that couldn't be reached at all.
    fn call_tool(
        &self,
        run_id: &str,
        name: &str,
        arguments: &Value,
    ) -> impl Future<Output = Result<Value, LinkError>> + Send;

    fn call_model(
        &self,
        run_id: &str,
        request: &Value,
    ) -> impl Future<Output = Result<Value, LinkError>> + Send;

    fn trace(&self, run_id: &str, event: &Value);
}

enum Incoming {
    Message(Envelope),
    Violation(String),
}

/// One live child and the pipes to it.
struct Link {
    child: Child,
    stdin: ChildStdin,
    incoming: mpsc::Receiver<Incoming>,
    info: AgentInfo,
    next_id: u64,
}

impl Link {
    fn next_id(&mut self) -> String {
        self.next_id += 1;
        format!("h-{}", self.next_id)
    }

    async fn send(&mut self, envelope: &Envelope) -> Result<(), SidecarError> {
        let bytes = encode(envelope).map_err(|error| SidecarError::Protocol(error.to_string()))?;
        match timeout(WRITE_TIMEOUT, async {
            self.stdin.write_all(&bytes).await?;
            self.stdin.flush().await
        })
        .await
        {
            Ok(Ok(())) => Ok(()),
            // A child that stopped reading is either gone or stuck; both end the link.
            Ok(Err(_)) | Err(_) => Err(SidecarError::Crashed {
                diagnostics: Vec::new(),
            }),
        }
    }

    /// Sends a call's answer. An answer too big for the wire becomes an error the agent can read.
    async fn answer(
        &mut self,
        id: &str,
        kind: MessageType,
        payload: Value,
    ) -> Result<(), SidecarError> {
        let envelope = Envelope::new(id, kind, payload);
        if encode(&envelope).is_err() {
            let too_big = LinkError::new(
                ErrorCode::Internal,
                format!("the result was larger than the {MAX_MESSAGE_BYTES}-byte message limit"),
                false,
            );
            let mut payload = result_payload(&Err(too_big));
            for key in ["callId", "tool"] {
                if let Some(value) = envelope.payload.get(key) {
                    payload[key] = value.clone();
                }
            }
            return self.send(&Envelope::new(id, kind, payload)).await;
        }
        self.send(&envelope).await
    }

    async fn kill(mut self) {
        let _ = self.child.start_kill();
        let _ = timeout(EXIT_WAIT, self.child.wait()).await;
    }

    /// Closes the child's stdin, which tells it to exit, and kills it if it doesn't.
    async fn close(self) {
        let Link {
            mut child, stdin, ..
        } = self;
        drop(stdin);
        if timeout(EXIT_WAIT, child.wait()).await.is_err() {
            let _ = child.start_kill();
            let _ = timeout(EXIT_WAIT, child.wait()).await;
        }
    }
}

struct Inner {
    link: Option<Link>,
    crashes: VecDeque<Instant>,
    last_used: Option<Instant>,
}

/// The one Python agent process the app may have. Cloning shares it.
#[derive(Clone)]
pub struct Sidecar {
    command: Arc<SidecarCommand>,
    limits: Arc<SidecarLimits>,
    inner: Arc<Mutex<Inner>>,
    stderr: Arc<StdMutex<VecDeque<String>>>,
}

impl Sidecar {
    pub fn new(command: SidecarCommand, limits: SidecarLimits) -> Self {
        Self {
            command: Arc::new(command),
            limits: Arc::new(limits),
            inner: Arc::new(Mutex::new(Inner {
                link: None,
                crashes: VecDeque::new(),
                last_used: None,
            })),
            stderr: Arc::new(StdMutex::new(VecDeque::new())),
        }
    }

    /// The child's most recent stderr lines, oldest first. Diagnostics only; never shown as is.
    pub fn diagnostics(&self) -> Vec<String> {
        self.stderr
            .lock()
            .map(|lines| lines.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Starts the child and completes the handshake if it isn't already running, and says what it
    /// is. Useful to warm up before the first run.
    pub async fn ensure_started(&self) -> Result<AgentInfo, SidecarError> {
        let mut inner = self.inner.lock().await;
        let link = self.ensure_link(&mut inner).await?;
        Ok(link.info.clone())
    }

    /// Whether a child is running right now. Starts nothing.
    pub async fn running(&self) -> bool {
        let mut inner = self.inner.lock().await;
        match inner.link.as_mut() {
            Some(link) => matches!(link.child.try_wait(), Ok(None)),
            None => false,
        }
    }

    /// The running child's process ID, for diagnostics. Starts nothing.
    pub async fn process_id(&self) -> Option<u32> {
        self.inner.lock().await.link.as_ref()?.child.id()
    }

    /// Stops the child. The next run starts a fresh one.
    pub async fn shutdown(&self) {
        let link = self.inner.lock().await.link.take();
        if let Some(link) = link {
            link.close().await;
        }
    }

    /// Stops the child if nothing has used it for `idle`. Never waits on a run in progress.
    pub async fn stop_if_idle(&self, idle: Duration) -> bool {
        let Ok(mut inner) = self.inner.try_lock() else {
            return false;
        };
        let stale = inner.last_used.is_some_and(|used| used.elapsed() >= idle);
        if !stale {
            return false;
        }
        match inner.link.take() {
            Some(link) => {
                drop(inner);
                link.close().await;
                true
            }
            None => false,
        }
    }

    /// Runs one task to its end. Only one run happens at a time; a second caller waits.
    pub async fn run<H: SidecarHost>(
        &self,
        request: &RunRequest,
        host: &H,
        cancel: &CancellationToken,
    ) -> Result<RunOutcome, SidecarError> {
        let mut inner = self.inner.lock().await;
        if cancel.is_cancelled() {
            return Err(SidecarError::Cancelled);
        }
        self.ensure_link(&mut inner).await?;
        inner.last_used = Some(Instant::now());
        let outcome = {
            let link = inner.link.as_mut().expect("link was just ensured");
            self.drive(link, request, host, cancel).await
        };
        inner.last_used = Some(Instant::now());
        match outcome {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                // A run that ended any way but a reported result leaves the child in an unknown
                // state, so it goes. Only a crash counts towards the restart limit.
                if let Some(link) = inner.link.take() {
                    link.kill().await;
                }
                match error {
                    SidecarError::Crashed { .. } | SidecarError::Protocol(_) => {
                        inner.crashes.push_back(Instant::now());
                        Err(match error {
                            SidecarError::Crashed { .. } => SidecarError::Crashed {
                                diagnostics: self.diagnostics(),
                            },
                            other => other,
                        })
                    }
                    other => Err(other),
                }
            }
        }
    }

    async fn ensure_link<'a>(&self, inner: &'a mut Inner) -> Result<&'a mut Link, SidecarError> {
        if let Some(link) = inner.link.as_mut() {
            if matches!(link.child.try_wait(), Ok(None)) {
                return Ok(inner.link.as_mut().expect("link checked above"));
            }
            // It died between runs.
            inner.link = None;
            inner.crashes.push_back(Instant::now());
        }
        let window = self.limits.restart_window;
        while inner
            .crashes
            .front()
            .is_some_and(|crash| crash.elapsed() > window)
        {
            inner.crashes.pop_front();
        }
        if inner.crashes.len() >= self.limits.max_restarts {
            return Err(SidecarError::CrashLoop);
        }
        match self.spawn().await {
            Ok(link) => Ok(inner.link.insert(link)),
            Err(error) => {
                if matches!(error, SidecarError::Handshake(_)) {
                    inner.crashes.push_back(Instant::now());
                }
                Err(error)
            }
        }
    }

    async fn spawn(&self) -> Result<Link, SidecarError> {
        let mut command = Command::new(&self.command.program);
        command
            .args(&self.command.args)
            .env_clear()
            .envs(minimal_environment())
            .envs(self.command.env.iter().cloned())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = &self.command.cwd {
            command.current_dir(cwd);
        }
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().map_err(SidecarError::Spawn)?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");

        let (sender, incoming) = mpsc::channel(64);
        tokio::spawn(read_messages(stdout, sender));
        tokio::spawn(read_diagnostics(stderr, self.stderr.clone()));

        let mut link = Link {
            child,
            stdin,
            incoming,
            info: AgentInfo::default(),
            next_id: 0,
        };
        match self.handshake(&mut link).await {
            Ok(info) => {
                tauri_plugin_log::log::info!("agent_sidecar_started");
                link.info = info;
                Ok(link)
            }
            Err(error) => {
                link.kill().await;
                Err(error)
            }
        }
    }

    async fn handshake(&self, link: &mut Link) -> Result<AgentInfo, SidecarError> {
        let id = link.next_id();
        let limits = &self.limits;
        let hello = Envelope::new(
            id.clone(),
            MessageType::Hello,
            json!({
                "protocolVersions": [PROTOCOL_VERSION],
                "maxMessageBytes": MAX_MESSAGE_BYTES,
                "limits": {
                    "toolTimeoutMs": millis(limits.tool_timeout),
                    "modelTimeoutMs": millis(limits.model_timeout),
                    "runTimeoutMs": millis(limits.run_timeout),
                    "cancelGraceMs": millis(limits.cancel_grace),
                },
                "host": { "app": "delve-planner-desktop", "version": env!("CARGO_PKG_VERSION") },
            }),
        );
        link.send(&hello)
            .await
            .map_err(|_| SidecarError::Handshake("the helper closed its input".into()))?;
        let reply = timeout(limits.handshake_timeout, link.incoming.recv())
            .await
            .map_err(|_| SidecarError::Handshake("no reply in time".into()))?;
        let envelope = match reply {
            Some(Incoming::Message(envelope)) => envelope,
            Some(Incoming::Violation(problem)) => return Err(SidecarError::Handshake(problem)),
            None => {
                return Err(SidecarError::Handshake(format!(
                    "the helper exited during startup{}",
                    self.diagnostics()
                        .last()
                        .map(|line| format!(": {line}"))
                        .unwrap_or_default()
                )))
            }
        };
        if envelope.kind == MessageType::Error {
            return Err(SidecarError::Handshake(
                envelope
                    .str_field("message")
                    .unwrap_or("the helper refused the handshake")
                    .to_string(),
            ));
        }
        if envelope.kind != MessageType::HelloAck || envelope.id != id {
            return Err(SidecarError::Handshake(
                "the first reply was not a handshake".into(),
            ));
        }
        let info: AgentInfo = serde_json::from_value(envelope.payload)
            .map_err(|error| SidecarError::Handshake(error.to_string()))?;
        if info.protocol_version != PROTOCOL_VERSION {
            return Err(SidecarError::Handshake(format!(
                "the helper chose protocol version {}",
                info.protocol_version
            )));
        }
        Ok(info)
    }

    async fn drive<H: SidecarHost>(
        &self,
        link: &mut Link,
        request: &RunRequest,
        host: &H,
        cancel: &CancellationToken,
    ) -> Result<RunOutcome, SidecarError> {
        let started = Instant::now();
        let run_id = format!("run-{}", uuid::Uuid::new_v4());
        let wall =
            Duration::from_secs(request.budget.max_wall_seconds).min(self.limits.run_timeout);
        let deadline = started + wall;
        link.send(&Envelope::new(
            run_id.clone(),
            MessageType::RunStart,
            json!({ "task": request, "budget": request.budget }),
        ))
        .await?;

        let mut run = RunState {
            run_id,
            stopping: None,
            acknowledged: false,
            tool_calls: 0,
            model_calls: 0,
            refusals: 0,
        };
        loop {
            let stop_at = run.stopping.map_or(deadline, |(_, at)| at);
            let incoming = tokio::select! {
                biased;
                () = cancel.cancelled(), if run.stopping.is_none() => {
                    self.stop(link, &mut run, StopReason::Cancelled).await?;
                    continue;
                }
                () = sleep_until(stop_at) => {
                    match run.stopping {
                        None => {
                            self.stop(link, &mut run, StopReason::TimedOut).await?;
                            continue;
                        }
                        Some((StopReason::Cancelled, _)) => return Err(SidecarError::Cancelled),
                        Some((StopReason::TimedOut, _)) => return Err(SidecarError::TimedOut),
                    }
                }
                incoming = link.incoming.recv() => incoming,
            };
            let envelope = match incoming {
                Some(Incoming::Message(envelope)) => envelope,
                Some(Incoming::Violation(problem)) => return Err(SidecarError::Protocol(problem)),
                None => {
                    return Err(SidecarError::Crashed {
                        diagnostics: Vec::new(),
                    })
                }
            };
            match envelope.kind {
                MessageType::ToolCall | MessageType::ModelCall => {
                    self.serve_call(link, &mut run, request, host, cancel, envelope)
                        .await?;
                }
                MessageType::TraceEvent => {
                    run.check_run_id(&envelope)?;
                    host.trace(&run.run_id, &envelope.payload);
                }
                MessageType::RunResult => {
                    if envelope.id != run.run_id {
                        return Err(SidecarError::Protocol(
                            "a run result named another run".into(),
                        ));
                    }
                    let status = envelope.str_field("status").unwrap_or_default();
                    if !RUN_STATUSES.contains(&status) {
                        return Err(SidecarError::Protocol(format!(
                            "unknown run status `{status}`"
                        )));
                    }
                    return Ok(RunOutcome {
                        run_id: run.run_id,
                        result: envelope.payload,
                        tool_calls: run.tool_calls,
                        model_calls: run.model_calls,
                        duration: started.elapsed(),
                        stopped: run.stopping.map(|(reason, _)| reason),
                    });
                }
                MessageType::Error => {
                    let error: LinkError = serde_json::from_value(envelope.payload.clone())
                        .map_err(|_| {
                            SidecarError::Protocol("an error message had no code".into())
                        })?;
                    if envelope.str_field("runId") == Some(run.run_id.as_str()) {
                        return Err(SidecarError::Agent(error));
                    }
                    // An error about nothing in particular is a diagnostic, not the end of the run.
                    tauri_plugin_log::log::warn!("agent_sidecar_error_reported");
                }
                MessageType::RunStartAck => {
                    if envelope.id != run.run_id || run.acknowledged {
                        return Err(SidecarError::Protocol("an unexpected run_start_ack".into()));
                    }
                    run.acknowledged = true;
                }
                MessageType::HelloAck => {
                    return Err(SidecarError::Protocol("a second handshake reply".into()))
                }
                // `decode_from_agent` already refused the app's own message types.
                _ => return Err(SidecarError::Protocol("unexpected message type".into())),
            }
        }
    }

    /// Asks the agent to stop and starts the grace period it has to report back.
    async fn stop(
        &self,
        link: &mut Link,
        run: &mut RunState,
        reason: StopReason,
    ) -> Result<(), SidecarError> {
        run.stopping = Some((reason, Instant::now() + self.limits.cancel_grace));
        let id = link.next_id();
        link.send(&Envelope::new(
            id,
            MessageType::Cancel,
            json!({
                "runId": run.run_id,
                "reason": match reason {
                    StopReason::Cancelled => "cancelled",
                    StopReason::TimedOut => "timeout",
                },
            }),
        ))
        .await
    }

    async fn serve_call<H: SidecarHost>(
        &self,
        link: &mut Link,
        run: &mut RunState,
        request: &RunRequest,
        host: &H,
        cancel: &CancellationToken,
        envelope: Envelope,
    ) -> Result<(), SidecarError> {
        let is_tool = envelope.kind == MessageType::ToolCall;
        if !is_tool {
            // Tool calls carry only `tool` and `arguments`, the same shape the stand-alone tool
            // server takes; there is one run at a time, so the run is implied.
            run.check_run_id(&envelope)?;
        }
        let answer_kind = if is_tool {
            MessageType::ToolResult
        } else {
            MessageType::ModelResult
        };
        let tool = envelope.str_field("tool").unwrap_or_default().to_string();
        let reply = |result: Result<Value, LinkError>, run: &mut RunState| {
            if is_tool {
                match result {
                    Ok(payload) => payload,
                    Err(error) => {
                        run.refusals += 1;
                        json!({
                            "callId": format!("link-{}", run.refusals),
                            "tool": tool,
                            "ok": false,
                            "error": error,
                            "durationMs": 0,
                        })
                    }
                }
            } else {
                result_payload(&result)
            }
        };

        let refusal = if run.stopping.is_some() {
            Some(LinkError::new(
                ErrorCode::Cancelled,
                "the run is stopping",
                false,
            ))
        } else if is_tool {
            if !request.permitted_tools.contains(&tool) {
                // Refused calls don't spend the budget, matching the tool broker.
                Some(LinkError::new(
                    ErrorCode::PermissionDenied,
                    format!("`{tool}` is not a permitted tool for this run"),
                    false,
                ))
            } else if run.tool_calls >= request.budget.max_tool_calls {
                Some(LinkError::new(
                    ErrorCode::BudgetExhausted,
                    "this run has used all of its tool calls",
                    false,
                ))
            } else {
                run.tool_calls += 1;
                None
            }
        } else if run.model_calls >= request.budget.max_model_calls {
            Some(LinkError::new(
                ErrorCode::BudgetExhausted,
                "this run has used all of its model calls",
                false,
            ))
        } else {
            run.model_calls += 1;
            None
        };
        if let Some(refusal) = refusal {
            let payload = reply(Err(refusal), run);
            return link.answer(&envelope.id, answer_kind, payload).await;
        }

        let empty = Value::Object(Default::default());
        let (limit, call) = if is_tool {
            let arguments = envelope.payload.get("arguments").unwrap_or(&empty);
            (
                self.limits.tool_timeout,
                CallFuture::Tool(host.call_tool(&run.run_id, &tool, arguments)),
            )
        } else {
            (
                self.limits.model_timeout,
                CallFuture::Model(host.call_model(&run.run_id, &envelope.payload)),
            )
        };
        let result = tokio::select! {
            biased;
            () = cancel.cancelled() => {
                let cancelled = LinkError::new(ErrorCode::Cancelled, "the run was cancelled", false);
                let payload = reply(Err(cancelled), run);
                link.answer(&envelope.id, answer_kind, payload).await?;
                return self.stop(link, run, StopReason::Cancelled).await;
            }
            result = timeout(limit, call.into_future()) => result.unwrap_or_else(|_| {
                Err(LinkError::new(
                    ErrorCode::Timeout,
                    format!("the call took longer than {} seconds", limit.as_secs()),
                    true,
                ))
            }),
        };
        let result = match result {
            Ok(payload) if is_tool && !payload.get("ok").is_some_and(Value::is_boolean) => {
                Err(LinkError::new(
                    ErrorCode::Internal,
                    "the tool broker's answer had no `ok` field",
                    false,
                ))
            }
            other => other,
        };
        let payload = reply(result, run);
        link.answer(&envelope.id, answer_kind, payload).await
    }
}

/// Either kind of host call, so both can share one timeout and cancel race.
enum CallFuture<T, M> {
    Tool(T),
    Model(M),
}

impl<T, M> CallFuture<T, M>
where
    T: Future<Output = Result<Value, LinkError>>,
    M: Future<Output = Result<Value, LinkError>>,
{
    async fn into_future(self) -> Result<Value, LinkError> {
        match self {
            Self::Tool(future) => future.await,
            Self::Model(future) => future.await,
        }
    }
}

struct RunState {
    run_id: String,
    stopping: Option<(StopReason, Instant)>,
    acknowledged: bool,
    tool_calls: u32,
    model_calls: u32,
    /// Tool calls the link refused itself, so they get call IDs of their own.
    refusals: u32,
}

impl RunState {
    fn check_run_id(&self, envelope: &Envelope) -> Result<(), SidecarError> {
        if envelope.str_field("runId") == Some(self.run_id.as_str()) {
            Ok(())
        } else {
            Err(SidecarError::Protocol(format!(
                "a `{:?}` message named another run",
                envelope.kind
            )))
        }
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Just enough environment for Python to start and speak UTF-8 on every platform.
fn minimal_environment() -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = [
        ("PYTHONUTF8", "1"),
        ("PYTHONIOENCODING", "utf-8"),
        ("PYTHONUNBUFFERED", "1"),
        ("PYTHONDONTWRITEBYTECODE", "1"),
        ("PYTHONNOUSERSITE", "1"),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value.into()))
    .collect();
    // Windows can't start Python without these; elsewhere a temp folder is all that's borrowed.
    let inherited: &[&str] = if cfg!(windows) {
        &["SYSTEMROOT", "WINDIR", "TEMP", "TMP", "LOCALAPPDATA"]
    } else {
        &["TMPDIR", "LANG"]
    };
    for key in inherited {
        if let Some(value) = std::env::var_os(key) {
            env.push(((*key).into(), value));
        }
    }
    env
}

async fn read_messages(stdout: tokio::process::ChildStdout, sender: mpsc::Sender<Incoming>) {
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    loop {
        let incoming = match read_bounded_line(&mut reader, MAX_MESSAGE_BYTES, &mut line).await {
            Ok(LineRead::Line) if line.iter().all(u8::is_ascii_whitespace) => continue,
            Ok(LineRead::Line) => match decode_from_agent(&line) {
                Ok(envelope) => Incoming::Message(envelope),
                Err(error) => Incoming::Violation(error.to_string()),
            },
            Ok(LineRead::TooLong) => Incoming::Violation(format!(
                "a message passed the {MAX_MESSAGE_BYTES}-byte limit"
            )),
            Ok(LineRead::Eof) | Err(_) => return,
        };
        let violation = matches!(incoming, Incoming::Violation(_));
        if sender.send(incoming).await.is_err() || violation {
            return;
        }
    }
}

async fn read_diagnostics(
    stderr: tokio::process::ChildStderr,
    lines: Arc<StdMutex<VecDeque<String>>>,
) {
    let mut reader = BufReader::new(stderr);
    let mut line = Vec::new();
    loop {
        let read = read_bounded_line(&mut reader, STDERR_LINE_BYTES, &mut line).await;
        let text = match read {
            Ok(LineRead::Line) => String::from_utf8_lossy(&line).into_owned(),
            Ok(LineRead::TooLong) => {
                // Keep the start of an overlong line and drop the rest of it.
                let text = format!("{}…", String::from_utf8_lossy(&line));
                let mut skip = Vec::new();
                while let Ok(LineRead::TooLong) =
                    read_bounded_line(&mut reader, STDERR_LINE_BYTES, &mut skip).await
                {
                }
                text
            }
            Ok(LineRead::Eof) | Err(_) => return,
        };
        if let Ok(mut lines) = lines.lock() {
            if lines.len() == STDERR_LINES {
                lines.pop_front();
            }
            lines.push_back(text);
        }
    }
}

#[cfg(test)]
mod tests;
