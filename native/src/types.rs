use crate::tools::FfmpegToolsSnapshot;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const DEFAULT_API_URL: &str = "https://app.miru.uno/api/v1";
pub const DEFAULT_FRONTEND_URL: &str = "https://app.miru.uno";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscordConfig {
    pub enabled: bool,
    pub webhook_set: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CountRuleOp {
    Eq,
    Gte,
    Lte,
    Between,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CountRule {
    pub enabled: bool,
    pub op: CountRuleOp,
    #[serde(
        deserialize_with = "deserialize_rule_number",
        serialize_with = "serialize_rule_number"
    )]
    pub value: f64,
    #[serde(default)]
    #[serde(
        deserialize_with = "deserialize_optional_rule_number",
        serialize_with = "serialize_optional_rule_number"
    )]
    pub max_value: Option<f64>,
}

impl Default for CountRule {
    fn default() -> Self {
        Self {
            enabled: false,
            op: CountRuleOp::Eq,
            value: 0.0,
            max_value: None,
        }
    }
}

fn deserialize_rule_number<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Number(number) => number
            .as_f64()
            .ok_or_else(|| serde::de::Error::custom("invalid numeric rule value")),
        _ => Err(serde::de::Error::custom("expected numeric rule value")),
    }
}

fn deserialize_optional_rule_number<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(number)) => number
            .as_f64()
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom("invalid numeric rule value")),
        Some(_) => Err(serde::de::Error::custom("expected numeric rule value")),
    }
}

fn serialize_rule_number<S>(value: &f64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if value.is_finite() && value.fract().abs() < f64::EPSILON {
        serializer.serialize_u64((*value).max(0.0) as u64)
    } else {
        serializer.serialize_f64(*value)
    }
}

fn serialize_optional_rule_number<S>(value: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        Some(value) => serialize_rule_number(value, serializer),
        None => serializer.serialize_none(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct JudgmentRules {
    #[serde(default)]
    pub max: CountRule,
    #[serde(default)]
    pub n300: CountRule,
    #[serde(default)]
    pub n200: CountRule,
    #[serde(default)]
    pub n100: CountRule,
    #[serde(default)]
    pub n50: CountRule,
    #[serde(default)]
    pub miss: CountRule,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AutoRendererSource {
    OsuStable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoRendererConfig {
    pub source: AutoRendererSource,
    #[serde(default)]
    pub osu_stable_path_override: String,
    #[serde(default)]
    pub selected_preset_id: Option<String>,
    #[serde(default = "default_skin_id")]
    pub selected_skin_id: String,
    #[serde(default)]
    pub key_counts: Vec<u8>,
    #[serde(default)]
    pub long_note_rule: CountRule,
    #[serde(default)]
    pub normal_note_rule: CountRule,
    #[serde(default)]
    pub total_note_rule: CountRule,
    #[serde(default)]
    pub max_combo_rule: CountRule,
    #[serde(default)]
    pub accuracy_rule: CountRule,
    #[serde(default)]
    pub pp_rule: CountRule,
    #[serde(default)]
    pub bpm_rule: CountRule,
    #[serde(default)]
    pub hp_rule: CountRule,
    #[serde(default)]
    pub cs_rule: CountRule,
    #[serde(default)]
    pub od_rule: CountRule,
    #[serde(default)]
    pub duration_rule: CountRule,
    #[serde(default)]
    pub judgment_rules: JudgmentRules,
}

impl Default for AutoRendererConfig {
    fn default() -> Self {
        Self {
            source: AutoRendererSource::OsuStable,
            osu_stable_path_override: String::new(),
            selected_preset_id: None,
            selected_skin_id: default_skin_id(),
            key_counts: Vec::new(),
            long_note_rule: CountRule::default(),
            normal_note_rule: CountRule::default(),
            total_note_rule: CountRule::default(),
            max_combo_rule: CountRule::default(),
            accuracy_rule: CountRule::default(),
            pp_rule: CountRule::default(),
            bpm_rule: CountRule::default(),
            hp_rule: CountRule::default(),
            cs_rule: CountRule::default(),
            od_rule: CountRule::default(),
            duration_rule: CountRule::default(),
            judgment_rules: JudgmentRules::default(),
        }
    }
}

fn default_skin_id() -> String {
    "default".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    #[serde(default)]
    pub machine_id: String,
    #[serde(default)]
    pub user_id: String,
    pub username: String,
    #[serde(default)]
    pub user_avatar_url: String,
    #[serde(default)]
    pub user_role: String,
    #[serde(default)]
    pub user_plan: String,
    #[serde(default)]
    pub discord_linked: bool,
    pub api_url: String,
    pub frontend_url: String,
    pub resolution: Resolution,
    #[serde(default)]
    pub auto_renderer: AutoRendererConfig,
    pub discord: DiscordConfig,
    pub is_server: bool,
    #[serde(default)]
    pub registered_user_id: String,
    #[serde(default)]
    pub server_client_id: String,
    #[serde(default)]
    pub server_status: String,
    pub server_name: String,
    pub server_gpu: String,
    pub server_auto_reconnect: bool,
    #[serde(default = "default_true")]
    pub show_discord_renderer_role: bool,
    #[serde(default = "default_true")]
    pub show_gpu_in_status_image: bool,
    #[serde(default = "default_true")]
    pub connect_worker_on_launch: bool,
    pub renderer_override_path: String,
    pub autostart: bool,
    #[serde(default)]
    pub start_minimized_to_tray: bool,
    #[serde(default = "default_true")]
    pub close_to_tray_on_exit: bool,
    pub imported_legacy_config: bool,
}

fn default_true() -> bool {
    true
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            machine_id: String::new(),
            username: String::new(),
            user_id: String::new(),
            user_avatar_url: String::new(),
            user_role: String::new(),
            user_plan: String::new(),
            discord_linked: false,
            api_url: DEFAULT_API_URL.to_string(),
            frontend_url: DEFAULT_FRONTEND_URL.to_string(),
            resolution: Resolution::P720,
            auto_renderer: AutoRendererConfig::default(),
            discord: DiscordConfig {
                enabled: false,
                webhook_set: false,
            },
            is_server: false,
            registered_user_id: String::new(),
            server_client_id: String::new(),
            server_status: String::new(),
            server_name: String::new(),
            server_gpu: String::new(),
            server_auto_reconnect: false,
            show_discord_renderer_role: true,
            show_gpu_in_status_image: true,
            connect_worker_on_launch: true,
            renderer_override_path: String::new(),
            autostart: false,
            start_minimized_to_tray: false,
            close_to_tray_on_exit: true,
            imported_legacy_config: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Resolution {
    P720,
    P1080,
}

impl Resolution {
    pub fn dimensions(self) -> (u32, u32, u32) {
        match self {
            Self::P720 => (1280, 720, 60),
            Self::P1080 => (1920, 1080, 60),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoRendererEvent {
    pub replay_name: String,
    pub title: String,
    pub detail: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSnapshot {
    pub is_authenticated: bool,
    pub watcher_status: WatcherStatus,
    pub osu_stable_detected: bool,
    pub replay_dir_ready: bool,
    pub stable_replay_dir_ready: bool,
    pub songs_dir_ready: bool,
    pub osu_stable_root: Option<String>,
    pub replay_dir: Option<String>,
    pub stable_replay_dir: Option<String>,
    pub songs_dir: Option<String>,
    pub last_auto_renderer_event: Option<AutoRendererEvent>,
    pub renderer_installed: bool,
    pub worker_status: WorkerStatus,
    pub active_job_id: Option<String>,
    pub benchmark: Option<BenchmarkProgress>,
    pub last_benchmark: Option<BenchmarkResult>,
    pub ffmpeg_tools: FfmpegToolsSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppStatePayload {
    pub config: AppConfig,
    pub runtime: RuntimeSnapshot,
    pub history: Vec<HistoryEntry>,
    pub logs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoRendererLibraryPreset {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoRendererLibrarySkin {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoRendererLibrary {
    pub presets: Vec<AutoRendererLibraryPreset>,
    pub skins: Vec<AutoRendererLibrarySkin>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WatcherStatus {
    Stopped,
    Starting,
    Running,
    Error,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WorkerStatus {
    Disconnected,
    Connecting,
    Connected,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkProgress {
    pub phase: String,
    pub percent: f64,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkResult {
    pub render_time_ms: u64,
    pub download_mbps: f64,
    pub upload_mbps: f64,
    pub latency_ms: u64,
    pub speed_test_bytes: u64,
    pub benchmark_source: String,
    pub max_render_ms: u64,
    pub min_mbps: f64,
    pub min_upload_mbps: f64,
    pub gpu_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadPlanItem {
    pub name: String,
    pub detail: String,
    pub size_bytes: Option<u64>,
    pub will_download: bool,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkDownloadPlan {
    pub install_path: String,
    pub release_url: Option<String>,
    pub items: Vec<DownloadPlanItem>,
    pub total_download_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerComplianceSummary {
    pub active_seconds_this_week: u64,
    pub required_seconds_per_week: u64,
    pub status: String,
    pub grace_ends_at: Option<String>,
    pub window_started_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerStatsPayload {
    pub registered: bool,
    pub status: String,
    pub is_online: bool,
    pub name: String,
    pub client_id: Option<String>,
    pub jobs_completed: u64,
    pub jobs_failed: u64,
    pub total_render_time_seconds: u64,
    pub slots_available: i64,
    pub slots_total: i64,
    pub compliance: Option<WorkerComplianceSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerHistoryEntry {
    pub id: String,
    pub status: String,
    pub replay_name: Option<String>,
    pub title: String,
    pub difficulty: Option<String>,
    pub output_size_bytes: Option<u64>,
    pub queued_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub duration_ms: Option<u64>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: String,
    pub timestamp: String,
    pub kind: String,
    pub title: String,
    pub detail: String,
    pub status: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreData {
    pub beatmap: BeatmapData,
    pub score: i64,
    pub combo: i64,
    pub max_combo: i64,
    pub accuracy: f64,
    pub hits: HitData,
    pub mods: String,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeatmapData {
    pub id: i64,
    pub set_id: i64,
    pub md5: String,
    pub artist: String,
    pub title: String,
    pub diff: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HitData {
    #[serde(rename = "300")]
    pub n300: i64,
    #[serde(rename = "100")]
    pub n100: i64,
    #[serde(rename = "50")]
    pub n50: i64,
    pub geki: i64,
    pub katu: i64,
    pub miss: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveSettingsInput {
    pub api_url: String,
    pub frontend_url: String,
    pub resolution: Resolution,
    pub auto_renderer: AutoRendererConfig,
    pub discord_enabled: bool,
    pub discord_webhook: Option<String>,
    pub server_name: String,
    pub renderer_override_path: String,
    pub autostart: bool,
    pub start_minimized_to_tray: bool,
    pub show_discord_renderer_role: bool,
    pub show_gpu_in_status_image: bool,
    pub connect_worker_on_launch: bool,
    pub close_to_tray_on_exit: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterServerInput {
    pub name: String,
}
