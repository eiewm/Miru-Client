use crate::config::AppPaths;
use crate::error::{AppError, AppResult};
use crate::state::{log_line, ManagedState};
use reqwest::header::{ACCEPT, USER_AGENT};
use semver::Version;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::AsyncWriteExt;

const CLIENT_RELEASE_API_URL: &str =
    "https://api.github.com/repos/eiewm/Miru-Client/releases/latest";
const CLIENT_USER_AGENT: &str = "Miru Desktop Client";
const RELEASE_MANIFEST_NAME: &str = "manifest.json";
const WINDOWS_PLATFORM: &str = "windows-x64";
const CLIENT_UPDATE_PROGRESS_EVENT: &str = "client-update-progress";
const CLIENT_UPDATE_CLOSE_DELAY: Duration = Duration::from_millis(1400);
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientUpdateStatus {
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
    pub release_url: String,
    pub download_url: String,
    pub asset_name: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub published_at: Option<String>,
    pub current_commit: Option<String>,
    pub latest_commit: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientUpdateProgress {
    pub phase: String,
    pub percent: f64,
    pub message: String,
    pub downloaded_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    published_at: Option<String>,
    assets: Vec<GitHubReleaseAsset>,
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubReleaseAsset {
    name: String,
    size: u64,
    browser_download_url: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClientManifest {
    version: String,
    client_commit: String,
    platform: String,
    asset_name: String,
    size_bytes: u64,
    sha256: String,
}

#[derive(Debug, Clone)]
struct ResolvedClientRelease {
    release_url: String,
    installer_asset: GitHubReleaseAsset,
    manifest: ClientManifest,
    published_at: Option<String>,
}

pub async fn check_client_update(client: &reqwest::Client) -> AppResult<ClientUpdateStatus> {
    let resolved = fetch_latest_client_release(client).await?;
    let current_version = env!("CARGO_PKG_VERSION").to_string();
    let latest_version = normalize_version_label(&resolved.manifest.version);
    let current_commit = current_client_commit().map(ToOwned::to_owned);
    let latest_commit = resolved.manifest.client_commit.trim().to_string();

    Ok(ClientUpdateStatus {
        update_available: client_update_available(
            &current_version,
            &latest_version,
            &latest_commit,
        ),
        current_version,
        latest_version,
        release_url: resolved.release_url,
        download_url: resolved.installer_asset.browser_download_url,
        asset_name: resolved.installer_asset.name,
        size_bytes: resolved.manifest.size_bytes,
        sha256: resolved.manifest.sha256,
        published_at: resolved.published_at,
        current_commit,
        latest_commit,
    })
}

pub async fn install_client_update(app: &AppHandle) -> AppResult<PathBuf> {
    emit_client_update_progress(
        app,
        "checking",
        2.0,
        "Buscando la ultima release del cliente...",
        None,
        None,
    );
    let state = app.state::<ManagedState>();
    let resolved = fetch_latest_client_release(&state.http).await?;
    let current_version = env!("CARGO_PKG_VERSION").to_string();
    let latest_version = normalize_version_label(&resolved.manifest.version);
    let latest_commit = resolved.manifest.client_commit.trim().to_string();
    if !client_update_available(&current_version, &latest_version, &latest_commit) {
        emit_client_update_progress(
            app,
            "error",
            100.0,
            format!("Miru Desktop Client ya esta actualizado ({current_version})."),
            None,
            None,
        );
        return Err(AppError::Binary(format!(
            "Miru Desktop Client is already up to date ({current_version})"
        )));
    }

    let installer_path = client_installer_path(&state.paths, &resolved.manifest.asset_name)?;
    if verify_file_sha256(&installer_path, &resolved.manifest.sha256).unwrap_or(false) {
        emit_client_update_progress(
            app,
            "verifying",
            88.0,
            "Instalador ya descargado. Verificando checksum...",
            Some(resolved.manifest.size_bytes),
            Some(resolved.manifest.size_bytes),
        );
    } else {
        download_client_installer(app, &resolved, &installer_path).await?;
    }

    emit_client_update_progress(
        app,
        "launching",
        96.0,
        "Preparando el instalador del cliente...",
        Some(resolved.manifest.size_bytes),
        Some(resolved.manifest.size_bytes),
    );
    log_line(
        app,
        format!(
            "Starting Miru Desktop Client {} installer. The app will close to finish updating.",
            latest_version
        ),
    );
    if let Err(error) = launch_client_installer_after_exit(&installer_path) {
        emit_client_update_progress(
            app,
            "error",
            100.0,
            format!("No se pudo iniciar el instalador: {error}"),
            None,
            None,
        );
        return Err(error);
    }
    emit_client_update_progress(
        app,
        "closing",
        100.0,
        "Instalador iniciado. Miru se cerrara para terminar la instalacion.",
        Some(resolved.manifest.size_bytes),
        Some(resolved.manifest.size_bytes),
    );
    tokio::time::sleep(CLIENT_UPDATE_CLOSE_DELAY).await;
    Ok(installer_path)
}

async fn fetch_latest_client_release(client: &reqwest::Client) -> AppResult<ResolvedClientRelease> {
    let release: GitHubRelease = client
        .get(CLIENT_RELEASE_API_URL)
        .header(USER_AGENT, CLIENT_USER_AGENT)
        .header(ACCEPT, "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let manifest_asset = release_asset(&release, RELEASE_MANIFEST_NAME)?.clone();
    let manifest: ClientManifest = fetch_json_asset(
        client,
        &manifest_asset.browser_download_url,
        "Miru Client release manifest",
    )
    .await?;

    validate_manifest(&release, &manifest)?;
    let installer_asset = release_asset(&release, &manifest.asset_name)?.clone();
    if installer_asset.size != manifest.size_bytes {
        return Err(AppError::Binary(format!(
            "client release asset size mismatch: manifest says {}, GitHub says {}",
            manifest.size_bytes, installer_asset.size
        )));
    }

    Ok(ResolvedClientRelease {
        release_url: release.html_url,
        installer_asset,
        manifest,
        published_at: release.published_at,
    })
}

async fn fetch_json_asset<T: DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    label: &str,
) -> AppResult<T> {
    let bytes = client
        .get(url)
        .header(USER_AGENT, CLIENT_USER_AGENT)
        .header(ACCEPT, "application/octet-stream")
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    parse_json_bytes(&bytes, label)
}

fn parse_json_bytes<T: DeserializeOwned>(bytes: &[u8], label: &str) -> AppResult<T> {
    let body = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    serde_json::from_slice(body)
        .map_err(|err| AppError::Binary(format!("{label} is not valid JSON: {err}")))
}

fn release_asset<'a>(
    release: &'a GitHubRelease,
    asset_name: &str,
) -> AppResult<&'a GitHubReleaseAsset> {
    release
        .assets
        .iter()
        .find(|asset| asset_names_match(&asset.name, asset_name))
        .ok_or_else(|| {
            AppError::Binary(format!(
                "Miru Client release {} is missing asset {asset_name}",
                release.tag_name
            ))
        })
}

fn asset_names_match(actual: &str, expected: &str) -> bool {
    actual.eq_ignore_ascii_case(expected)
        || normalize_github_asset_name(actual) == normalize_github_asset_name(expected)
}

fn normalize_github_asset_name(name: &str) -> String {
    name.trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_whitespace() || ch == '.' {
                '-'
            } else {
                ch.to_ascii_lowercase()
            }
        })
        .collect()
}

fn validate_manifest(release: &GitHubRelease, manifest: &ClientManifest) -> AppResult<()> {
    if manifest.platform != WINDOWS_PLATFORM {
        return Err(AppError::Binary(format!(
            "client release {} is for unsupported platform {}",
            release.tag_name, manifest.platform
        )));
    }
    if manifest.asset_name.trim().is_empty() {
        return Err(AppError::Binary(
            "client manifest has no asset name".to_string(),
        ));
    }
    if !is_safe_asset_name(&manifest.asset_name) {
        return Err(AppError::Binary(
            "client manifest has an invalid installer asset name".to_string(),
        ));
    }
    if !manifest.asset_name.to_ascii_lowercase().ends_with(".exe") {
        return Err(AppError::Binary(
            "client installer asset must be a Windows .exe".to_string(),
        ));
    }
    if manifest.size_bytes == 0 {
        return Err(AppError::Binary(
            "client manifest has an empty installer size".to_string(),
        ));
    }
    if manifest.sha256.len() != 64 || !manifest.sha256.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(AppError::Binary(
            "client manifest has an invalid SHA-256".to_string(),
        ));
    }
    if manifest.client_commit.trim().is_empty() {
        return Err(AppError::Binary(
            "client manifest has no source commit".to_string(),
        ));
    }
    Ok(())
}

async fn download_client_installer(
    app: &AppHandle,
    resolved: &ResolvedClientRelease,
    installer_path: &Path,
) -> AppResult<()> {
    let state = app.state::<ManagedState>();
    log_line(
        app,
        format!(
            "Downloading Miru Desktop Client {} installer ({}) from GitHub Releases",
            normalize_version_label(&resolved.manifest.version),
            format_bytes(resolved.manifest.size_bytes)
        ),
    );

    let Some(parent) = installer_path.parent() else {
        return Err(AppError::Binary(
            "client installer path has no parent directory".to_string(),
        ));
    };
    fs::create_dir_all(parent)?;
    let temp_path = installer_path.with_file_name(format!(
        "{}.download",
        installer_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("miru-client-update.exe")
    ));
    if temp_path.exists() {
        fs::remove_file(&temp_path)?;
    }

    let response = state
        .http
        .get(&resolved.installer_asset.browser_download_url)
        .header(USER_AGENT, CLIENT_USER_AGENT)
        .send()
        .await?
        .error_for_status()?;
    let total_bytes = response
        .content_length()
        .unwrap_or(resolved.manifest.size_bytes)
        .max(resolved.manifest.size_bytes);

    emit_client_update_progress(
        app,
        "downloading",
        5.0,
        format!(
            "Descargando instalador 0 B / {}...",
            format_bytes(resolved.manifest.size_bytes)
        ),
        Some(0),
        Some(resolved.manifest.size_bytes),
    );

    let mut response = response;
    let mut file = tokio::fs::File::create(&temp_path).await?;
    let mut hasher = Sha256::new();
    let mut downloaded_bytes = 0_u64;
    let mut last_emit = Instant::now() - Duration::from_millis(500);
    let mut last_percent = 0_u64;

    while let Some(chunk) = response.chunk().await? {
        downloaded_bytes += chunk.len() as u64;
        hasher.update(&chunk);
        file.write_all(&chunk).await?;

        let percent = download_progress_percent(downloaded_bytes, total_bytes);
        let percent_bucket = percent.floor() as u64;
        if percent_bucket != last_percent || last_emit.elapsed() >= Duration::from_millis(250) {
            emit_client_update_progress(
                app,
                "downloading",
                percent,
                format!(
                    "Descargando instalador {} / {}...",
                    format_bytes(downloaded_bytes),
                    format_bytes(resolved.manifest.size_bytes)
                ),
                Some(downloaded_bytes),
                Some(resolved.manifest.size_bytes),
            );
            last_percent = percent_bucket;
            last_emit = Instant::now();
        }
    }
    file.flush().await?;
    drop(file);

    emit_client_update_progress(
        app,
        "verifying",
        84.0,
        "Verificando tamano y checksum del instalador...",
        Some(downloaded_bytes),
        Some(resolved.manifest.size_bytes),
    );

    if downloaded_bytes != resolved.manifest.size_bytes {
        let _ = fs::remove_file(&temp_path);
        return Err(AppError::Binary(format!(
            "client installer size mismatch: expected {} bytes, got {} bytes",
            resolved.manifest.size_bytes, downloaded_bytes
        )));
    }
    let actual_sha256 = hex::encode(hasher.finalize());
    if !actual_sha256.eq_ignore_ascii_case(&resolved.manifest.sha256) {
        let _ = fs::remove_file(&temp_path);
        return Err(AppError::Binary(
            "client installer checksum mismatch after download".to_string(),
        ));
    }

    if installer_path.exists() {
        fs::remove_file(installer_path)?;
    }
    fs::rename(&temp_path, installer_path)?;
    emit_client_update_progress(
        app,
        "ready",
        92.0,
        "Instalador descargado y verificado.",
        Some(resolved.manifest.size_bytes),
        Some(resolved.manifest.size_bytes),
    );
    Ok(())
}

fn emit_client_update_progress(
    app: &AppHandle,
    phase: impl Into<String>,
    percent: f64,
    message: impl Into<String>,
    downloaded_bytes: Option<u64>,
    total_bytes: Option<u64>,
) {
    let payload = ClientUpdateProgress {
        phase: phase.into(),
        percent: percent.clamp(0.0, 100.0),
        message: message.into(),
        downloaded_bytes,
        total_bytes,
    };
    let _ = app.emit(CLIENT_UPDATE_PROGRESS_EVENT, payload);
}

fn download_progress_percent(downloaded_bytes: u64, total_bytes: u64) -> f64 {
    if total_bytes == 0 {
        return 5.0;
    }
    let ratio = (downloaded_bytes as f64 / total_bytes as f64).clamp(0.0, 1.0);
    5.0 + (ratio * 75.0)
}

fn client_installer_path(paths: &AppPaths, asset_name: &str) -> AppResult<PathBuf> {
    if !is_safe_asset_name(asset_name) {
        return Err(AppError::Binary(
            "client installer asset name is not safe".to_string(),
        ));
    }
    Ok(paths.data_dir.join("updates").join(asset_name))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn verify_file_sha256(path: &Path, expected: &str) -> AppResult<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let bytes = fs::read(path)?;
    Ok(sha256_hex(&bytes).eq_ignore_ascii_case(expected))
}

fn format_bytes(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    format!("{:.2} MiB", bytes as f64 / MIB)
}

fn is_safe_asset_name(asset_name: &str) -> bool {
    let trimmed = asset_name.trim();
    if trimmed.is_empty() || trimmed != asset_name {
        return false;
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return false;
    }
    Path::new(trimmed)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == trimmed)
}

#[cfg(target_os = "windows")]
fn launch_client_installer_after_exit(installer_path: &Path) -> AppResult<()> {
    let installer = escape_powershell_literal(&installer_path.display().to_string());
    let parent_pid = std::process::id();
    let script = format!(
        "\
$installer = '{installer}'\n\
$parentPid = {parent_pid}\n\
try {{ Wait-Process -Id $parentPid -Timeout 45 -ErrorAction SilentlyContinue }} catch {{ }}\n\
Start-Process -FilePath $installer -ArgumentList '/S' -WindowStyle Hidden\n\
"
    );

    let mut command = Command::new("powershell");
    command.args([
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-WindowStyle",
        "Hidden",
        "-Command",
        &script,
    ]);
    hide_command_output(&mut command);
    hide_child_console(&mut command);
    command
        .spawn()
        .map_err(|err| AppError::Process(format!("failed to start client installer: {err}")))?;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn launch_client_installer_after_exit(_installer_path: &Path) -> AppResult<()> {
    Err(AppError::Process(
        "client self-update is only supported on Windows".to_string(),
    ))
}

#[cfg(target_os = "windows")]
fn escape_powershell_literal(value: &str) -> String {
    value.replace('\'', "''")
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

fn normalize_version_label(version: &str) -> String {
    version.trim().trim_start_matches('v').to_string()
}

fn parse_version(version: &str) -> Option<Version> {
    Version::parse(&normalize_version_label(version)).ok()
}

fn is_newer_version(current: &str, latest: &str) -> bool {
    match (parse_version(current), parse_version(latest)) {
        (Some(current), Some(latest)) => latest > current,
        _ => normalize_version_label(latest) != normalize_version_label(current),
    }
}

fn current_client_commit() -> Option<&'static str> {
    option_env!("MIRU_CLIENT_COMMIT").filter(|commit| !commit.trim().is_empty())
}

fn client_update_available(
    current_version: &str,
    latest_version: &str,
    latest_commit: &str,
) -> bool {
    if is_newer_version(current_version, latest_version) {
        return true;
    }
    if normalize_version_label(current_version) != normalize_version_label(latest_version) {
        return false;
    }
    current_client_commit().is_some_and(|commit| !commit.eq_ignore_ascii_case(latest_commit.trim()))
}
