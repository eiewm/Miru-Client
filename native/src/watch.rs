use crate::auth::{
    ensure_fresh_session, require_auth_response, sync_current_user_profile, trim_trailing_slash,
};
use crate::cache::{load_beatmap_cache, save_beatmap_cache, BeatmapCache};
use crate::config::load_config;
use crate::error::{AppError, AppResult};
use crate::osu::{
    build_score_data, calculate_replay_pp, load_osu_db_beatmap_index, parse_replay_header_file,
    refresh_osu_db_beatmap_index_if_changed, replay_display_name, resolve_beatmap_by_hash,
    resolve_beatmap_from_replay_name_hint, resolve_osu_stable_paths, warm_beatmap_cache_from_songs,
    OsuDbBeatmapIndex, ReplayHeader, ResolvedBeatmap, ResolvedOsuStablePaths,
};
use crate::rules::{
    matches_auto_renderer_filters_with_metrics, AutoRendererFilterDecision,
    AutoRendererReplayMetrics,
};
use crate::state::{
    add_history, emit_state, history_entry, log_line, set_last_auto_renderer_event, ManagedState,
};
use crate::types::{AutoRendererEvent, ScoreData, WatcherStatus};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use reqwest::multipart::{Form, Part};
use serde_json::json;
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tauri::{AppHandle, Manager};
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};
use uuid::Uuid;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

const WATCHER_POLL_INTERVAL: Duration = Duration::from_secs(2);
const STABLE_SAMPLE_DELAY: Duration = Duration::from_millis(450);
const STABLE_SAMPLE_ATTEMPTS: usize = 12;
const MAX_REPLAY_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_LOCAL_MAPSET_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_LOCAL_MAPSET_FILES: usize = 4096;
const BEATMAP_CACHE_WARMING_MESSAGE: &str = "beatmap cache warming";

pub async fn start_watcher(app: AppHandle) -> AppResult<()> {
    {
        let state = app.state::<ManagedState>();
        let mut runtime = state.runtime.lock().expect("runtime state poisoned");
        if runtime.watcher_status == WatcherStatus::Running
            || runtime.watcher_status == WatcherStatus::Starting
        {
            return Ok(());
        }
        runtime.watcher_status = WatcherStatus::Starting;
    }
    emit_state(&app);

    let Some(_) = ensure_fresh_session(&app).await? else {
        set_watcher_status(&app, WatcherStatus::Error);
        return Err(AppError::Auth(
            "Auto Renderer requires a Miru Plus account.".to_string(),
        ));
    };
    sync_current_user_profile(&app).await?;
    let config = load_config(&app.state::<ManagedState>().paths)?;
    if !can_use_auto_renderer(&config) {
        set_watcher_status(&app, WatcherStatus::Error);
        return Err(AppError::Auth(
            "Auto Renderer requires a Miru Plus account.".to_string(),
        ));
    }
    let stable_paths = match resolve_osu_stable_paths(&config) {
        Ok(paths) => paths,
        Err(error) => {
            set_watcher_status(&app, WatcherStatus::Error);
            return Err(error);
        }
    };

    let replay_dirs = stable_paths.replay_dirs();
    let baseline: HashSet<String> = list_replay_files(&replay_dirs)?
        .into_iter()
        .map(|path| normalize_replay_key(&path))
        .collect();
    let cache = Arc::new(AsyncMutex::new(load_beatmap_cache(
        &app.state::<ManagedState>().paths,
    )?));
    let songs_cache_warming = Arc::new(AtomicBool::new(true));
    start_beatmap_cache_warmup(
        &app,
        &stable_paths,
        Arc::clone(&cache),
        Arc::clone(&songs_cache_warming),
    );
    let osu_db_index = match load_osu_db_beatmap_index(&stable_paths) {
        Ok(Some(index)) => {
            log_line(
                &app,
                format!("Loaded osu!.db beatmap index with {} entries", index.len()),
            );
            Some(index)
        }
        Ok(None) => {
            log_line(
                &app,
                "osu!.db beatmap index unavailable; Data/r fallback may scan Songs",
            );
            None
        }
        Err(error) => {
            log_line(
                &app,
                format!("Failed to load osu!.db beatmap index: {error}"),
            );
            None
        }
    };
    let (notify_tx, notify_rx) = mpsc::unbounded_channel();
    let watcher = create_replay_watcher(&replay_dirs, notify_tx)?;
    let (tx, rx) = oneshot::channel();
    {
        let state = app.state::<ManagedState>();
        let mut runtime = state.runtime.lock().expect("runtime state poisoned");
        runtime.watcher_cancel = Some(tx);
        runtime.watcher_status = WatcherStatus::Running;
    }

    log_line(
        &app,
        format!(
            "Auto renderer watcher started for osu!stable at {} (ignoring {} existing replays)",
            stable_paths.root.display(),
            baseline.len()
        ),
    );
    emit_state(&app);
    tokio::spawn(run_watcher_loop(
        app,
        stable_paths,
        cache,
        songs_cache_warming,
        osu_db_index,
        baseline,
        notify_rx,
        watcher,
        rx,
    ));
    Ok(())
}

fn can_use_auto_renderer(config: &crate::types::AppConfig) -> bool {
    let role = config.user_role.trim().to_ascii_uppercase();
    let plan = config.user_plan.trim().to_ascii_uppercase();
    matches!(role.as_str(), "PLUS" | "ADMIN") || plan == "PLUS"
}

pub fn stop_watcher(app: &AppHandle) -> AppResult<()> {
    {
        let state = app.state::<ManagedState>();
        let mut runtime = state.runtime.lock().expect("runtime state poisoned");
        if let Some(cancel) = runtime.watcher_cancel.take() {
            let _ = cancel.send(());
        }
        runtime.watcher_status = WatcherStatus::Stopped;
    }
    log_line(app, "Auto renderer watcher stopped");
    emit_state(app);
    Ok(())
}

fn start_beatmap_cache_warmup(
    app: &AppHandle,
    stable_paths: &ResolvedOsuStablePaths,
    cache: Arc<AsyncMutex<BeatmapCache>>,
    songs_cache_warming: Arc<AtomicBool>,
) {
    spawn_beatmap_cache_refresh(
        app.clone(),
        stable_paths.clone(),
        cache,
        songs_cache_warming,
        "Warming Auto Renderer Songs beatmap cache in the background",
        "Songs beatmap cache ready",
    );
}

fn start_beatmap_cache_refresh_if_idle(
    app: &AppHandle,
    stable_paths: &ResolvedOsuStablePaths,
    cache: Arc<AsyncMutex<BeatmapCache>>,
    songs_cache_warming: Arc<AtomicBool>,
) {
    if songs_cache_warming
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    spawn_beatmap_cache_refresh(
        app.clone(),
        stable_paths.clone(),
        cache,
        songs_cache_warming,
        "Refreshing Auto Renderer Songs beatmap cache in the background",
        "Songs beatmap cache refreshed",
    );
}

fn spawn_beatmap_cache_refresh(
    app: AppHandle,
    stable_paths: ResolvedOsuStablePaths,
    cache: Arc<AsyncMutex<BeatmapCache>>,
    songs_cache_warming: Arc<AtomicBool>,
    start_message: &'static str,
    done_message: &'static str,
) {
    tokio::spawn(async move {
        log_line(&app, start_message);
        match refresh_songs_beatmap_cache(&app, &stable_paths, &cache).await {
            Ok(added) => log_line(&app, format!("{done_message}; added {added} new entries")),
            Err(error) => log_line(
                &app,
                format!("Failed to update Songs beatmap cache: {error}"),
            ),
        }
        songs_cache_warming.store(false, Ordering::Relaxed);
    });
}

async fn run_watcher_loop(
    app: AppHandle,
    stable_paths: ResolvedOsuStablePaths,
    cache: Arc<AsyncMutex<BeatmapCache>>,
    songs_cache_warming: Arc<AtomicBool>,
    mut osu_db_index: Option<OsuDbBeatmapIndex>,
    mut seen: HashSet<String>,
    mut notify_rx: mpsc::UnboundedReceiver<PathBuf>,
    _watcher: RecommendedWatcher,
    mut cancel: oneshot::Receiver<()>,
) {
    let mut interval = tokio::time::interval(WATCHER_POLL_INTERVAL);
    let mut cache_refresh_retries: HashSet<String> = HashSet::new();
    loop {
        tokio::select! {
            _ = &mut cancel => break,
            Some(replay_path) = notify_rx.recv() => {
                handle_replay_candidate(&app, &stable_paths, &cache, &songs_cache_warming, &mut osu_db_index, &mut seen, &mut cache_refresh_retries, replay_path).await;
            }
            _ = interval.tick() => {
                let paths = match list_replay_files(&stable_paths.replay_dirs()) {
                    Ok(paths) => paths,
                    Err(error) => {
                        log_line(&app, format!("Failed to scan replay directories: {error}"));
                        set_watcher_status(&app, WatcherStatus::Error);
                        return;
                    }
                };

                for replay_path in paths {
                    handle_replay_candidate(&app, &stable_paths, &cache, &songs_cache_warming, &mut osu_db_index, &mut seen, &mut cache_refresh_retries, replay_path).await;
                }
            }
        }
    }
}

async fn resolve_replay_beatmap(
    app: &AppHandle,
    stable_paths: &ResolvedOsuStablePaths,
    cache: &Arc<AsyncMutex<BeatmapCache>>,
    songs_cache_warming: &Arc<AtomicBool>,
    osu_db_index: Option<&OsuDbBeatmapIndex>,
    beatmap_hash: &str,
    replay_name_hint: Option<&str>,
    allow_cache_refresh: bool,
) -> AppResult<Option<ResolvedBeatmap>> {
    if let Some(beatmap) = resolve_replay_beatmap_from_fast_indexes(
        app,
        stable_paths,
        cache,
        osu_db_index,
        beatmap_hash,
    )
    .await?
    {
        return Ok(Some(beatmap));
    }

    if let Some(replay_name) = replay_name_hint {
        let mut cache_guard = cache.lock().await;
        if let Some(beatmap) = resolve_beatmap_from_replay_name_hint(
            stable_paths,
            &mut cache_guard,
            beatmap_hash,
            replay_name,
        )? {
            save_beatmap_cache(&app.state::<ManagedState>().paths, &cache_guard)?;
            return Ok(Some(beatmap));
        }
    }

    if songs_cache_warming.load(Ordering::Relaxed) {
        return Err(AppError::Process(BEATMAP_CACHE_WARMING_MESSAGE.to_string()));
    }

    if allow_cache_refresh {
        start_beatmap_cache_refresh_if_idle(
            app,
            stable_paths,
            Arc::clone(cache),
            Arc::clone(songs_cache_warming),
        );
        return Err(AppError::Process(BEATMAP_CACHE_WARMING_MESSAGE.to_string()));
    }

    resolve_replay_beatmap_from_fast_indexes(app, stable_paths, cache, osu_db_index, beatmap_hash)
        .await
}

async fn resolve_replay_beatmap_from_fast_indexes(
    app: &AppHandle,
    stable_paths: &ResolvedOsuStablePaths,
    cache: &Arc<AsyncMutex<BeatmapCache>>,
    osu_db_index: Option<&OsuDbBeatmapIndex>,
    beatmap_hash: &str,
) -> AppResult<Option<ResolvedBeatmap>> {
    let mut cache_guard = cache.lock().await;
    let beatmap = resolve_beatmap_by_hash(
        stable_paths,
        &mut cache_guard,
        osu_db_index,
        beatmap_hash,
        false,
    )?;
    if beatmap.is_some() {
        save_beatmap_cache(&app.state::<ManagedState>().paths, &cache_guard)?;
    }
    Ok(beatmap)
}

async fn refresh_songs_beatmap_cache(
    app: &AppHandle,
    stable_paths: &ResolvedOsuStablePaths,
    cache: &Arc<AsyncMutex<BeatmapCache>>,
) -> AppResult<usize> {
    let app_paths = app.state::<ManagedState>().paths.clone();
    let songs_dir = stable_paths.songs_dir.clone();
    let mut warmed_cache = { cache.lock().await.clone() };

    let result = tokio::task::spawn_blocking(move || {
        warm_beatmap_cache_from_songs(&songs_dir, &mut warmed_cache)
            .map(|added| (warmed_cache, added))
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| AppError::Process(format!("Songs cache task failed: {error}")))?;

    let (warmed_cache, added) = result.map_err(AppError::Process)?;
    let mut cache_guard = cache.lock().await;
    for (hash, entry) in warmed_cache {
        cache_guard.entry(hash).or_insert(entry);
    }
    save_beatmap_cache(&app_paths, &cache_guard)?;
    Ok(added)
}

async fn handle_replay_candidate(
    app: &AppHandle,
    stable_paths: &ResolvedOsuStablePaths,
    cache: &Arc<AsyncMutex<BeatmapCache>>,
    songs_cache_warming: &Arc<AtomicBool>,
    osu_db_index: &mut Option<OsuDbBeatmapIndex>,
    seen: &mut HashSet<String>,
    cache_refresh_retries: &mut HashSet<String>,
    replay_path: PathBuf,
) {
    if !is_replay_file(&replay_path) {
        return;
    }

    let replay_key = normalize_replay_key(&replay_path);
    if seen.contains(&replay_key) {
        return;
    }

    match process_replay(
        app,
        stable_paths,
        cache,
        songs_cache_warming,
        osu_db_index,
        &replay_path,
        !cache_refresh_retries.contains(&replay_key),
    )
    .await
    {
        Ok(()) => {
            cache_refresh_retries.remove(&replay_key);
            seen.insert(replay_key);
        }
        Err(error) => {
            if is_beatmap_cache_warming_error(&error) {
                cache_refresh_retries.insert(replay_key);
                return;
            }
            if should_retry_replay(&error) {
                log_line(
                    app,
                    format!(
                        "Replay {} is still being written; will retry on the next watcher pass",
                        replay_path.display()
                    ),
                );
                return;
            }

            cache_refresh_retries.remove(&replay_key);
            seen.insert(replay_key);
            let replay_name = replay_path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("Replay");
            add_auto_event(
                app,
                replay_name,
                replay_name,
                &error.to_string(),
                "failed",
                "auto-render",
            );
            log_line(
                app,
                format!(
                    "Replay processing failed for {}: {}",
                    replay_path.display(),
                    error
                ),
            );
        }
    }
}

async fn process_replay(
    app: &AppHandle,
    stable_paths: &ResolvedOsuStablePaths,
    cache: &Arc<AsyncMutex<BeatmapCache>>,
    songs_cache_warming: &Arc<AtomicBool>,
    osu_db_index: &mut Option<OsuDbBeatmapIndex>,
    replay_path: &Path,
    allow_cache_refresh: bool,
) -> AppResult<()> {
    wait_for_stable_replay(replay_path).await?;
    let replay_name = replay_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("Replay")
        .to_string();
    let config = load_config(&app.state::<ManagedState>().paths)?;

    let header = match parse_replay_header_file(replay_path) {
        Ok(header) => header,
        Err(error) if error.to_string().contains("osu!mania replays") => {
            add_auto_event(
                app,
                &replay_name,
                &replay_name,
                "Ignored because the replay is not osu!mania.",
                "ignored",
                "auto-replay",
            );
            return Ok(());
        }
        Err(error) => return Err(error),
    };

    match refresh_osu_db_beatmap_index_if_changed(stable_paths, osu_db_index) {
        Ok(true) => {
            let count = osu_db_index
                .as_ref()
                .map(OsuDbBeatmapIndex::len)
                .unwrap_or(0);
            log_line(
                app,
                format!("Refreshed osu!.db beatmap index with {count} entries"),
            );
        }
        Ok(false) => {}
        Err(error) => log_line(
            app,
            format!("Failed to refresh osu!.db beatmap index: {error}"),
        ),
    }

    let is_f2_replay = is_stable_replay_saved_with_f2(stable_paths, replay_path);
    let beatmap = match resolve_replay_beatmap(
        app,
        stable_paths,
        cache,
        songs_cache_warming,
        osu_db_index.as_ref(),
        &header.beatmap_hash,
        is_f2_replay.then_some(replay_name.as_str()),
        allow_cache_refresh,
    )
    .await?
    {
        Some(beatmap) => beatmap,
        None => {
            let message = "Beatmap not found in osu!.db or the local Songs cache; replay skipped.";
            add_auto_event(
                app,
                &replay_name,
                &replay_name,
                message,
                "skipped",
                "auto-replay",
            );
            return Ok(());
        }
    };

    let display = replay_display_name(replay_path, Some(&beatmap));
    let detail = format!(
        "{}K | {} LN | {} notes | Player {}",
        beatmap.key_count,
        beatmap.long_note_count,
        beatmap
            .long_note_count
            .saturating_add(beatmap.normal_note_count),
        header.username
    );

    let pp = if config.auto_renderer.pp_rule.enabled {
        match calculate_replay_pp(&header, &beatmap) {
            Ok(value) => Some(value),
            Err(error) => {
                log_line(app, format!("PP calculation failed for {display}: {error}"));
                None
            }
        }
    } else {
        None
    };
    let metrics = AutoRendererReplayMetrics {
        max_combo: header.max_combo as f64,
        accuracy: crate::osu::build_score_data(&header, &beatmap).accuracy,
        pp,
        hits: header.hits.clone(),
    };

    match matches_auto_renderer_filters_with_metrics(&config.auto_renderer, &beatmap, &metrics) {
        AutoRendererFilterDecision::Match => {}
        AutoRendererFilterDecision::Rejected => {
            add_auto_event(
                app,
                &replay_name,
                &display,
                &format!("{detail} | Ignored by current auto-render filters."),
                "ignored",
                "auto-replay",
            );
            return Ok(());
        }
        AutoRendererFilterDecision::PpUnavailable => {
            add_auto_event(
                app,
                &replay_name,
                &display,
                &format!("{detail} | PP unavailable for current filters."),
                "skipped",
                "auto-replay",
            );
            return Ok(());
        }
    }

    let Some(token) = ensure_fresh_session(app).await? else {
        add_auto_event(
            app,
            &replay_name,
            &display,
            &format!("{detail} | Replay matched filters but Miru is not logged in."),
            "waiting",
            "auto-replay",
        );
        return Ok(());
    };

    let score = build_score_data(&header, &beatmap);
    submit_score(
        app,
        &config.api_url,
        &token,
        &score,
        &header,
        &beatmap,
        replay_path,
        config.resolution,
        config.auto_renderer.selected_preset_id.as_deref(),
        &config.auto_renderer.selected_skin_id,
        is_watched_osu_stable_replay(stable_paths, replay_path),
        &replay_name,
        &display,
        &detail,
    )
    .await?;
    Ok(())
}

async fn submit_score(
    app: &AppHandle,
    api_url: &str,
    token: &str,
    score: &ScoreData,
    header: &ReplayHeader,
    beatmap: &ResolvedBeatmap,
    replay_path: &Path,
    resolution: crate::types::Resolution,
    preset_id: Option<&str>,
    skin_id: &str,
    prefer_local_replay: bool,
    replay_name: &str,
    display: &str,
    detail_prefix: &str,
) -> AppResult<()> {
    if prefer_local_replay {
        return submit_local_replay(
            app,
            api_url,
            token,
            score,
            beatmap,
            replay_path,
            resolution,
            preset_id,
            skin_id,
            replay_name,
            display,
            detail_prefix,
            "local replay file detected by watcher",
        )
        .await;
    }

    if let Some(online_score_id) = header.online_score_id.filter(|value| *value > 0) {
        match submit_online_score(
            app,
            api_url,
            token,
            score,
            online_score_id,
            beatmap,
            resolution,
            preset_id,
            skin_id,
            replay_name,
            display,
            detail_prefix,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(error) if should_fallback_to_local_submit(&error) => {
                log_line(
                    app,
                    format!(
                        "Online auto-render submit failed for {display}: {error}. Falling back to local replay and Songs mapset upload."
                    ),
                );
            }
            Err(error) => return Err(error),
        }
    }

    submit_local_replay(
        app,
        api_url,
        token,
        score,
        beatmap,
        replay_path,
        resolution,
        preset_id,
        skin_id,
        replay_name,
        display,
        detail_prefix,
        "no usable online score replay",
    )
    .await
}

async fn submit_online_score(
    app: &AppHandle,
    api_url: &str,
    token: &str,
    score: &ScoreData,
    online_score_id: i64,
    beatmap: &ResolvedBeatmap,
    resolution: crate::types::Resolution,
    preset_id: Option<&str>,
    skin_id: &str,
    replay_name: &str,
    display: &str,
    detail_prefix: &str,
) -> AppResult<()> {
    let state = app.state::<ManagedState>();
    let response = state
        .http
        .post(format!("{}/client/score", trim_trailing_slash(api_url)))
        .bearer_auth(token)
        .json(&json!({
            "beatmapId": score.beatmap.id,
            "beatmapSetId": score.beatmap.set_id,
            "beatmapMd5": score.beatmap.md5,
            "score": score.score,
            "combo": score.combo,
            "accuracy": score.accuracy,
            "mods": score.mods,
            "onlineScoreId": online_score_id,
            "replayOriginalName": replay_name,
            "resolution": match resolution {
                crate::types::Resolution::P720 => "p720",
                crate::types::Resolution::P1080 => "p1080",
            },
            "presetId": preset_id.filter(|value| !value.trim().is_empty()),
            "skinId": if skin_id.trim().is_empty() { "default" } else { skin_id.trim() },
            "keyCount": beatmap.key_count,
            "longNoteCount": beatmap.long_note_count,
            "normalNoteCount": beatmap.normal_note_count
        }))
        .send()
        .await?;

    handle_submit_response(
        app,
        response,
        "/api/v1/client/score",
        display,
        detail_prefix,
    )
    .await
}

async fn submit_local_replay(
    app: &AppHandle,
    api_url: &str,
    token: &str,
    score: &ScoreData,
    beatmap: &ResolvedBeatmap,
    replay_path: &Path,
    resolution: crate::types::Resolution,
    preset_id: Option<&str>,
    skin_id: &str,
    replay_name: &str,
    display: &str,
    detail_prefix: &str,
    fallback_reason: &str,
) -> AppResult<()> {
    let archive_path = create_local_mapset_archive(app, beatmap)?;
    let _archive_cleanup = crate::work::CleanupPaths {
        paths: vec![archive_path.clone()],
    };
    log_line(
        app,
        format!(
            "Auto render using local replay and mapset from Songs for {display} ({fallback_reason})"
        ),
    );

    let replay_part = Part::file(replay_path)
        .await?
        .file_name(replay_name.to_string())
        .mime_str("application/octet-stream")?;
    let mapset_name = format!("{}.osz", safe_filename_component(display));
    let mapset_part = Part::file(&archive_path)
        .await?
        .file_name(mapset_name)
        .mime_str("application/octet-stream")?;

    let mut form = Form::new()
        .part("replay", replay_part)
        .part("mapset", mapset_part)
        .text("beatmapMd5", score.beatmap.md5.clone())
        .text("replayOriginalName", replay_name.to_string())
        .text(
            "skinId",
            if skin_id.trim().is_empty() {
                "default".to_string()
            } else {
                skin_id.trim().to_string()
            },
        )
        .text(
            "resolution",
            match resolution {
                crate::types::Resolution::P720 => "p720",
                crate::types::Resolution::P1080 => "p1080",
            },
        );

    if let Some(preset_id) = preset_id.filter(|value| !value.trim().is_empty()) {
        form = form.text("presetId", preset_id.trim().to_string());
    }
    if score.beatmap.id > 0 {
        form = form.text("beatmapId", score.beatmap.id.to_string());
    }
    if score.beatmap.set_id > 0 {
        form = form.text("beatmapSetId", score.beatmap.set_id.to_string());
    }

    let state = app.state::<ManagedState>();
    let response = state
        .http
        .post(format!(
            "{}/client/local-replay",
            trim_trailing_slash(api_url)
        ))
        .bearer_auth(token)
        .multipart(form)
        .send()
        .await;

    let cleanup_result = fs::remove_file(&archive_path);
    if let Err(error) = cleanup_result {
        log_line(
            app,
            format!(
                "Failed to remove temporary local mapset archive {}: {error}",
                archive_path.display()
            ),
        );
    }

    handle_submit_response(
        app,
        response?,
        "/api/v1/client/local-replay",
        display,
        detail_prefix,
    )
    .await
}

async fn handle_submit_response(
    app: &AppHandle,
    response: reqwest::Response,
    endpoint_path: &str,
    display: &str,
    detail_prefix: &str,
) -> AppResult<()> {
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        require_auth_response(app, response).await?;
        return Ok(());
    }

    if !response.status().is_success() {
        let status = response.status();
        let response_body = response.text().await.unwrap_or_default();
        if let Ok(parsed) = serde_json::from_str::<Value>(&response_body) {
            if let Some(message) = parsed
                .get("error")
                .and_then(Value::as_object)
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .or_else(|| parsed.get("message").and_then(Value::as_str))
                .or_else(|| parsed.pointer("/error/message").and_then(Value::as_str))
            {
                return Err(AppError::Process(format!(
                    "auto-render submit failed ({endpoint_path} {status}): {message}"
                )));
            }
        }
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(AppError::Process(format!(
                "auto-render submit endpoint is unavailable on the current backend ({endpoint_path} returned 404)"
            )));
        }
        return Err(AppError::Process(if response_body.trim().is_empty() {
            format!("auto-render submit failed ({endpoint_path} {status})")
        } else {
            format!(
                "auto-render submit failed ({endpoint_path} {status}): {}",
                response_body.trim()
            )
        }));
    }

    let body: Value = response.json().await.unwrap_or_default();
    let job_id = body
        .get("jobId")
        .or_else(|| body.pointer("/data/jobId"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let detail = if job_id.is_empty() {
        format!("{detail_prefix} | Upload accepted.")
    } else {
        format!("{detail_prefix} | Render job {job_id}.")
    };
    add_auto_event(app, display, display, &detail, "started", "render");
    log_line(app, format!("Auto render started for {display}"));
    Ok(())
}

fn should_fallback_to_local_submit(error: &AppError) -> bool {
    let AppError::Process(message) = error else {
        return false;
    };
    let normalized = message.to_ascii_lowercase();

    normalized.contains("no se pudo resolver ese score")
        || normalized.contains("no tiene replay descargable")
        || normalized.contains("no se pudo descargar el replay oficial")
        || normalized.contains("/api/v1/client/score 404")
        || normalized.contains("/api/v1/client/score 409")
        || normalized.contains("/api/v1/client/score 503")
        || normalized.contains("/api/v1/client/score returned 404")
}

fn create_local_mapset_archive(app: &AppHandle, beatmap: &ResolvedBeatmap) -> AppResult<PathBuf> {
    let mapset_dir = beatmap.osu_path.parent().ok_or_else(|| {
        AppError::Process("resolved beatmap path has no parent mapset directory".to_string())
    })?;
    let mapset_root = fs::canonicalize(mapset_dir)?;
    if !mapset_root.is_dir() {
        return Err(AppError::Process(
            "resolved mapset directory is not available".to_string(),
        ));
    }

    let state = app.state::<ManagedState>();
    let upload_dir = state.paths.data_dir.join("auto-render-local-mapsets");
    fs::create_dir_all(&upload_dir)?;
    let archive_path = upload_dir.join(format!(
        "{}-{}.osz",
        safe_filename_component(&beatmap.beatmap_hash),
        Uuid::new_v4()
    ));

    let files = collect_mapset_files(&mapset_root)?;
    let resolved_osu_path = fs::canonicalize(&beatmap.osu_path)?;
    if !files.iter().any(|path| path == &resolved_osu_path) {
        return Err(AppError::Process(
            "local mapset folder does not contain the replay beatmap file".to_string(),
        ));
    }

    let output = fs::File::create(&archive_path)?;
    let mut zip = ZipWriter::new(output);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut input_bytes = 0_u64;

    for file_path in files {
        let metadata = fs::metadata(&file_path)?;
        input_bytes = input_bytes.saturating_add(metadata.len());
        if input_bytes > MAX_LOCAL_MAPSET_ARCHIVE_BYTES {
            let _ = fs::remove_file(&archive_path);
            return Err(AppError::Process(format!(
                "local mapset exceeds {} MiB upload safety limit",
                MAX_LOCAL_MAPSET_ARCHIVE_BYTES / 1024 / 1024
            )));
        }

        let entry_name = zip_relative_name(&mapset_root, &file_path)?;
        zip.start_file(entry_name, options)?;
        let mut input = fs::File::open(&file_path)?;
        std::io::copy(&mut input, &mut zip)?;
    }

    zip.finish()?;
    if fs::metadata(&archive_path)?.len() > MAX_LOCAL_MAPSET_ARCHIVE_BYTES {
        let _ = fs::remove_file(&archive_path);
        return Err(AppError::Process(format!(
            "local mapset archive exceeds {} MiB upload safety limit",
            MAX_LOCAL_MAPSET_ARCHIVE_BYTES / 1024 / 1024
        )));
    }

    Ok(archive_path)
}

fn collect_mapset_files(mapset_root: &Path) -> AppResult<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![mapset_root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = fs::symlink_metadata(&path)?.file_type();
            if file_type.is_symlink() {
                continue;
            }

            if file_type.is_dir() {
                let canonical = match fs::canonicalize(&path) {
                    Ok(value) => value,
                    Err(_) => continue,
                };
                if canonical.starts_with(mapset_root) {
                    stack.push(canonical);
                }
                continue;
            }

            if !file_type.is_file() {
                continue;
            }

            let canonical = match fs::canonicalize(&path) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if !canonical.starts_with(mapset_root) {
                continue;
            }

            files.push(canonical);
            if files.len() > MAX_LOCAL_MAPSET_FILES {
                return Err(AppError::Process(format!(
                    "local mapset has more than {MAX_LOCAL_MAPSET_FILES} files"
                )));
            }
        }
    }

    files.sort();
    Ok(files)
}

fn zip_relative_name(root: &Path, file_path: &Path) -> AppResult<String> {
    let relative = file_path.strip_prefix(root).map_err(|_| {
        AppError::Process("local mapset file escaped the selected mapset directory".to_string())
    })?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(value) => {
                let part = value.to_string_lossy();
                if part.trim().is_empty() || part.contains('/') || part.contains('\\') {
                    return Err(AppError::Process(
                        "local mapset contains an invalid file name".to_string(),
                    ));
                }
                parts.push(part.to_string());
            }
            _ => {
                return Err(AppError::Process(
                    "local mapset contains an invalid relative path".to_string(),
                ));
            }
        }
    }

    if parts.is_empty() {
        return Err(AppError::Process(
            "local mapset file has an empty relative path".to_string(),
        ));
    }
    Ok(parts.join("/"))
}

fn safe_filename_component(value: &str) -> String {
    let mut output = String::with_capacity(value.len().min(96));
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ' ') {
            output.push(ch);
        } else {
            output.push('_');
        }
        if output.len() >= 96 {
            break;
        }
    }
    let trimmed = output.trim_matches([' ', '.', '_', '-']).to_string();
    if trimmed.is_empty() {
        "mapset".to_string()
    } else {
        trimmed
    }
}

async fn wait_for_stable_replay(path: &Path) -> AppResult<()> {
    let mut previous = None;

    for _ in 0..STABLE_SAMPLE_ATTEMPTS {
        let metadata = tokio::fs::metadata(path).await?;
        if metadata.len() > MAX_REPLAY_FILE_BYTES {
            return Err(AppError::Process(format!(
                "replay exceeds {} MiB safety limit",
                MAX_REPLAY_FILE_BYTES / 1024 / 1024
            )));
        }

        let signature = (
            metadata.len(),
            metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        );

        if previous.as_ref() == Some(&signature) {
            return Ok(());
        }

        previous = Some(signature);
        tokio::time::sleep(STABLE_SAMPLE_DELAY).await;
    }

    Err(AppError::Process(
        "replay file never reached a stable on-disk state".to_string(),
    ))
}

fn list_replay_files(replay_dirs: &[PathBuf]) -> AppResult<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for replay_dir in replay_dirs {
        for entry in fs::read_dir(replay_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file()
                && matches!(path.extension().and_then(|value| value.to_str()), Some(ext) if ext.eq_ignore_ascii_case("osr"))
            {
                paths.push(path);
            }
        }
    }
    paths.sort();
    Ok(paths)
}

fn create_replay_watcher(
    replay_dirs: &[PathBuf],
    notify_tx: mpsc::UnboundedSender<PathBuf>,
) -> AppResult<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
        let Ok(event) = result else {
            return;
        };
        for path in event.paths {
            if is_replay_path(&path) {
                let _ = notify_tx.send(path);
            }
        }
    })
    .map_err(|error| AppError::Process(format!("failed to create replay watcher: {error}")))?;

    for replay_dir in replay_dirs {
        watcher
            .watch(replay_dir, RecursiveMode::NonRecursive)
            .map_err(|error| {
                AppError::Process(format!(
                    "failed to watch replay directory {}: {error}",
                    replay_dir.display()
                ))
            })?;
    }

    Ok(watcher)
}

fn is_replay_file(path: &Path) -> bool {
    path.is_file() && is_replay_path(path)
}

fn is_replay_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|value| value.to_str()),
        Some(ext) if ext.eq_ignore_ascii_case("osr")
    )
}

fn is_stable_replay_saved_with_f2(stable_paths: &ResolvedOsuStablePaths, path: &Path) -> bool {
    let Some(replay_dir) = stable_paths.stable_replay_dir.as_ref() else {
        return false;
    };
    if path.starts_with(replay_dir) {
        return true;
    }
    fs::canonicalize(path)
        .map(|canonical| canonical.starts_with(replay_dir))
        .unwrap_or(false)
}

fn is_watched_osu_stable_replay(stable_paths: &ResolvedOsuStablePaths, path: &Path) -> bool {
    if path.starts_with(&stable_paths.replay_dir)
        || stable_paths
            .stable_replay_dir
            .as_ref()
            .is_some_and(|replay_dir| path.starts_with(replay_dir))
    {
        return true;
    }

    fs::canonicalize(path)
        .map(|canonical| {
            canonical.starts_with(&stable_paths.replay_dir)
                || stable_paths
                    .stable_replay_dir
                    .as_ref()
                    .is_some_and(|replay_dir| canonical.starts_with(replay_dir))
        })
        .unwrap_or(false)
}

fn should_retry_replay(error: &AppError) -> bool {
    matches!(
        error,
        AppError::Process(message)
            if message.contains("stable on-disk state") || message.contains(BEATMAP_CACHE_WARMING_MESSAGE)
    )
}

fn is_beatmap_cache_warming_error(error: &AppError) -> bool {
    matches!(
        error,
        AppError::Process(message) if message.contains(BEATMAP_CACHE_WARMING_MESSAGE)
    )
}

fn normalize_replay_key(path: &Path) -> String {
    path.display().to_string().to_ascii_lowercase()
}

fn add_auto_event(
    app: &AppHandle,
    replay_name: &str,
    title: &str,
    detail: &str,
    status: &str,
    kind: &str,
) {
    let event = AutoRendererEvent {
        replay_name: replay_name.to_string(),
        title: title.to_string(),
        detail: detail.to_string(),
        status: status.to_string(),
    };
    let _ = add_history(app, history_entry(kind, title, detail, status, None));
    set_last_auto_renderer_event(app, Some(event));
}

fn set_watcher_status(app: &AppHandle, status: WatcherStatus) {
    {
        let state = app.state::<ManagedState>();
        let mut runtime = state.runtime.lock().expect("runtime state poisoned");
        runtime.watcher_status = status;
    }
    emit_state(app);
}
