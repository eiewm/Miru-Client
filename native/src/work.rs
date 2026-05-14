use crate::auth::{ensure_fresh_session, require_auth_response, trim_trailing_slash};
use crate::bins::{ensure_renderer, renderer_download_info};
use crate::config::{load_config, read_secret, save_config, write_secret, SecretKey};
use crate::error::{AppError, AppResult};
use crate::state::{
    add_history, emit_state, history_entry, log_line, replace_worker_cancel, set_active_job_id,
    set_benchmark, set_last_benchmark, set_worker_status, ManagedState,
};
use crate::tools::{
    configure_ffmpeg_path, ffmpeg_download_size, ffmpeg_tool_info, install_ffmpeg_tools,
};
use crate::types::{
    AppConfig, BenchmarkDownloadPlan, BenchmarkProgress, BenchmarkResult, DownloadPlanItem,
    RegisterServerInput, WorkerHistoryEntry, WorkerStatsPayload, WorkerStatus,
};
use base64::{engine::general_purpose, Engine as _};
use futures_util::{SinkExt, StreamExt};
use reqwest::{
    header::{CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE},
    StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tokio::fs::{self, File};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot, watch, Mutex as AsyncMutex};
use tokio::time::{sleep, timeout, Instant as TokioInstant};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tokio_util::io::ReaderStream;
use uuid::Uuid;

const DEFAULT_MIN_DOWNLOAD_MBPS: f64 = 20.0;
const DEFAULT_MIN_UPLOAD_MBPS: f64 = 15.0;
const DEFAULT_MAX_RENDER_MS: u64 = 30 * 1000;
const DEFAULT_JOB_MAX_RENDER_MS: u64 = 10 * 60 * 1000;
const DEFAULT_REPLAY_SECONDS: u32 = 30;
const MAX_BENCHMARK_REPLAY_SECONDS: u32 = 30;
const DEFAULT_PUBLIC_SLOT_TOTAL: i64 = 5;
const WORKER_PROTOCOL_VERSION: u8 = 3;
const WORKER_CONNECT_TIMEOUT: Duration = Duration::from_secs(12);
const WORKER_SOCKET_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const WORKER_RECONNECT_INITIAL_DELAY: Duration = Duration::from_secs(10);
const WORKER_RECONNECT_MAX_DELAY: Duration = Duration::from_secs(5 * 60);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const MACHINE_ID_HEADER: &str = "x-miru-machine-id";
const MAX_SERVER_NAME_CHARS: usize = 18;

const REPLAY_LIMIT_BYTES: u64 = 64 * 1024 * 1024;
const BEATMAP_LIMIT_BYTES: u64 = 16 * 1024 * 1024;
const MAPSET_LIMIT_BYTES: u64 = 512 * 1024 * 1024;
const SKIN_LIMIT_BYTES: u64 = 128 * 1024 * 1024;
const INTRO_ASSET_LIMIT_BYTES: u64 = 16 * 1024 * 1024;
const HUD_ASSET_LIMIT_BYTES: u64 = 64 * 1024 * 1024;
const HUD_FONT_LIMIT_BYTES: u64 = 16 * 1024 * 1024;
const TOTAL_JOB_LIMIT_BYTES: u64 = 768 * 1024 * 1024;
const JSON_LIMIT_BYTES: usize = 512 * 1024;
const HUD_CONFIG_JSON_LIMIT_BYTES: usize = 32 * 1024 * 1024;
const RENDERER_OUTPUT_TAIL_BYTES: usize = 12 * 1024;
const RENDERER_OUTPUT_LINE_BYTES: usize = 2 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const FLAG_CDN_BASE_URL: &str = "https://flagcdn.com/h40";
const MIRU_AUTOPLAY_COUNTRY_CODE: &str = "EC";
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

type RendererOutputTail = Arc<AsyncMutex<String>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerSocketExit {
    Shutdown,
    Closed,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiResponse<T> {
    data: T,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegisterPayload {
    token: String,
    client_id: String,
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BenchmarkConfigPayload {
    min_download_mbps: f64,
    min_upload_mbps: f64,
    max_render_ms: u64,
    upload_bytes: u64,
    download_sources: Vec<BenchmarkDownloadSource>,
    upload_url: String,
    replay_url: Option<String>,
    mapset_url: Option<String>,
    replay_seconds: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BenchmarkDownloadSource {
    label: String,
    url: String,
    bytes: u64,
}

#[derive(Debug)]
struct SpeedMeasurement {
    download_mbps: f64,
    upload_mbps: f64,
    latency_ms: u64,
    bytes: u64,
    source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReplayIntegrityReport {
    has_summary_mismatch: bool,
}

#[derive(Debug)]
struct JobExecutionOutcome {
    duration_ms: u64,
    file_size: u64,
    replay_integrity: Option<ReplayIntegrityReport>,
}

#[derive(Debug)]
struct BenchmarkAssetPaths {
    replay_path: PathBuf,
    mapset_path: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerStatusResponse {
    registered: bool,
    status: String,
    slots: ServerSlots,
    worker: Option<ServerWorkerStatus>,
    compliance: Option<crate::types::WorkerComplianceSummary>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerSlots {
    slots: i64,
    total: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerWorkerStatus {
    client_id: String,
    name: Option<String>,
    is_online: bool,
    jobs_completed: u64,
    jobs_failed: u64,
    total_render_time_seconds: u64,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum WorkerRenderMode {
    Replay,
    Autoplay,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkerResolution {
    width: u32,
    height: u32,
    fps: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IntroUserAssignment {
    avatar_url: Option<String>,
    country_code: Option<String>,
    flag_url: Option<String>,
    team_badge_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobAssignment {
    id: String,
    attempt_id: Option<String>,
    attempt_number: Option<u32>,
    render_mode: WorkerRenderMode,
    replay_url: Option<String>,
    replay_extension: Option<String>,
    beatmap_url: Option<String>,
    beatmap_extension: Option<String>,
    mapset_url: Option<String>,
    mapset_extension: Option<String>,
    difficulty_index: Option<i64>,
    autoplay_mods: Option<Value>,
    skin_url: Option<String>,
    skin_extension: Option<String>,
    output_upload_url: String,
    output_storage_key: String,
    max_output_size_bytes: u64,
    max_render_duration_ms: Option<u64>,
    hud_config: Option<Value>,
    bg_opacity: Option<f64>,
    scroll_speed: Option<f64>,
    motion_blur_percent: Option<f64>,
    background_blur_percent: Option<f64>,
    background_video_enabled: Option<bool>,
    render_intro_enabled: Option<bool>,
    storyboard_enabled: Option<bool>,
    skin_animations_enabled: Option<bool>,
    combo_images_enabled: Option<bool>,
    music_volume: Option<f64>,
    hitsound_volume: Option<f64>,
    intro_user: Option<IntroUserAssignment>,
    resolution: WorkerResolution,
}

impl JobAssignment {
    fn attempt_id(&self) -> &str {
        self.attempt_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&self.id)
    }
}

#[derive(Debug)]
struct PreparedJobPaths {
    replay_path: Option<PathBuf>,
    beatmap_path: Option<PathBuf>,
    mapset_path: Option<PathBuf>,
    skin_path: Option<PathBuf>,
    hud_config_path: Option<PathBuf>,
    autoplay_mods_path: Option<PathBuf>,
    intro_user_json_path: Option<PathBuf>,
}

#[derive(Debug)]
struct ByteBudget {
    used_bytes: u64,
    max_bytes: u64,
}

#[derive(Debug)]
struct ActiveJobControl {
    job_id: String,
    cancel_tx: watch::Sender<bool>,
}

pub(crate) struct CleanupPaths {
    pub(crate) paths: Vec<PathBuf>,
}

impl Drop for CleanupPaths {
    fn drop(&mut self) {
        for path in &self.paths {
            if path.is_dir() {
                let _ = std::fs::remove_dir_all(path);
            } else {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

fn require_machine_id(config: &crate::types::AppConfig) -> AppResult<&str> {
    let machine_id = config.machine_id.trim();
    if machine_id.is_empty() {
        return Err(AppError::Config(
            "renderer machine id is missing; restart Miru and try again".to_string(),
        ));
    }
    Ok(machine_id)
}

fn append_machine_id_query(raw_url: &str, machine_id: &str) -> AppResult<String> {
    let mut url = url::Url::parse(raw_url)
        .map_err(|_| AppError::InvalidInput(format!("invalid URL: {raw_url}")))?;
    url.query_pairs_mut().append_pair("machineId", machine_id);
    Ok(url.to_string())
}

fn normalize_server_name(value: &str) -> AppResult<String> {
    let normalized = value.trim();
    if normalized.is_empty() || normalized.chars().count() > MAX_SERVER_NAME_CHARS {
        return Err(AppError::InvalidInput(format!(
            "server name must be 1-{MAX_SERVER_NAME_CHARS} characters"
        )));
    }
    Ok(normalized.to_string())
}

fn server_name_or_default(value: &str) -> AppResult<String> {
    let normalized = value.trim();
    if normalized.is_empty() {
        return Ok("Miru PC".to_string());
    }
    normalize_server_name(normalized)
}

pub async fn get_server_slots(app: &AppHandle) -> AppResult<i64> {
    let state = app.state::<ManagedState>();
    let config = load_config(&state.paths)?;
    let api_url = trim_trailing_slash(&config.api_url);
    let value: Value = state
        .http
        .get(format!("{api_url}/servers/slots"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    parse_slots_response(&value)
        .ok_or_else(|| AppError::InvalidInput("server slots response missing slots".to_string()))
}

pub async fn run_benchmark(app: AppHandle) -> AppResult<BenchmarkResult> {
    let token = ensure_fresh_session(&app)
        .await?
        .ok_or_else(|| AppError::Auth("login required to run benchmark".to_string()))?;
    let slots = get_server_slots(&app).await?;
    if slots <= 0 {
        return Err(AppError::Process(
            "No public renderer slots are available right now".to_string(),
        ));
    }
    ensure_ffmpeg_tools_available(&app).await?;

    set_benchmark(
        &app,
        Some(BenchmarkProgress {
            phase: "speed".to_string(),
            percent: 5.0,
            message: "Testing real connection speed".to_string(),
        }),
    );

    let benchmark_config = get_benchmark_config(&app, &token).await?;
    let speed = measure_speed(&app, &token, &benchmark_config).await?;
    let speed_eligible = speed.download_mbps >= benchmark_config.min_download_mbps
        && speed.upload_mbps >= benchmark_config.min_upload_mbps;

    set_benchmark(
        &app,
        Some(BenchmarkProgress {
            phase: "download".to_string(),
            percent: 20.0,
            message: if speed_eligible {
                format!(
                    "{:.1} Mbps down / {:.1} Mbps up",
                    speed.download_mbps, speed.upload_mbps
                )
            } else {
                format!(
                    "{:.1} Mbps down / {:.1} Mbps up; continuing render test",
                    speed.download_mbps, speed.upload_mbps
                )
            },
        }),
    );
    let renderer_path = ensure_renderer(&app).await?;
    let benchmark_assets = download_benchmark_assets(&app, &token, &benchmark_config).await?;

    set_benchmark(
        &app,
        Some(BenchmarkProgress {
            phase: "render".to_string(),
            percent: 35.0,
            message: "Running render benchmark".to_string(),
        }),
    );
    let replay_seconds = benchmark_config
        .replay_seconds
        .unwrap_or(DEFAULT_REPLAY_SECONDS)
        .clamp(1, MAX_BENCHMARK_REPLAY_SECONDS);
    let (render_time_ms, gpu_name) =
        render_benchmark(&app, renderer_path, benchmark_assets, replay_seconds).await?;
    let result = BenchmarkResult {
        render_time_ms,
        download_mbps: speed.download_mbps,
        upload_mbps: speed.upload_mbps,
        latency_ms: speed.latency_ms,
        speed_test_bytes: speed.bytes,
        benchmark_source: speed.source,
        max_render_ms: benchmark_config.max_render_ms,
        min_mbps: benchmark_config.min_download_mbps,
        min_upload_mbps: benchmark_config.min_upload_mbps,
        gpu_name,
    };
    let eligible = benchmark_meets_requirements(&result);

    {
        let state = app.state::<ManagedState>();
        let mut config = load_config(&state.paths)?;
        config.server_gpu = result.gpu_name.clone();
        save_config(&state.paths, &config)?;
    }

    set_last_benchmark(&app, result.clone());
    add_history(
        &app,
        history_entry(
            "benchmark",
            if eligible {
                "Benchmark passed"
            } else {
                "Benchmark completed but not eligible"
            },
            &format!(
                "{}ms | {:.1}/{:.1} Mbps | {}",
                result.render_time_ms, result.download_mbps, result.upload_mbps, result.gpu_name
            ),
            if eligible { "passed" } else { "failed" },
            None,
        ),
    )?;
    set_benchmark(
        &app,
        Some(BenchmarkProgress {
            phase: "done".to_string(),
            percent: 100.0,
            message: if eligible {
                "Benchmark passed".to_string()
            } else {
                "Benchmark complete; requirements not met".to_string()
            },
        }),
    );
    if eligible {
        log_line(&app, "Benchmark passed");
    } else {
        log_line(&app, "Benchmark completed but requirements were not met");
    }
    Ok(result)
}

pub async fn get_benchmark_download_plan(app: &AppHandle) -> AppResult<BenchmarkDownloadPlan> {
    let token = ensure_fresh_session(app)
        .await?
        .ok_or_else(|| AppError::Auth("login required to run benchmark".to_string()))?;
    let benchmark_config = get_benchmark_config(app, &token).await?;
    let renderer = renderer_download_info(app).await?;
    let state = app.state::<ManagedState>();
    let config = load_config(&state.paths)?;
    let api_url = trim_trailing_slash(&config.api_url);

    let mut items = Vec::new();
    items.push(DownloadPlanItem {
        name: "Miru renderer".to_string(),
        detail: if renderer.already_available {
            format!("Already available at {}", renderer.install_path.display())
        } else {
            format!(
                "Version {} from GitHub Releases to {}",
                renderer.version,
                renderer.install_path.display()
            )
        },
        size_bytes: Some(renderer.size_bytes),
        will_download: !renderer.already_available,
        status: if renderer.already_available {
            "local".to_string()
        } else {
            "download".to_string()
        },
    });

    let ffmpeg = ffmpeg_tool_info(&state.paths, "ffmpeg").await;
    let ffprobe = ffmpeg_tool_info(&state.paths, "ffprobe").await;
    let ffmpeg_available = ffmpeg.available && ffprobe.available;
    let detected_ffmpeg_size_bytes = [ffmpeg.size_bytes, ffprobe.size_bytes]
        .into_iter()
        .flatten()
        .sum::<u64>();
    let ffmpeg_download_size_bytes = if ffmpeg_available {
        Some(detected_ffmpeg_size_bytes).filter(|size| *size > 0)
    } else {
        ffmpeg_download_size(&state.http).await.unwrap_or(None)
    };
    items.push(DownloadPlanItem {
        name: "FFmpeg / FFprobe".to_string(),
        detail: ffmpeg_requirement_message(&state.paths, ffmpeg.available, ffprobe.available),
        size_bytes: ffmpeg_download_size_bytes,
        will_download: !ffmpeg_available,
        status: if ffmpeg_available {
            "local".to_string()
        } else {
            "download".to_string()
        },
    });

    let replay_url = benchmark_config
        .replay_url
        .as_deref()
        .unwrap_or("/api/v1/servers/benchmark/replay");
    items.push(DownloadPlanItem {
        name: "Benchmark replay".to_string(),
        detail: "Temporary .osr replay used only for the benchmark".to_string(),
        size_bytes: benchmark_content_length(app, &token, &api_url, replay_url)
            .await
            .unwrap_or(None),
        will_download: true,
        status: "download".to_string(),
    });

    let mapset_url = benchmark_config
        .mapset_url
        .as_deref()
        .unwrap_or("/api/v1/servers/benchmark/mapset");
    items.push(DownloadPlanItem {
        name: "Benchmark mapset".to_string(),
        detail: "Temporary .osz mapset used only for the benchmark render".to_string(),
        size_bytes: benchmark_content_length(app, &token, &api_url, mapset_url)
            .await
            .unwrap_or(None),
        will_download: true,
        status: "download".to_string(),
    });

    let total_download_bytes = items
        .iter()
        .filter(|item| item.will_download)
        .filter_map(|item| item.size_bytes)
        .sum();

    Ok(BenchmarkDownloadPlan {
        install_path: renderer.install_path.display().to_string(),
        release_url: (!renderer.release_url.trim().is_empty()).then_some(renderer.release_url),
        items,
        total_download_bytes,
    })
}

pub async fn register_server(app: AppHandle, input: RegisterServerInput) -> AppResult<()> {
    let name = normalize_server_name(&input.name)?;
    let token = ensure_fresh_session(&app)
        .await?
        .ok_or_else(|| AppError::Auth("login required to register server".to_string()))?;
    let state = app.state::<ManagedState>();
    let config = load_config(&state.paths)?;
    if config.user_id.trim().is_empty() {
        return Err(AppError::Auth(
            "current account id missing; log in again before registering".to_string(),
        ));
    }
    let slots = get_server_slots(&app).await?;
    if slots <= 0 && !config.is_server {
        return Err(AppError::Process(
            "No public renderer slots are available right now".to_string(),
        ));
    }
    let benchmark = {
        let runtime = state.runtime.lock().expect("runtime state poisoned");
        runtime.last_benchmark.clone()
    }
    .ok_or_else(|| {
        AppError::InvalidInput("run a benchmark before registering this PC".to_string())
    })?;
    if !benchmark_meets_requirements(&benchmark) {
        return Err(AppError::InvalidInput(
            "benchmark requirements were not met; this PC cannot be added as a renderer"
                .to_string(),
        ));
    }
    let api_url = trim_trailing_slash(&config.api_url);
    let machine_id = require_machine_id(&config)?;
    let response = state
        .http
        .post(format!("{api_url}/servers/register"))
        .bearer_auth(&token)
        .header(MACHINE_ID_HEADER, machine_id)
        .json(&json!({
            "machineId": machine_id,
            "name": name.as_str(),
            "benchmarkScore": benchmark.render_time_ms,
            "renderTimeMs": benchmark.render_time_ms,
            "downloadMbps": benchmark.download_mbps,
            "uploadMbps": benchmark.upload_mbps,
            "latencyMs": benchmark.latency_ms,
            "gpuName": benchmark.gpu_name,
            "benchmarkSource": benchmark.benchmark_source
        }))
        .send()
        .await?;
    let body: ApiResponse<RegisterPayload> =
        require_auth_response(&app, response).await?.json().await?;
    write_secret(&state.paths, SecretKey::WorkerToken, &body.data.token)?;
    let mut next_config = config;
    next_config.is_server = true;
    next_config.registered_user_id = next_config.user_id.clone();
    next_config.server_client_id = body.data.client_id.clone();
    next_config.server_status = body.data.status.unwrap_or_else(|| "ACTIVE".to_string());
    next_config.server_name = name;
    next_config.server_gpu = benchmark.gpu_name;
    save_config(&state.paths, &next_config)?;
    log_line(
        &app,
        format!("Registered server client {}", body.data.client_id),
    );
    Ok(())
}

pub async fn connect_worker(app: AppHandle) -> AppResult<()> {
    let token = ensure_fresh_session(&app)
        .await?
        .ok_or_else(|| AppError::Auth("login required to connect worker".to_string()))?;
    let state = app.state::<ManagedState>();
    let preferred_server_name = load_config(&state.paths)?.server_name;
    let status = refresh_server_status(&app, &token).await?;
    let config = load_config(&state.paths)?;
    if !status.registered || config.registered_user_id != config.user_id {
        write_secret(&state.paths, SecretKey::WorkerToken, "")?;
        return Err(AppError::Auth(
            "this account does not own an active renderer registration".to_string(),
        ));
    }
    if !config.is_server && read_secret(&state.paths, SecretKey::WorkerToken)?.is_none() {
        return Err(AppError::Auth(
            "worker token missing; register this PC first".to_string(),
        ));
    }
    let preferred_server_name = preferred_server_name.trim();
    if !preferred_server_name.is_empty() && preferred_server_name != config.server_name.trim() {
        let mut next_config = config.clone();
        next_config.server_name = normalize_server_name(preferred_server_name)?;
        sync_server_worker_settings(&app, &token, &next_config).await?;
        save_config(&state.paths, &next_config)?;
        emit_state(&app);
    }

    let reconnecting_stale_local_socket = {
        let runtime = state.runtime.lock().expect("runtime state poisoned");
        if matches!(runtime.worker_status, WorkerStatus::Connecting) {
            return Ok(());
        }
        if matches!(runtime.worker_status, WorkerStatus::Connected) && status.is_online {
            return Ok(());
        }
        matches!(runtime.worker_status, WorkerStatus::Connected) && !status.is_online
    };
    if reconnecting_stale_local_socket {
        log_line(
            &app,
            "Worker looked connected locally but server marked it offline; reconnecting",
        );
    }

    let worker_token = renew_worker_token(&app, &token).await?;
    write_secret(&state.paths, SecretKey::WorkerToken, &worker_token)?;

    let (cancel_tx, cancel_rx) = oneshot::channel();
    if let Some(previous) = replace_worker_cancel(&app, Some(cancel_tx)) {
        let _ = previous.send(());
    }

    set_worker_status(&app, WorkerStatus::Connecting);
    let (ready_tx, ready_rx) = oneshot::channel::<Result<(), String>>();
    let app_for_task = app.clone();
    tauri::async_runtime::spawn(async move {
        let result =
            worker_socket_loop(app_for_task.clone(), worker_token, cancel_rx, ready_tx).await;
        replace_worker_cancel(&app_for_task, None);
        set_active_job_id(&app_for_task, None);
        match result {
            Ok(WorkerSocketExit::Shutdown) => {
                set_worker_status(&app_for_task, WorkerStatus::Disconnected);
            }
            Ok(WorkerSocketExit::Closed) => {
                log_line(&app_for_task, "Worker socket closed by remote");
                set_worker_status(&app_for_task, WorkerStatus::Disconnected);
                schedule_worker_reconnect(app_for_task.clone(), "socket closed");
            }
            Err(err) => {
                log_line(&app_for_task, format!("Worker socket error: {err}"));
                set_worker_status(&app_for_task, WorkerStatus::Error);
                schedule_worker_reconnect(app_for_task.clone(), "socket error");
            }
        }
    });

    match timeout(WORKER_CONNECT_TIMEOUT, ready_rx).await {
        Ok(Ok(Ok(()))) => {
            let mut next_config = load_config(&state.paths)?;
            next_config.server_auto_reconnect = true;
            save_config(&state.paths, &next_config)?;
            Ok(())
        }
        Ok(Ok(Err(message))) => Err(AppError::Process(message)),
        Ok(Err(_)) => Err(AppError::Process(
            "worker socket startup failed".to_string(),
        )),
        Err(_) => {
            if let Some(cancel) = replace_worker_cancel(&app, None) {
                let _ = cancel.send(());
            }
            set_worker_status(&app, WorkerStatus::Error);
            Err(AppError::Process(
                "worker socket connection timeout".to_string(),
            ))
        }
    }
}

pub async fn disconnect_worker(app: AppHandle) -> AppResult<()> {
    if let Some(cancel) = replace_worker_cancel(&app, None) {
        let _ = cancel.send(());
    }

    let token = ensure_fresh_session(&app).await?;
    let state = app.state::<ManagedState>();
    let mut config = load_config(&state.paths)?;
    if let Some(token) = token {
        let api_url = trim_trailing_slash(&config.api_url);
        let machine_id = require_machine_id(&config)?;
        let _ = state
            .http
            .patch(format!("{api_url}/servers/me"))
            .bearer_auth(token)
            .header(MACHINE_ID_HEADER, machine_id)
            .json(&json!({
                "machineId": machine_id,
                "connected": false
            }))
            .send()
            .await;
    }
    config.server_auto_reconnect = false;
    save_config(&state.paths, &config)?;
    set_active_job_id(&app, None);
    set_worker_status(&app, WorkerStatus::Disconnected);
    log_line(&app, "Worker disconnected");
    Ok(())
}

fn should_auto_reconnect_worker(app: &AppHandle) -> bool {
    let state = app.state::<ManagedState>();
    let Ok(config) = load_config(&state.paths) else {
        return false;
    };

    config.server_auto_reconnect
        && config.connect_worker_on_launch
        && config.is_server
        && config.registered_user_id == config.user_id
}

fn schedule_worker_reconnect(app: AppHandle, reason: &'static str) {
    tauri::async_runtime::spawn(async move {
        let mut delay = WORKER_RECONNECT_INITIAL_DELAY;
        loop {
            if !should_auto_reconnect_worker(&app) {
                break;
            }

            log_line(
                &app,
                format!(
                    "Worker will reconnect in {}s after {reason}",
                    delay.as_secs()
                ),
            );
            sleep(delay).await;

            if !should_auto_reconnect_worker(&app) {
                break;
            }

            match connect_worker(app.clone()).await {
                Ok(()) => {
                    log_line(&app, "Worker reconnect attempt completed");
                    break;
                }
                Err(error) => {
                    log_line(&app, format!("Worker reconnect failed: {error}"));
                    delay = (delay * 2).min(WORKER_RECONNECT_MAX_DELAY);
                }
            }
        }
    });
}

pub async fn sync_server_worker_settings(
    app: &AppHandle,
    api_token: &str,
    config: &AppConfig,
) -> AppResult<()> {
    let state = app.state::<ManagedState>();
    let api_url = trim_trailing_slash(&config.api_url);
    let machine_id = require_machine_id(config)?;
    let response = state
        .http
        .patch(format!("{api_url}/servers/me"))
        .bearer_auth(api_token)
        .header(MACHINE_ID_HEADER, machine_id)
        .json(&json!({
            "machineId": machine_id,
            "name": server_name_or_default(&config.server_name)?,
            "showDiscordRendererRole": config.show_discord_renderer_role,
            "showGpuInStatusImage": config.show_gpu_in_status_image,
        }))
        .send()
        .await?;
    require_auth_response(app, response).await?;
    Ok(())
}

pub async fn remove_server(app: AppHandle) -> AppResult<()> {
    if let Some(cancel) = replace_worker_cancel(&app, None) {
        let _ = cancel.send(());
    }

    let token = ensure_fresh_session(&app)
        .await?
        .ok_or_else(|| AppError::Auth("login required to remove server".to_string()))?;
    let state = app.state::<ManagedState>();
    let mut config = load_config(&state.paths)?;
    let api_url = trim_trailing_slash(&config.api_url);
    let machine_id = require_machine_id(&config)?;
    let status_url = append_machine_id_query(&format!("{api_url}/servers/me"), machine_id)?;
    let response = state
        .http
        .delete(status_url)
        .bearer_auth(token)
        .header(MACHINE_ID_HEADER, machine_id)
        .send()
        .await?;
    require_auth_response(&app, response).await?;
    write_secret(&state.paths, SecretKey::WorkerToken, "")?;
    config.is_server = false;
    config.registered_user_id.clear();
    config.server_client_id.clear();
    config.server_status.clear();
    config.server_auto_reconnect = false;
    save_config(&state.paths, &config)?;
    set_active_job_id(&app, None);
    set_worker_status(&app, WorkerStatus::Disconnected);
    log_line(&app, "Server registration removed");
    Ok(())
}

async fn renew_worker_token(app: &AppHandle, api_token: &str) -> AppResult<String> {
    let state = app.state::<ManagedState>();
    let config = load_config(&state.paths)?;
    let api_url = trim_trailing_slash(&config.api_url);
    let machine_id = require_machine_id(&config)?;
    let renew_url = append_machine_id_query(&format!("{api_url}/servers/me/token"), machine_id)?;
    let response = state
        .http
        .post(renew_url)
        .bearer_auth(api_token)
        .header(MACHINE_ID_HEADER, machine_id)
        .send()
        .await?;
    let body: ApiResponse<RegisterPayload> =
        require_auth_response(app, response).await?.json().await?;
    Ok(body.data.token)
}

pub async fn refresh_server_status(
    app: &AppHandle,
    api_token: &str,
) -> AppResult<WorkerStatsPayload> {
    let state = app.state::<ManagedState>();
    let mut config = load_config(&state.paths)?;
    let api_url = trim_trailing_slash(&config.api_url);
    let machine_id = require_machine_id(&config)?;
    let status_url = append_machine_id_query(&format!("{api_url}/servers/me/status"), machine_id)?;
    let response = state
        .http
        .get(status_url)
        .bearer_auth(api_token)
        .header(MACHINE_ID_HEADER, machine_id)
        .send()
        .await?;
    if response.status() == StatusCode::NOT_FOUND {
        log_line(
            app,
            "Renderer status endpoint is not available on this API version; using local status fallback",
        );
        return legacy_worker_stats(app).await;
    }
    let body: ApiResponse<ServerStatusResponse> =
        require_auth_response(app, response).await?.json().await?;

    let worker = body.data.worker;
    let registered = body.data.registered && body.data.status == "ACTIVE";
    if registered {
        if let Some(worker) = worker.as_ref() {
            config.is_server = true;
            config.registered_user_id = config.user_id.clone();
            config.server_client_id = worker.client_id.clone();
            config.server_status = body.data.status.clone();
            config.server_name = worker
                .name
                .clone()
                .unwrap_or_else(|| config.server_name.clone());
        }
    } else {
        config.is_server = false;
        config.registered_user_id.clear();
        config.server_client_id.clear();
        config.server_status = body.data.status.clone();
        config.server_auto_reconnect = false;
        write_secret(&state.paths, SecretKey::WorkerToken, "")?;
    }
    save_config(&state.paths, &config)?;
    emit_state(app);

    Ok(WorkerStatsPayload {
        registered,
        status: body.data.status,
        is_online: worker.as_ref().is_some_and(|value| value.is_online),
        name: worker
            .as_ref()
            .and_then(|value| value.name.clone())
            .unwrap_or_else(|| config.server_name.clone()),
        client_id: worker.as_ref().map(|value| value.client_id.clone()),
        jobs_completed: worker.as_ref().map_or(0, |value| value.jobs_completed),
        jobs_failed: worker.as_ref().map_or(0, |value| value.jobs_failed),
        total_render_time_seconds: worker
            .as_ref()
            .map_or(0, |value| value.total_render_time_seconds),
        slots_available: body.data.slots.slots,
        slots_total: body.data.slots.total,
        compliance: body.data.compliance,
    })
}

pub async fn get_worker_stats(app: AppHandle) -> AppResult<WorkerStatsPayload> {
    let token = ensure_fresh_session(&app)
        .await?
        .ok_or_else(|| AppError::Auth("login required to load worker stats".to_string()))?;
    refresh_server_status(&app, &token).await
}

pub async fn get_worker_history(app: AppHandle) -> AppResult<Vec<WorkerHistoryEntry>> {
    let token = ensure_fresh_session(&app)
        .await?
        .ok_or_else(|| AppError::Auth("login required to load worker history".to_string()))?;
    let state = app.state::<ManagedState>();
    let config = load_config(&state.paths)?;
    let api_url = trim_trailing_slash(&config.api_url);
    let machine_id = require_machine_id(&config)?;
    let history_url = append_machine_id_query(
        &format!("{api_url}/servers/me/history?limit=80"),
        machine_id,
    )?;
    let response = state
        .http
        .get(history_url)
        .bearer_auth(token)
        .header(MACHINE_ID_HEADER, machine_id)
        .send()
        .await?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }
    let body: ApiResponse<Vec<WorkerHistoryEntry>> =
        require_auth_response(&app, response).await?.json().await?;
    Ok(body.data)
}

async fn legacy_worker_stats(app: &AppHandle) -> AppResult<WorkerStatsPayload> {
    let state = app.state::<ManagedState>();
    let config = load_config(&state.paths)?;
    let slots = get_server_slots(app).await.unwrap_or(0);
    let registered = config.is_server
        && !config.user_id.trim().is_empty()
        && config.registered_user_id == config.user_id;

    Ok(WorkerStatsPayload {
        registered,
        status: if registered { "ACTIVE" } else { "UNKNOWN" }.to_string(),
        is_online: false,
        name: config.server_name,
        client_id: (!config.server_client_id.trim().is_empty()).then_some(config.server_client_id),
        jobs_completed: 0,
        jobs_failed: 0,
        total_render_time_seconds: 0,
        slots_available: slots,
        slots_total: DEFAULT_PUBLIC_SLOT_TOTAL,
        compliance: None,
    })
}

fn parse_slots_response(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.get("slots").and_then(Value::as_i64))
        .or_else(|| value.get("available").and_then(Value::as_i64))
        .or_else(|| value.pointer("/data/slots").and_then(Value::as_i64))
        .or_else(|| value.pointer("/data/available").and_then(Value::as_i64))
}

async fn worker_socket_loop(
    app: AppHandle,
    worker_token: String,
    mut shutdown: oneshot::Receiver<()>,
    ready_tx: oneshot::Sender<Result<(), String>>,
) -> AppResult<WorkerSocketExit> {
    let state = app.state::<ManagedState>();
    let config = load_config(&state.paths)?;
    let socket_url = worker_socket_url(&config.api_url)?;
    let machine_name = if config.server_name.trim().is_empty() {
        "Miru Desktop".to_string()
    } else {
        config.server_name.clone()
    };
    let gpu = normalize_gpu_name(&config.server_gpu).unwrap_or_else(|| "Unknown".to_string());
    let os = detect_os();
    let auth = json!({
        "token": worker_token,
        "machineName": machine_name,
        "gpu": gpu,
        "os": os,
        "protocolVersion": WORKER_PROTOCOL_VERSION
    });

    let (socket, _) = connect_async(socket_url.as_str()).await?;
    let (mut write, mut read) = socket.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let writer = tauri::async_runtime::spawn(async move {
        while let Some(packet) = out_rx.recv().await {
            write.send(Message::Text(packet.into())).await?;
        }
        Ok::<(), tokio_tungstenite::tungstenite::Error>(())
    });

    let active_job: Arc<AsyncMutex<Option<ActiveJobControl>>> = Arc::new(AsyncMutex::new(None));
    let mut ready_tx = Some(ready_tx);
    let idle_timeout = tokio::time::sleep(WORKER_SOCKET_IDLE_TIMEOUT);
    tokio::pin!(idle_timeout);

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                cancel_active_job(&active_job).await;
                drop(out_tx);
                writer.abort();
                return Ok(WorkerSocketExit::Shutdown);
            }
            _ = &mut idle_timeout => {
                cancel_active_job(&active_job).await;
                drop(out_tx);
                writer.abort();
                return Err(AppError::Process(format!(
                    "worker socket idle timeout after {}s",
                    WORKER_SOCKET_IDLE_TIMEOUT.as_secs()
                )));
            }
            message = read.next() => {
                idle_timeout.as_mut().reset(TokioInstant::now() + WORKER_SOCKET_IDLE_TIMEOUT);
                let Some(message) = message else {
                    cancel_active_job(&active_job).await;
                    drop(out_tx);
                    writer.abort();
                    return Ok(WorkerSocketExit::Closed);
                };
                let message = message?;
                match message {
                    Message::Text(text) => {
                        let text = text.to_string();
                        if text.starts_with('0') {
                            queue_raw_packet(&out_tx, format!("40/workers,{auth}"))?;
                            continue;
                        }
                        if text == "2" {
                            queue_raw_packet(&out_tx, "3".to_string())?;
                            continue;
                        }
                        if text.starts_with("40/workers") {
                            set_worker_status(&app, WorkerStatus::Connected);
                            if let Some(tx) = ready_tx.take() {
                                let _ = tx.send(Ok(()));
                            }
                            queue_socket_event_no_payload(&out_tx, "worker:ready")?;
                            continue;
                        }
                        if let Some(message) = parse_socket_error(&text) {
                            if let Some(tx) = ready_tx.take() {
                                let _ = tx.send(Err(message.clone()));
                            }
                            return Err(AppError::Process(message));
                        }
                        if let Some((event, payload)) = parse_socket_event(&text) {
                            handle_worker_socket_event(
                                &app,
                                &out_tx,
                                active_job.clone(),
                                worker_token.clone(),
                                event,
                                payload,
                            ).await?;
                        }
                    }
                    Message::Ping(_) => {}
                    Message::Close(_) => {
                        cancel_active_job(&active_job).await;
                        drop(out_tx);
                        writer.abort();
                        return Ok(WorkerSocketExit::Closed);
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn handle_worker_socket_event(
    app: &AppHandle,
    out_tx: &mpsc::UnboundedSender<String>,
    active_job: Arc<AsyncMutex<Option<ActiveJobControl>>>,
    worker_token: String,
    event: String,
    payload: Value,
) -> AppResult<()> {
    match event.as_str() {
        "job:assigned" => {
            let job: JobAssignment = serde_json::from_value(payload)?;
            let attempt_id = job.attempt_id().to_string();
            let mut guard = active_job.lock().await;
            if guard.is_some() {
                queue_worker_error(
                    out_tx,
                    &job.id,
                    &attempt_id,
                    "Worker is already rendering another job",
                )?;
                return Ok(());
            }

            let (cancel_tx, cancel_rx) = watch::channel(false);
            *guard = Some(ActiveJobControl {
                job_id: job.id.clone(),
                cancel_tx,
            });
            drop(guard);

            set_active_job_id(app, Some(job.id.clone()));
            log_line(app, format!("Worker job assigned {}", job.id));
            let app_for_job = app.clone();
            let out_for_job = out_tx.clone();
            let active_for_job = active_job.clone();
            tauri::async_runtime::spawn(async move {
                let job_id = job.id.clone();
                let result = handle_job(
                    app_for_job.clone(),
                    out_for_job.clone(),
                    worker_token,
                    job,
                    cancel_rx,
                )
                .await;
                {
                    let mut guard = active_for_job.lock().await;
                    if guard.as_ref().map(|active| active.job_id.as_str()) == Some(job_id.as_str())
                    {
                        *guard = None;
                    }
                }
                set_active_job_id(&app_for_job, None);
                let _ = queue_socket_event_no_payload(&out_for_job, "worker:ready");
                if let Err(err) = result {
                    log_line(&app_for_job, format!("Worker job handler error: {err}"));
                }
            });
        }
        "job:cancel" => {
            let job_id = payload
                .get("jobId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let guard = active_job.lock().await;
            if let Some(active) = guard.as_ref().filter(|active| active.job_id == job_id) {
                let _ = active.cancel_tx.send(true);
                log_line(app, format!("Worker job cancel requested {job_id}"));
            }
        }
        _ => {}
    }

    Ok(())
}

async fn cancel_active_job(active_job: &Arc<AsyncMutex<Option<ActiveJobControl>>>) {
    let guard = active_job.lock().await;
    if let Some(active) = guard.as_ref() {
        let _ = active.cancel_tx.send(true);
    }
}

async fn handle_job(
    app: AppHandle,
    out_tx: mpsc::UnboundedSender<String>,
    worker_token: String,
    job: JobAssignment,
    cancel_rx: watch::Receiver<bool>,
) -> AppResult<()> {
    let attempt_id = job.attempt_id().to_string();
    let heartbeat = spawn_job_heartbeat(out_tx.clone(), job.clone());
    let outcome = execute_job(&app, &out_tx, &worker_token, &job, cancel_rx).await;
    heartbeat.abort();

    match outcome {
        Ok(outcome) => {
            queue_worker_complete(
                &out_tx,
                &job.id,
                &attempt_id,
                &job.output_storage_key,
                outcome.duration_ms,
                outcome.file_size,
                outcome.replay_integrity.as_ref(),
            )?;
            add_history(
                &app,
                history_entry(
                    "worker",
                    "Worker job completed",
                    &format!(
                        "{}ms | {:.1} MB",
                        outcome.duration_ms,
                        outcome.file_size as f64 / (1024.0 * 1024.0)
                    ),
                    "completed",
                    None,
                ),
            )?;
            log_line(&app, format!("Worker job completed {}", job.id));
        }
        Err(err) if err.to_string() == "process error: Render cancelled" => {
            log_line(&app, format!("Worker job cancelled {}", job.id));
        }
        Err(err) => {
            let message = err.to_string();
            queue_worker_error(&out_tx, &job.id, &attempt_id, &message)?;
            add_history(
                &app,
                history_entry("worker", "Worker job failed", &message, "failed", None),
            )?;
            log_line(&app, format!("Worker job failed {}: {message}", job.id));
        }
    }

    Ok(())
}

async fn execute_job(
    app: &AppHandle,
    out_tx: &mpsc::UnboundedSender<String>,
    worker_token: &str,
    job: &JobAssignment,
    cancel_rx: watch::Receiver<bool>,
) -> AppResult<JobExecutionOutcome> {
    validate_job_assignment(job)?;
    let renderer_path = ensure_renderer(app).await?;
    ensure_ffmpeg_tools_available(app).await?;
    let state = app.state::<ManagedState>();
    let config = load_config(&state.paths)?;
    let api_url = config.api_url.clone();
    let job_dir = std::env::temp_dir()
        .join("miru-worker")
        .join(safe_path_component(&job.id));
    match fs::remove_dir_all(&job_dir).await {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(AppError::Io(err)),
    }
    fs::create_dir_all(&job_dir).await?;

    let output_path = job_dir.join("output.mp4");
    let replay_path = job_dir.join(format!(
        "replay{}",
        resolve_file_extension(job.replay_extension.as_deref(), ".osr")
    ));
    let beatmap_path = job_dir.join(format!(
        "beatmap{}",
        resolve_file_extension(job.beatmap_extension.as_deref(), ".osu")
    ));
    let mapset_path = job_dir.join(format!(
        "mapset{}",
        resolve_file_extension(job.mapset_extension.as_deref(), ".osz")
    ));
    let skin_path = job_dir.join(format!(
        "skin{}",
        resolve_file_extension(job.skin_extension.as_deref(), ".osk")
    ));
    let hud_config_path = job_dir.join("hud-config.json");
    let autoplay_mods_path = job_dir.join("autoplay-mods.json");
    let intro_user_json_path = job_dir.join("intro-user.json");
    let replay_integrity_report_path = job_dir.join("render-report.json");
    let _cleanup = CleanupPaths {
        paths: vec![
            replay_path.clone(),
            beatmap_path.clone(),
            mapset_path.clone(),
            skin_path.clone(),
            hud_config_path.clone(),
            autoplay_mods_path.clone(),
            intro_user_json_path.clone(),
            replay_integrity_report_path.clone(),
            output_path.clone(),
            job_dir.clone(),
        ],
    };

    let mut budget = ByteBudget {
        used_bytes: 0,
        max_bytes: TOTAL_JOB_LIMIT_BYTES,
    };
    let mut cancel_rx = cancel_rx;
    queue_worker_progress(
        out_tx,
        job,
        0_u64,
        "Descargando archivos...",
        "input_download",
    )?;

    let mut paths = PreparedJobPaths {
        replay_path: None,
        beatmap_path: None,
        mapset_path: None,
        skin_path: None,
        hud_config_path: None,
        autoplay_mods_path: None,
        intro_user_json_path: None,
    };

    if job.render_intro_enabled != Some(false) {
        if let Some(path) = prepare_intro_user_json(
            app,
            job,
            &job_dir,
            &intro_user_json_path,
            &api_url,
            &mut budget,
            &mut cancel_rx,
        )
        .await?
        {
            paths.intro_user_json_path = Some(path);
        }
    }

    if job.render_mode == WorkerRenderMode::Autoplay {
        let mapset_url = job.mapset_url.as_deref().expect("validated mapset URL");
        download_file_with_limits(
            app,
            mapset_url,
            &mapset_path,
            &api_url,
            MAPSET_LIMIT_BYTES,
            &mut budget,
            &mut cancel_rx,
        )
        .await?;
        paths.mapset_path = Some(mapset_path.clone());
    } else {
        let replay_url = job.replay_url.as_deref().expect("validated replay URL");
        download_file_with_limits(
            app,
            replay_url,
            &replay_path,
            &api_url,
            REPLAY_LIMIT_BYTES,
            &mut budget,
            &mut cancel_rx,
        )
        .await?;
        paths.replay_path = Some(replay_path.clone());

        if let Some(mapset_url) = job.mapset_url.as_deref() {
            download_file_with_limits(
                app,
                mapset_url,
                &mapset_path,
                &api_url,
                MAPSET_LIMIT_BYTES,
                &mut budget,
                &mut cancel_rx,
            )
            .await?;
            paths.mapset_path = Some(mapset_path.clone());
        } else if let Some(beatmap_url) = job.beatmap_url.as_deref() {
            download_file_with_limits(
                app,
                beatmap_url,
                &beatmap_path,
                &api_url,
                BEATMAP_LIMIT_BYTES,
                &mut budget,
                &mut cancel_rx,
            )
            .await?;
            paths.beatmap_path = Some(beatmap_path.clone());
        }
    }

    if let Some(skin_url) = job.skin_url.as_deref() {
        download_file_with_limits(
            app,
            skin_url,
            &skin_path,
            &api_url,
            SKIN_LIMIT_BYTES,
            &mut budget,
            &mut cancel_rx,
        )
        .await?;
        paths.skin_path = Some(skin_path.clone());
    }

    if let Some(hud_config) = job.hud_config.as_ref() {
        let prepared_hud_config = prepare_hud_config_json(
            app,
            hud_config,
            &job_dir,
            &api_url,
            &mut budget,
            &mut cancel_rx,
        )
        .await?;
        write_json_file_with_limit(
            &hud_config_path,
            &prepared_hud_config,
            "HUD config",
            HUD_CONFIG_JSON_LIMIT_BYTES,
        )
        .await?;
        paths.hud_config_path = Some(hud_config_path.clone());
    }

    if job.render_mode == WorkerRenderMode::Autoplay {
        write_json_file_with_limit(
            &autoplay_mods_path,
            job.autoplay_mods
                .as_ref()
                .unwrap_or(&Value::Object(Default::default())),
            "Autoplay mods config",
            JSON_LIMIT_BYTES,
        )
        .await?;
        paths.autoplay_mods_path = Some(autoplay_mods_path.clone());
    }

    queue_worker_progress(out_tx, job, 5_u64, "Preparando render...", "input_download")?;
    let render_started = Instant::now();
    render_with_cli(
        app,
        out_tx,
        renderer_path,
        job,
        &paths,
        &output_path,
        &job_dir,
        (job.render_mode == WorkerRenderMode::Replay)
            .then_some(replay_integrity_report_path.as_path()),
        cancel_rx.clone(),
        render_timeout_for_job(job),
    )
    .await?;
    let duration_ms = render_started.elapsed().as_millis() as u64;
    let replay_integrity = if job.render_mode == WorkerRenderMode::Replay {
        match read_replay_integrity_report(&replay_integrity_report_path).await {
            Ok(report) => report,
            Err(err) => {
                log_line(
                    app,
                    format!("Replay integrity report unavailable for {}: {err}", job.id),
                );
                None
            }
        }
    } else {
        None
    };

    queue_worker_progress(out_tx, job, 95_u64, "Subiendo video...", "output_upload")?;
    let file_size = upload_file_with_limits(
        app,
        &output_path,
        &job.output_upload_url,
        &api_url,
        worker_token,
        job.attempt_id(),
        job.max_output_size_bytes,
        &mut cancel_rx,
    )
    .await?;
    Ok(JobExecutionOutcome {
        duration_ms,
        file_size,
        replay_integrity,
    })
}

async fn get_benchmark_config(app: &AppHandle, token: &str) -> AppResult<BenchmarkConfigPayload> {
    let state = app.state::<ManagedState>();
    let config = load_config(&state.paths)?;
    let api_url = trim_trailing_slash(&config.api_url);
    let response = state
        .http
        .get(format!("{api_url}/servers/benchmark/config"))
        .bearer_auth(token)
        .send()
        .await?;
    if response.status() == StatusCode::NOT_FOUND {
        log_line(
            app,
            "Benchmark config endpoint is not available on this API version; using legacy speed-test fallback",
        );
        return Ok(legacy_benchmark_config());
    }
    let body: ApiResponse<BenchmarkConfigPayload> =
        require_auth_response(app, response).await?.json().await?;
    let mut payload = body.data;
    if payload.download_sources.is_empty() {
        payload.download_sources.push(BenchmarkDownloadSource {
            label: "Miru API".to_string(),
            url: format!("/servers/benchmark/speed-test?bytes={}", 32 * 1024 * 1024),
            bytes: 32 * 1024 * 1024,
        });
    }
    apply_benchmark_requirement_overrides(&mut payload);
    if payload.max_render_ms == 0 {
        payload.max_render_ms = DEFAULT_MAX_RENDER_MS;
    }
    if payload
        .replay_url
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        payload.replay_url = Some("/api/v1/servers/benchmark/replay".to_string());
    }
    if payload
        .mapset_url
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        payload.mapset_url = Some("/api/v1/servers/benchmark/mapset".to_string());
    }
    Ok(payload)
}

fn apply_benchmark_requirement_overrides(payload: &mut BenchmarkConfigPayload) {
    payload.min_download_mbps = DEFAULT_MIN_DOWNLOAD_MBPS;
    payload.min_upload_mbps = DEFAULT_MIN_UPLOAD_MBPS;
}

fn legacy_benchmark_config() -> BenchmarkConfigPayload {
    BenchmarkConfigPayload {
        min_download_mbps: DEFAULT_MIN_DOWNLOAD_MBPS,
        min_upload_mbps: DEFAULT_MIN_UPLOAD_MBPS,
        max_render_ms: DEFAULT_MAX_RENDER_MS,
        upload_bytes: 0,
        download_sources: vec![BenchmarkDownloadSource {
            label: "Miru API legacy".to_string(),
            url: "/api/v1/servers/benchmark/speed-test?duration=12".to_string(),
            bytes: 32 * 1024 * 1024,
        }],
        upload_url: String::new(),
        replay_url: Some("/api/v1/servers/benchmark/replay".to_string()),
        mapset_url: Some("/api/v1/servers/benchmark/mapset".to_string()),
        replay_seconds: Some(DEFAULT_REPLAY_SECONDS),
    }
}

async fn benchmark_content_length(
    app: &AppHandle,
    token: &str,
    api_url: &str,
    raw_url: &str,
) -> AppResult<Option<u64>> {
    let url = resolve_worker_url(raw_url, api_url)?;
    let response = send_benchmark_metadata_request(app, token, api_url, &url, true, false).await?;
    if !matches!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_FOUND
    ) {
        if let Some(size) = response_declared_content_size(app, api_url, &url, response).await? {
            return Ok(Some(size));
        }
    }

    let response = send_benchmark_metadata_request(app, token, api_url, &url, false, true).await?;
    if matches!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE) {
        return Ok(content_range_total(response.headers().get(CONTENT_RANGE)));
    }
    if let Some(size) = response_declared_content_size(app, api_url, &url, response).await? {
        return Ok(Some(size));
    }

    let response = send_benchmark_metadata_request(app, token, api_url, &url, false, false).await?;
    response_declared_content_size(app, api_url, &url, response).await
}

async fn send_benchmark_metadata_request(
    app: &AppHandle,
    token: &str,
    api_url: &str,
    url: &str,
    head_only: bool,
    range_probe: bool,
) -> AppResult<reqwest::Response> {
    let state = app.state::<ManagedState>();
    let mut request = if head_only {
        state.http.head(url)
    } else {
        state.http.get(url)
    };
    if is_same_origin(url, api_url) {
        request = request.bearer_auth(token);
    }
    if range_probe {
        request = request.header(RANGE, "bytes=0-0");
    }
    Ok(request.send().await?)
}

async fn response_declared_content_size(
    app: &AppHandle,
    api_url: &str,
    url: &str,
    response: reqwest::Response,
) -> AppResult<Option<u64>> {
    let response = if is_same_origin(url, api_url) {
        require_auth_response(app, response).await?
    } else {
        response.error_for_status()?
    };
    if matches!(
        response.status(),
        StatusCode::PARTIAL_CONTENT | StatusCode::RANGE_NOT_SATISFIABLE
    ) {
        if let Some(size) = content_range_total(response.headers().get(CONTENT_RANGE)) {
            return Ok(Some(size));
        }
    }
    Ok(response.content_length().filter(|size| *size > 0))
}

fn content_range_total(value: Option<&reqwest::header::HeaderValue>) -> Option<u64> {
    let value = value?.to_str().ok()?.trim();
    let (_, total) = value.rsplit_once('/')?;
    total.trim().parse::<u64>().ok().filter(|size| *size > 0)
}

async fn ensure_ffmpeg_tools_available(app: &AppHandle) -> AppResult<()> {
    let state = app.state::<ManagedState>();
    let ffmpeg = ffmpeg_tool_info(&state.paths, "ffmpeg").await;
    let ffprobe = ffmpeg_tool_info(&state.paths, "ffprobe").await;
    if ffmpeg.available && ffprobe.available {
        return Ok(());
    }

    set_benchmark(
        app,
        Some(BenchmarkProgress {
            phase: "download".to_string(),
            percent: 25.0,
            message: "Downloading FFmpeg and FFprobe".to_string(),
        }),
    );
    log_line(app, "Downloading FFmpeg and FFprobe");
    let result = install_ffmpeg_tools(&state.paths, &state.http).await?;
    log_line(
        app,
        format!(
            "FFmpeg and FFprobe installed at {} ({})",
            result.directory.display(),
            format_bytes(result.size_bytes)
        ),
    );

    let ffmpeg = ffmpeg_tool_info(&state.paths, "ffmpeg").await;
    let ffprobe = ffmpeg_tool_info(&state.paths, "ffprobe").await;
    if ffmpeg.available && ffprobe.available {
        emit_state(app);
        return Ok(());
    }

    Err(AppError::Process(ffmpeg_requirement_message(
        &state.paths,
        ffmpeg.available,
        ffprobe.available,
    )))
}

fn ffmpeg_requirement_message(
    paths: &crate::config::AppPaths,
    ffmpeg_available: bool,
    ffprobe_available: bool,
) -> String {
    let snapshot = crate::tools::ffmpeg_tools_snapshot(paths);
    match (ffmpeg_available, ffprobe_available) {
        (true, true) => format!("Available for renderer encoding at {}", snapshot.directory),
        (false, true) => format!(
            "ffmpeg is missing. Download all will install it to {}",
            snapshot.ffmpeg.path
        ),
        (true, false) => format!(
            "ffprobe is missing. Download all will install it to {}",
            snapshot.ffprobe.path
        ),
        (false, false) => format!(
            "ffmpeg and ffprobe are missing. Download all will install them to {}",
            snapshot.directory
        ),
    }
}

fn format_bytes(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    format!("{:.2} MiB", bytes as f64 / MIB)
}

async fn measure_speed(
    app: &AppHandle,
    token: &str,
    benchmark_config: &BenchmarkConfigPayload,
) -> AppResult<SpeedMeasurement> {
    let state = app.state::<ManagedState>();
    let config = load_config(&state.paths)?;
    let api_url = trim_trailing_slash(&config.api_url);
    let source = benchmark_config
        .download_sources
        .first()
        .ok_or_else(|| AppError::InvalidInput("benchmark missing download source".to_string()))?;
    let source_url = resolve_worker_url(&source.url, &api_url)?;
    let started = Instant::now();
    let first_byte_deadline = Duration::from_secs(20);
    let mut bytes: u64 = 0;
    let mut first_byte_ms = None;
    let mut request = state.http.get(source_url.clone());
    if is_same_origin(&source_url, &api_url) {
        request = request.bearer_auth(token);
    }
    let response = request.send().await?;
    let response = if is_same_origin(&source_url, &api_url) {
        require_auth_response(app, response).await?
    } else {
        response.error_for_status()?
    };
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if first_byte_ms.is_none() {
            first_byte_ms = Some(started.elapsed().as_millis() as u64);
        }
        bytes += chunk.len() as u64;
        let elapsed = started.elapsed().as_secs_f64();
        if elapsed > 0.5 {
            let mbps = (bytes as f64 * 8.0) / elapsed / 1_000_000.0;
            set_benchmark(
                app,
                Some(BenchmarkProgress {
                    phase: "speed".to_string(),
                    percent: (5.0 + elapsed.min(10.0) / 10.0 * 10.0).min(15.0),
                    message: format!("Download {mbps:.1} Mbps via {}", source.label),
                }),
            );
        }
        if bytes >= source.bytes || started.elapsed() >= first_byte_deadline {
            break;
        }
    }

    let elapsed = started.elapsed().as_secs_f64().max(0.1);
    let download_mbps = (bytes as f64 * 8.0) / elapsed / 1_000_000.0;
    let upload_mbps = measure_upload_speed(app, token, benchmark_config).await?;

    Ok(SpeedMeasurement {
        download_mbps,
        upload_mbps,
        latency_ms: first_byte_ms.unwrap_or_else(|| started.elapsed().as_millis() as u64),
        bytes,
        source: source.label.clone(),
    })
}

async fn measure_upload_speed(
    app: &AppHandle,
    token: &str,
    benchmark_config: &BenchmarkConfigPayload,
) -> AppResult<f64> {
    let state = app.state::<ManagedState>();
    let config = load_config(&state.paths)?;
    let api_url = trim_trailing_slash(&config.api_url);
    if benchmark_config.upload_url.trim().is_empty() || benchmark_config.upload_bytes == 0 {
        log_line(
            app,
            "Upload benchmark endpoint is not available on this API version; skipping upload speed",
        );
        return Ok(0.0);
    }
    let upload_url = resolve_worker_url(&benchmark_config.upload_url, &api_url)?;
    let size = benchmark_config
        .upload_bytes
        .min(128 * 1024 * 1024)
        .max(1024 * 1024);
    let payload = vec![0x42_u8; size as usize];
    set_benchmark(
        app,
        Some(BenchmarkProgress {
            phase: "speed".to_string(),
            percent: 16.0,
            message: "Testing upload speed".to_string(),
        }),
    );
    let started = Instant::now();
    let response = state
        .http
        .post(upload_url)
        .bearer_auth(token)
        .header(CONTENT_TYPE, "application/octet-stream")
        .header(CONTENT_LENGTH, size)
        .body(payload)
        .send()
        .await?;
    if response.status() == StatusCode::NOT_FOUND {
        log_line(
            app,
            "Upload benchmark endpoint is not available on this API version; skipping upload speed",
        );
        return Ok(0.0);
    }
    require_auth_response(app, response).await?;
    let elapsed = started.elapsed().as_secs_f64().max(0.1);
    let upload_mbps = (size as f64 * 8.0) / elapsed / 1_000_000.0;
    set_benchmark(
        app,
        Some(BenchmarkProgress {
            phase: "speed".to_string(),
            percent: 20.0,
            message: format!("Upload {upload_mbps:.1} Mbps"),
        }),
    );
    Ok(upload_mbps)
}

async fn download_benchmark_assets(
    app: &AppHandle,
    token: &str,
    benchmark_config: &BenchmarkConfigPayload,
) -> AppResult<BenchmarkAssetPaths> {
    let replay_url = benchmark_config
        .replay_url
        .as_deref()
        .unwrap_or("/api/v1/servers/benchmark/replay");
    let mapset_url = benchmark_config
        .mapset_url
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::InvalidInput("benchmark mapset URL is missing".to_string()))?;

    set_benchmark(
        app,
        Some(BenchmarkProgress {
            phase: "download".to_string(),
            percent: 23.0,
            message: "Downloading benchmark replay".to_string(),
        }),
    );
    let replay_path =
        download_benchmark_file(app, token, replay_url, "osr", REPLAY_LIMIT_BYTES).await?;

    set_benchmark(
        app,
        Some(BenchmarkProgress {
            phase: "download".to_string(),
            percent: 29.0,
            message: "Downloading benchmark mapset".to_string(),
        }),
    );
    let mapset_path =
        download_benchmark_file(app, token, mapset_url, "osz", MAPSET_LIMIT_BYTES).await?;

    Ok(BenchmarkAssetPaths {
        replay_path,
        mapset_path,
    })
}

async fn download_benchmark_file(
    app: &AppHandle,
    token: &str,
    raw_url: &str,
    extension: &str,
    max_bytes: u64,
) -> AppResult<PathBuf> {
    let state = app.state::<ManagedState>();
    let config = load_config(&state.paths)?;
    let api_url = trim_trailing_slash(&config.api_url);
    let url = resolve_worker_url(raw_url, &api_url)?;
    let safe_extension = extension.trim_start_matches('.');
    let path = std::env::temp_dir().join(format!(
        "miru-benchmark-{}.{}",
        Uuid::new_v4(),
        safe_extension
    ));

    let mut request = state.http.get(url.clone());
    if is_same_origin(&url, &api_url) {
        request = request.bearer_auth(token);
    }

    let response = request.send().await?;
    let response = if is_same_origin(&url, &api_url) {
        require_auth_response(app, response).await?
    } else {
        response.error_for_status()?
    };

    if let Some(content_length) = response.content_length() {
        if content_length > max_bytes {
            return Err(AppError::Process(format!(
                "benchmark asset is too large: {content_length} bytes"
            )));
        }
    }

    let mut file = File::create(&path).await?;
    let mut downloaded: u64 = 0;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        downloaded += chunk.len() as u64;
        if downloaded > max_bytes {
            let _ = fs::remove_file(&path).await;
            return Err(AppError::Process(format!(
                "benchmark asset exceeded {max_bytes} bytes"
            )));
        }
        file.write_all(&chunk).await?;
    }
    file.flush().await?;

    if downloaded == 0 {
        let _ = fs::remove_file(&path).await;
        return Err(AppError::Process(
            "benchmark asset download was empty".to_string(),
        ));
    }

    Ok(path)
}

async fn render_benchmark(
    app: &AppHandle,
    renderer_path: PathBuf,
    assets: BenchmarkAssetPaths,
    replay_seconds: u32,
) -> AppResult<(u64, String)> {
    let state = app.state::<ManagedState>();
    let config = load_config(&state.paths)?;
    let (width, height, fps) = config.resolution.dimensions();
    let benchmark_dir =
        std::env::temp_dir().join(format!("miru-benchmark-render-{}", Uuid::new_v4()));
    let output_path = benchmark_dir.join("output.mp4");
    let songs_dir = benchmark_dir.join("songs");
    fs::create_dir_all(&songs_dir).await?;
    let _cleanup = CleanupPaths {
        paths: vec![
            assets.replay_path.clone(),
            assets.mapset_path.clone(),
            output_path.clone(),
            benchmark_dir.clone(),
        ],
    };
    let started = Instant::now();
    let mut command = Command::new(renderer_path);
    command
        .arg("--replay")
        .arg(&assets.replay_path)
        .arg("--mapset")
        .arg(&assets.mapset_path)
        .arg("--out")
        .arg(&output_path)
        .arg("--width")
        .arg(width.to_string())
        .arg("--height")
        .arg(height.to_string())
        .arg("--fps")
        .arg(fps.to_string())
        .arg("--songs-dir")
        .arg(&songs_dir)
        .arg("--end")
        .arg(replay_seconds.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_ffmpeg_path(&mut command, &state.paths);
    hide_child_console(&mut command);
    let mut child = command
        .spawn()
        .map_err(|err| AppError::Process(format!("failed to start renderer: {err}")))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::Process("missing renderer stdout".to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AppError::Process("missing renderer stderr".to_string()))?;
    let mut lines = BufReader::new(stdout).lines();
    let app_for_progress = app.clone();
    let progress_task = tokio::spawn(async move {
        let mut gpu_name = "Unknown".to_string();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(gpu) = parse_gpu_name(&line) {
                gpu_name = gpu;
            }
            if let Some(progress) = parse_percent(&line) {
                set_benchmark(
                    &app_for_progress,
                    Some(BenchmarkProgress {
                        phase: "render".to_string(),
                        percent: 35.0 + progress * 0.55,
                        message: format!("Rendering {:.0}%", progress),
                    }),
                );
            }
        }
        gpu_name
    });
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut tail = String::new();
        while let Ok(Some(line)) = lines.next_line().await {
            append_text_tail(&mut tail, &line, 8 * 1024);
        }
        tail
    });

    let status = child.wait().await?;
    let gpu_name = progress_task
        .await
        .unwrap_or_else(|_| "Unknown".to_string());
    let stderr_tail = stderr_task.await.unwrap_or_default();
    let _ = fs::remove_file(assets.replay_path).await;
    let _ = fs::remove_file(assets.mapset_path).await;
    let _ = fs::remove_file(output_path).await;
    if !status.success() {
        let details = stderr_tail.trim();
        let message = if details.is_empty() {
            format!("renderer exited with {status}")
        } else {
            format!("renderer exited with {status}: {details}")
        };
        return Err(AppError::Process(message));
    }

    Ok((started.elapsed().as_millis() as u64, gpu_name))
}

fn append_text_tail(buffer: &mut String, line: &str, max_bytes: usize) {
    if !buffer.is_empty() {
        buffer.push('\n');
    }
    buffer.push_str(line);
    if buffer.len() <= max_bytes {
        return;
    }

    let remove_to = buffer
        .char_indices()
        .find_map(|(index, _)| (buffer.len() - index <= max_bytes).then_some(index))
        .unwrap_or(buffer.len());
    buffer.drain(..remove_to);
}

fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }

    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

#[cfg(target_os = "windows")]
fn hide_child_console(command: &mut Command) {
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn hide_child_console(_command: &mut Command) {}

async fn render_with_cli(
    app: &AppHandle,
    out_tx: &mpsc::UnboundedSender<String>,
    renderer_path: PathBuf,
    job: &JobAssignment,
    paths: &PreparedJobPaths,
    output_path: &Path,
    songs_dir: &Path,
    report_path: Option<&Path>,
    cancel_rx: watch::Receiver<bool>,
    render_timeout: Duration,
) -> AppResult<()> {
    let args = build_render_cli_args(job, paths, output_path, songs_dir, report_path);
    let mut command = Command::new(renderer_path);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.kill_on_drop(true);
    let state = app.state::<ManagedState>();
    configure_ffmpeg_path(&mut command, &state.paths);
    hide_child_console(&mut command);
    let mut child = command
        .spawn()
        .map_err(|err| AppError::Process(format!("failed to start renderer: {err}")))?;

    let last_progress = Arc::new(AsyncMutex::new(5_u32));
    let renderer_output_tail = Arc::new(AsyncMutex::new(String::new()));
    let stdout_reader = if let Some(stdout) = child.stdout.take() {
        Some(spawn_renderer_progress_reader(
            app.clone(),
            out_tx.clone(),
            job.clone(),
            stdout,
            last_progress.clone(),
            renderer_output_tail.clone(),
            "stdout",
        ))
    } else {
        None
    };
    let stderr_reader = if let Some(stderr) = child.stderr.take() {
        Some(spawn_renderer_progress_reader(
            app.clone(),
            out_tx.clone(),
            job.clone(),
            stderr,
            last_progress,
            renderer_output_tail.clone(),
            "stderr",
        ))
    } else {
        None
    };

    let mut cancel_rx = cancel_rx;
    let status = tokio::select! {
        status = child.wait() => status?,
        _ = tokio::time::sleep(render_timeout) => {
            let _ = child.kill().await;
            return Err(AppError::Process(format!(
                "Render timed out after {} seconds",
                render_timeout.as_secs()
            )));
        }
        _ = wait_for_cancel(&mut cancel_rx) => {
            let _ = child.kill().await;
            return Err(AppError::Process("Render cancelled".to_string()));
        }
    };

    wait_renderer_output_readers(stdout_reader, stderr_reader).await;

    if status.success() {
        Ok(())
    } else {
        let details = renderer_output_tail.lock().await.trim().to_string();
        if details.is_empty() {
            Err(AppError::Process(format!("renderer exited with {status}")))
        } else {
            Err(AppError::Process(format!(
                "renderer exited with {status}: {details}"
            )))
        }
    }
}

fn render_timeout_for_job(job: &JobAssignment) -> Duration {
    let timeout_ms = job
        .max_render_duration_ms
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_JOB_MAX_RENDER_MS);
    Duration::from_millis(timeout_ms.max(1_000))
}

fn spawn_renderer_progress_reader<R>(
    app: AppHandle,
    out_tx: mpsc::UnboundedSender<String>,
    job: JobAssignment,
    reader: R,
    last_progress: Arc<AsyncMutex<u32>>,
    renderer_output_tail: RendererOutputTail,
    stream_name: &'static str,
) -> tokio::task::JoinHandle<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            {
                let mut tail = renderer_output_tail.lock().await;
                append_text_tail(
                    &mut tail,
                    &format!(
                        "{stream_name}: {}",
                        truncate_utf8(&line, RENDERER_OUTPUT_LINE_BYTES)
                    ),
                    RENDERER_OUTPUT_TAIL_BYTES,
                );
            }

            let Some(parsed) = parse_percent(&line) else {
                continue;
            };
            let progress = parsed.round().clamp(5.0, 94.0) as u32;
            let mut last = last_progress.lock().await;
            if progress <= *last {
                continue;
            }
            *last = progress;
            drop(last);
            let step = format!("Renderizando: {progress}%");
            let _ = queue_worker_progress(&out_tx, &job, progress, &step, "cli_render");
            let _ = app.emit(
                "job:progress",
                json!({
                    "jobId": job.id.as_str(),
                    "progress": progress,
                    "currentStep": step,
                    "phase": "cli_render"
                }),
            );
        }
    })
}

async fn wait_renderer_output_readers(
    stdout_reader: Option<tokio::task::JoinHandle<()>>,
    stderr_reader: Option<tokio::task::JoinHandle<()>>,
) {
    if let Some(reader) = stdout_reader {
        let _ = timeout(Duration::from_secs(2), reader).await;
    }
    if let Some(reader) = stderr_reader {
        let _ = timeout(Duration::from_secs(2), reader).await;
    }
}

async fn wait_for_cancel(cancel_rx: &mut watch::Receiver<bool>) {
    loop {
        if *cancel_rx.borrow() {
            return;
        }
        if cancel_rx.changed().await.is_err() {
            return;
        }
    }
}

fn build_render_cli_args(
    job: &JobAssignment,
    paths: &PreparedJobPaths,
    output_path: &Path,
    songs_dir: &Path,
    report_path: Option<&Path>,
) -> Vec<OsString> {
    let mut args = Vec::new();
    if job.render_mode == WorkerRenderMode::Autoplay {
        push_arg(&mut args, "--mapset");
        push_path(
            &mut args,
            paths.mapset_path.as_ref().expect("validated mapset path"),
        );
        push_arg(&mut args, "--diff-index");
        push_arg(
            &mut args,
            job.difficulty_index.unwrap_or_default().to_string(),
        );
        push_arg(&mut args, "--autoplay-mods-config");
        push_path(
            &mut args,
            paths
                .autoplay_mods_path
                .as_ref()
                .expect("validated autoplay mods path"),
        );
    } else {
        push_arg(&mut args, "--replay");
        push_path(
            &mut args,
            paths.replay_path.as_ref().expect("validated replay path"),
        );
        if let Some(mapset_path) = paths.mapset_path.as_ref() {
            push_arg(&mut args, "--mapset");
            push_path(&mut args, mapset_path);
            push_arg(&mut args, "--diff-index");
            push_arg(
                &mut args,
                job.difficulty_index.unwrap_or_default().to_string(),
            );
        }
    }

    push_arg(&mut args, "--width");
    push_arg(&mut args, job.resolution.width.to_string());
    push_arg(&mut args, "--height");
    push_arg(&mut args, job.resolution.height.to_string());
    push_arg(&mut args, "--fps");
    push_arg(&mut args, job.resolution.fps.to_string());
    push_arg(&mut args, "--out");
    push_path(&mut args, output_path);
    push_arg(&mut args, "--songs-dir");
    push_path(&mut args, songs_dir);

    if let Some(skin_path) = paths.skin_path.as_ref() {
        push_arg(&mut args, "--skin");
        push_path(&mut args, skin_path);
    }
    if let Some(hud_config_path) = paths.hud_config_path.as_ref() {
        push_arg(&mut args, "--hud-config");
        push_path(&mut args, hud_config_path);
    }
    if let Some(intro_user_json_path) = paths.intro_user_json_path.as_ref() {
        push_arg(&mut args, "--intro-user-json");
        push_path(&mut args, intro_user_json_path);
    }
    if job.render_intro_enabled == Some(false) {
        push_arg(&mut args, "--no-intro");
    }
    if let Some(value) = clamp_optional(job.bg_opacity, 0.0, 100.0) {
        push_arg(&mut args, "--bg-opacity");
        push_arg(&mut args, value.to_string());
    }
    if let Some(value) = clamp_optional(job.scroll_speed, 1.0, 60.0) {
        push_arg(&mut args, "--ss");
        push_arg(&mut args, value.to_string());
    }
    if let Some(value) =
        clamp_optional(job.motion_blur_percent, 0.0, 100.0).filter(|value| *value > 0.0)
    {
        push_arg(&mut args, "--motion-blur");
        push_arg(&mut args, value.round().to_string());
    }
    if let Some(value) =
        clamp_optional(job.background_blur_percent, 0.0, 100.0).filter(|value| *value > 0.0)
    {
        push_arg(&mut args, "--bg-blur");
        push_arg(&mut args, value.round().to_string());
    }
    if job.background_video_enabled == Some(false) {
        push_arg(&mut args, "--no-bg-video");
    }
    if job.storyboard_enabled == Some(false) {
        push_arg(&mut args, "--no-storyboard");
    }
    if job.skin_animations_enabled == Some(false) {
        push_arg(&mut args, "--no-skin-animations");
    }
    if job.combo_images_enabled == Some(false) {
        push_arg(&mut args, "--no-combo-burst");
    }
    if let Some(value) = clamp_optional(job.music_volume, 0.0, 100.0) {
        push_arg(&mut args, "--music-volume");
        push_arg(&mut args, value.to_string());
    }
    if let Some(value) = clamp_optional(job.hitsound_volume, 0.0, 100.0) {
        push_arg(&mut args, "--hitsound-volume");
        push_arg(&mut args, value.to_string());
    }
    if let Some(beatmap_path) = paths.beatmap_path.as_ref() {
        push_arg(&mut args, "--osu");
        push_path(&mut args, beatmap_path);
    }
    if let Some(report_path) = report_path {
        push_arg(&mut args, "--report-out");
        push_path(&mut args, report_path);
    }

    args
}

fn push_arg(args: &mut Vec<OsString>, value: impl Into<OsString>) {
    args.push(value.into());
}

fn push_path(args: &mut Vec<OsString>, path: &Path) {
    args.push(path.as_os_str().to_os_string());
}

async fn read_replay_integrity_report(
    report_path: &Path,
) -> AppResult<Option<ReplayIntegrityReport>> {
    let metadata = match fs::metadata(report_path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(AppError::Process(format!(
                "failed to read replay integrity report metadata: {error}"
            )))
        }
    };

    if metadata.len() == 0 {
        return Ok(None);
    }

    if metadata.len() > JSON_LIMIT_BYTES as u64 {
        return Err(AppError::Process(format!(
            "replay integrity report exceeded {} bytes",
            JSON_LIMIT_BYTES
        )));
    }

    let report_raw = fs::read_to_string(report_path).await.map_err(|error| {
        AppError::Process(format!("failed to read replay integrity report: {error}"))
    })?;
    let report = serde_json::from_str::<ReplayIntegrityReport>(&report_raw).map_err(|error| {
        AppError::Process(format!("failed to parse replay integrity report: {error}"))
    })?;

    Ok(Some(report))
}

async fn download_file_with_limits(
    app: &AppHandle,
    source_url: &str,
    destination_path: &Path,
    api_base_url: &str,
    max_bytes: u64,
    budget: &mut ByteBudget,
    cancel_rx: &mut watch::Receiver<bool>,
) -> AppResult<u64> {
    if let Some(bytes) = decode_data_url_bytes(source_url)? {
        let size = bytes.len() as u64;
        validate_transfer_size(size, max_bytes, budget)?;
        if size == 0 {
            return Err(AppError::InvalidInput(
                "downloaded file is empty".to_string(),
            ));
        }
        fs::write(destination_path, bytes).await?;
        budget.used_bytes += size;
        return Ok(size);
    }

    let url = resolve_worker_url(source_url, api_base_url)?;
    let state = app.state::<ManagedState>();
    let response = timeout(DOWNLOAD_TIMEOUT, state.http.get(url).send())
        .await
        .map_err(|_| AppError::Process("download timed out".to_string()))??
        .error_for_status()?;

    if let Some(content_length) = response.content_length() {
        validate_transfer_size(content_length, max_bytes, budget)?;
    }

    let mut file = File::create(destination_path).await?;
    let mut stream = response.bytes_stream();
    let mut bytes_written = 0_u64;

    while let Some(chunk) = tokio::select! {
        _ = wait_for_cancel(cancel_rx) => {
            let _ = fs::remove_file(destination_path).await;
            return Err(AppError::Process("Render cancelled".to_string()));
        }
        chunk = stream.next() => chunk
    } {
        let chunk = chunk.map_err(AppError::from)?;
        bytes_written = bytes_written
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| AppError::InvalidInput("download size overflow".to_string()))?;
        validate_transfer_size(bytes_written, max_bytes, budget)?;
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    if bytes_written == 0 {
        let _ = fs::remove_file(destination_path).await;
        return Err(AppError::InvalidInput(
            "downloaded file is empty".to_string(),
        ));
    }
    budget.used_bytes += bytes_written;
    Ok(bytes_written)
}

fn decode_data_url_bytes(source_url: &str) -> AppResult<Option<Vec<u8>>> {
    let source_url = source_url.trim();
    if !source_url.starts_with("data:") {
        return Ok(None);
    }
    let Some((metadata, payload)) = source_url.split_once(',') else {
        return Err(AppError::InvalidInput(
            "invalid data URL asset payload".to_string(),
        ));
    };
    if !metadata
        .split(';')
        .any(|part| part.eq_ignore_ascii_case("base64"))
    {
        return Err(AppError::InvalidInput(
            "data URL assets must be base64 encoded".to_string(),
        ));
    }
    let bytes = general_purpose::STANDARD
        .decode(payload.trim())
        .map_err(|_| AppError::InvalidInput("invalid base64 data URL asset".to_string()))?;
    Ok(Some(bytes))
}

fn safe_hud_asset_component(value: Option<&str>, fallback: &str) -> String {
    let raw = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback);
    let safe = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string();
    if safe.is_empty() {
        fallback.to_string()
    } else {
        safe
    }
}

fn hud_asset_extension(asset: &serde_json::Map<String, Value>) -> &'static str {
    let mime_type = asset
        .get("mimeType")
        .or_else(|| asset.get("mime_type"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if mime_type == "image/gif" {
        return ".gif";
    }
    if mime_type == "image/jpeg" || mime_type == "image/jpg" {
        return ".jpg";
    }
    if mime_type == "image/webp" {
        return ".webp";
    }
    if asset
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.eq_ignore_ascii_case("gif"))
    {
        return ".gif";
    }
    ".png"
}

fn hud_font_extension(source: Option<&str>) -> String {
    let extension = source
        .and_then(|value| value.split('?').next())
        .and_then(|value| Path::new(value).extension())
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase());
    match extension.as_deref() {
        Some("ttf") => ".ttf".to_string(),
        Some("otf") => ".otf".to_string(),
        Some("ttc") => ".ttc".to_string(),
        Some("woff2") => ".woff2".to_string(),
        _ => ".woff2".to_string(),
    }
}

fn hud_font_url_key(path_key: &str) -> Option<&'static str> {
    match path_key {
        "path" => Some("url"),
        "normalPath" => Some("normalUrl"),
        "boldPath" => Some("boldUrl"),
        _ => None,
    }
}

async fn prepare_hud_font_file(
    app: &AppHandle,
    font: &serde_json::Map<String, Value>,
    font_dir: &Path,
    font_index: usize,
    path_key: &str,
    api_base_url: &str,
    budget: &mut ByteBudget,
    cancel_rx: &mut watch::Receiver<bool>,
) -> AppResult<Option<String>> {
    let source_path = font
        .get(path_key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let source_url = hud_font_url_key(path_key)
        .and_then(|key| font.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if source_path.is_none() && source_url.is_none() {
        return Ok(None);
    }

    let source_for_extension = source_path.or(source_url);
    let font_id = safe_hud_asset_component(
        font.get("id").and_then(Value::as_str),
        &format!("font-{font_index}"),
    );
    let filename = format!(
        "hud-font-{font_index}-{font_id}-{path_key}{}",
        hud_font_extension(source_for_extension)
    );
    let destination_path = font_dir.join(filename);

    if let Some(path) = source_path {
        let source = Path::new(path);
        if source.exists() {
            if source == destination_path {
                return Ok(Some(path.to_string()));
            }
            fs::copy(source, &destination_path).await?;
            return Ok(Some(destination_path.to_string_lossy().to_string()));
        }
    }

    if let Some(url) = source_url {
        download_file_with_limits(
            app,
            url,
            &destination_path,
            api_base_url,
            HUD_FONT_LIMIT_BYTES,
            budget,
            cancel_rx,
        )
        .await?;
        return Ok(Some(destination_path.to_string_lossy().to_string()));
    }

    Ok(None)
}

async fn prepare_hud_config_json(
    app: &AppHandle,
    hud_config: &Value,
    job_dir: &Path,
    api_base_url: &str,
    budget: &mut ByteBudget,
    cancel_rx: &mut watch::Receiver<bool>,
) -> AppResult<Value> {
    let mut prepared = hud_config.clone();
    let Some(root) = prepared.as_object_mut() else {
        return Ok(prepared);
    };
    if root.get("version").and_then(Value::as_u64) != Some(4) {
        return Ok(prepared);
    }

    if let Some(assets) = root.get_mut("assets").and_then(Value::as_array_mut) {
        let asset_dir = job_dir.join("hud-assets");
        fs::create_dir_all(&asset_dir).await?;

        for (index, asset_value) in assets.iter_mut().enumerate() {
            let Some(asset) = asset_value.as_object_mut() else {
                continue;
            };
            let source_url = asset
                .get("url")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|url| !url.is_empty());
            let is_embedded_asset =
                source_url.is_some_and(|url| url.trim_start().starts_with("data:"));

            if !is_embedded_asset
                && asset
                    .get("path")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|path| !path.is_empty() && Path::new(path).exists())
                    .is_some()
            {
                continue;
            }

            let Some(url) = source_url else {
                continue;
            };

            let asset_id = safe_hud_asset_component(
                asset.get("id").and_then(Value::as_str),
                &format!("asset-{index}"),
            );
            let destination_path = asset_dir.join(format!(
                "hud-{index}-{asset_id}{}",
                hud_asset_extension(asset)
            ));
            download_file_with_limits(
                app,
                url,
                &destination_path,
                api_base_url,
                HUD_ASSET_LIMIT_BYTES,
                budget,
                cancel_rx,
            )
            .await?;
            asset.insert(
                "path".to_string(),
                Value::String(destination_path.to_string_lossy().to_string()),
            );
            if is_embedded_asset {
                asset.remove("url");
            }
        }
    }

    if let Some(fonts) = root.get_mut("fonts").and_then(Value::as_array_mut) {
        let font_dir = job_dir.join("hud-fonts");
        fs::create_dir_all(&font_dir).await?;

        for (index, font_value) in fonts.iter_mut().enumerate() {
            let Some(font) = font_value.as_object_mut() else {
                continue;
            };
            for path_key in ["path", "normalPath", "boldPath"] {
                if let Some(local_path) = prepare_hud_font_file(
                    app,
                    font,
                    &font_dir,
                    index,
                    path_key,
                    api_base_url,
                    budget,
                    cancel_rx,
                )
                .await?
                {
                    font.insert(path_key.to_string(), Value::String(local_path));
                    if let Some(url_key) = hud_font_url_key(path_key) {
                        font.remove(url_key);
                    }
                }
            }
        }
    }

    Ok(prepared)
}

async fn try_download_intro_asset(
    app: &AppHandle,
    job: &JobAssignment,
    source_url: Option<&str>,
    destination_path: &Path,
    api_base_url: &str,
    budget: &mut ByteBudget,
    cancel_rx: &mut watch::Receiver<bool>,
) -> AppResult<Option<String>> {
    let Some(source_url) = source_url.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };

    match download_file_with_limits(
        app,
        source_url,
        destination_path,
        api_base_url,
        INTRO_ASSET_LIMIT_BYTES,
        budget,
        cancel_rx,
    )
    .await
    {
        Ok(_) => Ok(destination_path
            .file_name()
            .and_then(|value| value.to_str())
            .map(str::to_string)),
        Err(error) => {
            if matches!(&error, AppError::Process(message) if message == "Render cancelled") {
                return Err(error);
            }
            log_line(
                app,
                format!("Intro asset download skipped for job {}: {error}", job.id),
            );
            Ok(None)
        }
    }
}

async fn prepare_intro_user_json(
    app: &AppHandle,
    job: &JobAssignment,
    job_dir: &Path,
    manifest_path: &Path,
    api_base_url: &str,
    budget: &mut ByteBudget,
    cancel_rx: &mut watch::Receiver<bool>,
) -> AppResult<Option<PathBuf>> {
    let Some(intro_user) = job.intro_user.as_ref() else {
        return Ok(None);
    };
    let country_code = if job.render_mode == WorkerRenderMode::Autoplay {
        Some(MIRU_AUTOPLAY_COUNTRY_CODE.to_string())
    } else {
        normalize_country_code(intro_user.country_code.as_deref())
    };
    let avatar_url = if job.render_mode == WorkerRenderMode::Autoplay {
        None
    } else {
        intro_user.avatar_url.as_deref()
    };
    let flag_url = if job.render_mode == WorkerRenderMode::Autoplay {
        country_code.as_deref().map(flag_url_for_country_code)
    } else {
        intro_flag_url(intro_user)
    };

    let avatar_path = job_dir.join(format!(
        "intro-avatar{}",
        resolve_file_extension(avatar_url, ".png")
    ));
    let flag_path = job_dir.join(format!(
        "intro-flag{}",
        resolve_file_extension(flag_url.as_deref(), ".png")
    ));
    let team_badge_path = job_dir.join(format!(
        "intro-team-badge{}",
        resolve_file_extension(intro_user.team_badge_url.as_deref(), ".png")
    ));

    let mut manifest = serde_json::Map::new();

    if let Some(filename) = try_download_intro_asset(
        app,
        job,
        avatar_url,
        &avatar_path,
        api_base_url,
        budget,
        cancel_rx,
    )
    .await?
    {
        manifest.insert("avatar_path".to_string(), Value::String(filename));
    }

    if let Some(country_code) = country_code {
        manifest.insert("country_code".to_string(), Value::String(country_code));
    }

    if let Some(filename) = try_download_intro_asset(
        app,
        job,
        flag_url.as_deref(),
        &flag_path,
        api_base_url,
        budget,
        cancel_rx,
    )
    .await?
    {
        manifest.insert("flag_path".to_string(), Value::String(filename));
    }

    if let Some(filename) = try_download_intro_asset(
        app,
        job,
        intro_user.team_badge_url.as_deref(),
        &team_badge_path,
        api_base_url,
        budget,
        cancel_rx,
    )
    .await?
    {
        manifest.insert("team_badge_path".to_string(), Value::String(filename));
    }

    if manifest.is_empty() {
        return Ok(None);
    }

    write_json_file_with_limit(
        manifest_path,
        &Value::Object(manifest),
        "Intro user manifest",
        JSON_LIMIT_BYTES,
    )
    .await?;
    Ok(Some(manifest_path.to_path_buf()))
}

async fn upload_file_with_limits(
    app: &AppHandle,
    file_path: &Path,
    upload_url: &str,
    api_base_url: &str,
    worker_token: &str,
    attempt_id: &str,
    max_output_size_bytes: u64,
    cancel_rx: &mut watch::Receiver<bool>,
) -> AppResult<u64> {
    let url = resolve_worker_url(upload_url, api_base_url)?;
    let metadata = fs::metadata(file_path).await?;
    let size = metadata.len();
    if size == 0 {
        return Err(AppError::InvalidInput(
            "rendered output is empty".to_string(),
        ));
    }
    if size > max_output_size_bytes {
        return Err(AppError::InvalidInput(format!(
            "rendered output exceeds allowed size ({size} > {max_output_size_bytes} bytes)"
        )));
    }

    let file = File::open(file_path).await?;
    let body = reqwest::Body::wrap_stream(ReaderStream::new(file));
    let state = app.state::<ManagedState>();
    let upload = timeout(
        UPLOAD_TIMEOUT,
        state
            .http
            .put(url)
            .bearer_auth(worker_token)
            .header(CONTENT_TYPE, "video/mp4")
            .header(CONTENT_LENGTH, size)
            .header("X-Miru-Attempt-Id", attempt_id)
            .body(body)
            .send(),
    );
    tokio::select! {
        _ = wait_for_cancel(cancel_rx) => {
            return Err(AppError::Process("Render cancelled".to_string()));
        }
        result = upload => {
            result
                .map_err(|_| AppError::Process("upload timed out".to_string()))??
                .error_for_status()?;
        }
    }
    Ok(size)
}

async fn write_json_file_with_limit(
    path: &Path,
    value: &Value,
    label: &str,
    max_bytes: usize,
) -> AppResult<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    if bytes.len() > max_bytes {
        return Err(AppError::InvalidInput(format!(
            "{label} exceeds allowed size ({} > {max_bytes} bytes)",
            bytes.len()
        )));
    }
    fs::write(path, bytes).await?;
    Ok(())
}

fn validate_transfer_size(size: u64, max_file_bytes: u64, budget: &ByteBudget) -> AppResult<()> {
    if size > max_file_bytes {
        return Err(AppError::InvalidInput(format!(
            "transfer exceeds file size limit ({size} > {max_file_bytes} bytes)"
        )));
    }
    let next_budget = budget
        .used_bytes
        .checked_add(size)
        .ok_or_else(|| AppError::InvalidInput("transfer budget overflow".to_string()))?;
    if next_budget > budget.max_bytes {
        return Err(AppError::InvalidInput(format!(
            "transfer exceeds total job size limit ({next_budget} > {} bytes)",
            budget.max_bytes
        )));
    }
    Ok(())
}

fn validate_job_assignment(job: &JobAssignment) -> AppResult<()> {
    if job.id.trim().is_empty() {
        return Err(AppError::InvalidInput("job missing id".to_string()));
    }
    if matches!(job.attempt_number, Some(0)) {
        return Err(AppError::InvalidInput(
            "job declared invalid attempt number".to_string(),
        ));
    }
    assert_allowed_resolution(&job.resolution)?;
    if job.output_upload_url.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "job missing output upload URL".to_string(),
        ));
    }
    if job.output_storage_key.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "job missing output storage key".to_string(),
        ));
    }
    if job.max_output_size_bytes == 0 {
        return Err(AppError::InvalidInput(
            "job declared invalid max output size".to_string(),
        ));
    }

    match job.render_mode {
        WorkerRenderMode::Autoplay => {
            if job
                .mapset_url
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
                || !matches!(job.difficulty_index, Some(value) if value >= 0)
            {
                return Err(AppError::InvalidInput(
                    "autoplay job missing valid mapset URL or difficulty index".to_string(),
                ));
            }
        }
        WorkerRenderMode::Replay => {
            if job
                .replay_url
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                return Err(AppError::InvalidInput(
                    "replay job missing replay URL".to_string(),
                ));
            }
            if job.mapset_url.is_none() && job.beatmap_url.is_none() {
                return Err(AppError::InvalidInput(
                    "replay job missing resolved mapset or beatmap URL".to_string(),
                ));
            }
            if job.mapset_url.is_some()
                && !matches!(job.difficulty_index, Some(value) if value >= 0)
            {
                return Err(AppError::InvalidInput(
                    "replay job missing valid difficulty index for mapset".to_string(),
                ));
            }
        }
    }

    Ok(())
}

fn assert_allowed_resolution(resolution: &WorkerResolution) -> AppResult<()> {
    let allowed_resolution = (resolution.width == 1280 && resolution.height == 720)
        || (resolution.width == 1920 && resolution.height == 1080);
    let allowed_fps = resolution.fps == 60;
    if allowed_resolution && allowed_fps {
        Ok(())
    } else {
        Err(AppError::InvalidInput(format!(
            "unexpected render resolution requested: {}x{}@{}",
            resolution.width, resolution.height, resolution.fps
        )))
    }
}

fn queue_worker_progress(
    tx: &mpsc::UnboundedSender<String>,
    job: &JobAssignment,
    progress: impl Into<u64>,
    current_step: &str,
    phase: &str,
) -> AppResult<()> {
    queue_socket_event(
        tx,
        "worker:progress",
        json!({
            "jobId": job.id.as_str(),
            "attemptId": job.attempt_id(),
            "progress": progress.into(),
            "currentStep": current_step,
            "phase": phase
        }),
    )
}

fn spawn_job_heartbeat(
    tx: mpsc::UnboundedSender<String>,
    job: JobAssignment,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(HEARTBEAT_INTERVAL);
        loop {
            interval.tick().await;
            let _ = queue_worker_heartbeat(&tx, &job);
        }
    })
}

fn queue_worker_heartbeat(
    tx: &mpsc::UnboundedSender<String>,
    job: &JobAssignment,
) -> AppResult<()> {
    queue_socket_event(
        tx,
        "worker:heartbeat",
        json!({
            "jobId": job.id.as_str(),
            "attemptId": job.attempt_id()
        }),
    )
}

fn queue_worker_complete(
    tx: &mpsc::UnboundedSender<String>,
    job_id: &str,
    attempt_id: &str,
    video_key: &str,
    duration: u64,
    file_size: u64,
    replay_integrity: Option<&ReplayIntegrityReport>,
) -> AppResult<()> {
    queue_socket_event(
        tx,
        "worker:complete",
        json!({
            "jobId": job_id,
            "attemptId": attempt_id,
            "videoKey": video_key,
            "duration": duration,
            "fileSize": file_size,
            "replayIntegrity": replay_integrity
        }),
    )
}

fn queue_worker_error(
    tx: &mpsc::UnboundedSender<String>,
    job_id: &str,
    attempt_id: &str,
    error: &str,
) -> AppResult<()> {
    queue_socket_event(
        tx,
        "worker:error",
        json!({
            "jobId": job_id,
            "attemptId": attempt_id,
            "error": error
        }),
    )
}

fn queue_socket_event(
    tx: &mpsc::UnboundedSender<String>,
    event: &str,
    payload: Value,
) -> AppResult<()> {
    let packet = format!(
        "42/workers,{}",
        serde_json::to_string(&json!([event, payload]))?
    );
    queue_raw_packet(tx, packet)
}

fn queue_socket_event_no_payload(tx: &mpsc::UnboundedSender<String>, event: &str) -> AppResult<()> {
    let packet = format!("42/workers,{}", serde_json::to_string(&json!([event]))?);
    queue_raw_packet(tx, packet)
}

fn queue_raw_packet(tx: &mpsc::UnboundedSender<String>, packet: String) -> AppResult<()> {
    tx.send(packet)
        .map_err(|_| AppError::Process("worker socket writer is closed".to_string()))
}

fn parse_socket_event(text: &str) -> Option<(String, Value)> {
    let payload = text.strip_prefix("42/workers,")?;
    let values: Vec<Value> = serde_json::from_str(payload).ok()?;
    let event = values.first()?.as_str()?.to_string();
    let data = values.get(1).cloned().unwrap_or(Value::Null);
    Some((event, data))
}

fn parse_socket_error(text: &str) -> Option<String> {
    let payload = text.strip_prefix("44/workers,")?;
    let value: Value = serde_json::from_str(payload).ok()?;
    value
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| value.as_str())
        .map(ToString::to_string)
        .or_else(|| Some("worker namespace rejected connection".to_string()))
}

fn worker_socket_url(api_url: &str) -> AppResult<String> {
    let parsed = url::Url::parse(api_url)
        .map_err(|_| AppError::InvalidInput(format!("invalid API URL: {api_url}")))?;
    let mut next = parsed;
    let scheme = match next.scheme() {
        "https" => "wss",
        _ => {
            return Err(AppError::InvalidInput(
                "worker socket URL must use HTTPS".to_string(),
            ))
        }
    };
    next.set_scheme(scheme)
        .map_err(|_| AppError::InvalidInput("invalid worker socket scheme".to_string()))?;
    let path = next.path().trim_end_matches('/');
    let base_path = path
        .find("/api/v")
        .map(|index| &path[..index])
        .unwrap_or(path);
    let socket_path = if base_path.is_empty() {
        "/socket.io/".to_string()
    } else {
        format!("{base_path}/socket.io/")
    };
    next.set_path(&socket_path);
    next.set_query(Some("EIO=4&transport=websocket"));
    Ok(next.to_string())
}

fn resolve_worker_url(raw_url: &str, api_base_url: &str) -> AppResult<String> {
    let raw_url = raw_url.trim();
    let base = url::Url::parse(api_base_url)
        .map_err(|_| AppError::InvalidInput(format!("invalid API URL: {api_base_url}")))?;
    let resolved = if raw_url.starts_with("http://") || raw_url.starts_with("https://") {
        url::Url::parse(raw_url).map_err(|_| {
            AppError::InvalidInput(format!("invalid worker transfer URL: {raw_url}"))
        })?
    } else if raw_url.starts_with("/api/") {
        let origin = format!(
            "{}://{}",
            base.scheme(),
            base.host_str()
                .ok_or_else(|| AppError::InvalidInput(format!(
                    "invalid API URL: {api_base_url}"
                )))?
        );
        let with_port = if let Some(port) = base.port() {
            format!("{origin}:{port}")
        } else {
            origin
        };
        url::Url::parse(&format!("{with_port}{raw_url}")).map_err(|_| {
            AppError::InvalidInput(format!("invalid worker transfer URL: {raw_url}"))
        })?
    } else {
        let api_base = format!("{}/", api_base_url.trim_end_matches('/'));
        let base = url::Url::parse(&api_base)
            .map_err(|_| AppError::InvalidInput(format!("invalid API URL: {api_base_url}")))?;
        base.join(raw_url.trim_start_matches('/')).map_err(|_| {
            AppError::InvalidInput(format!("invalid worker transfer URL: {raw_url}"))
        })?
    };
    if resolved.scheme() != "https" {
        return Err(AppError::InvalidInput(format!(
            "only HTTPS URLs are allowed for worker transfers: {resolved}"
        )));
    }
    Ok(resolved.to_string())
}

fn is_same_origin(left: &str, right: &str) -> bool {
    let Ok(left) = url::Url::parse(left) else {
        return false;
    };
    let Ok(right) = url::Url::parse(right) else {
        return false;
    };
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn benchmark_meets_requirements(result: &BenchmarkResult) -> bool {
    result.download_mbps >= result.min_mbps
        && result.upload_mbps >= result.min_upload_mbps
        && result.render_time_ms <= result.max_render_ms
}

fn resolve_file_extension(raw: Option<&str>, fallback: &str) -> String {
    let Some(value) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return fallback.to_string();
    };
    if value.starts_with('.') && is_safe_extension(&value[1..]) {
        return value.to_ascii_lowercase();
    }
    Path::new(value)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| is_safe_extension(extension))
        .map(|extension| format!(".{}", extension.to_ascii_lowercase()))
        .unwrap_or_else(|| fallback.to_string())
}

fn is_safe_extension(value: &str) -> bool {
    !value.is_empty() && value.len() <= 12 && value.chars().all(|ch| ch.is_ascii_alphanumeric())
}

fn normalize_country_code(value: Option<&str>) -> Option<String> {
    let normalized = value?.trim().to_ascii_uppercase();
    (normalized.len() == 2 && normalized.chars().all(|ch| ch.is_ascii_uppercase()))
        .then_some(normalized)
}

fn flag_url_for_country_code(country_code: &str) -> String {
    format!(
        "{FLAG_CDN_BASE_URL}/{}.png",
        country_code.to_ascii_lowercase()
    )
}

fn intro_flag_url(intro_user: &IntroUserAssignment) -> Option<String> {
    intro_user
        .flag_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            normalize_country_code(intro_user.country_code.as_deref())
                .map(|country_code| flag_url_for_country_code(&country_code))
        })
}

fn safe_path_component(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .take(96)
        .collect();
    if sanitized.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        sanitized
    }
}

fn clamp_optional(value: Option<f64>, min: f64, max: f64) -> Option<f64> {
    value
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(min, max))
}

fn parse_percent(line: &str) -> Option<f64> {
    let percent_index = line.find('%')?;
    let before = &line[..percent_index];
    let value = before
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .filter(|part| !part.is_empty())
        .next_back()?;
    value.parse::<f64>().ok()
}

fn parse_gpu_name(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let value = trimmed
        .strip_prefix("GPU:")
        .or_else(|| trimmed.strip_prefix("gpu:"))?
        .trim();
    if value.is_empty() || value.to_ascii_lowercase().contains("fps") {
        None
    } else {
        Some(value.to_string())
    }
}

fn normalize_gpu_name(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("unknown") {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn detect_os() -> String {
    if cfg!(target_os = "windows") {
        format!("Windows {}", std::env::consts::ARCH)
    } else {
        format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
    }
}
