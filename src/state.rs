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

// ── Main config (serialised to autosave + user config files) ─────────────────
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    // Paths
    pub folder:   String,
    pub ch_json:  String,
    pub vol_json: String,
    pub date_json:String,
    // Config
    pub workers: usize,
    pub dry_run: bool,
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
    // Metadata fields
    pub series:     String,
    pub writer:     String,
    pub penciller:  String,
    pub publisher:  String,
    pub language:   String,
    pub alt_series: String,
    pub web:        String,
    pub genre:      String,
    pub rating:     String,
    pub year:       String,
    pub month:      String,
    pub day:        String,
    pub count:      String,
    pub summary:    String,
    // Rules
    pub volume_rules:  Vec<Vec<String>>,  // [ch_start, ch_end, volume]
    pub date_rules:    Vec<Vec<String>>,  // [vol_start, vol_end, year, month, day]
    pub summ_rules:    Vec<Vec<String>>,  // [vol_start, vol_end, summary]
    pub custom_fields: Vec<Vec<String>>,  // [field_name, value]
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            folder: String::new(), ch_json: String::new(),
            vol_json: String::new(), date_json: String::new(),
            workers: 4, dry_run: false,
            mode: ComicMode::Manga,
            use_vol: true, use_vol_date: true, use_vol_summ: true,
            prefix_mode: PrefixMode::Auto,
            custom_pfx:  "Break".to_string(),
            post_finale: PostFinale::Strip,
            csep_on: false, csep: "...".to_string(),
            zero_pad: false, pad_width: 2,
            series: String::new(), writer: String::new(), penciller: String::new(),
            publisher: String::new(), language: "en".to_string(), alt_series: String::new(),
            web: String::new(), genre: String::new(), rating: String::new(),
            year: String::new(), month: String::new(), day: String::new(),
            count: String::new(), summary: String::new(),
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
            summ_rules:    Vec::new(),
            custom_fields: Vec::new(),
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
