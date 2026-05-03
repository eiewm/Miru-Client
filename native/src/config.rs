use crate::error::{AppError, AppResult};
use crate::types::{AppConfig, DiscordConfig, Resolution, DEFAULT_API_URL, DEFAULT_FRONTEND_URL};
use directories::ProjectDirs;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub bin_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub config_path: PathBuf,
    pub history_path: PathBuf,
    pub beatmap_cache_path: PathBuf,
    secrets_dir: PathBuf,
}

impl AppPaths {
    pub fn new() -> AppResult<Self> {
        let dirs = ProjectDirs::from("uno", "Miru", "Miru Desktop Client").ok_or_else(|| {
            AppError::Config("could not resolve Miru project directories".to_string())
        })?;
        let config_dir = dirs.config_dir().to_path_buf();
        let data_dir = dirs.data_local_dir().to_path_buf();
        let bin_dir = data_dir.join("bin");
        let logs_dir = data_dir.join("logs");
        let secrets_dir = config_dir.join("secrets");
        let config_path = config_dir.join("config.json");
        let history_path = data_dir.join("history.json");
        let beatmap_cache_path = data_dir.join("beatmap-cache.json");

        for dir in [&config_dir, &data_dir, &bin_dir, &logs_dir, &secrets_dir] {
            fs::create_dir_all(dir)?;
        }

        Ok(Self {
            config_dir,
            data_dir,
            bin_dir,
            logs_dir,
            config_path,
            history_path,
            beatmap_cache_path,
            secrets_dir,
        })
    }

    pub fn secret_path(&self, key: SecretKey) -> PathBuf {
        self.secrets_dir.join(format!("{}.dpapi", key.file_stem()))
    }
}

#[derive(Debug, Clone, Copy)]
pub enum SecretKey {
    ApiToken,
    RefreshToken,
    WorkerToken,
    DiscordWebhook,
}

impl SecretKey {
    fn file_stem(self) -> &'static str {
        match self {
            Self::ApiToken => "api_token",
            Self::RefreshToken => "refresh_token",
            Self::WorkerToken => "worker_token",
            Self::DiscordWebhook => "discord_webhook",
        }
    }
}

pub fn load_config(paths: &AppPaths) -> AppResult<AppConfig> {
    if paths.config_path.exists() {
        let raw = fs::read_to_string(&paths.config_path)?;
        let (mut config, normalized_config) = parse_app_config(raw.trim_start_matches('\u{feff}'))?;
        let mut should_save = normalized_config;
        if config.machine_id.trim().is_empty() {
            config.machine_id = generate_machine_id();
            should_save = true;
        }
        config.discord.webhook_set = read_secret(paths, SecretKey::DiscordWebhook)?.is_some();
        if migrate_client_endpoints(&mut config) {
            should_save = true;
        }
        if should_save {
            save_config(paths, &config)?;
        }
        return Ok(config);
    }

    let mut config = import_legacy_config().unwrap_or_default();
    if config.machine_id.trim().is_empty() {
        config.machine_id = generate_machine_id();
    }
    migrate_client_endpoints(&mut config);
    save_config(paths, &config)?;
    Ok(config)
}

pub fn save_config(paths: &AppPaths, config: &AppConfig) -> AppResult<()> {
    fs::create_dir_all(&paths.config_dir)?;
    let serialized = serde_json::to_string_pretty(config)?;
    fs::write(&paths.config_path, serialized)?;
    Ok(())
}

pub fn read_secret(paths: &AppPaths, key: SecretKey) -> AppResult<Option<String>> {
    let path = paths.secret_path(key);
    if !path.exists() {
        return Ok(None);
    }

    let protected_hex = fs::read_to_string(path)?;
    if protected_hex.trim().is_empty() {
        return Ok(None);
    }

    let protected = hex::decode(protected_hex.trim())
        .map_err(|err| AppError::Config(format!("invalid protected secret encoding: {err}")))?;
    let plaintext = unprotect_secret(&protected)?;
    Ok(Some(String::from_utf8_lossy(&plaintext).to_string()))
}

pub fn write_secret(paths: &AppPaths, key: SecretKey, value: &str) -> AppResult<()> {
    fs::create_dir_all(&paths.secrets_dir)?;
    let path = paths.secret_path(key);
    if value.trim().is_empty() {
        if path.exists() {
            fs::remove_file(path)?;
        }
        return Ok(());
    }

    let protected = protect_secret(value.as_bytes())?;
    fs::write(path, hex::encode(protected))?;
    Ok(())
}

pub fn clear_auth_secrets(paths: &AppPaths) -> AppResult<()> {
    write_secret(paths, SecretKey::ApiToken, "")?;
    write_secret(paths, SecretKey::RefreshToken, "")?;
    write_secret(paths, SecretKey::WorkerToken, "")?;
    Ok(())
}

fn import_legacy_config() -> Option<AppConfig> {
    let appdata = std::env::var("APPDATA").ok()?;
    let legacy_path = PathBuf::from(appdata)
        .join("miru-client-nodejs")
        .join("Config")
        .join("config.json");
    let raw = fs::read_to_string(legacy_path).ok()?;
    let value: Value = serde_json::from_str(raw.trim_start_matches('\u{feff}')).ok()?;
    let mut config = AppConfig::default();

    if let Some(username) = value.get("username").and_then(Value::as_str) {
        config.username = username.to_string();
    }
    if let Some(api_url) = value.get("apiUrl").and_then(Value::as_str) {
        config.api_url = normalize_legacy_url(api_url, DEFAULT_API_URL);
    }
    if let Some(frontend_url) = value.get("frontendUrl").and_then(Value::as_str) {
        config.frontend_url = normalize_legacy_url(frontend_url, DEFAULT_FRONTEND_URL);
    }
    if let Some(resolution) = value.get("resolution").and_then(Value::as_str) {
        config.resolution = resolution_from_string(resolution);
    }
    if let Some(server_name) = value.get("serverName").and_then(Value::as_str) {
        config.server_name = server_name.to_string();
    }
    if let Some(server_gpu) = value.get("serverGpu").and_then(Value::as_str) {
        config.server_gpu = server_gpu.to_string();
    }
    if let Some(is_server) = value.get("isServer").and_then(Value::as_bool) {
        config.is_server = is_server;
    }
    if let Some(auto_reconnect) = value.get("serverAutoReconnect").and_then(Value::as_bool) {
        config.server_auto_reconnect = auto_reconnect;
    }
    if let Some(connect_on_launch) = value.get("connectWorkerOnLaunch").and_then(Value::as_bool) {
        config.connect_worker_on_launch = connect_on_launch;
    }
    if let Some(show_role) = value
        .get("showDiscordRendererRole")
        .and_then(Value::as_bool)
    {
        config.show_discord_renderer_role = show_role;
    }
    if let Some(show_gpu) = value.get("showGpuInStatusImage").and_then(Value::as_bool) {
        config.show_gpu_in_status_image = show_gpu;
    }
    if let Some(renderer_path) = value.get("miruRendererPath").and_then(Value::as_str) {
        config.renderer_override_path = renderer_path.to_string();
    }
    if let Some(discord) = value.get("discord").and_then(Value::as_object) {
        config.discord = DiscordConfig {
            enabled: discord
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            webhook_set: false,
        };
    }

    config.imported_legacy_config = true;
    Some(config)
}

fn resolution_from_string(value: &str) -> Resolution {
    let first_number = value
        .trim_start_matches('p')
        .split(|character: char| !character.is_ascii_digit())
        .next()
        .and_then(|digits| digits.parse::<u32>().ok())
        .unwrap_or(720);

    if first_number >= 1080 {
        Resolution::P1080
    } else {
        Resolution::P720
    }
}

fn parse_app_config(raw: &str) -> AppResult<(AppConfig, bool)> {
    let mut value: Value = serde_json::from_str(raw)?;
    let mut changed = false;

    if let Some(resolution) = value.get("resolution").and_then(Value::as_str) {
        let normalized = match resolution_from_string(resolution) {
            Resolution::P1080 => "p1080",
            Resolution::P720 => "p720",
        };
        if resolution != normalized {
            value["resolution"] = Value::String(normalized.to_string());
            changed = true;
        }
    }

    Ok((serde_json::from_value(value)?, changed))
}

fn migrate_client_endpoints(config: &mut AppConfig) -> bool {
    let mut changed = false;
    if !is_expected_public_endpoint(&config.api_url, DEFAULT_API_URL) {
        config.api_url = DEFAULT_API_URL.to_string();
        changed = true;
    }
    if !is_expected_public_endpoint(&config.frontend_url, DEFAULT_FRONTEND_URL) {
        config.frontend_url = DEFAULT_FRONTEND_URL.to_string();
        changed = true;
    }
    changed
}

fn is_expected_public_endpoint(value: &str, expected: &str) -> bool {
    let normalized = value.trim().trim_end_matches('/');
    if normalized.is_empty() {
        return false;
    }

    let Ok(parsed) = url::Url::parse(normalized) else {
        return false;
    };
    let Ok(expected) = url::Url::parse(expected) else {
        return false;
    };

    parsed.scheme() == "https"
        && parsed.host_str() == expected.host_str()
        && parsed.path().trim_end_matches('/') == expected.path().trim_end_matches('/')
        && parsed.query().is_none()
        && parsed.fragment().is_none()
}

fn normalize_legacy_url(value: &str, production: &str) -> String {
    let normalized = value.trim().trim_end_matches('/');
    if is_expected_public_endpoint(normalized, production) {
        normalized.to_string()
    } else {
        production.to_string()
    }
}

fn generate_machine_id() -> String {
    format!("desktop:{}", Uuid::new_v4())
}

#[cfg(target_os = "windows")]
fn protect_secret(bytes: &[u8]) -> AppResult<Vec<u8>> {
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let mut input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: null_mut(),
    };

    let ok = unsafe {
        CryptProtectData(
            &mut input,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };

    if ok == 0 {
        return Err(AppError::Config(
            "DPAPI CryptProtectData failed".to_string(),
        ));
    }

    let protected =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        let _ = LocalFree(output.pbData.cast());
    }
    Ok(protected)
}

#[cfg(target_os = "windows")]
fn unprotect_secret(bytes: &[u8]) -> AppResult<Vec<u8>> {
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let mut input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: null_mut(),
    };

    let ok = unsafe {
        CryptUnprotectData(
            &mut input,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };

    if ok == 0 {
        return Err(AppError::Config(
            "DPAPI CryptUnprotectData failed".to_string(),
        ));
    }

    let plaintext =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        let _ = LocalFree(output.pbData.cast());
    }
    Ok(plaintext)
}

#[cfg(not(target_os = "windows"))]
fn protect_secret(_bytes: &[u8]) -> AppResult<Vec<u8>> {
    Err(AppError::Unsupported(
        "Miru Desktop Client v1 only supports Windows DPAPI secret storage".to_string(),
    ))
}

#[cfg(not(target_os = "windows"))]
fn unprotect_secret(_bytes: &[u8]) -> AppResult<Vec<u8>> {
    Err(AppError::Unsupported(
        "Miru Desktop Client v1 only supports Windows DPAPI secret storage".to_string(),
    ))
}
