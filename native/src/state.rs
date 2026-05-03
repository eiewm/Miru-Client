use crate::config::{load_config, read_secret, AppPaths, SecretKey};
use crate::error::AppResult;
use crate::osu::inspect_osu_stable_paths;
use crate::tools::ffmpeg_tools_snapshot;
use crate::types::{
    AppStatePayload, AutoRendererEvent, BenchmarkProgress, BenchmarkResult, HistoryEntry,
    RuntimeSnapshot, WatcherStatus, WorkerStatus,
};
use chrono::Utc;
use std::fs;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{oneshot, Mutex as AsyncMutex};
use uuid::Uuid;

pub struct ManagedState {
    pub paths: AppPaths,
    pub http: reqwest::Client,
    pub auth_refresh: AsyncMutex<()>,
    pub runtime: Mutex<RuntimeState>,
}

pub struct RuntimeState {
    pub watcher_status: WatcherStatus,
    pub last_auto_renderer_event: Option<AutoRendererEvent>,
    pub worker_status: WorkerStatus,
    pub active_job_id: Option<String>,
    pub benchmark: Option<BenchmarkProgress>,
    pub last_benchmark: Option<BenchmarkResult>,
    pub logs: Vec<String>,
    pub watcher_cancel: Option<oneshot::Sender<()>>,
    pub worker_cancel: Option<oneshot::Sender<()>>,
}

impl RuntimeState {
    fn new() -> Self {
        Self {
            watcher_status: WatcherStatus::Stopped,
            last_auto_renderer_event: None,
            worker_status: WorkerStatus::Disconnected,
            active_job_id: None,
            benchmark: None,
            last_benchmark: None,
            logs: Vec::new(),
            watcher_cancel: None,
            worker_cancel: None,
        }
    }
}

impl ManagedState {
    pub fn new() -> AppResult<Self> {
        Ok(Self {
            paths: AppPaths::new()?,
            http: reqwest::Client::builder()
                .user_agent(format!("MiruDesktopClient/{}", env!("CARGO_PKG_VERSION")))
                .build()?,
            auth_refresh: AsyncMutex::new(()),
            runtime: Mutex::new(RuntimeState::new()),
        })
    }
}

pub fn snapshot(app: &AppHandle) -> AppResult<AppStatePayload> {
    let state = app.state::<ManagedState>();
    let config = load_config(&state.paths)?;
    let osu_runtime = inspect_osu_stable_paths(&config);
    let has_session_secret = read_secret(&state.paths, SecretKey::ApiToken)?.is_some()
        || read_secret(&state.paths, SecretKey::RefreshToken)?.is_some();
    let is_authenticated = has_session_secret && !config.user_id.trim().is_empty();
    let history = load_history(&state.paths)?;
    let runtime = state.runtime.lock().expect("runtime state poisoned");
    let renderer_override_path = config.renderer_override_path.trim();
    let renderer_override_exists =
        !renderer_override_path.is_empty() && std::path::Path::new(renderer_override_path).exists();
    let managed_renderer_exists = state.paths.bin_dir.join("miru.exe").exists();

    Ok(AppStatePayload {
        runtime: RuntimeSnapshot {
            is_authenticated,
            watcher_status: runtime.watcher_status,
            osu_stable_detected: osu_runtime.osu_stable_detected,
            replay_dir_ready: osu_runtime.replay_dir_ready,
            stable_replay_dir_ready: osu_runtime.stable_replay_dir_ready,
            songs_dir_ready: osu_runtime.songs_dir_ready,
            osu_stable_root: osu_runtime
                .osu_stable_root
                .map(|path| path.display().to_string()),
            replay_dir: osu_runtime
                .replay_dir
                .map(|path| path.display().to_string()),
            stable_replay_dir: osu_runtime
                .stable_replay_dir
                .map(|path| path.display().to_string()),
            songs_dir: osu_runtime.songs_dir.map(|path| path.display().to_string()),
            last_auto_renderer_event: runtime.last_auto_renderer_event.clone(),
            renderer_installed: managed_renderer_exists || renderer_override_exists,
            worker_status: runtime.worker_status,
            active_job_id: runtime.active_job_id.clone(),
            benchmark: runtime.benchmark.clone(),
            last_benchmark: runtime.last_benchmark.clone(),
            ffmpeg_tools: ffmpeg_tools_snapshot(&state.paths),
        },
        config,
        history,
        logs: runtime.logs.clone(),
    })
}

pub fn emit_state(app: &AppHandle) {
    if let Ok(payload) = snapshot(app) {
        let _ = app.emit("app:state", payload);
    }
    let _ = crate::refresh_tray_menu(app);
}

pub fn set_worker_status(app: &AppHandle, status: WorkerStatus) {
    {
        let state = app.state::<ManagedState>();
        let mut runtime = state.runtime.lock().expect("runtime state poisoned");
        runtime.worker_status = status;
    }
    emit_state(app);
}

pub fn set_benchmark(app: &AppHandle, progress: Option<BenchmarkProgress>) {
    {
        let state = app.state::<ManagedState>();
        let mut runtime = state.runtime.lock().expect("runtime state poisoned");
        runtime.benchmark = progress;
    }
    emit_state(app);
}

pub fn set_last_auto_renderer_event(app: &AppHandle, event: Option<AutoRendererEvent>) {
    {
        let state = app.state::<ManagedState>();
        let mut runtime = state.runtime.lock().expect("runtime state poisoned");
        runtime.last_auto_renderer_event = event;
    }
    emit_state(app);
}

pub fn set_last_benchmark(app: &AppHandle, result: BenchmarkResult) {
    {
        let state = app.state::<ManagedState>();
        let mut runtime = state.runtime.lock().expect("runtime state poisoned");
        runtime.last_benchmark = Some(result);
    }
    emit_state(app);
}

pub fn set_active_job_id(app: &AppHandle, job_id: Option<String>) {
    {
        let state = app.state::<ManagedState>();
        let mut runtime = state.runtime.lock().expect("runtime state poisoned");
        runtime.active_job_id = job_id;
    }
    emit_state(app);
}

pub fn replace_worker_cancel(
    app: &AppHandle,
    cancel: Option<oneshot::Sender<()>>,
) -> Option<oneshot::Sender<()>> {
    let state = app.state::<ManagedState>();
    let mut runtime = state.runtime.lock().expect("runtime state poisoned");
    std::mem::replace(&mut runtime.worker_cancel, cancel)
}

pub fn log_line(app: &AppHandle, message: impl AsRef<str>) {
    let line = format!(
        "{} {}",
        Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        message.as_ref()
    );
    {
        let state = app.state::<ManagedState>();
        let mut runtime = state.runtime.lock().expect("runtime state poisoned");
        runtime.logs.push(line.clone());
        if runtime.logs.len() > 300 {
            let remove_count = runtime.logs.len() - 300;
            runtime.logs.drain(0..remove_count);
        }
        let _ = fs::create_dir_all(&state.paths.logs_dir);
        let log_path = state.paths.logs_dir.join("latest.log");
        let mut existing = fs::read_to_string(&log_path).unwrap_or_default();
        existing.push_str(&line);
        existing.push('\n');
        let _ = fs::write(log_path, existing);
    }
    let _ = app.emit("log", line);
    emit_state(app);
}

pub fn load_history(paths: &AppPaths) -> AppResult<Vec<HistoryEntry>> {
    if !paths.history_path.exists() {
        return Ok(Vec::new());
    }

    let raw = fs::read_to_string(&paths.history_path)?;
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

pub fn add_history(app: &AppHandle, mut entry: HistoryEntry) -> AppResult<()> {
    let state = app.state::<ManagedState>();
    let mut history = load_history(&state.paths)?;
    if entry.id.is_empty() {
        entry.id = Uuid::new_v4().to_string();
    }
    history.insert(0, entry);
    history.truncate(100);
    fs::create_dir_all(&state.paths.data_dir)?;
    fs::write(
        &state.paths.history_path,
        serde_json::to_string_pretty(&history)?,
    )?;
    emit_state(app);
    Ok(())
}

pub fn history_entry(
    kind: &str,
    title: &str,
    detail: &str,
    status: &str,
    url: Option<String>,
) -> HistoryEntry {
    HistoryEntry {
        id: Uuid::new_v4().to_string(),
        timestamp: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        kind: kind.to_string(),
        title: title.to_string(),
        detail: detail.to_string(),
        status: status.to_string(),
        url,
    }
}
