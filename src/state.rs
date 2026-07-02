use serde::{Deserialize, Serialize};

// ── Enums ─────────────────────────────────────────────────────────────────────
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PrefixMode {
    #[default] Auto, Episode, Chapter, Volume, Custom,
}
impl PrefixMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto    => "auto",
            Self::Episode => "episode",
            Self::Chapter => "chapter",
            Self::Volume  => "volume",
            Self::Custom  => "custom",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PostFinale { #[default] Strip, Keep }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ComicMode { #[default] Manga, Manhwa }

// ── Config versioning ──────────────────────────────────────────────────────────
// Bumped whenever AppConfig's structure changes in a way that breaks loading
// older saved configs (renamed/removed/restructured fields). Lets load_config
// detect and warn about a mismatch instead of silently dropping data with no
// explanation -- which is exactly what happened when the fixed metadata
// fields were replaced with the dynamic metadata_fields list.
pub const CURRENT_CONFIG_VERSION: u32 = 1;

// ── Main config (serialised to autosave + user config files) ─────────────────
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    // Bumped to CURRENT_CONFIG_VERSION on save. A loaded config with a lower
    // (or missing -- defaults to 0) version predates some structural change;
    // app.rs's load_config/load_autosave check this and warn accordingly.
    pub config_version: u32,
    // Paths
    pub folder:   String,
    pub ch_json:  String,
    pub vol_json: String,
    pub date_json:String,
    // Config
    pub workers: usize,
    pub dry_run: bool,
    // Output mode: overwrite the original CBZ in place (default, matches
    // every previous version of this app) vs. write a new CBZ and leave the
    // original untouched.
    pub write_new_cbz:    bool,
    // When write_new_cbz is on: true = new files go in the same folder as
    // the source; false = new files go to output_path instead.
    pub output_same_path: bool,
    pub output_path:      String,
    // Mode
    pub mode: ComicMode,
    // Volume metadata
    pub use_vol:      bool,
    pub use_vol_date: bool,
    pub use_vol_summ: bool,
    // Prefix
    pub prefix_mode: PrefixMode,
    pub custom_pfx:  String,
    // Post-finale
    pub post_finale: PostFinale,
    // Separator
    pub csep_on: bool,
    pub csep:    String,
    // Zero-pad
    pub zero_pad:  bool,
    pub pad_width: usize,
    // Constant metadata fields. Each entry is (ComicInfo tag name, value),
    // e.g. ("Series", "Shotgun Boy"). Order here is just the UI's add-order;
    // XML output is always re-sorted into canonical schema order regardless.
    // Choosable from the full ComicInfo v2.1 field list via "Add Tag" in the
    // Metadata tab -- see processing::COMICINFO_FIELDS.
    pub metadata_fields: Vec<(String, String)>,
    // CommunityRating is fixed at 0-5 in the ComicInfo schema, but most
    // community sites (MAL, AniList, ...) rate out of 10. When on, the
    // Metadata tab's CommunityRating box accepts 0-10 instead of 0-5, and
    // the value is converted (rating/10*5) once, at XML-write time --
    // what's stored/shown in the box is always exactly what was typed.
    pub community_rating_10_scale: bool,
    // Summary is kept separate from metadata_fields: it has its own
    // dedicated multi-line UI and chapter-1/fallback override logic.
    pub summary: String,
    // Rules
    pub volume_rules: Vec<Vec<String>>,  // [ch_start, ch_end, volume]
    pub date_rules:   Vec<Vec<String>>,  // [vol_start, vol_end, year, month, day]
    pub summ_rules:   Vec<Vec<String>>,  // [vol_start, vol_end, summary]
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            config_version: CURRENT_CONFIG_VERSION,
            folder: String::new(), ch_json: String::new(),
            vol_json: String::new(), date_json: String::new(),
            workers: 4, dry_run: false,
            write_new_cbz: false, output_same_path: true, output_path: String::new(),
            mode: ComicMode::Manga,
            use_vol: true, use_vol_date: true, use_vol_summ: true,
            prefix_mode: PrefixMode::Auto,
            custom_pfx:  "Break".to_string(),
            post_finale: PostFinale::Strip,
            csep_on: false, csep: "...".to_string(),
            zero_pad: false, pad_width: 2,
            // Same starting field set as before this feature, so existing
            // users see a familiar default. "Rating" is renamed to the
            // correct standard tag "CommunityRating" (the old "Rating" tag
            // was never part of the actual ComicInfo schema).
            metadata_fields: vec![
                ("Series".to_string(),          String::new()),
                ("Writer".to_string(),          String::new()),
                ("Penciller".to_string(),       String::new()),
                ("Publisher".to_string(),       String::new()),
                ("LanguageISO".to_string(),     "en".to_string()),
                ("AlternateSeries".to_string(), String::new()),
                ("Genre".to_string(),           String::new()),
                ("CommunityRating".to_string(), String::new()),
                ("Year".to_string(),            String::new()),
                ("Month".to_string(),           String::new()),
                ("Day".to_string(),             String::new()),
                ("Count".to_string(),           String::new()),
                ("Web".to_string(),              String::new()),
            ],
            community_rating_10_scale: false,
            summary: String::new(),
            volume_rules: vec![
                vec!["1".into(),  "3.5".into(), "1".into()],
                vec!["4".into(),  "8.5".into(), "2".into()],
                vec!["9".into(), "13.5".into(), "3".into()],
            ],
            date_rules: vec![
                vec!["1".into(),"1".into(),"2020".into(),"6".into(), "16".into()],
                vec!["2".into(),"2".into(),"2021".into(),"1".into(), "19".into()],
                vec!["3".into(),"3".into(),"2021".into(),"6".into(),  "1".into()],
            ],
            summ_rules: Vec::new(),
        }
    }
}

// ── App-wide settings (separate from AppConfig / per-job Save-Load-Config) ───
// These persist across all jobs/series, independently of whatever job config
// is currently loaded. Kept deliberately apart from AppConfig: if backup-
// before-overwrite lived in AppConfig instead, loading a different series'
// saved config would silently flip your global safety preference along with
// it -- a toggle like that should only ever change because you changed it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    // Before overwriting a CBZ in place (write_new_cbz off), copy the
    // untouched original to a "backups" subfolder first. Protects against a
    // bad rule or typo silently destroying every file in a batch -- the
    // worst-case failure mode of the default (overwrite-in-place) mode.
    pub backup_before_overwrite: bool,
    // Play a short system sound when a batch run finishes. Useful for long
    // unattended runs where the app isn't the focused window.
    pub play_sound_on_completion: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            backup_before_overwrite: true,   // safety feature: on by default
            play_sound_on_completion: true,  // convenience feature: on by default, easy to disable if noisy
        }
    }
}

// ── Worker stats ──────────────────────────────────────────────────────────────
#[derive(Debug, Clone, Default)]
pub struct RunStats {
    pub total:          usize,
    pub processed:      usize,
    pub renamed:        usize,
    pub rename_skipped: usize,
    pub xml_updated:    usize,
    pub errors:         usize,
}

// ── Log ───────────────────────────────────────────────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LogLevel { Ok, Err, Warn, Dim, Head, Sep, Renamed }

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub text:  String,
    pub level: LogLevel,
}

impl LogLevel {
    pub fn color(self) -> eframe::egui::Color32 {
        use crate::theme::*;
        match self {
            LogLevel::Ok      => TGOOD,
            LogLevel::Err     => TERR,
            LogLevel::Warn    => TWARN,
            LogLevel::Dim     => TDIM,
            LogLevel::Head    => ACC2,
            LogLevel::Sep     => BDR,
            LogLevel::Renamed => ACC2,
        }
    }
}