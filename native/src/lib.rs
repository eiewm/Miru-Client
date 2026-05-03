mod auth;
mod bins;
mod cache;
mod config;
mod error;
mod osu;
mod rules;
mod state;
mod tools;
mod types;
mod updates;
mod watch;
mod work;

use crate::config::{load_config, read_secret, save_config, write_secret, SecretKey};
use crate::error::{AppError, AppResult};
use crate::osu::{inspect_osu_stable_paths, validate_osu_stable_override};
use crate::state::{emit_state, load_history, log_line, snapshot, ManagedState};
use crate::types::{
    AppConfig, AppStatePayload, AutoRendererConfig, AutoRendererLibrary, AutoRendererLibraryPreset,
    AutoRendererLibrarySkin, AutoRendererSource, CountRule, HistoryEntry, JudgmentRules,
    RegisterServerInput, SaveSettingsInput, WatcherStatus, WorkerHistoryEntry, WorkerStatsPayload,
    WorkerStatus, DEFAULT_API_URL, DEFAULT_FRONTEND_URL,
};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::ffi::OsStr;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WindowEvent};
use tauri_plugin_notification::NotificationExt;

const START_MINIMIZED_ARG: &str = "--minimized-to-tray";
const TRAY_ID: &str = "miru-tray";
const TRAY_OPEN_ID: &str = "open";
const TRAY_START_WATCHER_ID: &str = "start_watcher";
const TRAY_STOP_WATCHER_ID: &str = "stop_watcher";
const TRAY_CONNECT_WORKER_ID: &str = "connect_worker";
const TRAY_DISCONNECT_WORKER_ID: &str = "disconnect_worker";
const TRAY_QUIT_ID: &str = "quit";
const MAX_SERVER_NAME_CHARS: usize = 18;
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[tauri::command]
async fn get_app_state(app: AppHandle) -> AppResult<AppStatePayload> {
    if let Err(error) = auth::sync_current_user_profile(&app).await {
        log_line(
            &app,
            format!("Failed to refresh current user profile: {error}"),
        );
    }
    snapshot(&app)
}

#[tauri::command]
async fn login(app: AppHandle) -> AppResult<()> {
    auth::login(app).await
}

#[tauri::command]
async fn logout(app: AppHandle) -> AppResult<()> {
    auth::logout(app).await
}

#[tauri::command]
async fn start_watcher(app: AppHandle) -> AppResult<()> {
    watch::start_watcher(app).await
}

#[tauri::command]
fn stop_watcher(app: AppHandle) -> AppResult<()> {
    watch::stop_watcher(&app)
}

#[tauri::command]
async fn ensure_renderer(app: AppHandle) -> AppResult<String> {
    Ok(bins::ensure_renderer(&app).await?.display().to_string())
}

#[tauri::command]
async fn run_benchmark(app: AppHandle) -> AppResult<crate::types::BenchmarkResult> {
    work::run_benchmark(app).await
}

#[tauri::command]
async fn get_benchmark_download_plan(
    app: AppHandle,
) -> AppResult<crate::types::BenchmarkDownloadPlan> {
    work::get_benchmark_download_plan(&app).await
}

#[tauri::command]
async fn get_server_slots(app: AppHandle) -> AppResult<i64> {
    work::get_server_slots(&app).await
}

#[tauri::command]
async fn register_server(app: AppHandle, input: RegisterServerInput) -> AppResult<()> {
    work::register_server(app, input).await
}

#[tauri::command]
async fn connect_worker(app: AppHandle) -> AppResult<()> {
    work::connect_worker(app).await
}

#[tauri::command]
async fn disconnect_worker(app: AppHandle) -> AppResult<()> {
    work::disconnect_worker(app).await
}

#[tauri::command]
async fn remove_server(app: AppHandle) -> AppResult<()> {
    work::remove_server(app).await
}

#[tauri::command]
async fn save_settings(app: AppHandle, input: SaveSettingsInput) -> AppResult<()> {
    let paths = app.state::<ManagedState>().paths.clone();
    validate_public_miru_url(&input.api_url, DEFAULT_API_URL, "API URL")?;
    validate_public_miru_url(&input.frontend_url, DEFAULT_FRONTEND_URL, "Frontend URL")?;
    let auto_renderer = sanitize_auto_renderer_config(input.auto_renderer)?;
    if let Some(webhook) = input.discord_webhook.as_deref() {
        if !webhook.trim().is_empty() {
            validate_discord_webhook(webhook)?;
            write_secret(&paths, SecretKey::DiscordWebhook, webhook.trim())?;
        } else {
            write_secret(&paths, SecretKey::DiscordWebhook, "")?;
        }
    }

    let mut config = load_config(&paths)?;
    let autostart_changed = config.autostart != input.autostart
        || config.start_minimized_to_tray != input.start_minimized_to_tray;
    let server_worker_settings_changed = config.server_name.trim() != input.server_name.trim()
        || config.show_discord_renderer_role != input.show_discord_renderer_role
        || config.show_gpu_in_status_image != input.show_gpu_in_status_image
        || config.connect_worker_on_launch != input.connect_worker_on_launch;
    config.api_url = DEFAULT_API_URL.to_string();
    config.frontend_url = DEFAULT_FRONTEND_URL.to_string();
    config.resolution = input.resolution;
    config.auto_renderer = auto_renderer;
    config.discord.enabled = input.discord_enabled;
    config.discord.webhook_set = read_secret(&paths, SecretKey::DiscordWebhook)?.is_some();
    config.server_name = sanitize_server_name(&input.server_name)?;
    config.renderer_override_path = input.renderer_override_path.trim().to_string();
    config.autostart = input.autostart;
    config.start_minimized_to_tray = input.start_minimized_to_tray;
    config.show_discord_renderer_role = input.show_discord_renderer_role;
    config.show_gpu_in_status_image = input.show_gpu_in_status_image;
    config.connect_worker_on_launch = input.connect_worker_on_launch;
    config.close_to_tray_on_exit = input.close_to_tray_on_exit;
    save_config(&paths, &config)?;
    if autostart_changed {
        apply_autostart(config.autostart, config.start_minimized_to_tray)?;
    }
    if server_worker_settings_changed && !config.user_id.trim().is_empty() {
        if let Some(token) = auth::ensure_fresh_session(&app).await? {
            if config.is_server && config.registered_user_id == config.user_id {
                work::sync_server_worker_settings(&app, &token, &config).await?;
            } else if work::refresh_server_status(&app, &token).await?.registered {
                let refreshed_config = load_config(&paths)?;
                work::sync_server_worker_settings(&app, &token, &refreshed_config).await?;
            }
        } else {
            log_line(
                &app,
                "Server worker settings saved locally; log in to sync Discord/status preferences",
            );
        }
    } else if config.is_server && config.registered_user_id == config.user_id {
        if let Some(token) = auth::ensure_fresh_session(&app).await? {
            work::sync_server_worker_settings(&app, &token, &config).await?;
        } else {
            log_line(
                &app,
                "Server worker settings saved locally; log in to sync Discord/status preferences",
            );
        }
    }
    log_line(&app, "Settings saved");
    emit_state(&app);
    Ok(())
}

#[tauri::command]
async fn test_discord_webhook(app: AppHandle) -> AppResult<bool> {
    let state = app.state::<ManagedState>();
    let Some(webhook) = read_secret(&state.paths, SecretKey::DiscordWebhook)? else {
        return Ok(false);
    };
    validate_discord_webhook(&webhook)?;
    let ok = state
        .http
        .post(webhook)
        .json(&serde_json::json!({
            "content": "Miru Desktop Client test notification"
        }))
        .send()
        .await?
        .status()
        .is_success();
    Ok(ok)
}

#[tauri::command]
fn open_logs_dir(app: AppHandle) -> AppResult<()> {
    let state = app.state::<ManagedState>();
    let mut command = Command::new("explorer");
    command.arg(&state.paths.logs_dir);
    hide_child_console(&mut command);
    command
        .spawn()
        .map_err(|err| AppError::Process(format!("failed to open logs directory: {err}")))?;
    Ok(())
}

#[tauri::command]
fn open_discord_invite() -> AppResult<()> {
    open_external_url("https://discord.gg/5kbA2NmwqS")
}

fn sanitize_server_name(value: &str) -> AppResult<String> {
    let normalized = value.trim();
    if normalized.chars().count() > MAX_SERVER_NAME_CHARS {
        return Err(AppError::InvalidInput(format!(
            "server name must be {MAX_SERVER_NAME_CHARS} characters or fewer"
        )));
    }
    Ok(normalized.to_string())
}

#[tauri::command]
fn select_renderer_path(app: AppHandle) -> AppResult<Option<String>> {
    let state = app.state::<ManagedState>();
    let config = load_config(&state.paths)?;
    let initial_dir = dialog_initial_dir(&config.renderer_override_path);
    pick_renderer_executable(initial_dir.as_deref())
}

fn open_external_url(url: &str) -> AppResult<()> {
    if url != "https://discord.gg/5kbA2NmwqS" {
        return Err(AppError::InvalidInput(
            "unsupported external URL".to_string(),
        ));
    }

    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("powershell");
        command.args([
            "-NoProfile",
            "-WindowStyle",
            "Hidden",
            "-Command",
            "Start-Process -FilePath 'https://discord.gg/5kbA2NmwqS'",
        ]);
        hide_child_console(&mut command);
        command
            .spawn()
            .map_err(|err| AppError::Process(format!("failed to open Discord invite: {err}")))?;
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        command.arg(url);
        hide_child_console(&mut command);
        command
            .spawn()
            .map_err(|err| AppError::Process(format!("failed to open Discord invite: {err}")))?;
        return Ok(());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        hide_child_console(&mut command);
        command
            .spawn()
            .map_err(|err| AppError::Process(format!("failed to open Discord invite: {err}")))?;
        return Ok(());
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
    {
        Err(AppError::Process(
            "opening external URLs is not supported on this platform".to_string(),
        ))
    }
}

#[tauri::command]
fn select_osu_stable_path(app: AppHandle) -> AppResult<Option<String>> {
    let state = app.state::<ManagedState>();
    let config = load_config(&state.paths)?;
    let initial_dir = dialog_initial_dir(&config.auto_renderer.osu_stable_path_override)
        .or_else(|| inspect_osu_stable_paths(&config).osu_stable_root);
    let Some(selected) = pick_local_folder(initial_dir.as_deref())? else {
        return Ok(None);
    };
    Ok(Some(validate_osu_stable_override(&selected)?))
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> AppResult<()> {
    let state = app.state::<ManagedState>();
    let mut config = load_config(&state.paths)?;
    if config.autostart == enabled {
        emit_state(&app);
        return Ok(());
    }
    config.autostart = enabled;
    save_config(&state.paths, &config)?;
    apply_autostart(enabled, config.start_minimized_to_tray)?;
    emit_state(&app);
    Ok(())
}

#[tauri::command]
fn get_history(app: AppHandle) -> AppResult<Vec<HistoryEntry>> {
    let state = app.state::<ManagedState>();
    load_history(&state.paths)
}

#[tauri::command]
async fn get_worker_history(app: AppHandle) -> AppResult<Vec<WorkerHistoryEntry>> {
    work::get_worker_history(app).await
}

#[tauri::command]
async fn get_worker_stats(app: AppHandle) -> AppResult<WorkerStatsPayload> {
    work::get_worker_stats(app).await
}

#[tauri::command]
async fn get_auto_renderer_library(app: AppHandle) -> AppResult<AutoRendererLibrary> {
    let Some(token) = auth::ensure_fresh_session(&app).await? else {
        return Ok(default_auto_renderer_library());
    };
    let state = app.state::<ManagedState>();
    let config = load_config(&state.paths)?;
    let api_url = auth::trim_trailing_slash(&config.api_url);

    let presets_response = state
        .http
        .get(format!("{api_url}/presets"))
        .bearer_auth(&token)
        .send()
        .await?;
    if presets_response.status() == reqwest::StatusCode::UNAUTHORIZED {
        auth::require_auth_response(&app, presets_response).await?;
        return Ok(default_auto_renderer_library());
    }
    let presets_body: ApiResponse<PresetsPayload> =
        presets_response.error_for_status()?.json().await?;

    let skins_response = state
        .http
        .get(format!("{api_url}/skins"))
        .bearer_auth(&token)
        .send()
        .await?;
    if skins_response.status() == reqwest::StatusCode::UNAUTHORIZED {
        auth::require_auth_response(&app, skins_response).await?;
        return Ok(default_auto_renderer_library());
    }
    let skins_body: ApiResponse<SkinsPayload> = skins_response.error_for_status()?.json().await?;

    let mut skins = vec![default_auto_renderer_skin()];
    for skin in skins_body.data.map(|data| data.skins).unwrap_or_default() {
        if skin.id != "default" {
            skins.push(skin);
        }
    }

    Ok(AutoRendererLibrary {
        presets: presets_body
            .data
            .map(|data| data.presets)
            .unwrap_or_default(),
        skins,
    })
}

#[tauri::command]
async fn check_client_update(app: AppHandle) -> AppResult<updates::ClientUpdateStatus> {
    let state = app.state::<ManagedState>();
    updates::check_client_update(&state.http).await
}

#[tauri::command]
async fn check_renderer_update(app: AppHandle) -> AppResult<bins::RendererUpdateStatus> {
    bins::renderer_update_status(&app).await
}

#[tauri::command]
async fn install_renderer_update(app: AppHandle) -> AppResult<String> {
    Ok(bins::ensure_renderer(&app).await?.display().to_string())
}

#[tauri::command]
async fn install_client_update(app: AppHandle) -> AppResult<()> {
    updates::install_client_update(&app).await?;
    app.exit(0);
    Ok(())
}

#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    data: Option<T>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PresetsPayload {
    presets: Vec<AutoRendererLibraryPreset>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkinsPayload {
    skins: Vec<AutoRendererLibrarySkin>,
}

fn default_auto_renderer_library() -> AutoRendererLibrary {
    AutoRendererLibrary {
        presets: Vec::new(),
        skins: vec![default_auto_renderer_skin()],
    }
}

fn default_auto_renderer_skin() -> AutoRendererLibrarySkin {
    AutoRendererLibrarySkin {
        id: "default".to_string(),
        name: "Default skin".to_string(),
        is_default: true,
        size_bytes: None,
        created_at: None,
    }
}

fn sanitize_auto_renderer_config(input: AutoRendererConfig) -> AppResult<AutoRendererConfig> {
    let mut deduped = BTreeSet::new();
    for value in input.key_counts {
        if !(1..=18).contains(&value) {
            return Err(AppError::InvalidInput(
                "Key count filters must stay between 1K and 18K".to_string(),
            ));
        }
        deduped.insert(value);
    }

    Ok(AutoRendererConfig {
        source: AutoRendererSource::OsuStable,
        osu_stable_path_override: validate_osu_stable_override(&input.osu_stable_path_override)?,
        selected_preset_id: sanitize_optional_id(input.selected_preset_id, "Preset")?,
        selected_skin_id: sanitize_required_id(input.selected_skin_id, "Skin", "default")?,
        key_counts: deduped.into_iter().collect(),
        long_note_rule: sanitize_count_rule(input.long_note_rule, "Long note count", 5_000_000.0)?,
        normal_note_rule: sanitize_count_rule(
            input.normal_note_rule,
            "Normal note count",
            5_000_000.0,
        )?,
        total_note_rule: sanitize_count_rule(
            input.total_note_rule,
            "Total note count",
            5_000_000.0,
        )?,
        max_combo_rule: sanitize_count_rule(input.max_combo_rule, "Max combo", 5_000_000.0)?,
        accuracy_rule: sanitize_count_rule(input.accuracy_rule, "Accuracy", 100.0)?,
        pp_rule: sanitize_count_rule(input.pp_rule, "PP", 100_000.0)?,
        bpm_rule: sanitize_count_rule(input.bpm_rule, "BPM", 10_000.0)?,
        hp_rule: sanitize_count_rule(input.hp_rule, "HP", 20.0)?,
        cs_rule: sanitize_count_rule(input.cs_rule, "CS", 18.0)?,
        od_rule: sanitize_count_rule(input.od_rule, "OD", 20.0)?,
        duration_rule: sanitize_count_rule(input.duration_rule, "Duration", 24.0 * 60.0 * 60.0)?,
        judgment_rules: sanitize_judgment_rules(input.judgment_rules)?,
    })
}

fn sanitize_judgment_rules(rules: JudgmentRules) -> AppResult<JudgmentRules> {
    Ok(JudgmentRules {
        max: sanitize_count_rule(rules.max, "MAX count", 5_000_000.0)?,
        n300: sanitize_count_rule(rules.n300, "300 count", 5_000_000.0)?,
        n200: sanitize_count_rule(rules.n200, "200 count", 5_000_000.0)?,
        n100: sanitize_count_rule(rules.n100, "100 count", 5_000_000.0)?,
        n50: sanitize_count_rule(rules.n50, "50 count", 5_000_000.0)?,
        miss: sanitize_count_rule(rules.miss, "Miss count", 5_000_000.0)?,
    })
}

fn sanitize_count_rule(mut rule: CountRule, label: &str, max_value: f64) -> AppResult<CountRule> {
    if !rule.value.is_finite() || rule.value < 0.0 {
        return Err(AppError::InvalidInput(format!(
            "{label} must be a non-negative finite number"
        )));
    }
    rule.value = round_rule_value(rule.value);

    if let Some(max) = rule.max_value {
        if !max.is_finite() || max < 0.0 {
            return Err(AppError::InvalidInput(format!(
                "{label} max must be a non-negative finite number"
            )));
        }
        rule.max_value = Some(round_rule_value(max));
    }

    let high = rule.max_value.unwrap_or(rule.value).max(rule.value);
    if rule.enabled && high > max_value {
        return Err(AppError::InvalidInput(format!(
            "{label} is above the supported safety limit"
        )));
    }
    Ok(rule)
}

fn round_rule_value(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn sanitize_optional_id(value: Option<String>, label: &str) -> AppResult<Option<String>> {
    let Some(raw) = value else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.len() > 128 {
        return Err(AppError::InvalidInput(format!("{label} id is too long")));
    }
    Ok(Some(trimmed.to_string()))
}

fn sanitize_required_id(value: String, label: &str, fallback: &str) -> AppResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(fallback.to_string());
    }
    if trimmed.len() > 128 {
        return Err(AppError::InvalidInput(format!("{label} id is too long")));
    }
    Ok(trimmed.to_string())
}

fn validate_public_miru_url(value: &str, expected: &str, label: &str) -> AppResult<()> {
    let parsed = url::Url::parse(value)
        .map_err(|_| AppError::InvalidInput(format!("invalid URL: {value}")))?;
    let expected = url::Url::parse(expected)
        .map_err(|_| AppError::Config(format!("invalid bundled {label}: {expected}")))?;
    let matches_expected = parsed.scheme() == "https"
        && parsed.host_str() == expected.host_str()
        && parsed.path().trim_end_matches('/') == expected.path().trim_end_matches('/')
        && parsed.query().is_none()
        && parsed.fragment().is_none();
    if matches_expected {
        Ok(())
    } else {
        Err(AppError::InvalidInput(format!(
            "{label} must be {expected}"
        )))
    }
}

fn validate_discord_webhook(value: &str) -> AppResult<()> {
    let parsed = url::Url::parse(value)
        .map_err(|_| AppError::InvalidInput("invalid Discord webhook URL".to_string()))?;
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    if parsed.scheme() == "https" && (host == "discord.com" || host == "discordapp.com") {
        Ok(())
    } else {
        Err(AppError::InvalidInput(
            "Discord webhook must be an HTTPS discord.com/discordapp.com URL".to_string(),
        ))
    }
}

fn apply_autostart(enabled: bool, start_minimized_to_tray: bool) -> AppResult<()> {
    let exe = std::env::current_exe()?;
    let mut startup_command = format!("\"{}\"", exe.display());
    if start_minimized_to_tray {
        startup_command.push(' ');
        startup_command.push_str(START_MINIMIZED_ARG);
    }
    let value_name = "MiruDesktopClient";
    let status = if enabled {
        let mut command = Command::new("reg");
        command
            .args([
                "add",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                value_name,
                "/t",
                "REG_SZ",
                "/d",
            ])
            .arg(startup_command)
            .arg("/f");
        hide_command_output(&mut command);
        hide_child_console(&mut command);
        command.status()?
    } else {
        let mut command = Command::new("reg");
        command.args([
            "delete",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "/v",
            value_name,
            "/f",
        ]);
        hide_command_output(&mut command);
        hide_child_console(&mut command);
        command.status()?
    };

    if status.success() || !enabled {
        Ok(())
    } else {
        Err(AppError::Process(
            "failed to update Windows autostart registry".to_string(),
        ))
    }
}

fn dialog_initial_dir(value: &str) -> Option<PathBuf> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let candidate = PathBuf::from(trimmed);
    if candidate.is_dir() {
        Some(candidate)
    } else {
        candidate.parent().map(Path::to_path_buf)
    }
}

#[cfg(target_os = "windows")]
fn pick_renderer_executable(initial_dir: Option<&Path>) -> AppResult<Option<String>> {
    let initial_dir = initial_dir
        .and_then(|path| path.to_str())
        .unwrap_or_default();
    let script = format!(
        "\
Add-Type -AssemblyName System.Windows.Forms\n\
$dialog = New-Object System.Windows.Forms.OpenFileDialog\n\
$dialog.Filter = 'Executable (*.exe)|*.exe|All files (*.*)|*.*'\n\
$dialog.CheckFileExists = $true\n\
$dialog.Multiselect = $false\n\
if ('{initial_dir}' -ne '') {{ $dialog.InitialDirectory = '{initial_dir}' }}\n\
if ($dialog.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {{ Write-Output $dialog.FileName }}\n",
        initial_dir = escape_powershell_literal(initial_dir),
    );
    run_powershell_picker(&script)
}

#[cfg(target_os = "windows")]
fn pick_local_folder(initial_dir: Option<&Path>) -> AppResult<Option<String>> {
    let initial_dir = initial_dir
        .and_then(|path| path.to_str())
        .unwrap_or_default();
    let script = format!(
        "\
Add-Type -AssemblyName System.Windows.Forms\n\
$dialog = New-Object System.Windows.Forms.FolderBrowserDialog\n\
$dialog.Description = 'Select osu!stable root'\n\
$dialog.ShowNewFolderButton = $false\n\
if ('{initial_dir}' -ne '') {{ $dialog.SelectedPath = '{initial_dir}' }}\n\
if ($dialog.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {{ Write-Output $dialog.SelectedPath }}\n",
        initial_dir = escape_powershell_literal(initial_dir),
    );
    run_powershell_picker(&script)
}

#[cfg(target_os = "windows")]
fn run_powershell_picker(script: &str) -> AppResult<Option<String>> {
    let mut command = Command::new("powershell");
    command.args(["-NoProfile", "-STA", "-Command", script]);
    hide_child_console(&mut command);
    let output = command.output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::Process(if stderr.is_empty() {
            "failed to open Windows picker dialog".to_string()
        } else {
            format!("failed to open Windows picker dialog: {stderr}")
        }));
    }

    let selected = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if selected.is_empty() {
        Ok(None)
    } else {
        Ok(Some(normalize_windows_dialog_path(selected)))
    }
}

#[cfg(target_os = "windows")]
fn escape_powershell_literal(value: &str) -> String {
    value.replace('\'', "''")
}

#[cfg(target_os = "windows")]
fn normalize_windows_dialog_path(value: String) -> String {
    if let Some(stripped) = value.strip_prefix(r"\\?\") {
        if !stripped.starts_with("UNC\\") {
            return stripped.to_string();
        }
    }
    value
}

#[cfg(target_os = "windows")]
fn hide_child_console(command: &mut Command) {
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn hide_child_console(_command: &mut Command) {}

fn hide_command_output(command: &mut Command) {
    command.stdout(Stdio::null()).stderr(Stdio::null());
}

#[cfg(not(target_os = "windows"))]
fn pick_renderer_executable(_initial_dir: Option<&Path>) -> AppResult<Option<String>> {
    Err(AppError::Unsupported(
        "renderer picker is only available on Windows".to_string(),
    ))
}

#[cfg(not(target_os = "windows"))]
fn pick_local_folder(_initial_dir: Option<&Path>) -> AppResult<Option<String>> {
    Err(AppError::Unsupported(
        "folder picker is only available on Windows".to_string(),
    ))
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn build_tray_menu(app: &AppHandle) -> AppResult<Menu<tauri::Wry>> {
    let menu = Menu::new(app)?;
    let open = MenuItem::with_id(app, TRAY_OPEN_ID, "Open Miru", true, None::<&str>)?;
    menu.append(&open)?;

    let state = app.state::<ManagedState>();
    let config = load_config(&state.paths)?;
    let (watcher_status, worker_status) = {
        let runtime = state.runtime.lock().expect("runtime state poisoned");
        (runtime.watcher_status, runtime.worker_status)
    };

    if is_authenticated_for_tray(&config) && can_use_auto_renderer_from_tray(&config) {
        match watcher_status {
            WatcherStatus::Running | WatcherStatus::Starting => {
                let stop = MenuItem::with_id(
                    app,
                    TRAY_STOP_WATCHER_ID,
                    "Stop Auto Renderer",
                    true,
                    None::<&str>,
                )?;
                menu.append(&stop)?;
            }
            WatcherStatus::Stopped | WatcherStatus::Error => {
                let start = MenuItem::with_id(
                    app,
                    TRAY_START_WATCHER_ID,
                    "Start Auto Renderer",
                    true,
                    None::<&str>,
                )?;
                menu.append(&start)?;
            }
        }
    }

    if is_authenticated_for_tray(&config) && can_control_server_worker_from_tray(&config) {
        match worker_status {
            WorkerStatus::Connected | WorkerStatus::Connecting => {
                let disconnect = MenuItem::with_id(
                    app,
                    TRAY_DISCONNECT_WORKER_ID,
                    "Disconnect Server Worker",
                    true,
                    None::<&str>,
                )?;
                menu.append(&disconnect)?;
            }
            WorkerStatus::Disconnected | WorkerStatus::Error => {
                let connect = MenuItem::with_id(
                    app,
                    TRAY_CONNECT_WORKER_ID,
                    "Connect Server Worker",
                    true,
                    None::<&str>,
                )?;
                menu.append(&connect)?;
            }
        }
    }

    let quit = MenuItem::with_id(app, TRAY_QUIT_ID, "Quit", true, None::<&str>)?;
    menu.append(&quit)?;
    Ok(menu)
}

fn is_authenticated_for_tray(config: &AppConfig) -> bool {
    !config.user_id.trim().is_empty()
}

fn can_use_auto_renderer_from_tray(config: &AppConfig) -> bool {
    let role = config.user_role.trim().to_ascii_uppercase();
    let plan = config.user_plan.trim().to_ascii_uppercase();
    matches!(role.as_str(), "PLUS" | "ADMIN") || plan == "PLUS"
}

fn can_control_server_worker_from_tray(config: &AppConfig) -> bool {
    config.is_server
        && !config.user_id.trim().is_empty()
        && config.registered_user_id.trim() == config.user_id.trim()
}

pub(crate) fn refresh_tray_menu(app: &AppHandle) -> AppResult<()> {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let menu = build_tray_menu(app)?;
        tray.set_menu(Some(menu))?;
    }
    Ok(())
}

fn setup_tray(app: &tauri::App) -> AppResult<()> {
    let menu = build_tray_menu(app.handle())?;
    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Miru Desktop Client")
        .menu(&menu)
        .show_menu_on_left_click(false);
    if let Some(icon) = app.default_window_icon().cloned() {
        tray = tray.icon(icon);
    }
    tray.on_tray_icon_event(|tray, event| match event {
        TrayIconEvent::DoubleClick {
            button: MouseButton::Left,
            ..
        } => show_main_window(tray.app_handle()),
        _ => {}
    })
    .on_menu_event(|app, event| match event.id().as_ref() {
        TRAY_OPEN_ID => show_main_window(app),
        TRAY_START_WATCHER_ID => {
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = watch::start_watcher(handle.clone()).await {
                    log_line(
                        &handle,
                        format!("Failed to start Auto Renderer from tray: {error}"),
                    );
                }
                let _ = refresh_tray_menu(&handle);
            });
        }
        TRAY_STOP_WATCHER_ID => {
            let _ = watch::stop_watcher(app);
            let _ = refresh_tray_menu(app);
        }
        TRAY_CONNECT_WORKER_ID => {
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = work::connect_worker(handle.clone()).await {
                    log_line(
                        &handle,
                        format!("Failed to connect Server Worker from tray: {error}"),
                    );
                }
                let _ = refresh_tray_menu(&handle);
            });
        }
        TRAY_DISCONNECT_WORKER_ID => {
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = work::disconnect_worker(handle.clone()).await {
                    log_line(
                        &handle,
                        format!("Failed to disconnect Server Worker from tray: {error}"),
                    );
                }
                let _ = refresh_tray_menu(&handle);
            });
        }
        TRAY_QUIT_ID => {
            let _ = watch::stop_watcher(app);
            app.exit(0);
        }
        _ => {}
    })
    .build(app)?;
    Ok(())
}

fn sync_autostart_config(app: &tauri::App) {
    let state = app.state::<ManagedState>();
    match load_config(&state.paths) {
        Ok(config) => {
            if let Err(error) = apply_autostart(config.autostart, config.start_minimized_to_tray) {
                log_line(
                    app.handle(),
                    format!("Failed to sync Windows autostart setting: {error}"),
                );
            }
        }
        Err(error) => log_line(
            app.handle(),
            format!("Failed to load config for Windows autostart sync: {error}"),
        ),
    }
}

fn should_start_minimized_to_tray() -> bool {
    std::env::args_os().any(|arg| arg == OsStr::new(START_MINIMIZED_ARG))
}

fn hide_main_window_if_requested(app: &tauri::App) {
    if !should_start_minimized_to_tray() {
        return;
    }

    let Some(window) = app.get_webview_window("main") else {
        log_line(
            app.handle(),
            "Start minimized requested, but the main window was not available",
        );
        return;
    };

    if let Err(error) = window.hide() {
        log_line(
            app.handle(),
            format!("Failed to start minimized to tray: {error}"),
        );
        return;
    }

    log_line(app.handle(), "Main window started hidden to tray");
}

fn handle_window_event(window: &tauri::Window, event: &WindowEvent) {
    if !matches!(event, WindowEvent::CloseRequested { .. }) {
        return;
    }

    let state = window.state::<ManagedState>();
    let should_close_to_tray = load_config(&state.paths)
        .map(|config| config.close_to_tray_on_exit)
        .unwrap_or(true);

    if !should_close_to_tray {
        return;
    }

    if let WindowEvent::CloseRequested { api, .. } = event {
        api.prevent_close();
    }

    if let Err(error) = window.hide() {
        log_line(
            window.app_handle(),
            format!("Failed to hide window to tray: {error}"),
        );
        return;
    }

    let app = window.app_handle();
    log_line(&app, "Main window hidden to tray");
    let _ = app
        .notification()
        .builder()
        .title("Miru is still running")
        .body(
            "Miru was sent to the tray. Reopen it from the tray icon, or quit from the tray menu.",
        )
        .show();
}

async fn hydrate_startup_state(app: AppHandle) {
    let token = match auth::ensure_fresh_session(&app).await {
        Ok(Some(token)) => token,
        Ok(None) => return,
        Err(error) => {
            log_line(
                &app,
                format!("Failed to refresh desktop session on startup: {error}"),
            );
            return;
        }
    };

    let status = match work::refresh_server_status(&app, &token).await {
        Ok(status) => status,
        Err(error) => {
            log_line(
                &app,
                format!("Failed to refresh renderer registration on startup: {error}"),
            );
            return;
        }
    };

    let state = app.state::<ManagedState>();
    let config = match load_config(&state.paths) {
        Ok(config) => config,
        Err(error) => {
            log_line(
                &app,
                format!("Failed to load desktop config after startup sync: {error}"),
            );
            return;
        }
    };

    if !status.registered
        || !config.connect_worker_on_launch
        || config.registered_user_id != config.user_id
    {
        return;
    }

    if let Err(error) = work::connect_worker(app.clone()).await {
        log_line(
            &app,
            format!("Failed to auto-connect renderer on startup: {error}"),
        );
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(ManagedState::new().expect("failed to initialize Miru app state"))
        .plugin(tauri_plugin_notification::init())
        .on_window_event(|window, event| handle_window_event(window, event))
        .setup(|app| {
            setup_tray(app)?;
            sync_autostart_config(app);
            hide_main_window_if_requested(app);
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                hydrate_startup_state(handle).await;
            });
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_app_state,
            login,
            logout,
            start_watcher,
            stop_watcher,
            ensure_renderer,
            run_benchmark,
            get_benchmark_download_plan,
            get_server_slots,
            register_server,
            connect_worker,
            disconnect_worker,
            remove_server,
            save_settings,
            test_discord_webhook,
            open_logs_dir,
            open_discord_invite,
            select_renderer_path,
            select_osu_stable_path,
            set_autostart,
            get_history,
            get_worker_history,
            get_worker_stats,
            get_auto_renderer_library,
            check_client_update,
            check_renderer_update,
            install_renderer_update,
            install_client_update
        ])
        .run(tauri::generate_context!())
        .expect("error while running Miru Desktop Client");
}
