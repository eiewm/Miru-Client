use crate::config::{load_config, save_config, AppPaths};
use crate::error::{AppError, AppResult};
use crate::state::{emit_state, log_line, ManagedState};
use reqwest::header::{ACCEPT, USER_AGENT};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

const RENDERER_RELEASE_API_URL: &str =
    "https://api.github.com/repos/eiewm/Miru-Renderer/releases/latest";
const RENDERER_USER_AGENT: &str = "Miru Desktop Client";
const RELEASE_MANIFEST_NAME: &str = "manifest.json";
const MANAGED_RENDERER_MANIFEST_NAME: &str = "miru-renderer-manifest.json";
const WINDOWS_PLATFORM: &str = "windows-x64";
const WINDOWS_RENDERER_ASSET_NAME: &str = "miru.exe";

#[derive(Debug, Clone)]
pub struct RendererDownloadInfo {
    pub version: String,
    pub size_bytes: u64,
    pub install_path: PathBuf,
    pub already_available: bool,
    pub release_url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RendererUpdateStatus {
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
    pub installed: bool,
    pub custom_override: bool,
    pub release_url: String,
    pub asset_name: String,
    pub size_bytes: u64,
    pub install_path: String,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    assets: Vec<GitHubReleaseAsset>,
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubReleaseAsset {
    name: String,
    size: u64,
    browser_download_url: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RendererManifest {
    version: String,
    renderer_commit: String,
    platform: String,
    asset_name: String,
    size_bytes: u64,
    sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RendererReleaseManifest {
    version: String,
    renderer_commit: String,
    #[serde(default)]
    assets: Vec<RendererManifestAsset>,
    #[serde(default)]
    platform: Option<String>,
    #[serde(default)]
    asset_name: Option<String>,
    #[serde(default)]
    size_bytes: Option<u64>,
    #[serde(default)]
    sha256: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RendererManifestAsset {
    platform: String,
    asset_name: String,
    size_bytes: u64,
    sha256: String,
}

#[derive(Debug, Clone)]
struct ResolvedRendererRelease {
    release_url: String,
    binary_asset: GitHubReleaseAsset,
    manifest: RendererManifest,
}

pub async fn renderer_download_info(app: &AppHandle) -> AppResult<RendererDownloadInfo> {
    let state = app.state::<ManagedState>();
    let config = load_config(&state.paths)?;
    if let Some(path) = custom_renderer_override(&state.paths, &config.renderer_override_path) {
        let size_bytes = fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        return Ok(RendererDownloadInfo {
            version: "custom".to_string(),
            size_bytes,
            install_path: path,
            already_available: true,
            release_url: String::new(),
        });
    }

    let resolved = fetch_latest_renderer_release(&state.http).await?;
    let install_path = managed_renderer_path(&state.paths, &resolved.manifest);
    let already_available =
        verify_file_sha256(&install_path, &resolved.manifest.sha256).unwrap_or(false);

    Ok(RendererDownloadInfo {
        version: resolved.manifest.version,
        size_bytes: resolved.manifest.size_bytes,
        install_path,
        already_available,
        release_url: resolved.release_url,
    })
}

pub async fn renderer_update_status(app: &AppHandle) -> AppResult<RendererUpdateStatus> {
    let state = app.state::<ManagedState>();
    let config = load_config(&state.paths)?;
    let resolved = fetch_latest_renderer_release(&state.http).await?;

    if let Some(path) = custom_renderer_override(&state.paths, &config.renderer_override_path) {
        return Ok(RendererUpdateStatus {
            current_version: "custom".to_string(),
            latest_version: resolved.manifest.version,
            update_available: false,
            installed: true,
            custom_override: true,
            release_url: resolved.release_url,
            asset_name: resolved.binary_asset.name,
            size_bytes: resolved.manifest.size_bytes,
            install_path: path.display().to_string(),
        });
    }

    let install_path = managed_renderer_path(&state.paths, &resolved.manifest);
    let latest_installed =
        verify_file_sha256(&install_path, &resolved.manifest.sha256).unwrap_or(false);
    let installed = install_path.exists();
    let current_version = if latest_installed {
        resolved.manifest.version.clone()
    } else if installed {
        read_managed_renderer_manifest(&state.paths)
            .map(|manifest| manifest.version)
            .unwrap_or_else(|| "unknown".to_string())
    } else {
        "not installed".to_string()
    };

    Ok(RendererUpdateStatus {
        current_version,
        latest_version: resolved.manifest.version,
        update_available: installed && !latest_installed,
        installed,
        custom_override: false,
        release_url: resolved.release_url,
        asset_name: resolved.binary_asset.name,
        size_bytes: resolved.manifest.size_bytes,
        install_path: install_path.display().to_string(),
    })
}

pub async fn ensure_renderer(app: &AppHandle) -> AppResult<PathBuf> {
    let state = app.state::<ManagedState>();
    let config = load_config(&state.paths)?;
    if let Some(path) = custom_renderer_override(&state.paths, &config.renderer_override_path) {
        return Ok(path);
    }

    let resolved = fetch_latest_renderer_release(&state.http).await?;
    let renderer_path = managed_renderer_path(&state.paths, &resolved.manifest);
    if verify_file_sha256(&renderer_path, &resolved.manifest.sha256).unwrap_or(false) {
        persist_managed_renderer_manifest(&state.paths, &resolved.manifest)?;
        persist_managed_renderer_path(app, &renderer_path)?;
        return Ok(renderer_path);
    }

    log_line(
        app,
        format!(
            "Downloading Miru renderer {} ({}) from GitHub Releases",
            resolved.manifest.version,
            format_bytes(resolved.manifest.size_bytes)
        ),
    );
    let response = state
        .http
        .get(&resolved.binary_asset.browser_download_url)
        .header(USER_AGENT, RENDERER_USER_AGENT)
        .send()
        .await?
        .error_for_status()?;
    let bytes = response.bytes().await?;

    if bytes.len() as u64 != resolved.manifest.size_bytes {
        return Err(AppError::Binary(format!(
            "renderer size mismatch: expected {} bytes, got {} bytes",
            resolved.manifest.size_bytes,
            bytes.len()
        )));
    }
    if !sha256_hex(&bytes).eq_ignore_ascii_case(&resolved.manifest.sha256) {
        return Err(AppError::Binary(
            "renderer checksum mismatch after download".to_string(),
        ));
    }

    fs::create_dir_all(&state.paths.bin_dir)?;
    fs::write(&renderer_path, &bytes)?;
    make_renderer_executable(&renderer_path)?;
    persist_managed_renderer_manifest(&state.paths, &resolved.manifest)?;
    persist_managed_renderer_path(app, &renderer_path)?;
    log_line(
        app,
        format!(
            "Miru renderer {} installed at {}",
            resolved.manifest.version,
            renderer_path.display()
        ),
    );
    Ok(renderer_path)
}

async fn fetch_latest_renderer_release(
    client: &reqwest::Client,
) -> AppResult<ResolvedRendererRelease> {
    let release: GitHubRelease = client
        .get(RENDERER_RELEASE_API_URL)
        .header(USER_AGENT, RENDERER_USER_AGENT)
        .header(ACCEPT, "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let manifest_asset = release_asset(&release, RELEASE_MANIFEST_NAME)?.clone();
    let release_manifest: RendererReleaseManifest = fetch_json_asset(
        client,
        &manifest_asset.browser_download_url,
        "Miru renderer release manifest",
    )
    .await?;

    let manifest = select_windows_manifest(&release, release_manifest)?;
    validate_manifest(&release, &manifest)?;
    let binary_asset = release_asset(&release, &manifest.asset_name)?.clone();
    if binary_asset.size != manifest.size_bytes {
        return Err(AppError::Binary(format!(
            "renderer release asset size mismatch: manifest says {}, GitHub says {}",
            manifest.size_bytes, binary_asset.size
        )));
    }

    Ok(ResolvedRendererRelease {
        release_url: release.html_url,
        binary_asset,
        manifest,
    })
}

async fn fetch_json_asset<T: DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    label: &str,
) -> AppResult<T> {
    let bytes = client
        .get(url)
        .header(USER_AGENT, RENDERER_USER_AGENT)
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

fn select_windows_manifest(
    release: &GitHubRelease,
    manifest: RendererReleaseManifest,
) -> AppResult<RendererManifest> {
    if !manifest.assets.is_empty() {
        let asset = manifest
            .assets
            .iter()
            .find(|asset| asset.platform.eq_ignore_ascii_case(WINDOWS_PLATFORM))
            .ok_or_else(|| {
                AppError::Binary(format!(
                    "Miru renderer release {} has no Windows renderer asset",
                    release.tag_name
                ))
            })?;
        return Ok(RendererManifest {
            version: manifest.version,
            renderer_commit: manifest.renderer_commit,
            platform: asset.platform.clone(),
            asset_name: asset.asset_name.clone(),
            size_bytes: asset.size_bytes,
            sha256: asset.sha256.clone(),
        });
    }

    let Some(legacy_platform) = manifest.platform else {
        return Err(AppError::Binary(format!(
            "Miru renderer release {} manifest has no platform assets",
            release.tag_name
        )));
    };
    Ok(RendererManifest {
        version: manifest.version,
        renderer_commit: manifest.renderer_commit,
        platform: legacy_platform,
        asset_name: manifest.asset_name.unwrap_or_default(),
        size_bytes: manifest.size_bytes.unwrap_or_default(),
        sha256: manifest.sha256.unwrap_or_default(),
    })
}

fn release_asset<'a>(
    release: &'a GitHubRelease,
    asset_name: &str,
) -> AppResult<&'a GitHubReleaseAsset> {
    release
        .assets
        .iter()
        .find(|asset| asset.name.eq_ignore_ascii_case(asset_name))
        .ok_or_else(|| {
            AppError::Binary(format!(
                "Miru renderer release {} is missing asset {asset_name}",
                release.tag_name
            ))
        })
}

fn validate_manifest(release: &GitHubRelease, manifest: &RendererManifest) -> AppResult<()> {
    if !manifest.platform.eq_ignore_ascii_case(WINDOWS_PLATFORM) {
        return Err(AppError::Binary(format!(
            "renderer release {} asset platform {} is not supported by the Windows client",
            release.tag_name, manifest.platform
        )));
    }
    if !manifest
        .asset_name
        .eq_ignore_ascii_case(WINDOWS_RENDERER_ASSET_NAME)
    {
        return Err(AppError::Binary(format!(
            "renderer release {} must expose {WINDOWS_RENDERER_ASSET_NAME} for the Windows client",
            release.tag_name
        )));
    }
    if !is_safe_asset_name(&manifest.asset_name) {
        return Err(AppError::Binary(
            "renderer manifest has an invalid asset name".to_string(),
        ));
    }
    if manifest.size_bytes == 0 {
        return Err(AppError::Binary(
            "renderer manifest has an empty binary size".to_string(),
        ));
    }
    if manifest.sha256.len() != 64 || !manifest.sha256.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(AppError::Binary(
            "renderer manifest has an invalid SHA-256".to_string(),
        ));
    }
    if manifest.renderer_commit.trim().is_empty() {
        return Err(AppError::Binary(
            "renderer manifest has no source commit".to_string(),
        ));
    }
    Ok(())
}

fn managed_renderer_path(paths: &AppPaths, manifest: &RendererManifest) -> PathBuf {
    let file_name = Path::new(&manifest.asset_name)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .map_or_else(
            || WINDOWS_RENDERER_ASSET_NAME.to_string(),
            ToString::to_string,
        );
    paths.bin_dir.join(file_name)
}

fn custom_renderer_override(paths: &AppPaths, raw_path: &str) -> Option<PathBuf> {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() {
        return None;
    }

    let path = PathBuf::from(trimmed);
    if !path.exists() || is_managed_renderer_path(paths, &path) {
        return None;
    }
    Some(path)
}

fn is_managed_renderer_path(paths: &AppPaths, path: &Path) -> bool {
    if let (Ok(left), Ok(bin_dir)) = (fs::canonicalize(path), fs::canonicalize(&paths.bin_dir)) {
        return left.parent() == Some(bin_dir.as_path());
    }

    let Some(file_name) = path.file_name() else {
        return false;
    };
    let fallback = paths.bin_dir.join(file_name);
    paths_equal(path, &fallback)
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    if let (Ok(left), Ok(right)) = (fs::canonicalize(left), fs::canonicalize(right)) {
        return left == right;
    }

    normalize_path_text(left).eq_ignore_ascii_case(&normalize_path_text(right))
}

fn normalize_path_text(path: &Path) -> String {
    path.display().to_string().replace('/', "\\")
}

fn persist_managed_renderer_path(app: &AppHandle, renderer_path: &Path) -> AppResult<()> {
    let state = app.state::<ManagedState>();
    let mut config = load_config(&state.paths)?;
    let renderer_path_text = renderer_path.display().to_string();
    if config.renderer_override_path != renderer_path_text {
        config.renderer_override_path = renderer_path_text;
        save_config(&state.paths, &config)?;
        emit_state(app);
    }
    Ok(())
}

fn read_managed_renderer_manifest(paths: &AppPaths) -> Option<RendererManifest> {
    let path = paths.bin_dir.join(MANAGED_RENDERER_MANIFEST_NAME);
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn persist_managed_renderer_manifest(
    paths: &AppPaths,
    manifest: &RendererManifest,
) -> AppResult<()> {
    fs::create_dir_all(&paths.bin_dir)?;
    let raw = serde_json::to_string_pretty(manifest)?;
    fs::write(paths.bin_dir.join(MANAGED_RENDERER_MANIFEST_NAME), raw)?;
    Ok(())
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

fn make_renderer_executable(_path: &Path) -> AppResult<()> {
    Ok(())
}
