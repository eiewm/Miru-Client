use crate::config::AppPaths;
use crate::error::{AppError, AppResult};
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, RANGE};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs as std_fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use uuid::Uuid;

const MANAGED_FFMPEG_DIR_NAME: &str = "ffmpeg";
const FFMPEG_ZIP_URL: &str = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip";
const FFMPEG_ZIP_SHA256_URL: &str =
    "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip.sha256";
const FFMPEG_ZIP_MAX_BYTES: u64 = 256 * 1024 * 1024;
const FFMPEG_TOOL_MAX_BYTES: u64 = 128 * 1024 * 1024;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedToolSnapshot {
    pub path: String,
    pub directory: String,
    pub exists: bool,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FfmpegToolsSnapshot {
    pub directory: String,
    pub ffmpeg: ManagedToolSnapshot,
    pub ffprobe: ManagedToolSnapshot,
}

#[derive(Debug)]
pub struct FfmpegToolInfo {
    pub available: bool,
    pub size_bytes: Option<u64>,
}

#[derive(Debug)]
pub struct FfmpegInstallResult {
    pub directory: PathBuf,
    pub size_bytes: u64,
}

pub fn ffmpeg_tools_snapshot(paths: &AppPaths) -> FfmpegToolsSnapshot {
    let directory = managed_ffmpeg_dir(paths);
    FfmpegToolsSnapshot {
        directory: directory.display().to_string(),
        ffmpeg: managed_tool_snapshot(paths, "ffmpeg"),
        ffprobe: managed_tool_snapshot(paths, "ffprobe"),
    }
}

pub async fn ffmpeg_tool_info(paths: &AppPaths, program: &str) -> FfmpegToolInfo {
    let resolved = resolve_ffmpeg_tool_path(paths, program);
    let mut command = Command::new(&resolved.path);
    command
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide_child_console(&mut command);
    let available = command
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false);

    FfmpegToolInfo {
        available,
        size_bytes: resolved
            .path
            .metadata()
            .ok()
            .map(|metadata| metadata.len())
            .filter(|size| *size > 0),
    }
}

pub async fn ffmpeg_download_size(client: &reqwest::Client) -> AppResult<Option<u64>> {
    let response = client.head(FFMPEG_ZIP_URL).send().await?;
    if response.status().is_success() {
        return Ok(content_length_or_range_total(&response));
    }

    let response = client
        .get(FFMPEG_ZIP_URL)
        .header(RANGE, "bytes=0-0")
        .send()
        .await?;
    if !response.status().is_success() {
        return Ok(None);
    }
    Ok(content_length_or_range_total(&response))
}

pub async fn install_ffmpeg_tools(
    paths: &AppPaths,
    client: &reqwest::Client,
) -> AppResult<FfmpegInstallResult> {
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (paths, client);
        return Err(AppError::Unsupported(
            "automatic FFmpeg installation is only available on Windows".to_string(),
        ));
    }

    #[cfg(target_os = "windows")]
    {
        let expected_sha256 = fetch_ffmpeg_archive_sha256(client).await?;
        let temp_zip = paths
            .data_dir
            .join(format!("ffmpeg-release-essentials-{}.zip", Uuid::new_v4()));
        let temp_dir = paths
            .data_dir
            .join(format!("ffmpeg-install-{}", Uuid::new_v4()));
        let target_dir = managed_ffmpeg_dir(paths);

        let downloaded = download_ffmpeg_archive(client, &temp_zip, &expected_sha256).await?;
        let extract_result = extract_ffmpeg_tools(&temp_zip, &temp_dir, &target_dir);
        let _ = fs::remove_file(&temp_zip).await;
        let _ = fs::remove_dir_all(&temp_dir).await;
        extract_result?;

        Ok(FfmpegInstallResult {
            directory: target_dir,
            size_bytes: downloaded,
        })
    }
}

pub fn configure_ffmpeg_path(command: &mut Command, paths: &AppPaths) {
    let directory = managed_ffmpeg_dir(paths);
    if !directory.is_dir() {
        return;
    }

    let mut entries = vec![directory];
    if let Some(current_path) = std::env::var_os("PATH") {
        entries.extend(std::env::split_paths(&current_path));
    }
    if let Ok(path) = std::env::join_paths(entries) {
        command.env("PATH", path);
    }
}

fn managed_tool_snapshot(paths: &AppPaths, program: &str) -> ManagedToolSnapshot {
    let resolved = resolve_ffmpeg_tool_path(paths, program);
    let directory = resolved
        .path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| managed_ffmpeg_dir(paths));
    ManagedToolSnapshot {
        path: resolved.path.display().to_string(),
        directory: directory.display().to_string(),
        exists: resolved.path.is_file(),
        source: resolved.source.to_string(),
    }
}

fn managed_ffmpeg_dir(paths: &AppPaths) -> PathBuf {
    paths.bin_dir.join(MANAGED_FFMPEG_DIR_NAME)
}

fn managed_ffmpeg_tool_path(paths: &AppPaths, program: &str) -> PathBuf {
    managed_ffmpeg_dir(paths).join(executable_file_name(program))
}

struct ResolvedToolPath {
    path: PathBuf,
    source: &'static str,
}

fn resolve_ffmpeg_tool_path(paths: &AppPaths, program: &str) -> ResolvedToolPath {
    let managed_path = managed_ffmpeg_tool_path(paths, program);
    if managed_path.is_file() {
        return ResolvedToolPath {
            path: managed_path,
            source: "managed",
        };
    }

    if let Some(path) = find_executable_on_path(program) {
        return ResolvedToolPath {
            path,
            source: "path",
        };
    }

    ResolvedToolPath {
        path: managed_path,
        source: "managed",
    }
}

async fn fetch_ffmpeg_archive_sha256(client: &reqwest::Client) -> AppResult<String> {
    let text = client
        .get(FFMPEG_ZIP_SHA256_URL)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    text.split(|character: char| character.is_whitespace() || character == '*' || character == '=')
        .find_map(|part| {
            let trimmed = part.trim().to_ascii_lowercase();
            (trimmed.len() == 64 && trimmed.chars().all(|ch| ch.is_ascii_hexdigit()))
                .then_some(trimmed)
        })
        .ok_or_else(|| {
            AppError::Binary("FFmpeg SHA-256 manifest did not contain a valid hash".to_string())
        })
}

async fn download_ffmpeg_archive(
    client: &reqwest::Client,
    destination: &Path,
    expected_sha256: &str,
) -> AppResult<u64> {
    let response = client
        .get(FFMPEG_ZIP_URL)
        .send()
        .await?
        .error_for_status()?;
    if let Some(size) = response.content_length() {
        if size > FFMPEG_ZIP_MAX_BYTES {
            return Err(AppError::Binary(format!(
                "FFmpeg archive is larger than the {} byte safety limit",
                FFMPEG_ZIP_MAX_BYTES
            )));
        }
    }

    let mut file = fs::File::create(destination).await?;
    let mut stream = response.bytes_stream();
    let mut hasher = Sha256::new();
    let mut downloaded = 0_u64;

    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        downloaded += chunk.len() as u64;
        if downloaded > FFMPEG_ZIP_MAX_BYTES {
            let _ = fs::remove_file(destination).await;
            return Err(AppError::Binary(format!(
                "FFmpeg archive exceeded the {} byte safety limit",
                FFMPEG_ZIP_MAX_BYTES
            )));
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.flush().await?;

    if downloaded == 0 {
        let _ = fs::remove_file(destination).await;
        return Err(AppError::Binary(
            "FFmpeg archive download was empty".to_string(),
        ));
    }

    let actual_sha256 = hex::encode(hasher.finalize());
    if !actual_sha256.eq_ignore_ascii_case(expected_sha256) {
        let _ = fs::remove_file(destination).await;
        return Err(AppError::Binary(
            "FFmpeg archive checksum mismatch".to_string(),
        ));
    }

    Ok(downloaded)
}

fn extract_ffmpeg_tools(zip_path: &Path, temp_dir: &Path, target_dir: &Path) -> AppResult<()> {
    std_fs::create_dir_all(temp_dir)?;
    let archive_file = std_fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(archive_file)?;
    let staged_ffmpeg = temp_dir.join(executable_file_name("ffmpeg"));
    let staged_ffprobe = temp_dir.join(executable_file_name("ffprobe"));

    extract_archive_tool(&mut archive, "ffmpeg.exe", &staged_ffmpeg)?;
    extract_archive_tool(&mut archive, "ffprobe.exe", &staged_ffprobe)?;

    std_fs::create_dir_all(target_dir)?;
    std_fs::copy(
        &staged_ffmpeg,
        target_dir.join(executable_file_name("ffmpeg")),
    )?;
    std_fs::copy(
        &staged_ffprobe,
        target_dir.join(executable_file_name("ffprobe")),
    )?;
    Ok(())
}

fn extract_archive_tool(
    archive: &mut zip::ZipArchive<std_fs::File>,
    executable_name: &str,
    destination: &Path,
) -> AppResult<()> {
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let normalized_name = entry.name().replace('\\', "/").to_ascii_lowercase();
        if !normalized_name.ends_with(&format!("/bin/{executable_name}"))
            && normalized_name != executable_name
        {
            continue;
        }
        if entry.size() == 0 || entry.size() > FFMPEG_TOOL_MAX_BYTES {
            return Err(AppError::Binary(format!(
                "{executable_name} in FFmpeg archive has an invalid size"
            )));
        }

        let expected_size = entry.size();
        let mut output = std_fs::File::create(destination)?;
        let mut reader = entry.take(FFMPEG_TOOL_MAX_BYTES + 1);
        let copied = std::io::copy(&mut reader, &mut output)?;
        output.flush()?;
        if copied != expected_size {
            return Err(AppError::Binary(format!(
                "{executable_name} in FFmpeg archive could not be extracted completely"
            )));
        }
        return Ok(());
    }

    Err(AppError::Binary(format!(
        "{executable_name} was not found in the FFmpeg archive"
    )))
}

fn content_length_or_range_total(response: &reqwest::Response) -> Option<u64> {
    content_range_total(response.headers().get(CONTENT_RANGE)).or_else(|| {
        response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .filter(|size| *size > 0)
    })
}

fn content_range_total(value: Option<&reqwest::header::HeaderValue>) -> Option<u64> {
    let value = value?.to_str().ok()?.trim();
    let (_, total) = value.rsplit_once('/')?;
    total.trim().parse::<u64>().ok().filter(|size| *size > 0)
}

fn find_executable_on_path(program: &str) -> Option<PathBuf> {
    let raw_path = Path::new(program);
    if raw_path.is_file() {
        return Some(raw_path.to_path_buf());
    }

    let path_value = std::env::var_os("PATH")?;
    let candidates = executable_candidate_names(program);
    for directory in std::env::split_paths(&path_value) {
        for candidate in &candidates {
            let path = directory.join(candidate);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn executable_candidate_names(program: &str) -> Vec<OsString> {
    if Path::new(program).extension().is_some() {
        return vec![OsString::from(program)];
    }

    let mut names = Vec::new();
    if let Some(raw_extensions) = std::env::var_os("PATHEXT") {
        for extension in raw_extensions.to_string_lossy().split(';') {
            let extension = extension.trim();
            if !extension.is_empty() {
                names.push(OsString::from(format!("{program}{extension}")));
            }
        }
    }
    names.push(OsString::from(format!("{program}.exe")));
    names
}

#[cfg(not(target_os = "windows"))]
fn executable_candidate_names(program: &str) -> Vec<OsString> {
    vec![OsString::from(program)]
}

#[cfg(target_os = "windows")]
fn executable_file_name(program: &str) -> String {
    if program.to_ascii_lowercase().ends_with(".exe") {
        program.to_string()
    } else {
        format!("{program}.exe")
    }
}

#[cfg(not(target_os = "windows"))]
fn executable_file_name(program: &str) -> String {
    program.to_string()
}

#[cfg(target_os = "windows")]
fn hide_child_console(command: &mut Command) {
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn hide_child_console(_command: &mut Command) {}
