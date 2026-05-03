use crate::cache::{get_cached_beatmap, put_cached_beatmap, BeatmapCache};
use crate::error::{AppError, AppResult};
use crate::types::{AppConfig, BeatmapData, HitData, ScoreData};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

const DEFAULT_OSU_DIR_NAME: &str = "osu!";
const OSU_STABLE_DB_FILENAME: &str = "osu!.db";
const MAX_REPLAY_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_OSU_DB_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct OsuStablePathsStatus {
    pub osu_stable_detected: bool,
    pub replay_dir_ready: bool,
    pub stable_replay_dir_ready: bool,
    pub songs_dir_ready: bool,
    pub osu_stable_root: Option<PathBuf>,
    pub replay_dir: Option<PathBuf>,
    pub stable_replay_dir: Option<PathBuf>,
    pub songs_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ResolvedOsuStablePaths {
    pub root: PathBuf,
    pub replay_dir: PathBuf,
    pub stable_replay_dir: Option<PathBuf>,
    pub songs_dir: PathBuf,
}

impl ResolvedOsuStablePaths {
    pub fn replay_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = vec![self.replay_dir.clone()];
        if let Some(stable_replay_dir) = &self.stable_replay_dir {
            dirs.push(stable_replay_dir.clone());
        }
        dirs
    }
}

#[derive(Debug, Clone)]
pub struct ReplayHeader {
    pub beatmap_hash: String,
    pub username: String,
    pub score: i64,
    pub max_combo: i64,
    pub hits: HitData,
    pub mods_bits: u32,
    pub online_score_id: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ResolvedBeatmap {
    pub beatmap_id: i64,
    pub beatmap_set_id: i64,
    pub beatmap_hash: String,
    pub artist: String,
    pub title: String,
    pub difficulty_name: String,
    pub key_count: u8,
    pub long_note_count: u32,
    pub normal_note_count: u32,
    pub hp: f64,
    pub cs: f64,
    pub od: f64,
    pub bpm: f64,
    pub duration_ms: u32,
    pub osu_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OsuDbIndexMetadata {
    len: u64,
    modified: Option<SystemTime>,
}

#[derive(Debug, Clone)]
pub struct OsuDbBeatmapIndex {
    entries: HashMap<String, OsuDbBeatmapEntry>,
    metadata: OsuDbIndexMetadata,
}

impl OsuDbBeatmapIndex {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    fn get(&self, beatmap_hash: &str) -> Option<&OsuDbBeatmapEntry> {
        self.entries.get(&beatmap_hash.trim().to_ascii_lowercase())
    }

    fn matches_metadata(&self, metadata: OsuDbIndexMetadata) -> bool {
        self.metadata == metadata
    }
}

pub fn inspect_osu_stable_paths(config: &AppConfig) -> OsuStablePathsStatus {
    let root = detect_osu_stable_root(config)
        .and_then(|path| fs::canonicalize(path).ok())
        .filter(|path| path.is_dir());
    let replay_dir = root
        .as_ref()
        .map(|root_path| root_path.join("Data").join("r"))
        .filter(|path| path.is_dir());
    let stable_replay_dir = root
        .as_ref()
        .map(|root_path| root_path.join("Replays"))
        .filter(|path| path.is_dir());
    let songs_dir = root
        .as_ref()
        .map(|root_path| root_path.join("Songs"))
        .filter(|path| path.is_dir());

    OsuStablePathsStatus {
        osu_stable_detected: root.is_some(),
        replay_dir_ready: replay_dir.is_some() || stable_replay_dir.is_some(),
        stable_replay_dir_ready: stable_replay_dir.is_some(),
        songs_dir_ready: songs_dir.is_some(),
        osu_stable_root: root,
        replay_dir,
        stable_replay_dir,
        songs_dir,
    }
}

pub fn resolve_osu_stable_paths(config: &AppConfig) -> AppResult<ResolvedOsuStablePaths> {
    let raw_root = detect_osu_stable_root(config).ok_or_else(|| {
        AppError::Config(
            "could not detect osu!stable. Set an osu!stable path override in Settings".to_string(),
        )
    })?;
    let root = canonicalize_local_dir(&raw_root)?;
    let replay_dir = canonicalize_local_dir(&root.join("Data").join("r"))?;
    let stable_replay_dir = canonicalize_optional_local_dir(&root.join("Replays"))?;
    let songs_dir = canonicalize_local_dir(&root.join("Songs"))?;

    if !replay_dir.starts_with(&root)
        || stable_replay_dir
            .as_ref()
            .is_some_and(|path| !path.starts_with(&root))
        || !songs_dir.starts_with(&root)
    {
        return Err(AppError::Config(
            "osu!stable directories must stay inside the selected osu!stable root".to_string(),
        ));
    }

    Ok(ResolvedOsuStablePaths {
        root,
        replay_dir,
        stable_replay_dir,
        songs_dir,
    })
}

pub fn validate_osu_stable_override(value: &str) -> AppResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }

    let canonical = canonicalize_local_dir(Path::new(trimmed))?;
    if !canonical.join(OSU_STABLE_DB_FILENAME).is_file() {
        return Err(AppError::InvalidInput(
            "osu!stable root must contain osu!.db".to_string(),
        ));
    }
    if !canonical.join("Songs").is_dir() {
        return Err(AppError::InvalidInput(
            "osu!stable root must contain a Songs folder".to_string(),
        ));
    }
    if !canonical.join("Data").join("r").is_dir() {
        return Err(AppError::InvalidInput(
            "osu!stable root must contain Data\\r for auto-saved replays".to_string(),
        ));
    }
    Ok(canonical.display().to_string())
}

pub fn parse_replay_header_file(path: &Path) -> AppResult<ReplayHeader> {
    let metadata = fs::metadata(path)?;
    if metadata.len() == 0 {
        return Err(AppError::Process("replay file is empty".to_string()));
    }
    if metadata.len() > MAX_REPLAY_FILE_BYTES {
        return Err(AppError::Process(format!(
            "replay file exceeds {} MiB safety limit",
            MAX_REPLAY_FILE_BYTES / 1024 / 1024
        )));
    }

    let bytes = fs::read(path)?;
    parse_replay_header_bytes(&bytes)
}

pub fn parse_replay_header_bytes(bytes: &[u8]) -> AppResult<ReplayHeader> {
    let mut reader = ReplayReader::new(bytes);
    let mode = reader.read_u8()?;
    if mode != 3 {
        return Err(AppError::Process(
            "only osu!mania replays are supported in Auto Renderer v1".to_string(),
        ));
    }

    let _version = reader.read_i32()?;
    let beatmap_hash = reader.read_string()?;
    let username = reader.read_string()?;
    let _replay_hash = reader.read_string()?;
    let n300 = reader.read_i16()? as i64;
    let n100 = reader.read_i16()? as i64;
    let n50 = reader.read_i16()? as i64;
    let geki = reader.read_i16()? as i64;
    let katu = reader.read_i16()? as i64;
    let miss = reader.read_i16()? as i64;
    let score = reader.read_i32()? as i64;
    let max_combo = reader.read_i16()? as i64;
    let _perfect = reader.read_bool()?;
    let mods_bits = reader.read_i32()? as u32;
    let _life_bar_graph = reader.read_string()?;
    let _timestamp = reader.read_i64()?;
    let replay_data_length = reader.read_i32()?;
    if replay_data_length > 0 {
        let _ = reader.read_exact(replay_data_length as usize)?;
    }
    let online_score_id = reader.read_i64().ok();

    if beatmap_hash.trim().is_empty() {
        return Err(AppError::Process(
            "replay beatmap hash is missing".to_string(),
        ));
    }

    Ok(ReplayHeader {
        beatmap_hash,
        username,
        score,
        max_combo,
        hits: HitData {
            n300,
            n100,
            n50,
            geki,
            katu,
            miss,
        },
        mods_bits,
        online_score_id,
    })
}

pub fn resolve_beatmap_by_hash(
    paths: &ResolvedOsuStablePaths,
    cache: &mut BeatmapCache,
    osu_db_index: Option<&OsuDbBeatmapIndex>,
    beatmap_hash: &str,
    allow_songs_scan: bool,
) -> AppResult<Option<ResolvedBeatmap>> {
    if let Some(entry) = get_cached_beatmap(cache, beatmap_hash) {
        let candidate = PathBuf::from(&entry.osu_path);
        if candidate.is_file() {
            let digest = md5_hex(&candidate)?;
            if digest.eq_ignore_ascii_case(beatmap_hash) {
                return Ok(Some(entry.to_resolved(&digest)));
            }
        }
    }

    if let Some(found) = resolve_beatmap_from_osu_db_index(paths, osu_db_index, beatmap_hash)? {
        put_cached_beatmap(cache, &found);
        return Ok(Some(found));
    }

    if osu_db_index.is_none() {
        if let Some(found) = resolve_beatmap_from_osu_db(paths, beatmap_hash)? {
            put_cached_beatmap(cache, &found);
            return Ok(Some(found));
        }
    }

    if !allow_songs_scan {
        return Ok(None);
    }

    if let Some(found) = scan_songs_for_beatmap(&paths.songs_dir, beatmap_hash)? {
        put_cached_beatmap(cache, &found);
        return Ok(Some(found));
    }

    Ok(None)
}

pub fn resolve_beatmap_from_replay_name_hint(
    paths: &ResolvedOsuStablePaths,
    cache: &mut BeatmapCache,
    beatmap_hash: &str,
    replay_name: &str,
) -> AppResult<Option<ResolvedBeatmap>> {
    if let Some(found) =
        scan_songs_for_beatmap_with_replay_name(&paths.songs_dir, beatmap_hash, replay_name)?
    {
        put_cached_beatmap(cache, &found);
        return Ok(Some(found));
    }

    Ok(None)
}

pub fn warm_beatmap_cache_from_songs(
    songs_dir: &Path,
    cache: &mut BeatmapCache,
) -> AppResult<usize> {
    let cached_paths: HashSet<String> = cache
        .values()
        .map(|entry| normalize_cache_path(Path::new(&entry.osu_path)))
        .collect();
    let mut added = 0_usize;

    for path in collect_osu_files(songs_dir) {
        if cached_paths.contains(&normalize_cache_path(&path)) {
            continue;
        }

        let digest = match md5_hex(&path) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if get_cached_beatmap(cache, &digest).is_some() {
            continue;
        }

        let beatmap = match parse_beatmap_file(&path, &digest) {
            Ok(value) => value,
            Err(_) => continue,
        };
        put_cached_beatmap(cache, &beatmap);
        added = added.saturating_add(1);
    }

    Ok(added)
}

pub fn load_osu_db_beatmap_index(
    paths: &ResolvedOsuStablePaths,
) -> AppResult<Option<OsuDbBeatmapIndex>> {
    let db_path = paths.root.join(OSU_STABLE_DB_FILENAME);
    let Some(metadata) = osu_db_index_metadata(&db_path)? else {
        return Ok(None);
    };
    read_osu_db_beatmap_index(&db_path, metadata).map(Some)
}

pub fn refresh_osu_db_beatmap_index_if_changed(
    paths: &ResolvedOsuStablePaths,
    index: &mut Option<OsuDbBeatmapIndex>,
) -> AppResult<bool> {
    let db_path = paths.root.join(OSU_STABLE_DB_FILENAME);
    let Some(metadata) = osu_db_index_metadata(&db_path)? else {
        let had_index = index.take().is_some();
        return Ok(had_index);
    };
    if index
        .as_ref()
        .is_some_and(|current| current.matches_metadata(metadata))
    {
        return Ok(false);
    }

    let next = read_osu_db_beatmap_index(&db_path, metadata)?;
    *index = Some(next);
    Ok(true)
}

fn resolve_beatmap_from_osu_db_index(
    paths: &ResolvedOsuStablePaths,
    osu_db_index: Option<&OsuDbBeatmapIndex>,
    beatmap_hash: &str,
) -> AppResult<Option<ResolvedBeatmap>> {
    let Some(entry) = osu_db_index.and_then(|index| index.get(beatmap_hash)) else {
        return Ok(None);
    };
    resolve_beatmap_from_osu_db_entry(paths, beatmap_hash, entry)
}

fn resolve_beatmap_from_osu_db(
    paths: &ResolvedOsuStablePaths,
    beatmap_hash: &str,
) -> AppResult<Option<ResolvedBeatmap>> {
    let db_path = paths.root.join(OSU_STABLE_DB_FILENAME);
    if !db_path.is_file() {
        return Ok(None);
    }

    let Some(entry) = (match find_beatmap_entry_in_osu_db(&db_path, beatmap_hash) {
        Ok(entry) => entry,
        Err(_) => return Ok(None),
    }) else {
        return Ok(None);
    };

    resolve_beatmap_from_osu_db_entry(paths, beatmap_hash, &entry)
}

fn resolve_beatmap_from_osu_db_entry(
    paths: &ResolvedOsuStablePaths,
    beatmap_hash: &str,
    entry: &OsuDbBeatmapEntry,
) -> AppResult<Option<ResolvedBeatmap>> {
    let candidate = paths
        .songs_dir
        .join(&entry.folder_name)
        .join(&entry.osu_file_name);
    let candidate = match fs::canonicalize(candidate) {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    if !candidate.starts_with(&paths.songs_dir) || !candidate.is_file() {
        return Ok(None);
    }

    let digest = md5_hex(&candidate)?;
    if !digest.eq_ignore_ascii_case(beatmap_hash) {
        return Ok(None);
    }

    Ok(Some(parse_beatmap_file(&candidate, &digest)?))
}

pub fn build_score_data(header: &ReplayHeader, beatmap: &ResolvedBeatmap) -> ScoreData {
    ScoreData {
        beatmap: BeatmapData {
            id: beatmap.beatmap_id,
            set_id: beatmap.beatmap_set_id,
            md5: beatmap.beatmap_hash.clone(),
            artist: beatmap.artist.clone(),
            title: beatmap.title.clone(),
            diff: beatmap.difficulty_name.clone(),
        },
        score: header.score,
        combo: header.max_combo,
        max_combo: header.max_combo,
        accuracy: mania_accuracy_from_hits(&header.hits),
        hits: header.hits.clone(),
        mods: stable_mods_string(header.mods_bits),
        passed: true,
    }
}

pub fn calculate_replay_pp(header: &ReplayHeader, beatmap: &ResolvedBeatmap) -> AppResult<f64> {
    let map = rosu_pp::Beatmap::from_path(&beatmap.osu_path).map_err(|error| {
        AppError::Process(format!(
            "PP unavailable: failed to parse beatmap for rosu-pp ({error})"
        ))
    })?;
    map.check_suspicion().map_err(|error| {
        AppError::Process(format!(
            "PP unavailable: beatmap is too suspicious for rosu-pp ({error})"
        ))
    })?;

    let state = rosu_pp::mania::ManiaScoreState {
        n320: nonnegative_hit_count(header.hits.geki),
        n300: nonnegative_hit_count(header.hits.n300),
        n200: nonnegative_hit_count(header.hits.katu),
        n100: nonnegative_hit_count(header.hits.n100),
        n50: nonnegative_hit_count(header.hits.n50),
        misses: nonnegative_hit_count(header.hits.miss),
    };
    let attributes = rosu_pp::mania::ManiaPerformance::new(&map)
        .mods(header.mods_bits)
        .lazer(false)
        .state(state)
        .calculate()
        .map_err(|error| AppError::Process(format!("PP unavailable: {error}")))?;
    Ok(attributes.pp())
}

pub fn replay_display_name(path: &Path, beatmap: Option<&ResolvedBeatmap>) -> String {
    if let Some(beatmap) = beatmap {
        format!(
            "{} - {} [{}]",
            beatmap.artist, beatmap.title, beatmap.difficulty_name
        )
    } else {
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("Replay")
            .to_string()
    }
}

fn detect_osu_stable_root(config: &AppConfig) -> Option<PathBuf> {
    let override_path = config.auto_renderer.osu_stable_path_override.trim();
    if !override_path.is_empty() {
        return Some(PathBuf::from(override_path));
    }

    let local_app_data = std::env::var("LOCALAPPDATA").ok()?;
    let fallback = PathBuf::from(local_app_data).join(DEFAULT_OSU_DIR_NAME);
    fallback.is_dir().then_some(fallback)
}

fn canonicalize_local_dir(path: &Path) -> AppResult<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(AppError::InvalidInput("path cannot be empty".to_string()));
    }
    if !path.is_absolute() {
        return Err(AppError::InvalidInput(
            "path override must be an absolute local directory".to_string(),
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(AppError::InvalidInput(
            "path override cannot contain parent-directory segments".to_string(),
        ));
    }

    let path_text = path.display().to_string();
    if is_disallowed_network_path(&path_text) {
        return Err(AppError::InvalidInput(
            "network paths are not allowed for osu!stable override".to_string(),
        ));
    }

    let canonical = fs::canonicalize(path).map_err(|error| {
        AppError::InvalidInput(format!("path override could not be resolved: {error}"))
    })?;
    let canonical = normalize_local_path(canonical);

    if !canonical.is_dir() {
        return Err(AppError::InvalidInput(
            "path override must point to a directory".to_string(),
        ));
    }

    Ok(canonical)
}

fn canonicalize_optional_local_dir(path: &Path) -> AppResult<Option<PathBuf>> {
    if path.is_dir() {
        canonicalize_local_dir(path).map(Some)
    } else {
        Ok(None)
    }
}

#[cfg(target_os = "windows")]
fn is_disallowed_network_path(value: &str) -> bool {
    let normalized = value.replace('/', "\\");
    if normalized.starts_with(r"\\?\UNC\") || normalized.starts_with(r"\\.\UNC\") {
        return true;
    }
    if normalized.starts_with(r"\\?\") || normalized.starts_with(r"\\.\") {
        return false;
    }
    normalized.starts_with(r"\\")
}

#[cfg(not(target_os = "windows"))]
fn is_disallowed_network_path(_value: &str) -> bool {
    false
}

#[cfg(target_os = "windows")]
fn normalize_local_path(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(stripped) = text.strip_prefix(r"\\?\") {
        if !stripped.starts_with("UNC\\") {
            return PathBuf::from(stripped);
        }
    }
    path
}

#[cfg(not(target_os = "windows"))]
fn normalize_local_path(path: PathBuf) -> PathBuf {
    path
}

fn scan_songs_for_beatmap(
    songs_dir: &Path,
    beatmap_hash: &str,
) -> AppResult<Option<ResolvedBeatmap>> {
    let mut stack = vec![songs_dir.to_path_buf()];

    while let Some(next_dir) = stack.pop() {
        let entries = match fs::read_dir(&next_dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if !matches!(path.extension().and_then(|ext| ext.to_str()), Some(ext) if ext.eq_ignore_ascii_case("osu"))
            {
                continue;
            }

            let digest = match md5_hex(&path) {
                Ok(digest) => digest,
                Err(_) => continue,
            };
            if !digest.eq_ignore_ascii_case(beatmap_hash) {
                continue;
            }

            return Ok(Some(parse_beatmap_file(&path, &digest)?));
        }
    }

    Ok(None)
}

fn scan_songs_for_beatmap_with_replay_name(
    songs_dir: &Path,
    beatmap_hash: &str,
    replay_name: &str,
) -> AppResult<Option<ResolvedBeatmap>> {
    let replay_lookup = normalize_lookup_text(replay_name);
    if replay_lookup.len() < 6 {
        return Ok(None);
    }

    let mut candidate_dirs = Vec::new();
    let entries = match fs::read_dir(songs_dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(None),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let folder_lookup = normalize_song_folder_lookup(name);
        if folder_lookup.len() >= 6 && replay_lookup.contains(&folder_lookup) {
            candidate_dirs.push(path);
        }
    }

    candidate_dirs.sort();
    for dir in candidate_dirs {
        if let Some(found) = scan_osu_files_in_dir(&dir, beatmap_hash)? {
            return Ok(Some(found));
        }
    }

    Ok(None)
}

fn scan_osu_files_in_dir(dir: &Path, beatmap_hash: &str) -> AppResult<Option<ResolvedBeatmap>> {
    for path in collect_osu_files(dir) {
        let digest = match md5_hex(&path) {
            Ok(digest) => digest,
            Err(_) => continue,
        };
        if digest.eq_ignore_ascii_case(beatmap_hash) {
            return Ok(Some(parse_beatmap_file(&path, &digest)?));
        }
    }

    Ok(None)
}

fn collect_osu_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(next_dir) = stack.pop() {
        let entries = match fs::read_dir(&next_dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if matches!(path.extension().and_then(|ext| ext.to_str()), Some(ext) if ext.eq_ignore_ascii_case("osu"))
            {
                files.push(path);
            }
        }
    }

    files.sort();
    files
}

fn normalize_song_folder_lookup(value: &str) -> String {
    let without_set_id = value.trim_start_matches(|character: char| {
        character.is_ascii_digit() || character.is_whitespace()
    });
    normalize_lookup_text(without_set_id)
}

fn normalize_lookup_text(value: &str) -> String {
    let mut output = String::new();
    let mut previous_was_space = true;

    for character in value.chars() {
        if character.is_alphanumeric() {
            for lowercase in character.to_lowercase() {
                output.push(lowercase);
            }
            previous_was_space = false;
        } else if !previous_was_space {
            output.push(' ');
            previous_was_space = true;
        }
    }

    output.trim().to_string()
}

fn normalize_cache_path(path: &Path) -> String {
    path.display()
        .to_string()
        .replace('/', "\\")
        .to_ascii_lowercase()
}

#[derive(Debug, Clone)]
struct OsuDbBeatmapEntry {
    osu_file_name: String,
    folder_name: String,
}

fn osu_db_index_metadata(db_path: &Path) -> AppResult<Option<OsuDbIndexMetadata>> {
    let metadata = match fs::metadata(db_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_OSU_DB_BYTES {
        return Ok(None);
    }

    Ok(Some(OsuDbIndexMetadata {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    }))
}

fn read_osu_db_beatmap_index(
    db_path: &Path,
    metadata: OsuDbIndexMetadata,
) -> AppResult<OsuDbBeatmapIndex> {
    let bytes = fs::read(db_path)?;
    let mut reader = OsuDbReader::new(&bytes);
    let version = reader.read_i32()?;
    let _folder_count = reader.read_i32()?;
    let _account_unlocked = reader.read_bool()?;
    let _unlock_date = reader.read_i64()?;
    let _player_name = reader.read_string()?;
    let beatmap_count = reader.read_i32()?;
    if !(0..=500_000).contains(&beatmap_count) {
        return Err(AppError::Process(
            "osu!.db beatmap count is invalid".to_string(),
        ));
    }

    let mut entries = HashMap::with_capacity(beatmap_count as usize);
    for _ in 0..beatmap_count {
        let entry = reader.read_beatmap_entry(version)?;
        let normalized_hash = entry.beatmap_hash.trim().to_ascii_lowercase();
        if normalized_hash.is_empty() {
            continue;
        }
        entries.insert(
            normalized_hash,
            OsuDbBeatmapEntry {
                osu_file_name: entry.osu_file_name,
                folder_name: entry.folder_name,
            },
        );
    }

    Ok(OsuDbBeatmapIndex { entries, metadata })
}

fn find_beatmap_entry_in_osu_db(
    db_path: &Path,
    beatmap_hash: &str,
) -> AppResult<Option<OsuDbBeatmapEntry>> {
    let Some(metadata) = osu_db_index_metadata(db_path)? else {
        return Ok(None);
    };
    let index = read_osu_db_beatmap_index(db_path, metadata)?;
    let normalized_hash = beatmap_hash.trim().to_ascii_lowercase();
    Ok(index.entries.get(&normalized_hash).cloned())
}

struct OsuDbEntry {
    beatmap_hash: String,
    osu_file_name: String,
    folder_name: String,
}

struct OsuDbReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> OsuDbReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn read_beatmap_entry(&mut self, version: i32) -> AppResult<OsuDbEntry> {
        if version < 20191106 {
            let _entry_size = self.read_i32()?;
        }

        let _artist = self.read_string()?;
        let _artist_unicode = self.read_string()?;
        let _title = self.read_string()?;
        let _title_unicode = self.read_string()?;
        let _creator = self.read_string()?;
        let _difficulty = self.read_string()?;
        let _audio_file_name = self.read_string()?;
        let beatmap_hash = self.read_string()?;
        let osu_file_name = self.read_string()?;
        let _ranked_status = self.read_u8()?;
        let _hit_circles = self.read_u16()?;
        let _sliders = self.read_u16()?;
        let _spinners = self.read_u16()?;
        let _last_modified = self.read_i64()?;
        let _approach_rate = self.read_f32()?;
        let _circle_size = self.read_f32()?;
        let _hp_drain = self.read_f32()?;
        let _overall_difficulty = self.read_f32()?;
        let _slider_velocity = self.read_f64()?;

        if version >= 20140609 {
            for _ in 0..4 {
                self.skip_star_rating_pairs(version)?;
            }
        }

        let _drain_time = self.read_i32()?;
        let _total_time = self.read_i32()?;
        let _preview_time = self.read_i32()?;
        let timing_point_count = self.read_i32()?;
        if !(0..=100_000).contains(&timing_point_count) {
            return Err(AppError::Process(
                "osu!.db timing point count is invalid".to_string(),
            ));
        }
        for _ in 0..timing_point_count {
            let _bpm = self.read_f64()?;
            let _offset = self.read_f64()?;
            let _inherited = self.read_bool()?;
        }

        let _beatmap_id = self.read_i32()?;
        let _beatmapset_id = self.read_i32()?;
        let _thread_id = self.read_i32()?;
        let _std_grade = self.read_u8()?;
        let _taiko_grade = self.read_u8()?;
        let _ctb_grade = self.read_u8()?;
        let _mania_grade = self.read_u8()?;
        let _local_offset = self.read_i16()?;
        let _stack_leniency = self.read_f32()?;
        let _gameplay_mode = self.read_u8()?;
        let _source = self.read_string()?;
        let _tags = self.read_string()?;
        let _online_offset = self.read_i16()?;
        let _font = self.read_string()?;
        let _unplayed = self.read_bool()?;
        let _last_played = self.read_i64()?;
        let _is_osz2 = self.read_bool()?;
        let folder_name = self.read_string()?;
        let _last_checked = self.read_i64()?;
        let _ignore_sound = self.read_bool()?;
        let _ignore_skin = self.read_bool()?;
        let _disable_storyboard = self.read_bool()?;
        let _disable_video = self.read_bool()?;
        let _visual_override = self.read_bool()?;
        if version < 20140609 {
            let _unknown_short = self.read_i16()?;
        }
        let _last_modified_again = self.read_i32()?;
        let _mania_scroll_speed = self.read_u8()?;

        Ok(OsuDbEntry {
            beatmap_hash,
            osu_file_name,
            folder_name,
        })
    }

    fn skip_star_rating_pairs(&mut self, version: i32) -> AppResult<()> {
        let count = self.read_i32()?;
        if !(0..=10_000).contains(&count) {
            return Err(AppError::Process(
                "osu!.db star rating count is invalid".to_string(),
            ));
        }

        for _ in 0..count {
            let _mod_marker = self.read_u8()?;
            let _mods = self.read_i32()?;
            let _star_marker = self.read_u8()?;
            if version >= 20250107 {
                let _stars = self.read_f32()?;
            } else {
                let _stars = self.read_f64()?;
            }
        }
        Ok(())
    }

    fn read_u8(&mut self) -> AppResult<u8> {
        let value = *self
            .bytes
            .get(self.offset)
            .ok_or_else(|| AppError::Process("unexpected end of osu!.db".to_string()))?;
        self.offset += 1;
        Ok(value)
    }

    fn read_bool(&mut self) -> AppResult<bool> {
        Ok(self.read_u8()? != 0)
    }

    fn read_u16(&mut self) -> AppResult<u16> {
        let bytes = self.read_exact(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn read_i16(&mut self) -> AppResult<i16> {
        let bytes = self.read_exact(2)?;
        Ok(i16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn read_i32(&mut self) -> AppResult<i32> {
        let bytes = self.read_exact(4)?;
        Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_i64(&mut self) -> AppResult<i64> {
        let bytes = self.read_exact(8)?;
        Ok(i64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn read_f32(&mut self) -> AppResult<f32> {
        let bytes = self.read_exact(4)?;
        Ok(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_f64(&mut self) -> AppResult<f64> {
        let bytes = self.read_exact(8)?;
        Ok(f64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn read_string(&mut self) -> AppResult<String> {
        let marker = self.read_u8()?;
        if marker == 0 {
            return Ok(String::new());
        }
        if marker != 0x0b {
            return Err(AppError::Process(
                "osu!.db string marker is invalid".to_string(),
            ));
        }

        let length = self.read_uleb128()? as usize;
        let bytes = self.read_exact(length)?;
        Ok(String::from_utf8_lossy(bytes).to_string())
    }

    fn read_uleb128(&mut self) -> AppResult<u64> {
        let mut result = 0_u64;
        let mut shift = 0_u32;
        loop {
            let byte = self.read_u8()?;
            result |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift = shift.saturating_add(7);
            if shift > 56 {
                return Err(AppError::Process(
                    "osu!.db string length is too large".to_string(),
                ));
            }
        }
        Ok(result)
    }

    fn read_exact(&mut self, length: usize) -> AppResult<&'a [u8]> {
        let end = self.offset.saturating_add(length);
        if end > self.bytes.len() {
            return Err(AppError::Process("unexpected end of osu!.db".to_string()));
        }
        let slice = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(slice)
    }
}

fn parse_beatmap_file(path: &Path, beatmap_hash: &str) -> AppResult<ResolvedBeatmap> {
    let raw_bytes = fs::read(path)?;
    let raw = String::from_utf8_lossy(&raw_bytes);
    let mut section = "";
    let mut beatmap_id = 0_i64;
    let mut beatmap_set_id = 0_i64;
    let mut artist = String::new();
    let mut title = String::new();
    let mut difficulty_name = String::new();
    let mut key_count = 0_u8;
    let mut hp = 0.0_f64;
    let mut cs = 0.0_f64;
    let mut od = 0.0_f64;
    let mut bpm = 0.0_f64;
    let mut total_notes = 0_u32;
    let mut long_notes = 0_u32;
    let mut duration_ms = 0_u32;

    for raw_line in raw.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = &line[1..line.len() - 1];
            continue;
        }

        match section {
            "Metadata" => {
                if let Some((key, value)) = line.split_once(':') {
                    let value = value.trim();
                    match key.trim() {
                        "BeatmapID" => beatmap_id = value.parse().unwrap_or_default(),
                        "BeatmapSetID" => beatmap_set_id = value.parse().unwrap_or_default(),
                        "Artist" => artist = value.to_string(),
                        "Title" => title = value.to_string(),
                        "Version" => difficulty_name = value.to_string(),
                        _ => {}
                    }
                }
            }
            "Difficulty" => {
                if let Some((key, value)) = line.split_once(':') {
                    let parsed = value.trim().parse::<f64>().unwrap_or_default();
                    match key.trim() {
                        "CircleSize" => {
                            cs = parsed;
                            key_count = parsed.round().clamp(1.0, 18.0) as u8;
                        }
                        "HPDrainRate" => hp = parsed,
                        "OverallDifficulty" => od = parsed,
                        _ => {}
                    }
                }
            }
            "TimingPoints" => {
                let mut columns = line.split(',');
                let _time = columns.next();
                let beat_length = columns
                    .next()
                    .and_then(|value| value.trim().parse::<f64>().ok())
                    .unwrap_or_default();
                let uninherited = columns
                    .nth(4)
                    .map(|value| value.trim() != "0")
                    .unwrap_or(true);
                if uninherited && beat_length > 0.0 {
                    bpm = bpm.max(60_000.0 / beat_length);
                }
            }
            "HitObjects" => {
                let mut columns = line.split(',');
                let _x = columns.next();
                let _y = columns.next();
                let time = columns
                    .next()
                    .and_then(|value| value.trim().parse::<u32>().ok())
                    .unwrap_or_default();
                let object_type = columns
                    .next()
                    .and_then(|value| value.parse::<u32>().ok())
                    .unwrap_or_default();
                let _hit_sound = columns.next();
                total_notes = total_notes.saturating_add(1);
                if object_type & 128 != 0 {
                    long_notes = long_notes.saturating_add(1);
                    let end_time = columns
                        .next()
                        .and_then(|value| value.split(':').next())
                        .and_then(|value| value.trim().parse::<u32>().ok())
                        .unwrap_or(time);
                    duration_ms = duration_ms.max(end_time);
                } else {
                    duration_ms = duration_ms.max(time);
                }
            }
            _ => {}
        }
    }

    Ok(ResolvedBeatmap {
        beatmap_id,
        beatmap_set_id,
        beatmap_hash: beatmap_hash.to_ascii_lowercase(),
        artist,
        title,
        difficulty_name,
        key_count,
        long_note_count: long_notes,
        normal_note_count: total_notes.saturating_sub(long_notes),
        hp,
        cs,
        od,
        bpm,
        duration_ms,
        osu_path: path.to_path_buf(),
    })
}

fn md5_hex(path: &Path) -> AppResult<String> {
    let bytes = fs::read(path)?;
    Ok(format!("{:x}", md5::compute(bytes)))
}

fn mania_accuracy_from_hits(hits: &HitData) -> f64 {
    let total = hits.n300 + hits.n100 + hits.n50 + hits.geki + hits.katu + hits.miss;
    if total <= 0 {
        return 0.0;
    }

    let weighted =
        hits.n50 * 50 + hits.n100 * 100 + hits.katu * 200 + (hits.n300 + hits.geki) * 300;
    (weighted as f64 / (total as f64 * 300.0)) * 100.0
}

fn nonnegative_hit_count(value: i64) -> u32 {
    u32::try_from(value.max(0)).unwrap_or(u32::MAX)
}

fn stable_mods_string(bits: u32) -> String {
    let mut parts = Vec::new();
    let mappings = [
        (1, "NF"),
        (2, "EZ"),
        (8, "HD"),
        (16, "HR"),
        (32, "SD"),
        (64, "DT"),
        (256, "HT"),
        (512, "NC"),
        (1024, "FL"),
        (16384, "PF"),
        (32768, "4K"),
        (65536, "5K"),
        (131072, "6K"),
        (262144, "7K"),
        (524288, "8K"),
        (1048576, "FI"),
        (2097152, "RD"),
        (16777216, "9K"),
        (33554432, "CO"),
        (67108864, "1K"),
        (134217728, "3K"),
        (268435456, "2K"),
        (536870912, "V2"),
        (1073741824, "MR"),
    ];

    for (bit, label) in mappings {
        if bits & bit != 0 {
            if label == "NC" && parts.iter().any(|existing| existing == &"DT") {
                continue;
            }
            if label == "PF" && parts.iter().any(|existing| existing == &"SD") {
                continue;
            }
            parts.push(label);
        }
    }

    if parts.is_empty() {
        "NM".to_string()
    } else {
        parts.join("")
    }
}

struct ReplayReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> ReplayReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn read_u8(&mut self) -> AppResult<u8> {
        let value = *self
            .bytes
            .get(self.offset)
            .ok_or_else(|| AppError::Process("unexpected end of replay data".to_string()))?;
        self.offset += 1;
        Ok(value)
    }

    fn read_bool(&mut self) -> AppResult<bool> {
        Ok(self.read_u8()? != 0)
    }

    fn read_i16(&mut self) -> AppResult<i16> {
        let bytes = self.read_exact(2)?;
        Ok(i16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn read_i32(&mut self) -> AppResult<i32> {
        let bytes = self.read_exact(4)?;
        Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_i64(&mut self) -> AppResult<i64> {
        let bytes = self.read_exact(8)?;
        Ok(i64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn read_string(&mut self) -> AppResult<String> {
        let marker = self.read_u8()?;
        if marker == 0 {
            return Ok(String::new());
        }
        if marker != 0x0b {
            return Err(AppError::Process(
                "replay string marker is invalid".to_string(),
            ));
        }

        let length = self.read_uleb128()? as usize;
        let bytes = self.read_exact(length)?;
        Ok(String::from_utf8_lossy(bytes).to_string())
    }

    fn read_uleb128(&mut self) -> AppResult<u64> {
        let mut result = 0_u64;
        let mut shift = 0_u32;
        loop {
            let byte = self.read_u8()?;
            result |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift = shift.saturating_add(7);
            if shift > 56 {
                return Err(AppError::Process(
                    "replay string length is too large".to_string(),
                ));
            }
        }
        Ok(result)
    }

    fn read_exact(&mut self, length: usize) -> AppResult<&'a [u8]> {
        let end = self.offset.saturating_add(length);
        if end > self.bytes.len() {
            return Err(AppError::Process(
                "unexpected end of replay data".to_string(),
            ));
        }
        let slice = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(slice)
    }
}
