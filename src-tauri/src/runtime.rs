use crate::agent::{OllamaStatus, PlannerAgent, MODEL_NAME};
use crate::error::{AppError, AppResult};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::fs;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

pub const BUNDLED_OLLAMA_VERSION: &str = "0.32.0";
const START_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_PULL_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
/// How long a stopping server gets to unload its model and exit before it is killed outright.
const STOP_GRACE: Duration = Duration::from_secs(2);
/// The runtime stops itself after this long without a planner request, so an open Delve Planner
/// that nobody is asking anything costs nothing.
const IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// How often the idle worker looks.
pub const IDLE_CHECK: Duration = Duration::from_secs(30);
/// What the file holding the runtime's own state across launches is called.
const RECORD_FILE: &str = "ai-runtime.json";
/// How long a measured model-folder size is trusted before it is walked again.
const STORAGE_CACHE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimePhase {
    Unavailable,
    /// Nothing is wrong; the runtime simply isn't running because nothing has needed it.
    Stopped,
    Starting,
    ReadyWithoutModel,
    Downloading,
    ModelReady,
    UpdateRequired,
    Error,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub completed: u64,
    pub total: Option<u64>,
    pub percent: Option<u8>,
    pub status: String,
}

struct RuntimeState {
    child: Option<Child>,
    phase: RuntimePhase,
    detail: String,
    download: Option<DownloadProgress>,
    download_cancel: Option<CancellationToken>,
    /// When the planner last needed the runtime, which is what the idle worker measures.
    last_used: Instant,
    /// Which models folder the running server was started with. Ollama serves one folder, so
    /// moving between Delve Planner's models and the machine's own means a restart.
    serving_dir: Option<PathBuf>,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            child: None,
            phase: RuntimePhase::Stopped,
            detail: "Delve Planner's local AI runtime starts when the planner needs it.".into(),
            download: None,
            download_cancel: None,
            last_used: Instant::now(),
            serving_dir: None,
        }
    }
}

struct RuntimeInner {
    endpoint: String,
    host: String,
    runtime_dir: PathBuf,
    model_dir: PathBuf,
    record_path: PathBuf,
    /// The machine's own models folder, if it has one. Held here so tests can point somewhere
    /// other than the home directory they happen to run in.
    system_dir: Option<PathBuf>,
    /// Held across a read and write of the record file, so recording a server can't undo a model
    /// the user chose a moment earlier.
    record_lock: Mutex<()>,
    state: Mutex<RuntimeState>,
    storage: Mutex<Option<(Instant, u64)>>,
    lifecycle: tokio::sync::Mutex<()>,
}

impl Drop for RuntimeInner {
    fn drop(&mut self) {
        if let Ok(state) = self.state.get_mut() {
            if let Some(child) = state.child.as_mut() {
                end_tree(child);
            }
        }
    }
}

/// What the runtime remembers between launches: which model the user chose, whether it answered
/// Delve Planner's format check, and the server's process ID. A Delve Planner that crashed can't
/// clean up after itself, and every leftover server holds on to its model.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RuntimeRecord {
    server_pid: Option<u32>,
    model: Option<String>,
    /// Models that have passed the format check, so an answered question isn't asked again.
    #[serde(default)]
    checked: Vec<String>,
}

/// Where a model's files live. Delve Planner reads both folders and only ever writes to its own.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    /// Downloaded by Delve Planner, under its own application data.
    DelvePlanner,
    /// Already on the machine, in the folder Ollama itself uses.
    System,
}

/// One model the user could plan with.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledModel {
    pub name: String,
    pub size_bytes: u64,
    pub source: ModelSource,
    /// Whether this is the model Delve Planner's own evaluations run against.
    pub tested: bool,
    /// Whether this model has answered Delve Planner's format check on this machine.
    pub checked: bool,
    pub selected: bool,
}

#[derive(Clone)]
pub struct OllamaRuntimeManager {
    inner: Arc<RuntimeInner>,
}

impl OllamaRuntimeManager {
    pub fn new(resource_dir: PathBuf, app_data_dir: PathBuf) -> AppResult<Self> {
        let port = reserve_loopback_port()?;
        Self::new_with_port(resource_dir, app_data_dir, port, system_model_dir())
    }

    fn new_with_port(
        resource_dir: PathBuf,
        app_data_dir: PathBuf,
        port: u16,
        system_dir: Option<PathBuf>,
    ) -> AppResult<Self> {
        let host = format!("127.0.0.1:{port}");
        let model_dir = app_data_dir.join("ai-models");
        fs::create_dir_all(&model_dir)?;
        let manager = Self {
            inner: Arc::new(RuntimeInner {
                endpoint: format!("http://{host}"),
                host,
                runtime_dir: resource_dir.join("resources").join("ollama"),
                model_dir,
                record_path: app_data_dir.join(RECORD_FILE),
                system_dir,
                record_lock: Mutex::new(()),
                state: Mutex::new(RuntimeState::default()),
                storage: Mutex::new(None),
                lifecycle: tokio::sync::Mutex::new(()),
            }),
        };
        manager.end_leftover_server();
        Ok(manager)
    }

    /// Ends a server left behind by a Delve Planner that crashed or was force-quit. Without this,
    /// every such launch adds another idle server holding its model.
    fn end_leftover_server(&self) {
        let Some(pid) = self.record().server_pid else {
            return;
        };
        if is_our_server(pid, &self.binary_path()) {
            tauri_plugin_log::log::info!("ai_runtime_leftover_ended");
            terminate_tree(pid);
            let deadline = Instant::now() + STOP_GRACE;
            loop {
                // Once the process is gone the ID may belong to anything, so nothing more is sent.
                if !is_our_server(pid, &self.binary_path()) {
                    break;
                }
                if Instant::now() >= deadline {
                    kill_tree(pid);
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        self.forget_server();
    }

    fn record(&self) -> RuntimeRecord {
        fs::read(&self.inner.record_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn write_record(&self, record: &RuntimeRecord) {
        if let Ok(bytes) = serde_json::to_vec(record) {
            let _ = fs::write(&self.inner.record_path, bytes);
        }
    }

    /// Changes one part of the record without disturbing the rest.
    fn update_record(&self, change: impl FnOnce(&mut RuntimeRecord)) {
        let _guard = self.inner.record_lock.lock();
        let mut record = self.record();
        change(&mut record);
        self.write_record(&record);
    }

    /// Forgets the running server without forgetting which model the user chose.
    fn forget_server(&self) {
        self.update_record(|record| record.server_pid = None);
    }

    fn remember_server(&self, pid: Option<u32>) {
        self.update_record(|record| record.server_pid = pid);
    }

    pub fn endpoint(&self) -> &str {
        &self.inner.endpoint
    }

    pub fn model_dir(&self) -> &Path {
        &self.inner.model_dir
    }

    /// Every model Delve Planner could use: the ones it downloaded itself and the ones already on
    /// the machine. Read from the manifests on disk, so browsing them starts nothing.
    pub fn installed_models(&self) -> Vec<InstalledModel> {
        let record = self.record();
        let selected = self.selected_model(&record);
        let mut models: Vec<InstalledModel> = Vec::new();
        for (source, dir) in self.model_dirs() {
            for (name, size_bytes) in models_in(&dir) {
                // A model in both folders is listed once; Delve Planner's own copy is the one it
                // runs.
                if models.iter().any(|model| model.name == name) {
                    continue;
                }
                models.push(InstalledModel {
                    tested: name == MODEL_NAME,
                    checked: name == MODEL_NAME || record.checked.contains(&name),
                    selected: selected.as_deref() == Some(name.as_str()),
                    name,
                    size_bytes,
                    source,
                });
            }
        }
        models.sort_by(|left, right| {
            right
                .tested
                .cmp(&left.tested)
                .then_with(|| left.name.cmp(&right.name))
        });
        models
    }

    /// Both model folders, Delve Planner's first: `OLLAMA_MODELS` if the user set one, otherwise
    /// the place Ollama keeps models by default.
    fn model_dirs(&self) -> Vec<(ModelSource, PathBuf)> {
        let mut dirs = vec![(ModelSource::DelvePlanner, self.inner.model_dir.clone())];
        if let Some(system) = self.inner.system_dir.clone() {
            if system != self.inner.model_dir && system.is_dir() {
                dirs.push((ModelSource::System, system));
            }
        }
        dirs
    }

    /// The model the planner should use: the user's choice while it still exists, otherwise the
    /// model Delve Planner ships against, otherwise nothing and the app asks.
    fn selected_model(&self, record: &RuntimeRecord) -> Option<String> {
        if let Some(chosen) = record.model.as_ref() {
            if self.model_dir_for(chosen).is_some() {
                return Some(chosen.clone());
            }
        }
        self.model_dir_for(MODEL_NAME).map(|_| MODEL_NAME.into())
    }

    pub fn chosen_model(&self) -> Option<String> {
        self.selected_model(&self.record())
    }

    /// Which folder holds a model, Delve Planner's own taking precedence.
    fn model_dir_for(&self, model: &str) -> Option<(ModelSource, PathBuf)> {
        self.model_dirs()
            .into_iter()
            .find(|(_, dir)| manifest_path(dir, model).is_some_and(|manifest| manifest.is_file()))
    }

    /// The folder the server should serve from. Only one folder can be active at a time, so it
    /// follows the chosen model; downloads always use Delve Planner's own.
    fn active_model_dir(&self) -> PathBuf {
        self.chosen_model()
            .and_then(|model| self.model_dir_for(&model))
            .map(|(_, dir)| dir)
            .unwrap_or_else(|| self.inner.model_dir.clone())
    }

    /// Remembers which model the planner should use. The server restarts on the next request if
    /// the choice moved to the other folder.
    pub fn choose_model(&self, model: &str) -> AppResult<()> {
        if self.model_dir_for(model).is_none() {
            return Err(AppError::Validation(
                "That model is no longer installed.".into(),
            ));
        }
        self.update_record(|record| record.model = Some(model.to_string()));
        Ok(())
    }

    /// Records that a model answered Delve Planner's format check, so it isn't asked again.
    pub fn mark_checked(&self, model: &str) {
        self.update_record(|record| {
            if !record.checked.iter().any(|name| name == model) {
                record.checked.push(model.to_string());
            }
        });
    }

    /// Starts the runtime for the model the user chose, serving from whichever folder holds it.
    pub async fn ensure_started(&self, agent: &PlannerAgent) -> AppResult<()> {
        self.ensure_started_in(agent, self.active_model_dir()).await
    }

    async fn ensure_started_in(&self, agent: &PlannerAgent, models: PathBuf) -> AppResult<()> {
        let _lifecycle = self.inner.lifecycle.lock().await;
        let ready = {
            let mut state = self.lock_state()?;
            let running = match state.child.as_mut() {
                Some(child) => child.try_wait()?.is_none(),
                None => false,
            };
            running && state.serving_dir.as_deref() == Some(models.as_path())
        };
        if ready && agent.status().await.running {
            return Ok(());
        }
        self.stop_locked().await?;
        let binary = self.binary_path();
        if !binary.is_file() {
            self.set_failure(
                RuntimePhase::Unavailable,
                "The signed Delve Planner package does not contain its local AI runtime.",
            );
            return Err(AppError::OllamaRuntime(format!(
                "missing {}",
                binary.display()
            )));
        }

        self.set_phase(
            RuntimePhase::Starting,
            "Starting Delve Planner's local AI runtime…",
        );
        let mut command = Command::new(&binary);
        command
            .arg("serve")
            .current_dir(binary.parent().unwrap_or(&self.inner.runtime_dir))
            .env("OLLAMA_HOST", &self.inner.host)
            .env("OLLAMA_MODELS", &models)
            .env("OLLAMA_NOHISTORY", "1")
            .env("OLLAMA_NO_CLOUD", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(if cfg!(debug_assertions) {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .kill_on_drop(true);
        // Its own process group, so Delve Planner can end the server and its model runners
        // together.
        #[cfg(unix)]
        command.process_group(0);
        let child = command.spawn().map_err(|error| {
            self.set_failure(
                RuntimePhase::Error,
                "The bundled AI runtime could not start.",
            );
            AppError::OllamaRuntime(error.to_string())
        })?;
        self.remember_server(child.id());
        {
            let mut state = self.lock_state()?;
            state.child = Some(child);
            state.last_used = Instant::now();
            state.serving_dir = Some(models);
        }

        let deadline = tokio::time::Instant::now() + START_TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            let status = agent.status().await;
            if status.running {
                if status.ollama_version.as_deref() != Some(BUNDLED_OLLAMA_VERSION) {
                    self.set_failure(
                        RuntimePhase::UpdateRequired,
                        "The bundled AI runtime version does not match this Delve Planner release.",
                    );
                    self.stop_locked().await?;
                    return Err(AppError::OllamaRuntime("runtime version mismatch".into()));
                }
                self.set_phase(
                    if status.model_installed {
                        RuntimePhase::ModelReady
                    } else {
                        RuntimePhase::ReadyWithoutModel
                    },
                    &status.detail,
                );
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        self.stop_locked().await?;
        self.set_failure(
            RuntimePhase::Error,
            "The bundled AI runtime did not become ready in time.",
        );
        Err(AppError::OllamaUnavailable)
    }

    /// What the app can say about the runtime without changing it. Starting a server to answer
    /// "is it running?" is what used to leave Ollama running from the moment Delve Planner opened,
    /// and a status that could restart one would cut off whatever it was in the middle of — a model
    /// download is polled by this very call.
    pub async fn status(&self, agent: &PlannerAgent) -> OllamaStatus {
        if self.running() {
            return self.running_status(agent).await;
        }
        let state = self.lock_state().ok();
        let phase = state
            .as_ref()
            .map_or(RuntimePhase::Stopped, |value| match value.phase {
                // A phase that describes a running server no longer holds once it has stopped.
                RuntimePhase::Starting
                | RuntimePhase::ReadyWithoutModel
                | RuntimePhase::ModelReady => RuntimePhase::Stopped,
                other => other,
            });
        OllamaStatus {
            phase,
            running: false,
            model_installed: self.model_is_installed(&agent.model_name()),
            model_name: agent.model_name(),
            model_digest: None,
            ollama_version: Some(BUNDLED_OLLAMA_VERSION.into()),
            model_license: None,
            detail: state
                .as_ref()
                .map(|value| value.detail.clone())
                .unwrap_or_else(|| "Delve Planner's local AI runtime is not running.".into()),
            download: None,
            storage_bytes: self.storage_bytes(),
        }
    }

    /// The full picture, which needs the server: starts it if it isn't up. Only a deliberate
    /// action calls this, never a poll.
    pub async fn started_status(&self, agent: &PlannerAgent) -> OllamaStatus {
        if let Err(error) = self.ensure_started(agent).await {
            let state = self.lock_state().ok();
            return OllamaStatus {
                phase: state
                    .as_ref()
                    .map_or(RuntimePhase::Error, |value| value.phase),
                running: false,
                model_installed: false,
                model_name: MODEL_NAME.into(),
                model_digest: None,
                ollama_version: Some(BUNDLED_OLLAMA_VERSION.into()),
                model_license: None,
                detail: state
                    .as_ref()
                    .map(|value| value.detail.clone())
                    .unwrap_or_else(|| error.to_string()),
                download: None,
                storage_bytes: self.storage_bytes(),
            };
        }
        self.running_status(agent).await
    }

    /// Asks the running server how it is, and nothing more.
    async fn running_status(&self, agent: &PlannerAgent) -> OllamaStatus {
        let mut status = agent.status().await;
        let state = self.lock_state().ok();
        status.phase = state.as_ref().map_or(
            if status.model_installed {
                RuntimePhase::ModelReady
            } else {
                RuntimePhase::ReadyWithoutModel
            },
            |value| value.phase,
        );
        status.download = state.and_then(|value| value.download.clone());
        status.storage_bytes = self.storage_bytes();
        status
    }

    /// Downloads the model Delve Planner ships against. Downloads always land in Delve Planner's
    /// own folder: the machine's model library belongs to the user, and Delve Planner only ever
    /// reads it.
    pub async fn pull_model(&self, agent: &PlannerAgent) -> AppResult<()> {
        self.ensure_started_in(agent, self.inner.model_dir.clone())
            .await?;
        let token = CancellationToken::new();
        {
            let mut state = self.lock_state()?;
            if state.download_cancel.is_some() {
                return Err(AppError::Validation(
                    "A model download is already running.".into(),
                ));
            }
            state.phase = RuntimePhase::Downloading;
            state.detail = "Downloading qwen3:8b…".into();
            state.download = Some(DownloadProgress {
                status: "starting".into(),
                ..Default::default()
            });
            state.download_cancel = Some(token.clone());
        }
        let mut result = self.pull_model_inner(token).await;
        if result.is_ok() {
            let installed = agent.status().await;
            if !installed.model_installed || installed.model_digest.is_none() {
                result = Err(AppError::OllamaRuntime(
                    "download completed without a verifiable model digest".into(),
                ));
            }
        }
        let mut state = self.lock_state()?;
        state.download_cancel = None;
        match &result {
            Ok(()) => {
                state.phase = RuntimePhase::ModelReady;
                state.detail = "Delve Planner's local qwen3:8b model is ready.".into();
                state.download = None;
            }
            Err(AppError::ModelDownloadCancelled) => {
                state.phase = RuntimePhase::ReadyWithoutModel;
                state.detail =
                    "Model download cancelled. Downloaded layers are retained for retry.".into();
                state.download = None;
            }
            Err(error) => {
                state.phase = RuntimePhase::Error;
                state.detail = format!("Model download failed: {error}");
            }
        }
        result
    }

    async fn pull_model_inner(&self, token: CancellationToken) -> AppResult<()> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60 * 60))
            .build()?;
        let response = client
            .post(format!("{}/api/pull", self.inner.endpoint))
            .json(&serde_json::json!({ "model": MODEL_NAME, "stream": true }))
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(AppError::OllamaUnavailable);
        }
        let mut stream = response.bytes_stream();
        let mut buffered = Vec::new();
        let mut received = 0_u64;
        loop {
            let chunk = tokio::select! {
                _ = token.cancelled() => return Err(AppError::ModelDownloadCancelled),
                value = stream.next() => value,
            };
            let Some(chunk) = chunk else { break };
            let chunk = chunk?;
            received = received.saturating_add(chunk.len() as u64);
            if received > MAX_PULL_RESPONSE_BYTES {
                return Err(AppError::OllamaRuntime(
                    "model download response exceeded its safety limit".into(),
                ));
            }
            buffered.extend_from_slice(&chunk);
            while let Some(position) = buffered.iter().position(|byte| *byte == b'\n') {
                let line = buffered.drain(..=position).collect::<Vec<_>>();
                if let Ok(update) = serde_json::from_slice::<PullUpdate>(&line) {
                    if let Some(error) = update.error {
                        return Err(AppError::OllamaRuntime(error));
                    }
                    let percent = update.total.filter(|total| *total > 0).map(|total| {
                        ((update.completed.unwrap_or(0).saturating_mul(100) / total).min(100)) as u8
                    });
                    self.lock_state()?.download = Some(DownloadProgress {
                        completed: update.completed.unwrap_or(0),
                        total: update.total,
                        percent,
                        status: update.status,
                    });
                }
            }
        }
        Ok(())
    }

    pub fn cancel_download(&self) {
        if let Ok(state) = self.inner.state.lock() {
            if let Some(token) = &state.download_cancel {
                token.cancel();
            }
        }
    }

    pub async fn restart(&self, agent: &PlannerAgent) -> AppResult<()> {
        self.cancel_download();
        self.stop().await?;
        self.ensure_started(agent).await
    }

    pub async fn remove_model(&self) -> AppResult<()> {
        self.cancel_download();
        self.stop().await?;
        if self.inner.model_dir.exists() {
            fs::remove_dir_all(&self.inner.model_dir)?;
        }
        fs::create_dir_all(&self.inner.model_dir)?;
        self.set_phase(
            RuntimePhase::Unavailable,
            "Local AI model data was removed.",
        );
        Ok(())
    }

    async fn stop(&self) -> AppResult<()> {
        let _lifecycle = self.inner.lifecycle.lock().await;
        self.stop_locked().await
    }

    async fn stop_locked(&self) -> AppResult<()> {
        let child = self.lock_state()?.child.take();
        if let Some(mut child) = child {
            if let Some(pid) = child.id() {
                // Ask the whole group to go, so no runner is left holding the model.
                terminate_tree(pid);
                if tokio::time::timeout(STOP_GRACE, child.wait())
                    .await
                    .is_err()
                {
                    kill_tree(pid);
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
            } else {
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
        }
        if let Ok(mut state) = self.inner.state.lock() {
            state.serving_dir = None;
        }
        self.forget_server();
        self.set_phase(
            RuntimePhase::Stopped,
            "Delve Planner's local AI runtime starts when the planner needs it.",
        );
        Ok(())
    }

    /// Ends the runtime while the app is quitting, without an async runtime to wait on. Tauri
    /// skips destructors on exit, so this is what keeps a quit from leaving a server behind.
    pub fn shutdown(&self) {
        if let Ok(mut state) = self.inner.state.lock() {
            if let Some(token) = &state.download_cancel {
                token.cancel();
            }
            if let Some(child) = state.child.as_mut() {
                end_tree(child);
            }
            state.child = None;
            state.phase = RuntimePhase::Stopped;
            state.serving_dir = None;
        }
        tauri_plugin_log::log::info!("ai_runtime_shutdown");
        self.forget_server();
    }

    /// Marks the runtime as wanted right now, which holds off the idle stop.
    pub fn note_used(&self) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.last_used = Instant::now();
        }
    }

    /// Whether the server is running at all, without starting it to find out.
    fn running(&self) -> bool {
        self.lock_state()
            .map(|mut state| match state.child.as_mut() {
                Some(child) => child.try_wait().map(|exit| exit.is_none()).unwrap_or(false),
                None => false,
            })
            .unwrap_or(false)
    }

    /// Stops a runtime nobody has used for a while. Returns whether it stopped anything.
    pub async fn stop_if_idle(&self) -> bool {
        // Held across the check, so a request that arrives while this is deciding either starts
        // the runtime after it stopped or keeps it: never has it pulled out mid-answer.
        let _lifecycle = self.inner.lifecycle.lock().await;
        if !self.is_idle() || !self.running() {
            return false;
        }
        let stopped = self.stop_locked().await.is_ok();
        if stopped {
            tauri_plugin_log::log::info!("ai_runtime_stopped_idle");
        }
        stopped
    }

    fn is_idle(&self) -> bool {
        self.inner.state.lock().is_ok_and(|state| {
            state.download_cancel.is_none() && state.last_used.elapsed() >= IDLE_TIMEOUT
        })
    }

    /// Drops the model from memory but leaves the server ready, for when the user steps away from
    /// a planner session. Ollama takes `keep_alive: 0` as "unload as soon as this returns".
    pub async fn unload_model(&self, model: &str) {
        if !self.running() {
            return;
        }
        let client = reqwest::Client::new();
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            client
                .post(format!("{}/api/generate", self.inner.endpoint))
                .json(&serde_json::json!({ "model": model, "keep_alive": 0 }))
                .send(),
        )
        .await;
    }

    /// How much room the models take. Walking a folder of multi-gigabyte blobs on every status
    /// poll is wasted disk work, so the answer is kept for a while.
    fn storage_bytes(&self) -> Option<u64> {
        if let Ok(cached) = self.inner.storage.lock() {
            if let Some((measured_at, bytes)) = *cached {
                if measured_at.elapsed() < STORAGE_CACHE {
                    return Some(bytes);
                }
            }
        }
        let bytes = directory_size(&self.inner.model_dir).ok()?;
        if let Ok(mut cached) = self.inner.storage.lock() {
            *cached = Some((Instant::now(), bytes));
        }
        Some(bytes)
    }

    /// Whether a model is already on disk, answered without a running server.
    fn model_is_installed(&self, model: &str) -> bool {
        manifest_path(&self.inner.model_dir, model).is_some_and(|path| path.is_file())
    }

    fn binary_path(&self) -> PathBuf {
        if let Some(path) = std::env::var_os("DELVE_PLANNER_OLLAMA_RUNTIME") {
            return PathBuf::from(path);
        }
        let relative = if cfg!(windows) {
            "ollama.exe"
        } else {
            "ollama"
        };
        self.inner
            .runtime_dir
            .join(platform_directory())
            .join(relative)
    }

    fn lock_state(&self) -> AppResult<std::sync::MutexGuard<'_, RuntimeState>> {
        self.inner
            .state
            .lock()
            .map_err(|_| AppError::Internal("The local AI runtime state is unavailable.".into()))
    }

    fn set_phase(&self, phase: RuntimePhase, detail: &str) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.phase = phase;
            state.detail = detail.into();
        }
    }

    fn set_failure(&self, phase: RuntimePhase, detail: &str) {
        self.set_phase(phase, detail);
    }
}

/// Ollama serves a model from a separate runner process. Killing the server alone leaves that
/// runner holding the model in memory, so Delve Planner always ends the whole tree.
#[cfg(unix)]
fn signal_tree(pid: u32, signal: i32) {
    // The server leads its own process group, so one signal reaches every runner under it.
    unsafe { libc::killpg(pid as libc::pid_t, signal) };
}

#[cfg(unix)]
fn terminate_tree(pid: u32) {
    signal_tree(pid, libc::SIGTERM);
}

#[cfg(unix)]
fn kill_tree(pid: u32) {
    signal_tree(pid, libc::SIGKILL);
}

/// Windows has no polite signal for a console child, so the tree goes at once.
#[cfg(windows)]
fn terminate_tree(pid: u32) {
    kill_tree(pid);
}

#[cfg(windows)]
fn kill_tree(pid: u32) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Ends a server Delve Planner started, without waiting on an async runtime: the app calls this
/// while it is already quitting. Ollama unloads its model on `SIGTERM`, so it is asked first and
/// killed only if it doesn't go.
fn end_tree(child: &mut Child) {
    let Some(pid) = child.id() else {
        return;
    };
    terminate_tree(pid);
    let deadline = Instant::now() + STOP_GRACE;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    kill_tree(pid);
    let _ = child.start_kill();
    let _ = child.try_wait();
}

/// Whether `pid` is still a server Delve Planner started. A process ID alone isn't enough — the
/// system reuses them — so the command behind it has to be this build's own runtime binary.
fn is_our_server(pid: u32, binary: &Path) -> bool {
    #[cfg(unix)]
    let listing = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output();
    #[cfg(windows)]
    let listing = {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
    };
    let Ok(listing) = listing else {
        return false;
    };
    let line = String::from_utf8_lossy(&listing.stdout);
    // Windows lists only the image name, which the process ID Delve Planner recorded already
    // narrows.
    let needle = if cfg!(windows) {
        binary.file_name().unwrap_or_default().to_string_lossy()
    } else {
        binary.to_string_lossy()
    };
    !needle.is_empty() && line.contains(needle.as_ref())
}

#[derive(Deserialize)]
struct PullUpdate {
    #[serde(default)]
    status: String,
    completed: Option<u64>,
    total: Option<u64>,
    error: Option<String>,
}

fn reserve_loopback_port() -> std::io::Result<u16> {
    TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))?
        .local_addr()
        .map(|address| address.port())
}

/// The folder Ollama itself keeps models in, which is where anything the user has already pulled
/// lives. Delve Planner reads it and never writes to it.
fn system_model_dir() -> Option<PathBuf> {
    if let Some(configured) = std::env::var_os("OLLAMA_MODELS") {
        let path = PathBuf::from(configured);
        if !path.as_os_str().is_empty() {
            return Some(path);
        }
    }
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })?;
    Some(PathBuf::from(home).join(".ollama").join("models"))
}

/// Every model in a folder, read from its manifests: name and the size of its layers. Ollama lays
/// them out as `manifests/<registry>/<namespace>/<name>/<tag>`.
fn models_in(model_dir: &Path) -> Vec<(String, u64)> {
    let root = model_dir.join("manifests");
    let mut found = Vec::new();
    for registry in children(&root) {
        let registry_name = file_name(&registry);
        for namespace in children(&registry) {
            let namespace_name = file_name(&namespace);
            for name in children(&namespace) {
                let model_name = file_name(&name);
                for tag in children(&name) {
                    if !tag.is_file() {
                        continue;
                    }
                    let Some(size) = manifest_size(&tag) else {
                        continue;
                    };
                    let tag_name = file_name(&tag);
                    let full = match (registry_name.as_str(), namespace_name.as_str()) {
                        ("registry.ollama.ai", "library") => format!("{model_name}:{tag_name}"),
                        ("registry.ollama.ai", namespace) => {
                            format!("{namespace}/{model_name}:{tag_name}")
                        }
                        (registry, namespace) => {
                            format!("{registry}/{namespace}/{model_name}:{tag_name}")
                        }
                    };
                    found.push((full, size));
                }
            }
        }
    }
    found
}

fn children(path: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(path) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| !file_name(path).starts_with('.'))
        .collect()
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// How much disk a model takes, as the sum of the layers its manifest names.
fn manifest_size(path: &Path) -> Option<u64> {
    let manifest: Manifest = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    Some(
        manifest
            .layers
            .iter()
            .chain(manifest.config.iter())
            .map(|layer| layer.size)
            .sum(),
    )
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    layers: Vec<ManifestLayer>,
    #[serde(default)]
    config: Option<ManifestLayer>,
}

#[derive(Deserialize)]
struct ManifestLayer {
    #[serde(default)]
    size: u64,
}

/// Where Ollama keeps the manifest for a model inside a models folder. `qwen3:8b` lives at
/// `manifests/registry.ollama.ai/library/qwen3/8b`; a namespaced or registry-qualified name fills
/// in the parts it names itself.
fn manifest_path(model_dir: &Path, model: &str) -> Option<PathBuf> {
    let (name, tag) = match model.rsplit_once(':') {
        Some((name, tag)) if !name.is_empty() && !tag.is_empty() => (name, tag),
        _ => (model, "latest"),
    };
    let parts: Vec<&str> = name.split('/').filter(|part| !part.is_empty()).collect();
    let (registry, namespace, name) = match parts.as_slice() {
        [name] => ("registry.ollama.ai", "library", *name),
        [namespace, name] => ("registry.ollama.ai", *namespace, *name),
        [registry, namespace, name] => (*registry, *namespace, *name),
        _ => return None,
    };
    if ![registry, namespace, name, tag]
        .iter()
        .all(|part| is_name_part(part))
    {
        return None;
    }
    Some(
        model_dir
            .join("manifests")
            .join(registry)
            .join(namespace)
            .join(name)
            .join(tag),
    )
}

/// One part of a model name: letters, digits, and `_ . -`, which is all Ollama's own names use.
/// Anything else is refused, because a part that is a path of its own walks the manifest out of
/// the models folder — joining an absolute path throws away everything before it.
fn is_name_part(part: &str) -> bool {
    !part.is_empty()
        && part != "."
        && part != ".."
        && part.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-')
        })
}

fn platform_directory() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") | ("macos", "x86_64") => "macos-universal",
        ("windows", "x86_64") => "windows-x86_64",
        _ => "unsupported",
    }
}

fn directory_size(path: &Path) -> std::io::Result<u64> {
    let mut total = 0_u64;
    if !path.exists() {
        return Ok(0);
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        total = total.saturating_add(if metadata.is_dir() {
            directory_size(&entry.path())?
        } else {
            metadata.len()
        });
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use tokio::io::{AsyncBufReadExt, BufReader};

    #[test]
    fn reserves_private_loopback_port() {
        match reserve_loopback_port() {
            Ok(port) => assert_ne!(port, 11434),
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {}
            Err(error) => panic!("could not reserve loopback port: {error}"),
        }
    }

    /// The behaviour that used to leave a model server running after every quit: killing the
    /// server alone leaves the runner it spawned holding the model, so the whole group has to go.
    #[cfg(unix)]
    #[tokio::test]
    async fn ending_the_server_ends_the_runners_it_spawned() {
        // Stands in for `ollama serve`: it spawns a long-lived child, reports that child's process
        // ID, and then waits, exactly as the server does with a model runner.
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("sleep 30 & echo $!; wait")
            .stdout(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        // One line, not end of output: the shell holds its pipe open until it exits.
        let mut reported = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut reported)
            .await
            .unwrap();
        let runner: u32 = reported.trim().parse().unwrap();
        assert!(alive(runner), "the runner should be running to begin with");

        end_tree(&mut child);

        // The group is signalled together, so the runner goes with the server it belonged to.
        for _ in 0..40 {
            if !alive(runner) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        kill_tree(runner);
        panic!("the runner outlived the server that spawned it");
    }

    #[cfg(unix)]
    fn alive(pid: u32) -> bool {
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    #[test]
    fn model_names_map_onto_ollamas_manifest_layout() {
        let root = Path::new("/models");
        assert_eq!(
            manifest_path(root, "qwen3:8b").unwrap(),
            root.join("manifests/registry.ollama.ai/library/qwen3/8b")
        );
        // An untagged name is the `latest` tag, the same as Ollama treats it.
        assert_eq!(
            manifest_path(root, "mistral").unwrap(),
            root.join("manifests/registry.ollama.ai/library/mistral/latest")
        );
        assert_eq!(
            manifest_path(root, "hf.co/user/model:q4").unwrap(),
            root.join("manifests/hf.co/user/model/q4")
        );
        // A name is never allowed to walk out of the models folder. An absolute tag is the one
        // that bites: joining an absolute path throws away everything before it.
        assert!(manifest_path(root, "../../etc/passwd").is_none());
        assert!(manifest_path(root, "x:/etc/passwd").is_none());
        assert!(manifest_path(root, "x:../../../etc/passwd").is_none());
        assert!(manifest_path(root, "a/b/c/d:1").is_none());
        // A stray separator in the name is dropped rather than followed.
        for path in [
            manifest_path(root, "qwen3:8b"),
            manifest_path(root, "hf.co/user/model:q4"),
            manifest_path(root, "/etc:passwd"),
        ] {
            assert!(path.unwrap().starts_with(root));
        }
    }

    /// Writes a manifest where Ollama would keep one, so the folder looks like a real model store.
    fn install(model_dir: &Path, model: &str, size: u64) {
        let manifest = manifest_path(model_dir, model).unwrap();
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        fs::write(
            &manifest,
            format!(
                r#"{{"schemaVersion":2,"config":{{"size":100}},"layers":[{{"size":{}}}]}}"#,
                size - 100
            ),
        )
        .unwrap();
    }

    fn manager_with(root: &Path, system: Option<PathBuf>) -> OllamaRuntimeManager {
        OllamaRuntimeManager::new_with_port(
            root.join("resources"),
            root.join("data"),
            49_152,
            system,
        )
        .unwrap()
    }

    #[test]
    fn lists_models_from_both_folders_and_keeps_the_tested_one_first() {
        let root = tempfile::tempdir().unwrap();
        let system = root.path().join("home/.ollama/models");
        let manager = manager_with(root.path(), Some(system.clone()));
        install(manager.model_dir(), MODEL_NAME, 5_200_000_100);
        install(&system, "llama3.1:8b", 4_700_000_100);
        install(&system, "mistral", 4_100_000_100);

        let models = manager.installed_models();
        let names: Vec<&str> = models.iter().map(|model| model.name.as_str()).collect();
        // A model stored without a tag is `latest`, which is how Ollama itself lists it.
        assert_eq!(names, [MODEL_NAME, "llama3.1:8b", "mistral:latest"]);
        assert!(models[0].tested && models[0].checked && models[0].selected);
        assert_eq!(models[0].source, ModelSource::DelvePlanner);
        assert_eq!(models[0].size_bytes, 5_200_000_100);
        // A model Delve Planner hasn't evaluated is offered, but never as one it has checked.
        assert!(!models[1].tested && !models[1].checked);
        assert_eq!(models[1].source, ModelSource::System);
    }

    #[test]
    fn the_chosen_model_decides_which_folder_the_server_reads() {
        let root = tempfile::tempdir().unwrap();
        let system = root.path().join("home/.ollama/models");
        let manager = manager_with(root.path(), Some(system.clone()));
        install(manager.model_dir(), MODEL_NAME, 5_200_000_100);
        install(&system, "llama3.1:8b", 4_700_000_100);

        assert_eq!(manager.chosen_model().as_deref(), Some(MODEL_NAME));
        assert_eq!(manager.active_model_dir(), manager.model_dir());

        manager.choose_model("llama3.1:8b").unwrap();
        assert_eq!(manager.chosen_model().as_deref(), Some("llama3.1:8b"));
        assert_eq!(manager.active_model_dir(), system);

        // A model that isn't installed is never chosen, and the choice already made stands.
        assert!(manager.choose_model("phantom:70b").is_err());
        assert_eq!(manager.chosen_model().as_deref(), Some("llama3.1:8b"));
    }

    /// A model the user chose in one folder must not make a download into the other folder look
    /// like a runtime that needs restarting: the status poll behind the download progress bar
    /// would tear down the server mid-download, every 750 ms.
    #[tokio::test]
    async fn a_download_is_not_interrupted_by_a_model_chosen_elsewhere() {
        let root = tempfile::tempdir().unwrap();
        let system = root.path().join("home/.ollama/models");
        let manager = manager_with(root.path(), Some(system.clone()));
        install(&system, "llama3.1:8b", 4_700_000_100);
        manager.choose_model("llama3.1:8b").unwrap();
        // A download serves Delve Planner's own folder while the chosen model lives in the other
        // one.
        assert_eq!(manager.active_model_dir(), system);
        assert_ne!(manager.model_dir(), system);

        // Nothing is running, so a status poll reports that and starts nothing.
        let agent = PlannerAgent::new("http://127.0.0.1:1", "llama3.1:8b");
        let status = manager.status(&agent).await;
        assert_eq!(status.phase, RuntimePhase::Stopped);
        assert!(!status.running);
        assert!(manager.record().server_pid.is_none());
    }

    #[tokio::test]
    async fn removing_model_data_never_touches_the_machines_own_models() {
        let root = tempfile::tempdir().unwrap();
        let system = root.path().join("home/.ollama/models");
        let manager = manager_with(root.path(), Some(system.clone()));
        install(manager.model_dir(), MODEL_NAME, 5_200_000_100);
        install(&system, "llama3.1:8b", 4_700_000_100);
        manager.choose_model("llama3.1:8b").unwrap();

        manager.remove_model().await.unwrap();

        assert!(manifest_path(&system, "llama3.1:8b").unwrap().is_file());
        assert!(!manifest_path(manager.model_dir(), MODEL_NAME)
            .unwrap()
            .is_file());
        assert_eq!(
            manager.installed_models().len(),
            1,
            "the machine's own model survives"
        );
    }

    #[test]
    fn model_storage_is_isolated() {
        let root = tempfile::tempdir().unwrap();
        let manager = OllamaRuntimeManager::new_with_port(
            root.path().join("resources"),
            root.path().join("data"),
            49_152,
            None,
        )
        .unwrap();
        assert_eq!(manager.model_dir(), root.path().join("data/ai-models"));
        assert_ne!(manager.endpoint(), "http://127.0.0.1:11434");
        assert_eq!(
            manager.binary_path(),
            root.path()
                .join("resources/resources/ollama")
                .join(platform_directory())
                .join(if cfg!(windows) {
                    "ollama.exe"
                } else {
                    "ollama"
                })
        );
    }
}
