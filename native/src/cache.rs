use crate::config::AppPaths;
use crate::error::AppResult;
use crate::osu::ResolvedBeatmap;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

pub type BeatmapCache = HashMap<String, CachedBeatmapEntry>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedBeatmapEntry {
    pub osu_path: String,
    pub beatmap_id: i64,
    pub beatmap_set_id: i64,
    pub artist: String,
    pub title: String,
    pub difficulty_name: String,
    pub key_count: u8,
    pub long_note_count: u32,
    pub normal_note_count: u32,
    #[serde(default)]
    pub hp: f64,
    #[serde(default)]
    pub cs: f64,
    #[serde(default)]
    pub od: f64,
    #[serde(default)]
    pub bpm: f64,
    #[serde(default)]
    pub duration_ms: u32,
    pub last_verified_at: String,
}

impl CachedBeatmapEntry {
    pub fn to_resolved(&self, beatmap_hash: &str) -> ResolvedBeatmap {
        ResolvedBeatmap {
            beatmap_id: self.beatmap_id,
            beatmap_set_id: self.beatmap_set_id,
            beatmap_hash: beatmap_hash.to_string(),
            artist: self.artist.clone(),
            title: self.title.clone(),
            difficulty_name: self.difficulty_name.clone(),
            key_count: self.key_count,
            long_note_count: self.long_note_count,
            normal_note_count: self.normal_note_count,
            hp: self.hp,
            cs: self.cs,
            od: self.od,
            bpm: self.bpm,
            duration_ms: self.duration_ms,
            osu_path: PathBuf::from(&self.osu_path),
        }
    }
}

pub fn load_beatmap_cache(paths: &AppPaths) -> AppResult<BeatmapCache> {
    if !paths.beatmap_cache_path.exists() {
        return Ok(HashMap::new());
    }

    let raw = fs::read_to_string(&paths.beatmap_cache_path)?;
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

pub fn save_beatmap_cache(paths: &AppPaths, cache: &BeatmapCache) -> AppResult<()> {
    fs::create_dir_all(&paths.data_dir)?;
    let serialized = serde_json::to_string_pretty(cache)?;
    fs::write(&paths.beatmap_cache_path, serialized)?;
    Ok(())
}

pub fn get_cached_beatmap(cache: &BeatmapCache, beatmap_hash: &str) -> Option<CachedBeatmapEntry> {
    cache.get(&normalize_hash(beatmap_hash)).cloned()
}

pub fn put_cached_beatmap(cache: &mut BeatmapCache, beatmap: &ResolvedBeatmap) {
    cache.insert(
        normalize_hash(&beatmap.beatmap_hash),
        CachedBeatmapEntry {
            osu_path: beatmap.osu_path.display().to_string(),
            beatmap_id: beatmap.beatmap_id,
            beatmap_set_id: beatmap.beatmap_set_id,
            artist: beatmap.artist.clone(),
            title: beatmap.title.clone(),
            difficulty_name: beatmap.difficulty_name.clone(),
            key_count: beatmap.key_count,
            long_note_count: beatmap.long_note_count,
            normal_note_count: beatmap.normal_note_count,
            hp: beatmap.hp,
            cs: beatmap.cs,
            od: beatmap.od,
            bpm: beatmap.bpm,
            duration_ms: beatmap.duration_ms,
            last_verified_at: Utc::now().to_rfc3339(),
        },
    );
}

fn normalize_hash(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}
