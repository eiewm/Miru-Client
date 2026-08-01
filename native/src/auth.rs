use crate::config::{
    clear_auth_secrets, load_config, read_secret, save_config, write_secret, SecretKey,
};
use crate::error::{AppError, AppResult};
use crate::state::{
    emit_state, log_line, replace_worker_cancel, set_active_job_id, set_worker_status, ManagedState,
};
use crate::types::WorkerStatus;
use crate::work;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::Ipv4Addr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use uuid::Uuid;

const SESSION_EXPIRED_MESSAGE: &str = "session expired; log in again";

#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    data: Option<T>,
}

#[derive(Debug, Deserialize)]
struct ApiErrorResponse {
    error: Option<ApiErrorBody>,
}

#[derive(Debug, Deserialize)]
struct ApiErrorBody {
    code: String,
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CliTokenExchange {
    token: String,
    refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurrentUser {
    id: String,
    username: String,
    avatar_url: Option<String>,
    #[serde(default)]
    role: Value,
    #[serde(default)]
    plan: Value,
    linked_accounts: Option<CurrentUserLinkedAccounts>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurrentUserLinkedAccounts {
    #[serde(default)]
    discord: bool,
}

pub async fn login(app: AppHandle) -> AppResult<()> {
    let state = app.state::<ManagedState>();
    let mut config = load_config(&state.paths)?;
    let previous_user_id = config.user_id.clone();
    let previous_username = config.username.clone();
    let api_url = trim_trailing_slash(&config.api_url);
    let frontend_url = trim_trailing_slash(&config.frontend_url);
    let request_id = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let challenge = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let port = listener.local_addr()?.port();

    let request_res = state
        .http
        .post(format!("{api_url}/auth/cli-token/request"))
        .json(&json!({
            "requestId": request_id,
            "challenge": challenge,
            "port": port
        }))
        .send()
        .await?;

    if !request_res.status().is_success() {
        return Err(AppError::Auth(format!(
            "failed to create desktop auth request: {}",
            request_res.status()
        )));
    }

    let auth_url = format!("{frontend_url}/cli-auth?port={port}&request={request_id}");
    log_line(
        &app,
        format!("Opening browser auth flow on local callback port {port}"),
    );
    open::that(&auth_url)
        .map_err(|err| AppError::Process(format!("failed to open browser: {err}")))?;

    let code = tokio::time::timeout(Duration::from_secs(120), wait_for_code(listener))
        .await
        .map_err(|_| AppError::Auth("authentication timed out".to_string()))??;

    let exchange_res = state
        .http
        .post(format!("{api_url}/auth/cli-token/exchange"))
        .json(&json!({
            "code": code,
            "requestId": request_id,
            "challenge": challenge
        }))
        .send()
        .await?;

    if !exchange_res.status().is_success() {
        return Err(AppError::Auth(format!(
            "failed to exchange desktop login code: {}",
            exchange_res.status()
        )));
    }

    let body: ApiResponse<CliTokenExchange> = exchange_res.json().await?;
    let exchanged = body
        .data
        .ok_or_else(|| AppError::Auth("missing token exchange payload".to_string()))?;

    let user = fetch_current_user(&app, &api_url, &exchanged.token)
        .await
        .map_err(|err| AppError::Auth(format!("failed to load desktop account profile: {err}")))?;
    let switched_account = !previous_user_id.trim().is_empty() && previous_user_id != user.id;
    if switched_account {
        log_line(
            &app,
            format!(
                "Account changed from {} to {}; disconnecting previous renderer session",
                if previous_username.trim().is_empty() {
                    previous_user_id.as_str()
                } else {
                    previous_username.as_str()
                },
                user.username
            ),
        );
        let _ = work::disconnect_worker(app.clone()).await;
        write_secret(&state.paths, SecretKey::WorkerToken, "")?;
        config.is_server = false;
        config.registered_user_id.clear();
        config.server_client_id.clear();
        config.server_status.clear();
        config.server_auto_reconnect = false;
    }
    apply_current_user(&mut config, &api_url, &frontend_url, user);

    write_secret(&state.paths, SecretKey::ApiToken, &exchanged.token)?;
    if let Some(refresh_token) = exchanged.refresh_token {
        write_secret(&state.paths, SecretKey::RefreshToken, &refresh_token)?;
    }
    save_config(&state.paths, &config)?;
    let _ = work::refresh_server_status(&app, &exchanged.token).await;

    log_line(&app, "Authenticated successfully");
    emit_state(&app);
    Ok(())
}

pub async fn logout(app: AppHandle) -> AppResult<()> {
    let _ = work::disconnect_worker(app.clone()).await;
    clear_local_session_state(&app, "Logged out")
}

pub async fn ensure_fresh_session(app: &AppHandle) -> AppResult<Option<String>> {
    let state = app.state::<ManagedState>();
    let api_token = read_secret(&state.paths, SecretKey::ApiToken)?;
    if let Some(token) = api_token.as_ref() {
        if is_jwt_fresh(token, unix_now_seconds(), 60) {
            if !ensure_session_profile(app, token).await? {
                log_line(
                    app,
                    "Could not refresh desktop account profile; keeping cached local session",
                );
            }
            return Ok(Some(token.clone()));
        }
    }

    refresh_session_with_lock(app, false).await
}

pub async fn refresh_session_after_auth_failure(app: &AppHandle) -> AppResult<Option<String>> {
    refresh_session_with_lock(app, true).await
}

async fn refresh_session_with_lock(
    app: &AppHandle,
    force_refresh: bool,
) -> AppResult<Option<String>> {
    let state = app.state::<ManagedState>();
    let _guard = state.auth_refresh.lock().await;
    let api_token = read_secret(&state.paths, SecretKey::ApiToken)?;
    if !force_refresh {
        if let Some(token) = api_token.as_ref() {
            if is_jwt_fresh(token, unix_now_seconds(), 60) {
                return Ok(Some(token.clone()));
            }
        }
    }

    refresh_session(app, api_token).await
}

async fn refresh_session(
    app: &AppHandle,
    existing_api_token: Option<String>,
) -> AppResult<Option<String>> {
    let state = app.state::<ManagedState>();
    let Some(refresh_token) = read_secret(&state.paths, SecretKey::RefreshToken)? else {
        if existing_api_token.is_some() {
            log_line(
                app,
                "Desktop refresh token is missing; clearing cached local session",
            );
            clear_local_session_state(app, "Desktop session expired; log in again")?;
        }
        return Ok(None);
    };

    let config = load_config(&state.paths)?;
    let api_url = trim_trailing_slash(&config.api_url);
    let res = state
        .http
        .post(format!("{api_url}/auth/refresh"))
        .json(&json!({ "refreshToken": refresh_token }))
        .send()
        .await?;

    if !res.status().is_success() {
        if matches!(
            res.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ) {
            log_line(
                app,
                "Desktop refresh token was rejected; clearing cached local session",
            );
            clear_local_session_state(app, "Desktop session expired; log in again")?;
            return Ok(None);
        }
        return Err(AppError::Auth(format!(
            "failed to refresh desktop session: {}",
            res.status()
        )));
    }

    let body: ApiResponse<RefreshTokens> = res.json().await?;
    let Some(tokens) = body.data else {
        log_line(
            app,
            "Desktop session refresh returned no CLI tokens; clearing cached local session",
        );
        clear_local_session_state(app, "Desktop session expired; log in again")?;
        return Ok(None);
    };

    write_secret(&state.paths, SecretKey::ApiToken, &tokens.access_token)?;
    write_secret(&state.paths, SecretKey::RefreshToken, &tokens.refresh_token)?;
    if !ensure_session_profile(app, &tokens.access_token).await? {
        log_line(
            app,
            "Desktop session refreshed but profile sync failed; keeping cached account profile",
        );
    }
    emit_state(app);
    Ok(Some(tokens.access_token))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RefreshTokens {
    access_token: String,
    refresh_token: String,
}

pub async fn require_auth_response(
    app: &AppHandle,
    response: reqwest::Response,
) -> AppResult<reqwest::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let api_error = read_api_error_message(response).await;
    if is_session_auth_status(status) {
        log_line(
            app,
            "Desktop API rejected the current session; refreshing desktop session",
        );
        if refresh_session_after_auth_failure(app).await?.is_some() {
            return Err(AppError::Auth(
                "session refreshed; retry the action".to_string(),
            ));
        }
        if let Some(detail) = api_error {
            log_line(app, format!("Desktop session rejected by API: {detail}"));
        }
        return Err(AppError::Auth(SESSION_EXPIRED_MESSAGE.to_string()));
    }

    Err(AppError::Api(api_error.unwrap_or_else(|| {
        format!(
            "{} {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("HTTP error")
        )
    })))
}

fn clear_local_session_state(app: &AppHandle, message: &str) -> AppResult<()> {
    if let Some(cancel) = replace_worker_cancel(app, None) {
        let _ = cancel.send(());
    }

    let state = app.state::<ManagedState>();
    clear_auth_secrets(&state.paths)?;
    let mut config = load_config(&state.paths)?;
    config.username.clear();
    config.user_id.clear();
    config.user_avatar_url.clear();
    config.user_role.clear();
    config.user_plan.clear();
    config.discord_linked = false;
    config.is_server = false;
    config.registered_user_id.clear();
    config.server_client_id.clear();
    config.server_status.clear();
    config.server_auto_reconnect = false;
    save_config(&state.paths, &config)?;
    set_active_job_id(app, None);
    set_worker_status(app, WorkerStatus::Disconnected);
    log_line(app, message);
    emit_state(app);
    Ok(())
}

fn is_session_auth_status(status: StatusCode) -> bool {
    status == StatusCode::UNAUTHORIZED
}

async fn read_api_error_message(response: reqwest::Response) -> Option<String> {
    let status = response.status();
    let text = response.text().await.ok()?;
    parse_api_error_message(status, &text)
}

fn parse_api_error_message(status: StatusCode, text: &str) -> Option<String> {
    let parsed: ApiErrorResponse = serde_json::from_str(text).ok()?;
    let error = parsed.error?;
    let detail = if error.message.trim().is_empty() {
        error.code
    } else {
        format!("{}: {}", error.code, error.message)
    };
    Some(format!(
        "{} {}: {}",
        status.as_u16(),
        status.canonical_reason().unwrap_or("HTTP error"),
        detail
    ))
}

async fn ensure_session_profile(app: &AppHandle, token: &str) -> AppResult<bool> {
    let state = app.state::<ManagedState>();
    let config = load_config(&state.paths)?;
    if !config.user_id.trim().is_empty()
        && !config.username.trim().is_empty()
        && !config.user_role.trim().is_empty()
    {
        return Ok(true);
    }

    let api_url = trim_trailing_slash(&config.api_url);
    let frontend_url = trim_trailing_slash(&config.frontend_url);
    match fetch_current_user(app, &api_url, token).await {
        Ok(user) => {
            let mut next_config = config;
            apply_current_user(&mut next_config, &api_url, &frontend_url, user);
            save_config(&state.paths, &next_config)?;
            emit_state(app);
            Ok(true)
        }
        Err(error) if is_session_expired_error(&error) => {
            log_line(
                app,
                "Desktop profile request was rejected; keeping cached local account",
            );
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

pub async fn sync_current_user_profile(app: &AppHandle) -> AppResult<()> {
    let Some(token) = ensure_fresh_session(app).await? else {
        return Ok(());
    };
    let state = app.state::<ManagedState>();
    let config = load_config(&state.paths)?;
    let api_url = trim_trailing_slash(&config.api_url);
    let frontend_url = trim_trailing_slash(&config.frontend_url);
    let user = match fetch_current_user(app, &api_url, &token).await {
        Ok(user) => user,
        Err(error) if is_session_expired_error(&error) => {
            log_line(
                app,
                "Desktop profile sync was rejected; keeping cached local account",
            );
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let mut next_config = config;
    let previous_role = next_config.user_role.clone();
    let previous_plan = next_config.user_plan.clone();
    let previous_discord_linked = next_config.discord_linked;
    apply_current_user(&mut next_config, &api_url, &frontend_url, user);
    if previous_role != next_config.user_role
        || previous_plan != next_config.user_plan
        || previous_discord_linked != next_config.discord_linked
    {
        log_line(
            app,
            format!(
                "Account profile refreshed: role={}, plan={}, discordLinked={}",
                profile_label(&next_config.user_role),
                profile_label(&next_config.user_plan),
                next_config.discord_linked
            ),
        );
    }
    save_config(&state.paths, &next_config)?;
    emit_state(app);
    Ok(())
}

fn apply_current_user(
    config: &mut crate::types::AppConfig,
    api_url: &str,
    frontend_url: &str,
    user: CurrentUser,
) {
    config.user_id = user.id;
    config.username = normalize_current_username(&config.username, &user.username);
    config.user_avatar_url =
        normalize_avatar_url(api_url, frontend_url, user.avatar_url.as_deref());
    config.user_role = normalize_current_user_label(&user.role);
    config.user_plan = normalize_current_user_label(&user.plan);
    config.discord_linked = user
        .linked_accounts
        .as_ref()
        .is_some_and(|accounts| accounts.discord);
}

fn normalize_current_username(existing: &str, incoming: &str) -> String {
    let incoming = incoming.trim();
    let existing = existing.trim();
    if incoming.is_empty() {
        if is_generic_desktop_username(existing) {
            return String::new();
        }
        return existing.to_string();
    }
    if is_generic_desktop_username(incoming) {
        return if !existing.is_empty() && !is_generic_desktop_username(existing) {
            existing.to_string()
        } else {
            String::new()
        };
    }
    incoming.to_string()
}

fn is_generic_desktop_username(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case("miru user")
}

fn profile_label(value: &str) -> &str {
    if value.trim().is_empty() {
        "UNKNOWN"
    } else {
        value.trim()
    }
}

fn normalize_current_user_label(value: &Value) -> String {
    match value {
        Value::String(raw) => raw.trim().to_ascii_uppercase(),
        Value::Object(map) => {
            for key in ["plan", "role", "name", "tier", "slug", "id"] {
                let Some(next) = map.get(key) else {
                    continue;
                };
                let normalized = normalize_current_user_label(next);
                if !normalized.is_empty() {
                    return normalized;
                }
            }
            String::new()
        }
        _ => String::new(),
    }
}

fn is_session_expired_error(error: &AppError) -> bool {
    matches!(error, AppError::Auth(message) if message == SESSION_EXPIRED_MESSAGE)
}

fn unix_now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn is_jwt_fresh(token: &str, now: u64, leeway_seconds: u64) -> bool {
    let Some(payload) = jwt_payload(token) else {
        return false;
    };
    match payload.get("exp").and_then(serde_json::Value::as_u64) {
        Some(exp) => exp > now.saturating_add(leeway_seconds),
        None => true,
    }
}

fn jwt_payload(token: &str) -> Option<serde_json::Value> {
    use base64::{engine::general_purpose, Engine as _};

    let payload = token.split('.').nth(1)?;
    let decoded = general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| general_purpose::URL_SAFE.decode(payload))
        .ok()?;
    serde_json::from_slice(&decoded).ok()
}

async fn wait_for_code(listener: TcpListener) -> AppResult<String> {
    let (mut socket, _) = listener.accept().await?;
    let mut buffer = vec![0_u8; 4096];
    let read = socket.read(&mut buffer).await?;
    let request = String::from_utf8_lossy(&buffer[..read]);
    let first_line = request.lines().next().unwrap_or_default();
    let path = first_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| AppError::Auth("invalid auth callback".to_string()))?;
    let query = path
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or_default();
    let code = url::form_urlencoded::parse(query.as_bytes())
        .find_map(|(key, value)| (key == "code").then(|| value.to_string()))
        .ok_or_else(|| AppError::Auth("missing login code".to_string()))?;

    let html = "<html><body style=\"font-family:sans-serif;background:#121418;color:#f4f5f7\"><h1>Miru login complete</h1><p>You can close this window.</p></body></html>";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        html.len(),
        html
    );
    socket.write_all(response.as_bytes()).await?;
    Ok(code)
}

pub fn trim_trailing_slash(value: &str) -> String {
    value.trim_end_matches('/').to_string()
}

fn normalize_avatar_url(api_url: &str, frontend_url: &str, raw: Option<&str>) -> String {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return String::new();
    };
    if raw.starts_with("https://") {
        return raw.to_string();
    }
    if raw.starts_with("http://") {
        return String::new();
    }
    if raw.starts_with('/') {
        return format!("{}{}", trim_trailing_slash(frontend_url), raw);
    }
    if raw.starts_with("api/") {
        let base = url::Url::parse(api_url)
            .ok()
            .and_then(|parsed| {
                let host = parsed.host_str()?;
                Some(format!("{}://{}", parsed.scheme(), host))
            })
            .unwrap_or_else(|| trim_trailing_slash(frontend_url));
        return format!("{}/{}", base.trim_end_matches('/'), raw);
    }
    String::new()
}

async fn fetch_current_user(app: &AppHandle, api_url: &str, token: &str) -> AppResult<CurrentUser> {
    let state = app.state::<ManagedState>();
    let response = state
        .http
        .get(format!("{api_url}/users/me"))
        .bearer_auth(token)
        .send()
        .await?;
    if is_session_auth_status(response.status()) {
        return Err(AppError::Auth(SESSION_EXPIRED_MESSAGE.to_string()));
    }
    let body: ApiResponse<CurrentUser> = response.error_for_status()?.json().await?;
    body.data
        .ok_or_else(|| AppError::Auth("missing current user payload".to_string()))
}
