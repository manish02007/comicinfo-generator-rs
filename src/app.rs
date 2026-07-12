use crate::{processing::*, state::*, theme, worker::{UiMsg, WorkerConfig, WorkerMsg}};
use eframe::egui::{self, Color32, RichText};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{atomic::AtomicBool, mpsc, Arc};

// ── Autosave ──────────────────────────────────────────────────────────────────
fn autosave_path() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".comicinfo_autosave.json")
}

// ── App settings (separate file from autosave/per-job configs) ───────────────
fn settings_path() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".comicinfo_settings.json")
}

// Best-effort, dependency-free notification sound for AppSettings::
// play_sound_on_completion: shells out to whatever each OS already ships
// rather than pulling in an audio-playback crate to make one beep.
// Fire-and-forget -- a missing player on an unusual setup just means
// silence, never a blocked UI thread or a visible error.
fn play_completion_sound() {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("rundll32")
            .arg("user32.dll,MessageBeep")
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("afplay")
            .arg("/System/Library/Sounds/Glass.aiff")
            .spawn();
    }
    #[cfg(target_os = "linux")]
    {
        // Try common players in order; the first that exists wins. Neither
        // existing is equally likely across distros, but a silent no-op if
        // neither is present is fine -- this is a convenience feature,
        // never worth failing a run over.
        let candidates: [(&str, &[&str]); 2] = [
            ("canberra-gtk-play", &["-i", "complete"]),
            ("paplay", &["/usr/share/sounds/freedesktop/stereo/complete.oga"]),
        ];
        for (cmd, args) in candidates {
            if std::process::Command::new(cmd).args(args).spawn().is_ok() {
                break;
            }
        }
    }
}

// ── Tabs ──────────────────────────────────────────────────────────────────────
#[derive(Clone, Copy, PartialEq, Default)]
pub enum Tab { #[default] Paths, Processing, Metadata, Rules, Run }

// ── Rule-edit dialog state ────────────────────────────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RuleTarget { Volume, Date, Summary }

#[derive(Debug, Clone)]
pub struct RuleEditState {
    pub target:  RuleTarget,
    pub row_idx: Option<usize>,
    pub labels:  Vec<String>,
    pub values:  Vec<String>,
    pub is_new:  bool,
    // "Vol Start and Vol End are the same" checkbox state (Date/Summary
    // rules only). A REAL piece of state, not re-derived each frame from
    // whether values[0]==values[1] and non-empty -- deriving it from the
    // values meant ticking the checkbox with a blank Vol Start did
    // nothing, since "" == "" was deliberately excluded from counting as
    // "same" (to avoid two blank fields looking pre-merged). The checkbox
    // now controls the field layout directly regardless of what's typed;
    // only the STARTING value (when the dialog opens) is derived from
    // the existing values, as a sensible default for re-opening a
    // previously-saved matching-range rule.
    pub same_start_end: bool,
}

#[derive(Debug, Clone)]
pub struct DecimalState {
    pub filename:  String,
    pub raw_title: String,
    pub choice:    u8,
    pub custom:    String,
}

#[derive(Debug, Clone)]
pub enum Dialog {
    EditRule(RuleEditState),
    Decimal(DecimalState),
    ResumeSession { cbz_files: Vec<PathBuf>, processed_set: HashSet<String>, count: usize },
    FinalChapter  { cbz_files: Vec<PathBuf>, processed_set: HashSet<String>, resume: bool,
                    finale_num: String, finale_idx: usize },
    Notice(String),
    ConfirmReset,
    ConfirmClearLog,
    /// Remove clicked with nothing selected -- confirms clearing every
    /// rule in the given table rather than silently doing nothing.
    ConfirmClearAllRules(RuleTarget),
    /// Warns about empty constant-metadata fields before starting a run.
    EmptyFieldsWarning { fields: Vec<String>, cbzs: Vec<PathBuf> },
    /// Lists all ComicInfo schema fields not currently in metadata_fields,
    /// letting the user pick one to add.
    AddMetadataTag,
    /// Drag-and-drop reordering of the tags written to ComicInfo.xml.
    ReorderTags,
    /// Shows the list of fields imported from a .py or .json metadata file
    ImportResult { filename: String, items: Vec<(String, String)> },
}

#[derive(Debug, Clone)]
pub enum PathPick { Folder, TitlesJson, DateJson, LoadConfig, SaveConfig(String), ImportMeta, OutputPath }

#[derive(Default, Clone)]
pub struct DisplayStats {
    pub total: usize, pub processed: usize, pub renamed: usize,
    pub skipped: usize, pub xml: usize, pub errors: usize,
}

// ── Main struct ───────────────────────────────────────────────────────────────
pub struct ComicInfoApp {
    pub cfg:   AppConfig,
    pub tab:   Tab,
    pub sep_preview: String,
    pub status:      String,
    pub verbose:     bool,
    // App-wide settings (backup-before-overwrite, completion sound) --
    // persisted separately from AppConfig, see state.rs::AppSettings.
    pub settings:      AppSettings,
    pub settings_open: bool,
    // Tag Order dialog's "show only active tags" filter -- a view
    // preference for that dialog, not config data, so it lives here rather
    // than in AppConfig (doesn't get saved/loaded with a job config).
    pub reorder_show_active_only: bool,
    // Tag Order dialog's button row height, measured via ui.scope() on the
    // previous frame -- used to size the tag list's scroll area so the
    // buttons always have guaranteed room below it, regardless of exact
    // button/font metrics. A hand-estimated constant undercounted the
    // real button height once already (assumed 24px; theme::btn_* is
    // actually 28px), so this measures the real thing instead of trying
    // to get every contributing pixel right by hand a second time.
    pub reorder_button_row_h: f32,
    // Table selection
    pub vol_sel:  Option<usize>,
    pub date_sel: Option<usize>,
    pub summ_sel: Option<usize>,
    pub meta_field_sel: Option<String>,
    // Dialog
    pub dialog: Option<Dialog>,
    // Path-picker
    pub pick_kind: Option<PathPick>,
    pub pick_rx:   Option<mpsc::Receiver<Option<PathBuf>>>,
    // Worker
    pub running:    bool,
    pub stop_flag:  Arc<AtomicBool>,
    pub worker_rx:  Option<mpsc::Receiver<WorkerMsg>>,
    pub ui_tx:      Option<mpsc::Sender<UiMsg>>,
    pub progress:   (usize, usize),
    pub disp_stats: DisplayStats,
    pub log:        Vec<LogEntry>,
    // Per-file log blocks, indexed by the file's position in the originally
    // sorted file list. Rendered in index order (skipping unfilled slots)
    // so completed files always display in numeric order regardless of
    // which thread finishes processing them first.
    pub file_slots: Vec<Option<Vec<LogEntry>>>,
    // The [DONE] footer + its separators, kept apart from file_slots so it
    // always renders after every file block instead of wherever it happened
    // to arrive chronologically.
    pub log_footer: Vec<LogEntry>,
    // Deferred run (set in dialogs, processed before rendering)
    pub pending_start: Option<(Vec<std::path::PathBuf>, std::collections::HashSet<String>, bool)>,
    // Autosave
    pub last_save:  std::time::Instant,
}

impl ComicInfoApp {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        theme::setup_style(&cc.egui_ctx);
        let mut app = Self {
            cfg:         AppConfig::default(),
            tab:         Tab::default(),
            sep_preview: String::new(),
            status:      "Ready.".to_string(),
            verbose:     false,
            settings:      AppSettings::default(),
            settings_open: false,
            reorder_show_active_only: false,
            reorder_button_row_h: 44.0, // generous first-frame guess; corrected next frame
            vol_sel:     None, date_sel: None, summ_sel: None, meta_field_sel: None,
            dialog:      None,
            pick_kind:   None, pick_rx: None,
            running:     false,
            stop_flag:   Arc::new(AtomicBool::new(false)),
            worker_rx:   None, ui_tx: None,
            progress:    (0, 0),
            disp_stats:  DisplayStats::default(),
            log:         Vec::new(),
            file_slots:  Vec::new(),
            log_footer:  Vec::new(),
            pending_start: None,
            last_save:   std::time::Instant::now(),
        };
        app.load_autosave();
        app.load_settings();
        // If the user has saved a preferred tag order, it should win over
        // AppConfig::default()'s hardcoded canonical order on a fresh
        // start (no autosave yet) -- same reasoning as reset_all() below.
        if let Some(order) = app.settings.preferred_tag_order.clone() {
            app.cfg.tag_order = order;
        }
        app.rebuild_sep_preview();
        app
    }

    // ── Settings (persisted separately from AppConfig / per-job configs) ──────
    fn save_settings(&self) {
        if let Ok(s) = serde_json::to_string_pretty(&self.settings) {
            let _ = std::fs::write(settings_path(), s);
        }
    }
    fn load_settings(&mut self) {
        if let Ok(data) = std::fs::read_to_string(settings_path()) {
            if let Ok(settings) = serde_json::from_str::<AppSettings>(&data) {
                self.settings = settings;
            }
        }
    }

    // ── Autosave ──────────────────────────────────────────────────────────────
    fn autosave(&self) {
        if let Ok(s) = serde_json::to_string_pretty(&self.cfg) {
            let _ = std::fs::write(autosave_path(), s);
        }
    }
    fn load_autosave(&mut self) {
        if let Ok(data) = std::fs::read_to_string(autosave_path()) {
            if let Ok(mut cfg) = serde_json::from_str::<AppConfig>(&data) {
                let loaded_version = cfg.config_version;
                cfg.config_version = CURRENT_CONFIG_VERSION;
                self.cfg = cfg;
                self.status = if loaded_version != CURRENT_CONFIG_VERSION {
                    "Session restored (from an older app version -- some fields may be reset).".to_string()
                } else {
                    "Session restored.".to_string()
                };
            }
        }
    }

    // ── Config ────────────────────────────────────────────────────────────────
    fn save_config(&self, path: &Path) {
        if let Ok(s) = serde_json::to_string_pretty(&self.cfg) { let _ = std::fs::write(path, s); }
    }
    fn load_config(&mut self, path: &Path) {
        if let Ok(data) = std::fs::read_to_string(path) {
            if let Ok(mut cfg) = serde_json::from_str::<AppConfig>(&data) {
                let loaded_version = cfg.config_version;
                cfg.config_version = CURRENT_CONFIG_VERSION;
                self.cfg = cfg;
                self.rebuild_sep_preview();
                let fname = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                self.status = format!("Loaded: {fname}");
                // A version mismatch means this config predates (or postdates)
                // a structural change to AppConfig -- serde's #[serde(default)]
                // already prevented a hard load failure, but fields that were
                // renamed/restructured since then won't have carried over.
                // Surface that explicitly rather than letting it look like
                // silently "lost" data with no explanation.
                if loaded_version < CURRENT_CONFIG_VERSION {
                    self.dialog = Some(Dialog::Notice(format!(
                        "'{fname}' was saved with an older version of this app \
                         (config v{loaded_version} vs current v{CURRENT_CONFIG_VERSION}).\n\n\
                         Some fields may not have carried over if the config \
                         format changed since then. Worth double-checking the \
                         Metadata tab before running."
                    )));
                } else if loaded_version > CURRENT_CONFIG_VERSION {
                    self.dialog = Some(Dialog::Notice(format!(
                        "'{fname}' was saved with a NEWER version of this app \
                         (config v{loaded_version} vs current v{CURRENT_CONFIG_VERSION}).\n\n\
                         Some fields may not load correctly. Consider updating \
                         the app if you run into issues."
                    )));
                }
            }
        }
    }
    fn import_meta(&mut self, path: &Path) {
        let Ok(data) = std::fs::read_to_string(path) else {
            self.dialog = Some(Dialog::Notice(format!(
                "Could not read file:\n{}", path.display()
            )));
            return;
        };

        let fname = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let ext   = path.extension().unwrap_or_default().to_string_lossy().to_lowercase();

        // Collect (field_label, value) pairs for the result dialog
        let mut imported: Vec<(String, String)> = Vec::new();

        let kv_pairs: Vec<(String, String)> = if ext == "py" {
            // ── Python file parser ────────────────────────────────────────
            // Handles:  CONSTANT_METADATA = { "Key": "Value", ... }
            //           SUMMARY = """..."""  or  SUMMARY = "..."
            let mut pairs = Vec::<(String, String)>::new();

            // ── Parse CONSTANT_METADATA dict ─────────────────────────────
            let mut in_dict = false;
            for line in data.lines() {
                let trimmed = line.trim();
                if !in_dict {
                    if trimmed.starts_with("CONSTANT_METADATA") && trimmed.contains('{') {
                        in_dict = true;
                    }
                    continue;
                }
                // End of dict
                if trimmed == "}" || trimmed == "}," {
                    break;
                }
                // Match lines like:  "Key": "Value with spaces",
                // Key has no embedded quotes; value may contain commas and spaces.
                // Separator is `": "` (3 chars: closing-quote, colon, space).
                if let Some(sep) = trimmed.find("\": \"") {
                    // Everything before sep is `"Key`  -> strip leading `"`
                    let key_raw = trimmed[..sep].trim().trim_start_matches('"');
                    // After sep we have: `"` + 3 chars(`": "`) = sep+4 starts the value
                    let rest = &trimmed[sep + 4..];
                    // Value ends at the LAST `"` on the line (before the trailing `,`)
                    if let Some(end_q) = rest.rfind('"') {
                        let val = rest[..end_q]
                            .replace("\\n", "\n")
                            .replace("\\\"", "\"");
                        pairs.push((key_raw.to_string(), val));
                    }
                }
            }

            // ── Parse SUMMARY ─────────────────────────────────────────────
            // Triple-quoted first
            if let Some(ts) = data.find("SUMMARY = \"\"\"") {
                let after = &data[ts + "SUMMARY = \"\"\"".len()..];
                if let Some(te) = after.find("\"\"\"") {
                    let s = after[..te].trim().to_string();
                    if !s.is_empty() { pairs.push(("Summary".to_string(), s)); }
                }
            } else if let Some(ss) = data.find("SUMMARY = \"") {
                let after = &data[ss + "SUMMARY = \"".len()..];
                if let Some(se) = after.find('"') {
                    let s = after[..se].replace("\\n", "\n");
                    if !s.is_empty() { pairs.push(("Summary".to_string(), s)); }
                }
            }

            // ── Parse rule tables ─────────────────────────────────────────
            // VOLUME_RULES / DATE_RULES / VOLUME_SUMMARY_RULES = [ (...), ... ]
            // Rules aren't simple key-value pairs, so they're applied
            // directly here rather than folded into `pairs`.
            let vol_rows  = Self::parse_py_tuple_block(&data, "VOLUME_RULES");
            let date_rows = Self::parse_py_tuple_block(&data, "DATE_RULES");
            let summ_rows = Self::parse_py_tuple_block(&data, "VOLUME_SUMMARY_RULES");
            if !vol_rows.is_empty() {
                imported.push(("Volume Rules".to_string(), format!("{} rule(s)", vol_rows.len())));
                self.cfg.volume_rules = vol_rows;
            }
            if !date_rows.is_empty() {
                imported.push(("Date Rules".to_string(), format!("{} rule(s)", date_rows.len())));
                self.cfg.date_rules = date_rows;
            }
            if !summ_rows.is_empty() {
                imported.push(("Summary Rules".to_string(), format!("{} rule(s)", summ_rows.len())));
                self.cfg.summ_rules = summ_rows;
            }

            pairs
        } else {
            // ── JSON file parser ──────────────────────────────────────────
            let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) else {
                self.dialog = Some(Dialog::Notice(
                    "Could not parse file as JSON.\nFor Python files use a .py extension.".to_string()
                ));
                return;
            };
            let Some(map) = json.as_object() else {
                self.dialog = Some(Dialog::Notice(
                    "No recognised metadata fields found in the file.\n\n                 Expected a Python file with CONSTANT_METADATA dict and/or SUMMARY,\n                 or a JSON object with ComicInfo field names.".to_string()
                ));
                return;
            };

            // Full app config? Import the structural settings (paths,
            // rules, prefix/separator/mode, ...) directly, then fall
            // through to the flat-field scan below on the SAME map --
            // this used to return immediately here with one generic
            // "Config loaded" line, silently dropping any flat metadata
            // field that pre-dates the dynamic metadata_fields list
            // (series, writer, rating, ...), since AppConfig no longer
            // has a named struct field for any of them.
            if map.contains_key("folder") || map.contains_key("prefix_mode") {
                match serde_json::from_str::<AppConfig>(&data) {
                    Ok(cfg) => {
                        self.cfg = cfg;
                        self.rebuild_sep_preview();
                        for (label, count) in [
                            ("Volume Rules",  self.cfg.volume_rules.len()),
                            ("Date Rules",    self.cfg.date_rules.len()),
                            ("Summary Rules", self.cfg.summ_rules.len()),
                        ] {
                            if count > 0 {
                                imported.push((label.to_string(), format!("{count} rule(s)")));
                            }
                        }
                        // Custom (non-standard) fields the old tool had no
                        // named slot for. Accepts ["Tag","Value"] pairs or
                        // {"tag":...,"value":...} objects; anything else is
                        // reported rather than silently dropped.
                        if let Some(arr) = map.get("custom_fields").and_then(|v| v.as_array()) {
                            let mut unrecognised = 0;
                            for entry in arr {
                                let pair = match entry {
                                    serde_json::Value::Array(a) if a.len() == 2 =>
                                        a[0].as_str().zip(a[1].as_str())
                                            .map(|(t, v)| (t.to_string(), v.to_string())),
                                    serde_json::Value::Object(o) => {
                                        o.get("tag").and_then(|t| t.as_str())
                                            .zip(o.get("value").and_then(|v| v.as_str()))
                                            .map(|(t, v)| (t.to_string(), v.to_string()))
                                            .or_else(|| if o.len() == 1 {
                                                o.iter().next()
                                                    .and_then(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                                            } else { None })
                                    }
                                    _ => None,
                                };
                                match pair {
                                    Some((tag, val)) => {
                                        let display_val = if val.len() > 80 { format!("{}...", &val[..77]) } else { val.clone() };
                                        self.set_metadata_field(&tag, val);
                                        imported.push((tag, display_val));
                                    }
                                    None => unrecognised += 1,
                                }
                            }
                            if unrecognised > 0 {
                                imported.push((
                                    "Custom Fields".to_string(),
                                    format!("{unrecognised} entr{} in an unrecognised format -- check manually",
                                        if unrecognised == 1 { "y" } else { "ies" }),
                                ));
                            }
                        }
                    }
                    Err(e) => {
                        self.dialog = Some(Dialog::Notice(format!(
                            "Recognised this as a full config file, but couldn't load it:\n{e}"
                        )));
                        return;
                    }
                }
            }

            // Flat metadata fields -- also runs after a full-config load
            // above, so legacy per-field names that were never AppConfig
            // struct fields (and so were invisible to the deserialize
            // above) still make it into metadata_fields.
            map.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        };

        // ── Apply extracted key-value pairs to config ─────────────────────
        for (key, val) in &kv_pairs {
            let display_val = if val.len() > 80 {
                format!("{}...", &val[..77])
            } else {
                val.clone()
            };

            if key.eq_ignore_ascii_case("Summary") {
                self.cfg.summary = val.clone();
                imported.push(("Summary".to_string(), display_val));
                continue;
            }

            // A key that's already a real ComicInfo tag takes priority;
            // legacy_field_alias only covers names the OLD app used that
            // either never were real tags ("Rating") or changed shape
            // (snake_case -> PascalCase, e.g. "alt_series").
            let resolved = field_spec(key).map(|s| (s.tag, false))
                .or_else(|| Self::legacy_field_alias(key));

            if let Some((tag, is_legacy_rating)) = resolved {
                if is_legacy_rating {
                    // The old app's "Rating" field was always a 1-10
                    // score -- there's no such tag in the ComicInfo
                    // schema. The real field is CommunityRating on a 0-5
                    // scale, so this also switches on the 1-10 input
                    // scale to match how the value was actually entered.
                    self.cfg.community_rating_10_scale = true;
                }
                self.set_metadata_field(tag, val.clone());
                let label = field_spec(tag).map(|s| s.label).unwrap_or(tag);
                imported.push((label.to_string(), display_val));
            }
            // else: not a recognised ComicInfo field or legacy alias -- silently skip.
        }

        if imported.is_empty() {
            self.dialog = Some(Dialog::Notice(
                "No recognised metadata fields found in the file.\n\n                 Expected a Python file with CONSTANT_METADATA dict and/or SUMMARY,\n                 or a JSON object with ComicInfo field names.".to_string()
            ));
        } else {
            self.status = format!("Imported {} field(s) from {fname}", imported.len());
            self.dialog = Some(Dialog::ImportResult { filename: fname, items: imported });
        }
    }

    // Legacy field names used by the old Python/tkinter tool's JSON export,
    // mapped to (real_comicinfo_tag, is_legacy_rating). is_legacy_rating
    // marks the one case ("Rating"/"rating") that needs a side effect
    // beyond the rename -- see the call site in import_meta.
    fn legacy_field_alias(key: &str) -> Option<(&'static str, bool)> {
        match key.to_lowercase().as_str() {
            "series"     => Some(("Series", false)),
            "writer"     => Some(("Writer", false)),
            "penciller"  => Some(("Penciller", false)),
            "publisher"  => Some(("Publisher", false)),
            "language"   => Some(("LanguageISO", false)),
            "alt_series" => Some(("AlternateSeries", false)),
            "web"        => Some(("Web", false)),
            "genre"      => Some(("Genre", false)),
            "count"      => Some(("Count", false)),
            "year"       => Some(("Year", false)),
            "month"      => Some(("Month", false)),
            "day"        => Some(("Day", false)),
            "rating"     => Some(("CommunityRating", true)),
            _ => None,
        }
    }

    // Extracts rows from a Python list-of-tuples block:
    //   VOLUME_RULES = [
    //       # optional comment lines, skipped
    //       (1, 17, "1"),
    //       (18, 34, "3"),
    //   ]
    // Each tuple must be on its own line. Comment lines (starting with #)
    // and blank lines between tuples are skipped. Stops at the first line
    // starting with `]`.
    fn parse_py_tuple_block(data: &str, marker: &str) -> Vec<Vec<String>> {
        let mut rows = Vec::new();
        let Some(start) = data.find(marker) else { return rows; };
        let after = &data[start..];
        let Some(bracket) = after.find('[') else { return rows; };
        let body_start = start + bracket + 1;

        for line in data[body_start..].lines() {
            let trimmed = line.trim();
            if trimmed.starts_with(']') { break; }
            if trimmed.is_empty() || trimmed.starts_with('#') { continue; }
            let Some(open) = trimmed.find('(') else { continue; };
            let Some(close) = trimmed.rfind(')') else { continue; };
            if close <= open { continue; }
            let fields = Self::parse_py_tuple_fields(&trimmed[open + 1..close]);
            if !fields.is_empty() {
                rows.push(fields);
            }
        }
        rows
    }

    // Splits the inside of a Python tuple literal into its fields, e.g.
    // `1,  17, "1"` -> ["1", "17", "1"]. Handles a mix of bare tokens
    // (numbers) and double-quoted strings (which may contain commas,
    // apostrophes, or other punctuation) -- commas inside quotes are not
    // treated as field separators. Quotes are stripped from the result;
    // bare tokens are used as-is (trimmed).
    fn parse_py_tuple_fields(inner: &str) -> Vec<String> {
        let mut fields = Vec::new();
        let mut current = String::new();
        let mut in_quotes = false;
        let mut chars = inner.chars().peekable();
        while let Some(c) = chars.next() {
            if in_quotes {
                match c {
                    '\\' => {
                        if let Some(&next) = chars.peek() {
                            match next {
                                'n'  => { current.push('\n'); chars.next(); }
                                '"'  => { current.push('"');  chars.next(); }
                                '\\' => { current.push('\\'); chars.next(); }
                                _    => current.push(c),
                            }
                        } else {
                            current.push(c);
                        }
                    }
                    '"' => { in_quotes = false; }
                    _   => current.push(c),
                }
            } else {
                match c {
                    '"' => { in_quotes = true; }
                    ',' => {
                        fields.push(current.trim().to_string());
                        current.clear();
                    }
                    _ => current.push(c),
                }
            }
        }
        let last = current.trim();
        if !last.is_empty() {
            fields.push(last.to_string());
        }
        fields
    }

    fn reset_all(&mut self) {
        self.cfg = AppConfig::default();
        // AppConfig::default() ships with example Volume/Date rules so a
        // first-time user has a worked example to learn the format from.
        // "Reset All" is an explicit user action expecting a truly blank
        // slate, not a reappearance of placeholder data they never entered
        // themselves -- so clear those out here specifically.
        self.cfg.volume_rules.clear();
        self.cfg.date_rules.clear();
        // A user-saved preferred tag order (Tag Order dialog -> "Set as
        // Default") is a deliberate standing preference, not per-job
        // config -- Reset All shouldn't discard it any more than it
        // discards AppSettings' other toggles.
        if let Some(order) = self.settings.preferred_tag_order.clone() {
            self.cfg.tag_order = order;
        }
        self.rebuild_sep_preview();
        self.status = "Reset to defaults.".to_string();
    }

    /// Opens `path` in the OS's default file manager. Uses a platform-
    /// specific shell-out instead of adding a new crate dependency just
    /// for this one action.
    fn open_in_file_manager(path: &Path) {
        #[cfg(target_os = "windows")]
        { let _ = std::process::Command::new("explorer").arg(path).spawn(); }
        #[cfg(target_os = "macos")]
        { let _ = std::process::Command::new("open").arg(path).spawn(); }
        #[cfg(target_os = "linux")]
        { let _ = std::process::Command::new("xdg-open").arg(path).spawn(); }
    }

    fn smart_filename(&self) -> String {
        let src = if !self.cfg.folder.is_empty() {
            Path::new(&self.cfg.folder).file_name().unwrap_or_default().to_string_lossy().into_owned()
        } else {
            self.cfg.metadata_fields.iter()
                .find(|(t, _)| t == "Series")
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        let slug: String = src.chars()
            .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '_' })
            .collect::<String>().split("__").collect::<Vec<_>>().join("_").to_lowercase();
        if slug.is_empty() { "metadata_gui.json".to_string() } else { format!("{slug}_gui.json") }
    }

    // ── Sep preview ───────────────────────────────────────────────────────────
    fn rebuild_sep_preview(&mut self) {
        let mode   = self.cfg.prefix_mode.as_str().to_string();
        let mut pfx = "Episode".to_string();
        let mut num = "1".to_string();
        if !self.cfg.folder.is_empty() {
            if let Ok(entries) = std::fs::read_dir(&self.cfg.folder) {
                let mut cbzs: Vec<String> = entries.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .filter(|n| n.to_lowercase().ends_with(".cbz")).collect();
                cbzs.sort_by(|a, b| natural_sort_key(a).cmp(&natural_sort_key(b)));
                if let Some(s) = cbzs.first() {
                    pfx = match detect_file_type(s) {
                        "volume" => "Volume", "chapter" => "Chapter", _ => "Episode"
                    }.to_string();
                    if let Some(m) = regex::Regex::new(r"\d+(?:\.\d+)?").unwrap().find(s) {
                        num = m.as_str().to_string();
                    }
                }
            }
        }
        let chosen = match mode.as_str() {
            "custom"  => if self.cfg.custom_pfx.is_empty() { "Custom".into() } else { self.cfg.custom_pfx.clone() },
            "episode" => "Episode".into(),
            "chapter" => "Chapter".into(),
            "volume"  => "Volume".into(),
            _         => pfx,
        };
        // Zero-padding, matching worker.rs's exact behavior: skipped for
        // decimal numbers (e.g. "5.5" stays as-is), applied to whole
        // numbers using pad_width. Deliberately does NOT reflect
        // worker.rs's separate auto-detected padding width (inferred from
        // existing filenames in the folder) -- that's a background
        // heuristic the Processing tab has no visible control for, and
        // making the preview depend on it would make it track something
        // other than the Zero-Padding toggle/width sitting right next to
        // this preview.
        if self.cfg.zero_pad && !num.contains('.') {
            if let Ok(n) = num.parse::<u64>() {
                num = format!("{n:0>width$}", width = self.cfg.pad_width);
            }
        }
        self.sep_preview = if self.cfg.csep_on && !self.cfg.csep.is_empty() {
            format!("{chosen} {num} {} My Title", self.cfg.csep.trim())
        } else {
            let s = if chosen.to_lowercase().contains("chapter") || chosen.to_lowercase().contains("volume")
                { ": " } else { " - " };
            format!("{chosen} {num}{s}My Title")
        };
    }

    // ── File picker ───────────────────────────────────────────────────────────
    fn start_pick(&mut self, kind: PathPick) {
        if self.pick_rx.is_some() { return; }
        let (tx, rx) = mpsc::channel::<Option<PathBuf>>();
        let k = kind.clone();
        std::thread::spawn(move || {
            let res = match &k {
                PathPick::Folder | PathPick::OutputPath => rfd::FileDialog::new().pick_folder(),
                PathPick::ImportMeta |
                PathPick::TitlesJson | PathPick::DateJson =>
                    rfd::FileDialog::new().add_filter("JSON / Python", &["json","py"]).pick_file(),
                PathPick::LoadConfig =>
                    rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file(),
                PathPick::SaveConfig(name) =>
                    rfd::FileDialog::new().add_filter("JSON", &["json"]).set_file_name(name).save_file(),
            };
            let _ = tx.send(res);
        });
        self.pick_kind = Some(kind);
        self.pick_rx   = Some(rx);
    }
    fn poll_pick(&mut self) {
        let result = match &self.pick_rx {
            Some(rx) => match rx.try_recv() {
                Ok(r)  => r,
                Err(mpsc::TryRecvError::Empty)       => return,
                Err(mpsc::TryRecvError::Disconnected) => { self.pick_rx = None; return; }
            },
            None => return,
        };
        self.pick_rx = None;
        let Some(path) = result else { self.pick_kind = None; return; };
        match self.pick_kind.take() {
            Some(PathPick::Folder)    => { self.cfg.folder = path.to_string_lossy().into(); self.rebuild_sep_preview(); }
            Some(PathPick::TitlesJson) => self.cfg.titles_json = path.to_string_lossy().into(),
            Some(PathPick::DateJson)  => self.cfg.date_json = path.to_string_lossy().into(),
            Some(PathPick::LoadConfig) => self.load_config(&path),
            Some(PathPick::SaveConfig(_)) => {
                self.save_config(&path);
                self.status = format!("Saved: {}", path.file_name().unwrap_or_default().to_string_lossy());
            }
            Some(PathPick::ImportMeta) => self.import_meta(&path),
            Some(PathPick::OutputPath) => self.cfg.output_path = path.to_string_lossy().into(),
            None => {}
        }
    }

    // ── Worker polling ────────────────────────────────────────────────────────
    fn poll_worker(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.worker_rx else { return };
        loop {
            match rx.try_recv() {
                Ok(WorkerMsg::Log { text, level }) => {
                    self.log.push(LogEntry { text, level });
                    ctx.request_repaint();
                }
                Ok(WorkerMsg::LogBatch { idx, lines }) => {
                    // Fill this file's reserved slot so it renders in
                    // correct numeric order, regardless of which thread
                    // actually finished processing it first.
                    let entries: Vec<LogEntry> = lines.into_iter()
                        .map(|(text, level)| LogEntry { text, level })
                        .collect();
                    if idx < self.file_slots.len() {
                        self.file_slots[idx] = Some(entries);
                    } else {
                        // Defensive fallback (shouldn't happen): just append.
                        self.log.extend(entries);
                    }
                    ctx.request_repaint();
                }
                Ok(WorkerMsg::Progress { done, total }) => {
                    self.progress = (done, total);
                    ctx.request_repaint();
                }
                Ok(WorkerMsg::Stats { stats }) => {
                    self.disp_stats = DisplayStats {
                        total: stats.total, processed: stats.processed,
                        renamed: stats.renamed, skipped: stats.rename_skipped,
                        xml: stats.xml_updated, errors: stats.errors,
                    };
                }
                Ok(WorkerMsg::DecimalRequest { filename, raw_title }) => {
                    self.dialog = Some(Dialog::Decimal(DecimalState {
                        filename, raw_title, choice: 1, custom: String::new(),
                    }));
                    ctx.request_repaint();
                    return; // pause polling until dialog answered
                }
                Ok(WorkerMsg::Done { stats }) => {
                    self.running = false;
                    if self.settings.play_sound_on_completion {
                        play_completion_sound();
                    }
                    self.disp_stats = DisplayStats {
                        total: stats.total, processed: stats.processed,
                        renamed: stats.renamed, skipped: stats.rename_skipped,
                        xml: stats.xml_updated, errors: stats.errors,
                    };
                    let sep = "-".repeat(60);
                    let ts  = chrono::Local::now().format("%H:%M:%S");
                    self.log_footer.push(LogEntry { text: sep.clone(), level: LogLevel::Sep });
                    let (msg, lvl, st) = if stats.errors > 0 {
                        (format!("  [DONE] {ts}  -  {} errors", stats.errors), LogLevel::Warn,
                         format!("Done  -  {} error(s).", stats.errors))
                    } else {
                        (format!("  [DONE] {ts}  -  {} processed  -  {} renamed  -  0 errors",
                                 stats.processed, stats.renamed), LogLevel::Ok, "Done.".to_string())
                    };
                    self.log_footer.push(LogEntry { text: msg, level: lvl });
                    self.log_footer.push(LogEntry { text: sep, level: LogLevel::Sep });
                    self.status = st;
                    ctx.request_repaint();
                    return;
                }
                Err(mpsc::TryRecvError::Empty)        => break,
                Err(mpsc::TryRecvError::Disconnected) => { self.running = false; break; }
            }
        }
    }

    // ── Build WorkerConfig ────────────────────────────────────────────────────
    fn make_worker_cfg(
        &self,
        processed_files: HashSet<String>,
        resume_mode:     bool,
        finale_index:    Option<usize>,
        finale_number:   Option<String>,
        final_ch_mode:   String,
        progress_file:   PathBuf,
        error_log_file:  PathBuf,
    ) -> WorkerConfig {
        // Loaded once and cloned below for both chapter_titles and
        // volume_titles (see AppConfig::titles_json) -- avoids reading
        // the same file from disk twice per run.
        let titles = safe_json_load(&self.cfg.titles_json);
        WorkerConfig {
            dry_run: self.cfg.dry_run,
            write_new_cbz: self.cfg.write_new_cbz,
            output_same_path: self.cfg.output_same_path,
            output_path: self.cfg.output_path.clone(),
            backup_before_overwrite: self.settings.backup_before_overwrite,
            use_vol: self.cfg.use_vol, use_vol_date: self.cfg.use_vol_date, use_vol_summ: self.cfg.use_vol_summ,
            prefix_mode: self.cfg.prefix_mode.as_str().to_string(),
            custom_pfx:  self.cfg.custom_pfx.clone(),
            post_finale_mode: match self.cfg.post_finale { PostFinale::Strip => "strip", PostFinale::Keep => "keep" }.to_string(),
            use_csep: self.cfg.csep_on, csep: self.cfg.csep.clone(),
            zero_pad: self.cfg.zero_pad, pad_width: self.cfg.pad_width,
            metadata_fields: self.cfg.metadata_fields.iter().cloned().collect(),
            community_rating_10_scale: self.cfg.community_rating_10_scale,
            tag_order: self.cfg.tag_order.clone(),
            summary: self.cfg.summary.clone(),
            volume_rules:  self.cfg.volume_rules.clone(),
            date_rules:    self.cfg.date_rules.clone(),
            summ_rules:    self.cfg.summ_rules.clone(),
            // Both use the same merged data (see AppConfig::titles_json)
            // -- worker.rs still branches on which map to consult
            // per-file based on numbering mode, this only unifies where
            // the data comes from.
            chapter_titles: titles.clone(),
            volume_titles:  titles,
            dates_json:     safe_json_load(&self.cfg.date_json),
            max_workers: self.cfg.workers,
            processed_files, resume_mode,
            finale_index, finale_number, final_ch_mode,
            progress_file, error_log_file,
            verbose: self.verbose,
        }
    }

    // ── Kick off run ──────────────────────────────────────────────────────────
    fn start_worker(
        &mut self, cbz_files: Vec<PathBuf>, processed: HashSet<String>,
        resume: bool, fin_idx: Option<usize>, fin_num: Option<String>, fin_mode: String,
    ) {
        let folder_name = Path::new(&self.cfg.folder)
            .canonicalize().unwrap_or_else(|_| PathBuf::from(&self.cfg.folder))
            .file_name().unwrap_or_default().to_string_lossy().to_string();
        let log_dir = std::env::current_dir().unwrap_or_default().join("logs");
        let _ = std::fs::create_dir_all(&log_dir);

        let wcfg = self.make_worker_cfg(
            processed, resume, fin_idx, fin_num,
            fin_mode,
            log_dir.join(format!("{folder_name}_progress.log")),
            log_dir.join(format!("{folder_name}_errors.log")),
        );

        let (wtx, wrx) = mpsc::channel::<WorkerMsg>();
        let (utx, urx) = mpsc::channel::<UiMsg>();
        self.worker_rx = Some(wrx);
        self.ui_tx     = Some(utx);
        self.running   = true;
        self.progress  = (0, cbz_files.len());
        self.file_slots = vec![None; cbz_files.len()];
        self.log_footer.clear();

        use std::sync::atomic::Ordering;
        self.stop_flag.store(false, Ordering::Relaxed);
        let stop = self.stop_flag.clone();

        if self.cfg.dry_run {
            self.log.push(LogEntry { text: "  [DRY RUN] -- no files will be modified".to_string(), level: LogLevel::Warn });
        }
        std::thread::spawn(move || crate::worker::run(cbz_files, wcfg, wtx, urx, stop));

        self.tab    = Tab::Run;
        self.status = "Processing...".to_string();
    }

    // ── "Start" button ────────────────────────────────────────────────────────
    /// Lists constant-metadata fields the user has added that are still
    /// empty and would render as blank tags in every generated ComicInfo.xml.
    /// Year/Month/Day are skipped when volume-date rules are active, since
    /// those rules supply the actual date per-file -- warning about them
    /// would be a false positive for anyone relying on per-volume dates.
    /// Fields the user never added in the first place are never flagged --
    /// choosing not to include a field IS the "this is optional" signal now.
    fn empty_metadata_fields(&self) -> Vec<String> {
        let mut empty = Vec::new();
        for (tag, val) in &self.cfg.metadata_fields {
            if self.cfg.use_vol_date && matches!(tag.as_str(), "Year" | "Month" | "Day") {
                continue;
            }
            if val.trim().is_empty() {
                let label = field_spec(tag).map(|s| s.label).unwrap_or(tag.as_str());
                empty.push(label.to_string());
            }
        }
        if self.cfg.summary.trim().is_empty() {
            empty.push("Summary".to_string());
        }
        empty
    }

    /// Inserts or updates a tag in metadata_fields (add-if-missing semantics).
    fn set_metadata_field(&mut self, tag: &str, val: String) {
        if let Some(entry) = self.cfg.metadata_fields.iter_mut().find(|(t, _)| t == tag) {
            entry.1 = val;
        } else {
            self.cfg.metadata_fields.push((tag.to_string(), val));
        }
    }

    fn on_start(&mut self) {
        if self.running { return; }
        let folder = self.cfg.folder.trim().to_string();
        if folder.is_empty() || !Path::new(&folder).is_dir() {
            self.dialog = Some(Dialog::Notice("Set a valid CBZ folder in Paths & Config first.".to_string()));
            self.tab    = Tab::Paths;
            return;
        }
        let mut cbzs: Vec<PathBuf> = std::fs::read_dir(&folder)
            .map(|e| e.filter_map(|e| e.ok()).map(|e| e.path())
                .filter(|p| p.extension().map_or(false, |x| x.eq_ignore_ascii_case("cbz")))
                .collect())
            .unwrap_or_default();
        cbzs.sort_by(|a, b| {
            let an = a.file_name().unwrap_or_default().to_string_lossy().to_string();
            let bn = b.file_name().unwrap_or_default().to_string_lossy().to_string();
            natural_sort_key(&an).cmp(&natural_sort_key(&bn))
        });
        if cbzs.is_empty() {
            self.dialog = Some(Dialog::Notice("No .cbz files found in the selected folder.".to_string()));
            return;
        }

        let empty_fields = self.empty_metadata_fields();
        if !empty_fields.is_empty() {
            self.dialog = Some(Dialog::EmptyFieldsWarning { fields: empty_fields, cbzs });
            return;
        }

        self.continue_start(cbzs);
    }

    /// Resume-detection + finale-detection, continuing on to start_worker.
    /// Split out from on_start so the EmptyFieldsWarning dialog's "Continue
    /// Anyway" button can resume this same flow after the user confirms.
    fn continue_start(&mut self, cbzs: Vec<PathBuf>) {
        let folder = self.cfg.folder.trim().to_string();
        // Check for resume
        let fname = Path::new(&folder).canonicalize().unwrap_or_default()
            .file_name().unwrap_or_default().to_string_lossy().to_string();
        let pf = std::env::current_dir().unwrap_or_default().join("logs").join(format!("{fname}_progress.log"));
        if pf.exists() {
            let done: HashSet<String> = std::fs::read_to_string(&pf).unwrap_or_default()
                .lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
            if !done.is_empty() {
                let cnt = done.len();
                self.dialog = Some(Dialog::ResumeSession { cbz_files: cbzs, processed_set: done, count: cnt });
                return;
            }
        }
        self.check_finale(cbzs, HashSet::new(), false);
    }

    fn check_finale(&mut self, cbzs: Vec<PathBuf>, done: HashSet<String>, resume: bool) {
        let titles = safe_json_load(&self.cfg.titles_json);
        let mut nums: Vec<(f64, usize, String)> = Vec::new();
        for (i, p) in cbzs.iter().enumerate() {
            let n = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            if let Some(m) = regex::Regex::new(r"\d+(?:\.\d+)?").unwrap().find(&n) {
                let s = m.as_str().to_string();
                if let Ok(f) = s.parse::<f64>() { nums.push((f, i, s)); }
            }
        }
        let int_nums: Vec<_> = nums.iter().filter(|(_, _, s)| !s.contains('.')).cloned().collect();
        let src = if !int_nums.is_empty() { &int_nums } else { &nums };
        let finale = src.iter().max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        if let Some((_, idx, num)) = finale {
            if titles.contains_key(num.as_str()) {
                self.dialog = Some(Dialog::FinalChapter {
                    cbz_files: cbzs, processed_set: done, resume,
                    finale_num: num.clone(), finale_idx: *idx,
                });
                return;
            }
        }
        let (fi, fn_) = finale.map(|(_, i, n)| (Some(*i), Some(n.clone()))).unwrap_or((None, None));
        self.start_worker(cbzs, done, resume, fi, fn_, "normal".to_string());
    }

    // ── Dialogs ───────────────────────────────────────────────────────────────
    fn render_dialogs(&mut self, ctx: &egui::Context) {
        let Some(dlg) = self.dialog.take() else { return };
        match dlg {
            Dialog::EditRule(mut s) => {
                let mut saved = false; let mut cancelled = false;
                // "Same as Start" only makes sense for Date/Summary rules
                // (their range is Vol Start -> Vol End). Volume rules map
                // a CHAPTER range to a single volume number -- a
                // different shape entirely, no Start==End concept here.
                let same_start_end_applicable = matches!(s.target, RuleTarget::Date | RuleTarget::Summary);

                // Validated in terms of whichever field(s) are actually
                // showing: with Same ticked, only Vol No. (values[0])
                // needs a value -- values[1] is a mirrored copy, not
                // something the person can see or edit right now, so it
                // shouldn't gate Save or appear in the error message.
                let range_valid = if same_start_end_applicable && s.same_start_end {
                    s.values.get(0).map_or(false, |v| !v.trim().is_empty() && v.trim().parse::<f64>().is_ok())
                } else {
                    s.values.get(0).map_or(false, |v| !v.trim().is_empty() && v.trim().parse::<f64>().is_ok())
                        && s.values.get(1).map_or(false, |v| !v.trim().is_empty() && v.trim().parse::<f64>().is_ok())
                };

                egui::Window::new(if s.is_new { "Add Rule" } else { "Edit Rule" })
                    .resizable(true).collapsible(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        if same_start_end_applicable {
                            // s.same_start_end is real, persistent state
                            // on RuleEditState -- NOT re-derived each
                            // frame from whether values[0]==values[1] and
                            // non-empty. That derivation was the actual
                            // bug: ticking the checkbox with a blank Vol
                            // Start did nothing, since "" wasn't allowed
                            // to count as "same" (to avoid two blank
                            // fields defaulting to looking pre-merged),
                            // which meant the checkbox's own ticked state
                            // and the layout it should control were
                            // silently out of sync. The checkbox now
                            // drives the layout directly, unconditionally.
                            if ui.checkbox(&mut s.same_start_end, "Vol Start and Vol End are the same").changed() && s.same_start_end {
                                // Ticking it immediately syncs End to
                                // Start's current value (even if that's
                                // blank), rather than waiting for the
                                // next edit to Start.
                                if let Some(start_val) = s.values.get(0).cloned() {
                                    if let Some(end_val) = s.values.get_mut(1) { *end_val = start_val; }
                                }
                            }
                            ui.add_space(6.0);
                        }
                        egui::Grid::new("re_g").num_columns(2).spacing([8.0,6.0]).show(ui, |ui| {
                            for (i, lbl) in s.labels.iter().enumerate() {
                                // With Same ticked, Start's row is relabeled
                                // "Vol No." and End's row is skipped
                                // entirely -- one field instead of two,
                                // rather than showing both with End
                                // disabled/greyed (which would still look
                                // like 2 fields, just one of them inert).
                                if s.same_start_end && i == 1 { continue; }
                                let display_label = if s.same_start_end && i == 0 { "Vol No." } else { lbl.as_str() };
                                ui.label(RichText::new(display_label).color(theme::TXT));
                                if lbl.to_lowercase().contains("summary") {
                                    ui.add(egui::TextEdit::multiline(&mut s.values[i]).desired_rows(4).desired_width(420.0));
                                } else {
                                    let r = ui.add(egui::TextEdit::singleline(&mut s.values[i]).desired_width(280.0));
                                    // Keep End mirrored to Start live while
                                    // Same is ticked, so a mid-edit Save
                                    // (or just re-reading the fields
                                    // afterward) never sees them diverge.
                                    if s.same_start_end && i == 0 && r.changed() {
                                        let start_val = s.values[0].clone();
                                        if let Some(end_val) = s.values.get_mut(1) { *end_val = start_val; }
                                    }
                                }
                                ui.end_row();
                            }
                        });
                        if !range_valid {
                            ui.add_space(4.0);
                            let msg = if same_start_end_applicable && s.same_start_end {
                                "Vol No. is required and must be a number.".to_string()
                            } else {
                                format!(
                                    "{} and {} are required and must be numbers.",
                                    s.labels.first().map(String::as_str).unwrap_or("Start"),
                                    s.labels.get(1).map(String::as_str).unwrap_or("End"),
                                )
                            };
                            ui.label(RichText::new(msg).color(theme::TERR).size(11.0));
                        }
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.add(theme::btn_primary("  Save  ")).clicked() && range_valid {
                                saved = true;
                            }
                            if ui.add(theme::btn_secondary("  Cancel  ")).clicked() { cancelled = true; }
                        });
                    });
                if saved {
                    let vals = s.values.clone();
                    match (s.target, s.row_idx) {
                        (RuleTarget::Volume,      None)    => self.cfg.volume_rules.push(vals),
                        (RuleTarget::Volume,      Some(i)) => { if i < self.cfg.volume_rules.len() { self.cfg.volume_rules[i] = vals; } }
                        (RuleTarget::Date,        None)    => self.cfg.date_rules.push(vals),
                        (RuleTarget::Date,        Some(i)) => { if i < self.cfg.date_rules.len() { self.cfg.date_rules[i] = vals; } }
                        (RuleTarget::Summary,     None)    => self.cfg.summ_rules.push(vals),
                        (RuleTarget::Summary,     Some(i)) => { if i < self.cfg.summ_rules.len() { self.cfg.summ_rules[i] = vals; } }
                    }
                } else if !cancelled {
                    // Also covers Save being clicked while range_valid was
                    // false -- the dialog simply stays open, same as
                    // clicking neither button, so the person can fix the
                    // fields and try again rather than losing their input.
                    self.dialog = Some(Dialog::EditRule(s));
                }
            }

            Dialog::Decimal(mut ds) => {
                let mut ok = false;
                egui::Window::new("Decimal Chapter")
                    .resizable(false).collapsible(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label(RichText::new("Decimal Chapter Detected").color(theme::TWARN).strong().size(13.0));
                        ui.separator();
                        ui.label(RichText::new(format!("File:  {}", ds.filename)).color(theme::TDIM).size(11.0));
                        ui.label(RichText::new(format!("Title: {}", ds.raw_title)).color(theme::TXT));
                        ui.add_space(6.0);
                        let rt = ds.raw_title.clone();
                        for (v, lbl) in [(1u8, rt.as_str()), (2,"Bonus Manga"), (3,"Bonus Chapter"), (4,"Extra Chapter"), (5,"Custom ->")] {
                            ui.radio_value(&mut ds.choice, v, lbl);
                        }
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("   Prefix:").color(theme::TDIM));
                            ui.add_enabled(ds.choice==5, egui::TextEdit::singleline(&mut ds.custom).desired_width(180.0));
                        });
                        ui.add_space(6.0);
                        if ui.add(theme::btn_primary("  Confirm  ")).clicked() { ok = true; }
                    });
                if ok {
                    let result = match ds.choice {
                        2 => format!("Bonus Manga: {}", ds.raw_title),
                        3 => format!("Bonus Chapter: {}", ds.raw_title),
                        4 => format!("Extra Chapter: {}", ds.raw_title),
                        5 => { let p = ds.custom.trim().trim_end_matches(':');
                               if p.is_empty() { ds.raw_title.clone() } else { format!("{p}: {}", ds.raw_title) } }
                        _ => ds.raw_title.clone(),
                    };
                    if let Some(utx) = &self.ui_tx { let _ = utx.send(UiMsg::DecimalResponse { result }); }
                } else { self.dialog = Some(Dialog::Decimal(ds)); }
            }

            Dialog::ResumeSession { cbz_files, processed_set, count } => {
                let mut choice = 0i8;
                egui::Window::new("Previous Session Found")
                    .resizable(false).collapsible(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0,0.0])
                    .show(ctx, |ui| {
                        ui.label(format!("{count} files already processed in a previous run."));
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.add(theme::btn_primary("Resume")).clicked() { choice=1; }
                            ui.add_space(4.0);
                            if ui.add(theme::btn_secondary("Start Fresh")).clicked() { choice=2; }
                            ui.add_space(4.0);
                            if ui.add(egui::Button::new("Cancel").fill(theme::SURF3)).clicked() { choice=-1; }
                        });
                    });
                match choice {
                    1 => { self.pending_start = Some((cbz_files, processed_set, true)); }
                    2 => {
                        let fname = Path::new(&self.cfg.folder).canonicalize().unwrap_or_default()
                            .file_name().unwrap_or_default().to_string_lossy().to_string();
                        let pf = std::env::current_dir().unwrap_or_default().join("logs").join(format!("{fname}_progress.log"));
                        let _ = std::fs::write(&pf, "");
                        self.log.push(LogEntry { text: "[Fresh start]".to_string(), level: LogLevel::Dim });
                        self.pending_start = Some((cbz_files, HashSet::new(), false));
                    }
                    -1 => {}
                    _  => { self.dialog = Some(Dialog::ResumeSession { cbz_files, processed_set, count }); }
                }
            }

            Dialog::FinalChapter { cbz_files, processed_set, resume, finale_num, finale_idx } => {
                let mut choice = 0i8;
                egui::Window::new("Final Chapter Detected")
                    .resizable(false).collapsible(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0,0.0])
                    .show(ctx, |ui| {
                        ui.label(RichText::new(format!("Chapter {finale_num} is the last chapter.")).strong());
                        ui.add_space(6.0);
                        ui.label("Format as  \"Final Chapter: <title>\"?");
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.add(theme::btn_primary("Yes  -  Final Chapter")).clicked() { choice=1; }
                            ui.add_space(4.0);
                            if ui.add(theme::btn_secondary("No  -  Normal")).clicked() { choice=2; }
                        });
                    });
                if choice != 0 {
                    let mode = if choice==1 { "final" } else { "normal" };
                    self.start_worker(cbz_files, processed_set, resume, Some(finale_idx), Some(finale_num), mode.to_string());
                } else {
                    self.dialog = Some(Dialog::FinalChapter { cbz_files, processed_set, resume, finale_num, finale_idx });
                }
            }

            Dialog::Notice(msg) => {
                let mut ok_clicked = false;
                egui::Window::new("Notice").resizable(false).collapsible(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0,0.0]).show(ctx, |ui| {
                        ui.label(&msg); ui.add_space(8.0);
                        if ui.add(theme::btn_secondary("  OK  ")).clicked() { ok_clicked = true; }
                    });
                if !ok_clicked { self.dialog = Some(Dialog::Notice(msg)); }
            }

            Dialog::ConfirmReset => {
                let mut yes = false; let mut cancel = false;
                egui::Window::new("Confirm Reset").resizable(false).collapsible(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0,0.0]).show(ctx, |ui| {
                        ui.label("Clear ALL settings, metadata, paths and rules?\nThis cannot be undone.");
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.add(theme::btn_danger("  Reset All  ")).clicked() { yes=true; }
                            ui.add_space(4.0);
                            if ui.add(theme::btn_secondary("  Cancel  ")).clicked() { cancel=true; }
                        });
                    });
                if yes { self.reset_all(); }
                else if !cancel { self.dialog = Some(Dialog::ConfirmReset); }
            }

            Dialog::ConfirmClearAllRules(target) => {
                let mut yes = false; let mut cancel = false;
                let (name, count) = match target {
                    RuleTarget::Volume  => ("Volume Rules",  self.cfg.volume_rules.len()),
                    RuleTarget::Date    => ("Date Rules",    self.cfg.date_rules.len()),
                    RuleTarget::Summary => ("Summary Rules", self.cfg.summ_rules.len()),
                };
                egui::Window::new("Confirm Remove").resizable(false).collapsible(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0,0.0]).show(ctx, |ui| {
                        ui.label(format!(
                            "No rule is selected. Remove all {count} rule(s) in {name}?\nThis cannot be undone."
                        ));
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.add(theme::btn_danger("  Remove All  ")).clicked() { yes = true; }
                            ui.add_space(4.0);
                            if ui.add(theme::btn_secondary("  Cancel  ")).clicked() { cancel = true; }
                        });
                    });
                if yes {
                    match target {
                        RuleTarget::Volume  => { self.cfg.volume_rules.clear(); self.vol_sel  = None; }
                        RuleTarget::Date    => { self.cfg.date_rules.clear();   self.date_sel = None; }
                        RuleTarget::Summary => { self.cfg.summ_rules.clear();   self.summ_sel = None; }
                    }
                } else if !cancel {
                    self.dialog = Some(Dialog::ConfirmClearAllRules(target));
                }
            }

            Dialog::ConfirmClearLog => {
                let mut yes = false; let mut cancel = false;
                egui::Window::new("Clear Log").resizable(false).collapsible(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0,0.0]).show(ctx, |ui| {
                        ui.label("Clear the log output?\nThis only clears the displayed log, not the on-disk progress or error logs.");
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.add(theme::btn_primary("  Clear  ")).clicked() { yes = true; }
                            ui.add_space(4.0);
                            if ui.add(theme::btn_secondary("  Cancel  ")).clicked() { cancel = true; }
                        });
                    });
                if yes {
                    self.log.clear();
                    self.file_slots.clear();
                    self.log_footer.clear();
                } else if !cancel {
                    self.dialog = Some(Dialog::ConfirmClearLog);
                }
            }

            // ── Empty metadata fields warning ────────────────────────────────
            Dialog::EmptyFieldsWarning { fields, cbzs } => {
                let mut continue_anyway = false;
                let mut go_back = false;
                egui::Window::new("Empty Metadata Fields")
                    .resizable(false).collapsible(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label(RichText::new(
                            "These fields are empty and will be blank in every generated file:"
                        ).color(theme::TXT));
                        ui.add_space(6.0);
                        egui::Frame::none()
                            .fill(theme::SURF3)
                            .rounding(egui::Rounding::same(4.0))
                            .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                            .show(ui, |ui| {
                                ui.label(RichText::new(fields.join(", "))
                                    .color(theme::TWARN).strong());
                            });
                        ui.add_space(8.0);
                        ui.label(RichText::new(
                            "You can continue anyway, or go back to the Metadata tab and fill them in first."
                        ).color(theme::TDIM).size(11.0));
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.add(theme::btn_primary("  Continue Anyway  ")).clicked() { continue_anyway = true; }
                            ui.add_space(4.0);
                            if ui.add(theme::btn_secondary("  Go Back  ")).clicked() { go_back = true; }
                        });
                    });
                if continue_anyway {
                    self.continue_start(cbzs);
                } else if go_back {
                    self.tab = Tab::Metadata;
                } else {
                    self.dialog = Some(Dialog::EmptyFieldsWarning { fields, cbzs });
                }
            }

            // ── Add metadata tag ─────────────────────────────────────────────
            Dialog::AddMetadataTag => {
                let mut open = true;
                let mut picked: Option<&'static str> = None;
                let existing: HashSet<&str> = self.cfg.metadata_fields.iter()
                    .map(|(t, _)| t.as_str()).collect();

                // Sorted alphabetically by display label for a predictable,
                // easy-to-scan picker rather than schema/insertion order.
                let mut available: Vec<&'static FieldSpec> = COMICINFO_FIELDS.iter()
                    .filter(|f| !existing.contains(f.tag))
                    .collect();
                available.sort_by_key(|f| f.label);

                egui::Window::new("Add Metadata Tag")
                    .resizable(true).collapsible(false)
                    .min_width(320.0)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label(RichText::new("Choose a field to add:")
                            .color(theme::TDIM).size(11.0));
                        ui.add_space(6.0);

                        if available.is_empty() {
                            ui.label(RichText::new("All available fields have already been added.")
                                .color(theme::TDIM).size(12.0));
                        } else {
                            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                                for spec in &available {
                                    let resp = ui.add(
                                        egui::Button::new(RichText::new(spec.label).color(theme::TXT).size(12.0))
                                            .fill(theme::SURF3)
                                            .stroke(egui::Stroke::new(1.0, theme::BDR))
                                            .rounding(egui::Rounding::same(4.0))
                                            .min_size(egui::vec2(ui.available_width(), 26.0))
                                    ).on_hover_text(spec.tip);
                                    if resp.clicked() { picked = Some(spec.tag); }
                                    ui.add_space(2.0);
                                }
                            });
                        }
                        ui.add_space(8.0);
                        if ui.add(theme::btn_secondary("  Close  ")).clicked() { open = false; }
                    });

                if let Some(tag) = picked {
                    self.cfg.metadata_fields.push((tag.to_string(), String::new()));
                } else if open {
                    self.dialog = Some(Dialog::AddMetadataTag);
                }
            }

            // ── Tag order (drag and drop) ─────────────────────────────────
            Dialog::ReorderTags => {
                let mut open = true;
                let mut reset = false;
                let mut set_default = false;
                let active: HashSet<String> = self.cfg.metadata_fields.iter()
                    .map(|(t, _)| t.clone())
                    .chain(["Title", "Number", "Volume", "Summary", "Year", "Month", "Day"]
                        .map(String::from))
                    .collect();

                const TAG_ORDER_SIZE: egui::Vec2 = egui::vec2(340.0, 480.0);
                // Center on the main app window rather than leaving initial
                // placement up to the OS -- read from the main window's own
                // outer_rect (ctx here is still the main viewport's context;
                // the switch to the new viewport only happens inside the
                // nested closure below). None on the rare frame this isn't
                // yet known (e.g. very first frame) -- ViewportBuilder
                // simply omits with_position and the OS picks a default spot.
                let center_pos = ctx.input(|i| i.viewport().outer_rect).map(|r| {
                    r.center() - TAG_ORDER_SIZE / 2.0
                });
                let mut vp_builder = egui::ViewportBuilder::default()
                    .with_title("Tag Order")
                    .with_inner_size(TAG_ORDER_SIZE)
                    .with_min_inner_size([300.0, 300.0]);
                if let Some(pos) = center_pos {
                    vp_builder = vp_builder.with_position(pos);
                }

                // A genuine separate OS window (egui "viewport"), not an
                // egui::Window confined to the main app window -- so it can
                // be dragged anywhere on screen, including entirely outside
                // the main window, instead of blocking the view of
                // whatever's behind it. show_viewport_immediate must be
                // called every frame the viewport should stay visible
                // (confirmed via egui's own maintainer guidance:
                // github.com/emilk/egui/discussions/5306) -- satisfied
                // here the same way the old egui::Window was kept open:
                // self.dialog is re-set to Some(Dialog::ReorderTags) below
                // whenever `open` is still true, so this whole arm,
                // including this call, re-runs next frame.
                ctx.show_viewport_immediate(
                    egui::ViewportId::from_hash_of("tag_order_viewport"),
                    vp_builder,
                    |ctx, class| {
                        // Falls back to a normal embedded egui::Window if
                        // the backend can't give us a real OS window (per
                        // egui's own documented fallback) -- degrades
                        // gracefully instead of losing the dialog entirely.
                        if class == egui::ViewportClass::Embedded {
                            egui::Window::new("Tag Order")
                                .resizable(true).collapsible(false)
                                .min_width(300.0)
                                .default_pos(egui::pos2(360.0, 120.0))
                                .show(ctx, |ui| {
                                    self.reorder_tags_contents(ui, &active, &mut reset, &mut set_default, &mut open);
                                });
                            return;
                        }

                        egui::CentralPanel::default().show(ctx, |ui| {
                            self.reorder_tags_contents(ui, &active, &mut reset, &mut set_default, &mut open);
                        });

                        if ctx.input(|i| i.viewport().close_requested()) {
                            open = false;
                        }
                    },
                );

                if reset {
                    self.cfg.tag_order = default_tag_order();
                }
                if set_default {
                    self.settings.preferred_tag_order = Some(self.cfg.tag_order.clone());
                    self.save_settings();
                    self.status = "Tag order saved as default.".to_string();
                }
                if open {
                    self.dialog = Some(Dialog::ReorderTags);
                }
            }

            // ── Import result ─────────────────────────────────────────────
            Dialog::ImportResult { filename, items } => {
                let mut close = false;
                egui::Window::new("Import Successful")
                    .resizable(true)
                    .collapsible(false)
                    .min_width(480.0)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        // Header
                        ui.label(
                            RichText::new(format!("{} field(s) imported from  {filename}", items.len()))
                                .color(theme::TGOOD).strong().size(13.0),
                        );
                        ui.separator();
                        ui.add_space(4.0);

                        // Scrollable list of imported fields
                        egui::ScrollArea::vertical()
                            .id_salt("import_result_scroll")
                            .max_height(340.0)
                            .show(ui, |ui| {
                                egui::Grid::new("import_result_grid")
                                    .num_columns(2)
                                    .spacing([16.0, 4.0])
                                    .striped(true)
                                    .show(ui, |ui| {
                                        for (field, value) in &items {
                                            ui.label(
                                                RichText::new(field.as_str())
                                                    .color(theme::ACC2)
                                                    .strong()
                                                    .size(12.0),
                                            );
                                            // Truncate long values (e.g. Summary) for display
                                            let display = if value.len() > 120 {
                                                format!("{}...", &value[..117])
                                            } else {
                                                value.clone()
                                            };
                                            ui.label(
                                                RichText::new(display)
                                                    .color(theme::TXT)
                                                    .size(12.0),
                                            );
                                            ui.end_row();
                                        }
                                    });
                            });

                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("Fields have been applied. You can review them in the Metadata tab.")
                                .color(theme::TDIM)
                                .size(11.0),
                        );
                        ui.add_space(6.0);
                        if ui.add(theme::btn_primary("  OK  ")).clicked() { close = true; }
                    });

                if !close {
                    self.dialog = Some(Dialog::ImportResult { filename, items });
                }
            }
        }
    }

    // ── Keyboard shortcuts ────────────────────────────────────────────────────
    pub fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::CTRL, egui::Key::S) {
                let n = self.smart_filename(); self.start_pick(PathPick::SaveConfig(n));
            }
            if i.consume_key(egui::Modifiers::CTRL, egui::Key::O) { self.start_pick(PathPick::LoadConfig); }
            if i.consume_key(egui::Modifiers::CTRL, egui::Key::I) { self.start_pick(PathPick::ImportMeta); }
            if i.consume_key(egui::Modifiers::CTRL, egui::Key::R) { self.dialog = Some(Dialog::ConfirmReset); }
        });
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  eframe::App
// ═══════════════════════════════════════════════════════════════════════════════
impl eframe::App for ComicInfoApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.viewport().close_requested()) { self.autosave(); }
        if self.last_save.elapsed().as_secs() > 30 { self.autosave(); self.last_save = std::time::Instant::now(); }

        // Process any action deferred from last frame's dialog rendering.
        // Must run BEFORE rendering so egui never sees stale window layers.
        if let Some((cbz, ps, resume)) = self.pending_start.take() {
            self.check_finale(cbz, ps, resume);
        }

        self.handle_shortcuts(ctx);
        self.poll_pick();
        self.poll_worker(ctx);
        self.render_dialogs(ctx);
        self.show_settings_window(ctx);

        // Vertical margins are kept EQUAL (top == bottom) on every bar so the
        // panel sizes itself naturally around its content and that content
        // ends up truly centered -- no guessed exact_height() + leftover slack.
        egui::TopBottomPanel::top("toolbar")
            .frame(egui::Frame::none().fill(theme::SURF).stroke(egui::Stroke::new(1.0, theme::BDR))
                .inner_margin(egui::Margin::symmetric(8.0, 8.0)))
            .show(ctx, |ui| self.show_toolbar(ui));
        egui::TopBottomPanel::bottom("statusbar")
            .frame(egui::Frame::none().fill(theme::BG).stroke(egui::Stroke::new(1.0, theme::BDR))
                .inner_margin(egui::Margin::symmetric(12.0, 6.0)))
            .show(ctx, |ui| self.show_statusbar(ui));
        egui::TopBottomPanel::top("tabbar")
            .frame(egui::Frame::none().fill(theme::SURF).stroke(egui::Stroke::new(1.0, theme::BDR))
                .inner_margin(egui::Margin::symmetric(14.0, 8.0)))
            .show(ctx, |ui| self.show_tabbar(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(theme::BG))
            .show(ctx, |ui| {
                match self.tab {
                    Tab::Paths      => self.show_paths(ui),
                    Tab::Processing => self.show_processing(ui),
                    Tab::Metadata   => self.show_metadata(ui),
                    Tab::Rules      => self.show_rules(ui),
                    Tab::Run        => self.show_run(ui),
                }
            });
    }
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) { self.autosave(); }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Panels
// ═══════════════════════════════════════════════════════════════════════════════
impl ComicInfoApp {
    fn show_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("ComicInfo Generator")
                .size(14.0).color(theme::TXT).strong());

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);
                if ui.add(theme::btn_secondary("\u{2699} Settings")).on_hover_text("App settings").clicked() {
                    self.settings_open = true;
                }
                ui.add_space(10.0);
                if ui.add(theme::btn_danger("Reset All")).on_hover_text("Clear all settings (Ctrl+R)").clicked() {
                    self.dialog = Some(Dialog::ConfirmReset);
                }
                ui.add_space(6.0);
                if ui.add(theme::btn_secondary("Import")).on_hover_text("Import metadata from .py or .json (Ctrl+I)").clicked() {
                    self.start_pick(PathPick::ImportMeta);
                }
                ui.add_space(4.0);
                if ui.add(theme::btn_secondary("Load")).on_hover_text("Load a config file (Ctrl+O)").clicked() {
                    self.start_pick(PathPick::LoadConfig);
                }
                ui.add_space(4.0);
                if ui.add(theme::btn_secondary("Save")).on_hover_text("Save config file (Ctrl+S)").clicked() {
                    let n = self.smart_filename();
                    self.start_pick(PathPick::SaveConfig(n));
                }
                ui.add_space(20.0);
                ui.label(RichText::new("Ctrl+S / O / I / R").color(theme::TMUT).size(10.0));
            });
        });
    }

    fn show_statusbar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // Status dot — drawn as a vector circle (never depends on font glyph coverage)
            let dot_col = if self.running { theme::TGOOD }
                          else if self.status.contains("error") || self.status.contains("Error") { theme::TERR }
                          else { theme::TMUT };
            let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
            ui.painter().circle_filled(dot_rect.center(), 3.5, dot_col);
            ui.add_space(6.0);
            ui.label(RichText::new(&self.status).color(theme::TDIM).size(11.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new("ComicInfo Generator  -  Rust Edition")
                    .color(theme::TMUT).size(10.0));
            });
        });
    }

    fn show_tabbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.style_mut().spacing.item_spacing.x = 6.0;
            for (tab, label) in [
                (Tab::Paths,      "Paths"),
                (Tab::Processing, "Processing"),
                (Tab::Metadata,   "Metadata"),
                (Tab::Rules,      "Rules"),
                (Tab::Run,        "Run"),
            ] {
                let active = self.tab == tab;
                if ui.add(
                    egui::Button::new(
                        RichText::new(label).size(12.0)
                            .color(if active { Color32::WHITE } else { theme::TDIM })
                    )
                    .fill(if active { theme::ACC } else { Color32::TRANSPARENT })
                    .stroke(egui::Stroke::new(1.0,
                        if active { theme::ACC } else { theme::BDR }))
                    .rounding(egui::Rounding::same(18.0))
                    .min_size(egui::vec2(0.0, 28.0))
                ).clicked() { self.tab = tab; }
            }
        });
    }

    // Floating window (not a 6th tab, deliberately) for app-wide preferences
    // that apply across every job/series rather than belonging to a single
    // saved config. Non-modal and independent of the Dialog enum so it can
    // stay open (or be dismissed) without interrupting anything else --
    // each toggle saves to disk immediately on change, no separate Save
    // button, since there's nothing here worth risking losing on a crash.
    fn show_settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open { return; }
        let mut open = true;
        egui::Window::new("Settings")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .anchor(egui::Align2::RIGHT_TOP, [-16.0, 48.0])
            .show(ctx, |ui| {
                ui.set_min_width(280.0);
                theme::section_hdr(ui, "Safety");
                if ui.checkbox(&mut self.settings.backup_before_overwrite,
                    RichText::new("Back up originals before overwriting").size(12.0))
                    .on_hover_text(
                        "Copies each CBZ to a \"backups\" subfolder next to it \
                         before modifying it in place. Only applies when \
                         \"Write new CBZ\" (Paths tab) is off.")
                    .changed()
                {
                    self.save_settings();
                }
                ui.add_space(10.0);
                theme::section_hdr(ui, "Notifications");
                if ui.checkbox(&mut self.settings.play_sound_on_completion,
                    RichText::new("Play a sound when a run finishes").size(12.0))
                    .changed()
                {
                    self.save_settings();
                }
            });
        self.settings_open = open;
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Helper widgets (static)
// ═══════════════════════════════════════════════════════════════════════════════
impl ComicInfoApp {


    // Wraps a single-line text field in a horizontal ScrollArea so its
    // content can be scrolled with the mouse wheel or a touchpad swipe
    // while just hovering (no need to click in and use arrow keys) when
    // the text is wider than the box. The inner TextEdit is sized to the
    // text's own measured width -- via egui's real text layout, not an
    // estimate -- so there's no dead scrollable space past short text.
    // The ScrollArea itself is held to exactly `width` so the metadata
    // grid's row-wrapping math (which assumes each field consumes exactly
    // `width` pixels) still holds.
    fn scrollable_text_edit(ui: &mut egui::Ui, tag: &str, val: &mut String, width: f32) -> egui::Response {
        let font_id = egui::TextStyle::Body.resolve(ui.style());
        let text_w = ui.fonts(|f| f.layout_no_wrap(val.clone(), font_id, Color32::WHITE).size().x);
        let inner_w = (text_w + 18.0).max(width); // +18 ~ cursor/margin room inside TextEdit
        ui.scope(|ui| {
            // A horizontal-only ScrollArea only reacts to genuinely
            // horizontal scroll input by default -- a plain vertical mouse
            // wheel produces zero movement on that axis, so hovering and
            // scrolling normally would do nothing at all. This makes it
            // also accept vertical wheel input as horizontal movement,
            // while still reading real horizontal input (touchpad swipes)
            // the same as before. Scoped to just this field via
            // ui.scope(), not applied globally.
            ui.style_mut().always_scroll_the_only_direction = true;
            // Thinner scrollbar for these compact single-line fields --
            // default is 6.0, which reads as bulky against a ~24px-tall
            // box. Scoped to just this field via the same ui.scope().
            ui.style_mut().spacing.scroll.bar_width = 3.0;
            egui::ScrollArea::horizontal()
                .id_salt(("meta_field_scroll", tag))
                .max_width(width)
                .show(ui, |ui| {
                    // Building the TextEdit directly in this closure would
                    // NOT work, even with inner_w passed to desired_width:
                    // TextEdit clamps to min(desired_width, available_
                    // width), and available_width() here is capped to
                    // `width` by the ScrollArea itself -- so the field
                    // could never actually become wider than the box, and
                    // there would be nothing to scroll to (confirmed by
                    // tracing egui's ScrollArea source and reproducing it
                    // in a real instrumented build). Building a genuinely
                    // wide child Ui via UiBuilder::max_rect bypasses that
                    // clamp entirely.
                    let rect = egui::Rect::from_min_size(
                        ui.cursor().min,
                        egui::vec2(inner_w, ui.available_height().max(20.0)),
                    );
                    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect), |ui| {
                        ui.add(egui::TextEdit::singleline(val).desired_width(inner_w))
                    }).inner
                }).inner
        }).inner
    }

    fn path_row(ui: &mut egui::Ui, label: &str, val: &mut String, tip: &str) -> bool {
        let mut clicked = false;
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.add_sized([152.0, 26.0], egui::Label::new(
                RichText::new(label).color(theme::TDIM).size(12.0)
            ));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(
                    egui::Button::new(RichText::new("Browse").size(11.5).color(theme::TXT))
                        .fill(theme::SURF3)
                        .stroke(egui::Stroke::new(1.0, theme::BDR))
                        .rounding(egui::Rounding::same(5.0))
                        .min_size(egui::vec2(74.0, 26.0))
                ).clicked() { clicked = true; }
                let resp = ui.add(
                    egui::TextEdit::singleline(val)
                        .font(egui::FontId::new(12.0, egui::FontFamily::Monospace))
                        .hint_text("Browse or type a path...")
                        .desired_width(f32::INFINITY)
                ).on_hover_text(tip);
                if resp.changed() && val.starts_with('"') && val.ends_with('"') && val.len() > 2 {
                    *val = val[1..val.len()-1].to_string();
                }
            });
        });
        clicked
    }

    /// Flat table with header + scrollable rows and row selection.
    fn table(
        ui:      &mut egui::Ui,
        cols:    &[(&str, f32)],
        rows:    &[Vec<String>],
        sel:     &mut Option<usize>,
        expand_last_to_content: bool,
    ) -> bool { // returns true if a row was double-clicked
        let mut dblclk = false;
        let last_col = cols.len().saturating_sub(1);
        let fixed_w_except_last: f32 = cols[..last_col].iter().map(|(_, w)| w).sum();

        // When expand_last_to_content, the last column is sized to the
        // widest ACTUAL text in it (measured via egui's real text layout,
        // not estimated) instead of stretching to fill whatever's
        // available. The previous "stretch to fill available width"
        // behavior caps the column at the container's width no matter how
        // long the text is, so wrapping the table in a horizontal
        // ScrollArea had nothing to scroll -- the column, and therefore
        // the table, could never exceed the viewport in the first place.
        // This makes the table genuinely wider than its container when
        // the content needs it; the caller is expected to wrap it in a
        // horizontal ScrollArea in that case (see rule_section).
        let last_col_w = if expand_last_to_content {
            let measured = rows.iter()
                .filter_map(|r| r.get(last_col))
                .map(|s| {
                    // Measure the same flattened text that's actually
                    // painted below (see the row-painting loop) -- for a
                    // multi-line string, layout_no_wrap's width is only
                    // its widest single line, not the full text as it
                    // will be joined onto one line for display.
                    let flat = if s.contains('\n') {
                        s.split_whitespace().collect::<Vec<_>>().join(" ")
                    } else {
                        s.clone()
                    };
                    ui.fonts(|f| f.layout_no_wrap(
                        flat,
                        egui::FontId::new(12.0, egui::FontFamily::Monospace),
                        Color32::WHITE,
                    ).size().x)
                })
                .fold(0.0_f32, f32::max);
            (measured + 12.0).max(cols[last_col].1)
        } else {
            cols[last_col].1
        };
        let total_w: f32 = fixed_w_except_last + last_col_w + 12.0;

        // Header
        let hdr_h = 24.0;
        let (hr, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(total_w), hdr_h), egui::Sense::hover());
        ui.painter().rect_filled(hr, egui::Rounding::same(4.0), theme::SURF3);
        let mut cx = hr.left() + 6.0;
        for (name, w) in cols {
            ui.painter().text(
                egui::pos2(cx, hr.center().y),
                egui::Align2::LEFT_CENTER, *name,
                egui::FontId::new(11.5, egui::FontFamily::Proportional), theme::ACC2,
            );
            cx += w;
        }

        // Rows -- rendered directly (no inner ScrollArea). The box's total
        // height is simply rows.len() * row_h, growing naturally as rows
        // are added rather than becoming independently scrollable inside a
        // fixed-size box. The Rules tab's outer page-level ScrollArea
        // handles any eventual overall overflow instead.
        ui.set_min_width(total_w);
        for (i, row) in rows.iter().enumerate() {
            let is_sel = *sel == Some(i);
            let bg = if is_sel { theme::ACC } else if i%2==0 { theme::SURF2 } else { theme::ROW_ALT };
            let tc = if is_sel { Color32::WHITE } else { theme::TXT };
            let row_h = 24.0;
            let (rect, resp) = ui.allocate_exact_size(
                egui::vec2(ui.available_width().max(total_w), row_h), egui::Sense::click());
            if resp.clicked() {
                *sel = if *sel == Some(i) { None } else { Some(i) };
            }
            if resp.double_clicked() { *sel = Some(i); dblclk = true; }
            if ui.is_rect_visible(rect) {
                ui.painter().rect_filled(rect, egui::Rounding::ZERO, bg);
                // Last column either stretches to fill whatever width is
                // left in the row (default), or uses the content-measured
                // width computed above (expand_last_to_content) -- never
                // re-derived from rect.width() in that case, since rect is
                // already sized to total_w and would just hand the same
                // value straight back.
                let stretch_last_w = if expand_last_to_content {
                    last_col_w
                } else {
                    (rect.width() - 12.0 - fixed_w_except_last).max(cols[last_col].1)
                };
                let mut cx = rect.left() + 6.0;
                for (j, (_, w)) in cols.iter().enumerate() {
                    let raw_txt = row.get(j).map(|s| s.as_str()).unwrap_or("");
                    // Flatten embedded newlines (and collapse the runs of
                    // whitespace they usually leave behind, e.g. a blank
                    // line between paragraphs) for this preview only. A
                    // multi-line Summary painted as-is here would stack
                    // every line on top of the others at one anchor point,
                    // then get clipped to the row's fixed 24px height --
                    // exactly the squished, overlapping look this avoids.
                    // The full text with its real line breaks is
                    // untouched everywhere else: the hover tooltip below,
                    // and the Edit Rule dialog's textbox.
                    let flattened;
                    let txt: &str = if raw_txt.contains('\n') {
                        flattened = raw_txt.split_whitespace().collect::<Vec<_>>().join(" ");
                        &flattened
                    } else {
                        raw_txt
                    };
                    let col_w = if j == last_col { stretch_last_w } else { *w };
                    // Clip text to column — rect must have positive dims or egui panics
                    let clip = egui::Rect::from_min_size(
                        egui::pos2(cx, rect.top()),
                        egui::vec2((col_w - 4.0).max(1.0), row_h.max(1.0)),
                    );
                    ui.painter().with_clip_rect(clip).text(
                        egui::pos2(cx, rect.center().y),
                        egui::Align2::LEFT_CENTER, txt,
                        egui::FontId::new(12.0, egui::FontFamily::Monospace), tc,
                    );
                    cx += col_w;
                }
            }
            // Hover over any row to read its full, untruncated last-column
            // text (e.g. a long Summary) even if it doesn't fit on screen.
            if let Some(full_text) = row.get(last_col) {
                if !full_text.is_empty() {
                    resp.on_hover_text(full_text.as_str());
                }
            }
        }
        dblclk
    }

    // Body of the Tag Order dialog -- shared between the real-viewport
    // render path (Dialog::ReorderTags's main branch) and the embedded
    // egui::Window fallback used if the backend can't give us a real OS
    // window. Written as a plain &mut self method (not a closure) so it
    // can be called identically from either place without duplicating
    // the drag-and-drop list, filter checkbox, or button row.
    fn reorder_tags_contents(
        &mut self,
        ui: &mut egui::Ui,
        active: &HashSet<String>,
        reset: &mut bool,
        set_default: &mut bool,
        open: &mut bool,
    ) {
        ui.label(RichText::new(
            "Drag to reorder. Controls the order tags are written to \
             ComicInfo.xml -- dimmed tags aren't currently in use.")
            .color(theme::TDIM).size(11.0));
        ui.add_space(4.0);
        ui.checkbox(&mut self.reorder_show_active_only,
            RichText::new("Show only tags currently in use").size(11.0));
        // Read after the checkbox so a click this frame is reflected in
        // this same frame's filtering rather than lagging a frame behind.
        let show_active_only = self.reorder_show_active_only;
        ui.add_space(6.0);

        // Reserve room below the list for: the button row's own real
        // height (measured last frame via ui.scope(), not a hand-guessed
        // constant -- an earlier 24px estimate undercounted the actual
        // 28px button height and clipped them), the 8px gap before the
        // row, and 6px of breathing room after it so the buttons aren't
        // flush against the window's bottom edge.
        let list_h = (ui.available_height() - self.reorder_button_row_h - 8.0 - 6.0).max(80.0);

        egui::ScrollArea::vertical().max_height(list_h).show(ui, |ui| {
            // Filtering by skipping items entirely (egui_dnd's own
            // recommended approach): zero the item spacing globally so
            // hidden rows don't leave gaps, then restore it right before
            // each row that actually renders.
            let normal_spacing =
                std::mem::replace(&mut ui.spacing_mut().item_spacing.y, 0.0);

            egui_dnd::dnd(ui, "tag_order_dnd")
                .show_vec(&mut self.cfg.tag_order, |ui, tag, handle, _state| {
                    let is_active = active.contains(tag.as_str());
                    if show_active_only && !is_active {
                        return;
                    }
                    ui.spacing_mut().item_spacing.y = normal_spacing;
                    let label = field_spec(tag.as_str())
                        .map(|s| s.label).unwrap_or(tag.as_str());
                    let color = if is_active { theme::TXT } else { theme::TDIM };
                    // Whole row is the handle -- there's nothing else
                    // interactive in it to conflict with.
                    handle.ui(ui, |ui| {
                        egui::Frame::none()
                            .fill(theme::SURF3)
                            .stroke(egui::Stroke::new(1.0, theme::BDR))
                            .rounding(egui::Rounding::same(4.0))
                            .inner_margin(egui::Margin::symmetric(8.0, 5.0))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.label(RichText::new(label).size(12.0).color(color));
                            });
                    });
                });
        });

        ui.add_space(8.0);
        let button_row = ui.scope(|ui| {
            ui.horizontal(|ui| {
                if ui.add(theme::btn_secondary("Reset to Default")).clicked() {
                    *reset = true;
                }
                ui.add_space(4.0);
                if ui.add(theme::btn_secondary("Set as Default"))
                    .on_hover_text(
                        "Save the current order as your standing preference -- \
                         it will be used for new sessions and survive Reset All, \
                         instead of reverting to the built-in schema order.")
                    .clicked()
                {
                    *set_default = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(theme::btn_primary("  Done  ")).clicked() { *open = false; }
                });
            });
        });
        self.reorder_button_row_h = button_row.response.rect.height();
        ui.add_space(6.0);
    }

    /// Reusable rule section: title + Add/Edit/Remove buttons + table.
    fn rule_section(
        ui:     &mut egui::Ui,
        title:  &str,
        cols:   &[(&str, f32)],
        rows:   &mut Vec<Vec<String>>,
        sel:    &mut Option<usize>,
        target: RuleTarget,
    ) -> Option<Dialog> {
        let mut pending: Option<Dialog> = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).color(theme::ACC2).strong().size(12.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::new(RichText::new("Remove").size(11.0).color(theme::TERR)).fill(Color32::TRANSPARENT).stroke(egui::Stroke::new(1.0, theme::BDR)).rounding(egui::Rounding::same(5.0)).min_size(egui::vec2(0.0,24.0))).clicked() {
                    if let Some(idx) = *sel {
                        if idx < rows.len() { rows.remove(idx); }
                        *sel = None;
                    } else if !rows.is_empty() {
                        pending = Some(Dialog::ConfirmClearAllRules(target));
                    }
                }
                ui.add_space(2.0);
                if ui.add(egui::Button::new(RichText::new("Edit").size(11.0).color(theme::ACC2)).fill(Color32::TRANSPARENT).stroke(egui::Stroke::new(1.0, theme::BDR)).rounding(egui::Rounding::same(5.0)).min_size(egui::vec2(0.0,24.0))).clicked() {
                    if let Some(idx) = *sel {
                        if let Some(row) = rows.get(idx) {
                            let vals = padded(row, cols.len());
                            // Starting value only -- reflects what this
                            // specific saved row actually has, as a
                            // sensible default. From here on the checkbox
                            // is independent, real state (see
                            // RuleEditState::same_start_end).
                            let same_start_end = matches!(target, RuleTarget::Date | RuleTarget::Summary)
                                && vals.get(0).zip(vals.get(1)).map_or(false, |(a, b)| a == b && !a.trim().is_empty());
                            pending = Some(Dialog::EditRule(RuleEditState {
                                target, row_idx: Some(idx), is_new: false,
                                labels: cols.iter().map(|(h,_)| h.to_string()).collect(),
                                values: vals,
                                same_start_end,
                            }));
                        }
                    }
                }
                ui.add_space(2.0);
                if ui.add(egui::Button::new(RichText::new("Add").size(11.0).color(theme::TGOOD)).fill(Color32::TRANSPARENT).stroke(egui::Stroke::new(1.0, theme::BDR)).rounding(egui::Rounding::same(5.0)).min_size(egui::vec2(0.0,24.0))).clicked() {
                    pending = Some(Dialog::EditRule(RuleEditState {
                        target, row_idx: None, is_new: true,
                        labels: cols.iter().map(|(h,_)| h.to_string()).collect(),
                        values: vec![String::new(); cols.len()],
                        same_start_end: false, // fresh rule always starts with 2 separate fields
                    }));
                }
            });
        });
        // Summary Rules' Summary column is wide enough to regularly get
        // cut off at the card's edge with no way to see the rest -- a
        // horizontal scrollbar (and mouse wheel / touchpad scrolling)
        // fixes that. Volume/Date Rules' columns comfortably fit in
        // practice, so they're left as plain tables rather than adding a
        // scroll wrapper that would rarely do anything.
        let dbl = if target == RuleTarget::Summary {
            let mut clicked = false;
            ui.scope(|ui| {
                // Same fix as scrollable_text_edit: without this, a plain
                // vertical mouse wheel does nothing on a horizontal-only
                // ScrollArea, which is exactly the "scrollbar appeared but
                // scrolling does nothing" symptom.
                ui.style_mut().always_scroll_the_only_direction = true;
                // Same thinning as scrollable_text_edit's metadata fields
                // -- default 6px reads as bulky against this table's rows.
                ui.style_mut().spacing.scroll.bar_width = 3.0;
                egui::ScrollArea::horizontal()
                    .id_salt("summ_rules_table_scroll")
                    .show(ui, |ui| { clicked = Self::table(ui, cols, rows, sel, true); });
            });
            clicked
        } else {
            Self::table(ui, cols, rows, sel, false)
        };
        if dbl {
            if let Some(idx) = *sel {
                if let Some(row) = rows.get(idx) {
                    let vals = padded(row, cols.len());
                    let same_start_end = matches!(target, RuleTarget::Date | RuleTarget::Summary)
                        && vals.get(0).zip(vals.get(1)).map_or(false, |(a, b)| a == b && !a.trim().is_empty());
                    pending = Some(Dialog::EditRule(RuleEditState {
                        target, row_idx: Some(idx), is_new: false,
                        labels: cols.iter().map(|(h,_)| h.to_string()).collect(),
                        values: vals,
                        same_start_end,
                    }));
                }
            }
        }
        pending
    }
}

fn padded(row: &[String], n: usize) -> Vec<String> {
    let mut r = row.to_vec();
    while r.len() < n { r.push(String::new()); }
    r
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Tab content
// ═══════════════════════════════════════════════════════════════════════════════
impl ComicInfoApp {
    fn show_paths(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().id_salt("paths_scr").show(ui, |ui| {
            egui::Frame::none()
                .inner_margin(egui::Margin::symmetric(20.0, 16.0))
                .show(ui, |ui| {

            theme::card().show(ui, |ui| {
                theme::section_hdr(ui, "File Paths");
                if Self::path_row(ui, "CBZ Folder:", &mut self.cfg.folder, "Folder containing the .cbz files.") {
                    self.start_pick(PathPick::Folder);
                }
                if Self::path_row(ui, "Titles JSON:", &mut self.cfg.titles_json,
                    r#"{"1":"Chapter/Episode or Volume Title","2":"..."}"#) {
                    self.start_pick(PathPick::TitlesJson);
                }
                if Self::path_row(ui, "Episode Dates JSON:", &mut self.cfg.date_json, r#"{"1":"Jul 25, 2019"}"#) {
                    self.start_pick(PathPick::DateJson);
                }
            });
            ui.add_space(14.0);

            // Output Mode is genuinely about where files get written, so
            // it stays here alongside File Paths rather than moving to
            // Processing with Max Workers/Dry Run (see show_processing).
            // Captured before entering the card: inside card().show()'s
            // closure, available_width() reflects the frame's own
            // content-driven size, not the panel's true width, so reading
            // it there would just report back whatever the checkbox/radio
            // content already shrank the frame to.
            let full_w = ui.available_width();
            theme::card().show(ui, |ui| {
                ui.set_min_width(full_w - 28.0); // - card's own left+right inner_margin (14px each)
                theme::section_hdr(ui, "Output Mode");
                ui.checkbox(&mut self.cfg.write_new_cbz,
                    RichText::new("Write new CBZ  -  don't overwrite the original file").size(12.0))
                    .on_hover_text("When off (default), the original .cbz is modified and renamed in place.\nWhen on, a new file is written and the original is left completely untouched.");

                if self.cfg.write_new_cbz {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.add_space(20.0);
                        ui.radio_value(&mut self.cfg.output_same_path, true, "Subfolder next to source")
                            .on_hover_text("Writes into an \"output\" folder created inside the source folder --\nnot directly into the source folder itself, so the new file can\nnever end up overwriting the original by sharing its name.");
                        ui.add_space(12.0);
                        ui.radio_value(&mut self.cfg.output_same_path, false, "Custom folder:");
                    });

                    if !self.cfg.output_same_path {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.add_space(20.0);
                            ui.add_sized([108.0, 26.0], egui::Label::new(
                                RichText::new("Output Folder:").color(theme::TDIM).size(12.0)
                            ));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add(
                                    egui::Button::new(RichText::new("Browse").size(11.5).color(theme::TXT))
                                        .fill(theme::SURF3)
                                        .stroke(egui::Stroke::new(1.0, theme::BDR))
                                        .rounding(egui::Rounding::same(5.0))
                                        .min_size(egui::vec2(74.0, 26.0))
                                ).clicked() {
                                    self.start_pick(PathPick::OutputPath);
                                }
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.cfg.output_path)
                                        .font(egui::FontId::new(12.0, egui::FontFamily::Monospace))
                                        .hint_text("Browse or type a folder path...")
                                        .desired_width(f32::INFINITY)
                                ).on_hover_text("New CBZ files are written here instead of the source folder.");
                            });
                        });
                        if self.cfg.output_path.trim().is_empty() {
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.add_space(20.0);
                                ui.label(RichText::new("Choose a folder, or switch back to \"Subfolder next to source\".")
                                    .color(theme::TWARN).size(11.0));
                            });
                        }
                    }
                }
            });

                }); // Frame
        });
    }

    fn show_processing(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().id_salt("proc_scr").show(ui, |ui| {
            ui.add_space(16.0);
            ui.columns(2, |cols| {
                let lc = &mut cols[0];
                egui::Frame::none().outer_margin(egui::Margin { left:20.0, right:8.0, ..Default::default() }).show(lc, |ui| {
                    // Mode + Volume metadata -- merged into one card since
                    // Mode's only effect is setting these 3 checkboxes, and
                    // Mode alone was a two-radio-button sliver next to much
                    // taller neighbors.
                    theme::card().show(ui, |ui| {
                        theme::section_hdr(ui, "Mode & Volume Metadata");
                        ui.horizontal(|ui| {
                            let was = self.cfg.mode.clone();
                            ui.radio_value(&mut self.cfg.mode, ComicMode::Manga, "Manga")
                                .on_hover_text("Turns ON all Volume Metadata options below (default for most manga).");
                            ui.add_space(8.0);
                            ui.radio_value(&mut self.cfg.mode, ComicMode::Manhwa, "Manhwa / Manhua")
                                .on_hover_text("Turns OFF all Volume Metadata options below (no volumes in manhwa).");
                            if self.cfg.mode != was {
                                let is_m = matches!(self.cfg.mode, ComicMode::Manga);
                                self.cfg.use_vol = is_m; self.cfg.use_vol_date = is_m; self.cfg.use_vol_summ = is_m;
                            }
                        });
                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(6.0);
                        ui.checkbox(&mut self.cfg.use_vol, RichText::new("Include volume number in metadata").size(12.0))
                            .on_hover_text("Enables Volume field in ComicInfo.xml. Disable for manhwa.");
                        ui.checkbox(&mut self.cfg.use_vol_date, RichText::new("Use volume date rules for publication").size(12.0))
                            .on_hover_text("Overrides Year/Month/Day from Date Rules table. Disable for manhwa.");
                        ui.checkbox(&mut self.cfg.use_vol_summ, RichText::new("Use per-volume summary rules").size(12.0))
                            .on_hover_text("Overrides Summary from Summary Rules table. Disable for manhwa.");
                    });
                    ui.add_space(10.0);
                    // Separator
                    theme::card().show(ui, |ui| {
                        theme::section_hdr(ui, "Title Separator");
                        if ui.checkbox(&mut self.cfg.csep_on,
                            RichText::new("Override separator").size(12.0))
                            .on_hover_text("Replaces the default ' - ' or ': ' between number and title.")
                            .changed() {
                            self.rebuild_sep_preview();
                        }
                        ui.horizontal(|ui| {
                            ui.add_space(20.0);
                            ui.label(RichText::new("Separator:").color(theme::TXT).size(12.0));
                            let r = ui.add_enabled(self.cfg.csep_on,
                                egui::TextEdit::singleline(&mut self.cfg.csep).desired_width(80.0))
                                .on_hover_text("e.g. \"-\" or \"~\"   (avoid  / \\ : * ? \" < > |  - invalid in filenames)");
                            if r.changed() { self.rebuild_sep_preview(); }
                        });
                        ui.add_space(4.0);
                        egui::Frame::none().fill(theme::SURF3).rounding(egui::Rounding::same(4.0))
                            .inner_margin(egui::Margin::symmetric(8.0, 4.0)).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Preview:").color(theme::TDIM).size(11.0));
                                ui.label(RichText::new(&self.sep_preview).color(theme::ACC2)
                                    .font(egui::FontId::new(12.0, egui::FontFamily::Monospace)));
                            });
                        });
                    });
                    ui.add_space(10.0);
                    // Processing Settings -- moved here from the Paths tab.
                    // Max Workers, Dry Run, and the log folder are genuine
                    // processing concerns; they ended up in Paths only
                    // because that tab's layout needed a second card to
                    // fill out a column, not because they belonged there.
                    theme::card().show(ui, |ui| {
                        theme::section_hdr(ui, "Processing Settings");
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Max Workers:").color(theme::TDIM).size(12.0));
                            ui.add_space(4.0);
                            ui.add(egui::DragValue::new(&mut self.cfg.workers)
                                .range(1..=32).speed(0.1));
                        });
                        ui.add_space(8.0);
                        ui.checkbox(&mut self.cfg.dry_run,
                            RichText::new("Dry Run  -  preview only, no files modified").size(12.0));
                        ui.add_space(10.0);
                        let log_path = std::env::current_dir().unwrap_or_default().join("logs");
                        ui.label(RichText::new(format!("Log directory: {}", log_path.display()))
                            .color(theme::TMUT).size(11.0));
                        ui.add_space(4.0);
                        if ui.add(
                            egui::Button::new(RichText::new("Open Folder").size(11.0).color(theme::TDIM))
                                .fill(Color32::TRANSPARENT)
                                .stroke(egui::Stroke::new(1.0, theme::BDR))
                                .rounding(egui::Rounding::same(4.0))
                                .min_size(egui::vec2(0.0, 20.0))
                        ).on_hover_text("Open the folder containing progress and error logs for past runs.").clicked() {
                            let _ = std::fs::create_dir_all(&log_path);
                            Self::open_in_file_manager(&log_path);
                        }
                    });
                });

                let rc = &mut cols[1];
                egui::Frame::none().outer_margin(egui::Margin { left:8.0, right:20.0, ..Default::default() }).show(rc, |ui| {
                    // Prefix mode + Post-finale -- merged into one card
                    // since post-finale behaviour is a refinement of the
                    // same numbering scheme, and Post-Finale Behaviour
                    // alone was a one-dropdown sliver next to much taller
                    // neighbors.
                    theme::card().show(ui, |ui| {
                        theme::section_hdr(ui, "Number Prefix");
                        for (val, lbl) in [
                            (PrefixMode::Auto,    "Auto-detect from filename"),
                            (PrefixMode::Episode, "Always: Episode"),
                            (PrefixMode::Chapter, "Always: Chapter"),
                            (PrefixMode::Volume,  "Always: Volume"),
                            (PrefixMode::Custom,  "Custom:"),
                        ] {
                            if ui.radio_value(&mut self.cfg.prefix_mode, val, lbl).changed() {
                                self.rebuild_sep_preview();
                            }
                        }
                        ui.horizontal(|ui| {
                            ui.add_space(20.0);
                            ui.label(RichText::new("Custom text:").color(theme::TXT).size(12.0));
                            let r = ui.add_enabled(
                                matches!(self.cfg.prefix_mode, PrefixMode::Custom),
                                egui::TextEdit::singleline(&mut self.cfg.custom_pfx).desired_width(120.0),
                            ).on_hover_text("Used when prefix mode is \"custom\". E.g. \"Break\".");
                            if r.changed() { self.rebuild_sep_preview(); }
                        });
                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("After finale:").color(theme::TXT).size(12.0));
                            egui::ComboBox::from_id_salt("pf")
                                .selected_text(match self.cfg.post_finale { PostFinale::Strip => "strip", PostFinale::Keep => "keep" })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut self.cfg.post_finale, PostFinale::Strip, "strip");
                                    ui.selectable_value(&mut self.cfg.post_finale, PostFinale::Keep,  "keep");
                                })
                                .response
                                .on_hover_text("\"strip\" removes the prefix from post-finale chapters.\n\"keep\" preserves it.");
                        });
                    });
                    ui.add_space(10.0);
                    // Zero-pad
                    theme::card().show(ui, |ui| {
                        theme::section_hdr(ui, "Zero-Padding");
                        if ui.checkbox(&mut self.cfg.zero_pad, RichText::new("Zero-pad numbers  (e.g. 01, 02 ...)").size(12.0)).changed() {
                            self.rebuild_sep_preview();
                        }
                        ui.horizontal(|ui| {
                            ui.add_space(20.0);
                            ui.add_enabled(self.cfg.zero_pad, egui::Label::new(RichText::new("Width:").color(theme::TXT).size(12.0)));
                            if ui.add_enabled(self.cfg.zero_pad, egui::DragValue::new(&mut self.cfg.pad_width).range(1..=5)).changed() {
                                self.rebuild_sep_preview();
                            }
                        });
                    });
                });
            });
        });
    }

    fn show_metadata(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().id_salt("meta_scr").show(ui, |ui| {
            egui::Frame::none()
                .inner_margin(egui::Margin::symmetric(20.0, 16.0))
                .show(ui, |ui| {

            let mut pending_dialog: Option<Dialog> = None;
            let mut pending_remove: bool = false;

            theme::card().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Constant Metadata  (applied to every CBZ)")
                        .color(theme::TXT).strong().size(12.5));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(
                            egui::Button::new(RichText::new("Remove").size(11.0).color(theme::TERR))
                                .fill(Color32::TRANSPARENT)
                                .stroke(egui::Stroke::new(1.0, theme::BDR))
                                .rounding(egui::Rounding::same(5.0))
                                .min_size(egui::vec2(0.0, 24.0))
                        ).on_hover_text("Remove the selected field below.").clicked() {
                            pending_remove = true;
                        }
                        ui.add_space(2.0);
                        if ui.add(
                            egui::Button::new(RichText::new("Add Tag").size(11.0).color(theme::TGOOD))
                                .fill(Color32::TRANSPARENT)
                                .stroke(egui::Stroke::new(1.0, theme::BDR))
                                .rounding(egui::Rounding::same(5.0))
                                .min_size(egui::vec2(0.0, 24.0))
                        ).on_hover_text("Add another ComicInfo field.").clicked() {
                            pending_dialog = Some(Dialog::AddMetadataTag);
                        }
                        ui.add_space(2.0);
                        if ui.add(
                            egui::Button::new(RichText::new("Tag Order").size(11.0).color(theme::ACC))
                                .fill(Color32::TRANSPARENT)
                                .stroke(egui::Stroke::new(1.0, theme::BDR))
                                .rounding(egui::Rounding::same(5.0))
                                .min_size(egui::vec2(0.0, 24.0))
                        ).on_hover_text("See and drag to rearrange the order tags are written to ComicInfo.xml.").clicked() {
                            pending_dialog = Some(Dialog::ReorderTags);
                        }
                    });
                });
                ui.add_space(8.0);

                // Display fields in the current tag order (custom via the Tag
                // Order dialog, defaulting to canonical schema order) rather
                // than insertion order, for a stable layout that always
                // matches what's actually written to the XML.
                let sel_tag = self.meta_field_sel.clone();
                let mut new_sel = sel_tag.clone();
                let mut order: Vec<usize> = (0..self.cfg.metadata_fields.len()).collect();
                order.sort_by_key(|&i| tag_rank(&self.cfg.metadata_fields[i].0, &self.cfg.tag_order));

                // ui.horizontal_wrapped() doesn't reliably detect the row
                // boundary when nested this deep inside Frame/ScrollArea --
                // content just overflows sideways instead of wrapping. Work
                // out row breaks manually instead: measure each field's
                // actual rendered label width and start a new row whenever
                // the next field wouldn't fit.
                let avail_w = (ui.available_width() - 16.0).max(100.0);
                let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
                let mut row_w: f32 = 0.0;
                for &i in &order {
                    let tag       = &self.cfg.metadata_fields[i].0;
                    let spec      = field_spec(tag);
                    let label_txt = spec.map(|s| s.label).unwrap_or(tag.as_str());
                    let width     = spec.map(|s| s.width).unwrap_or(150.0);
                    // Measure the actual rendered text width of "Label:" --
                    // more accurate than a flat per-character heuristic.
                    let label_w = ui.fonts(|f| {
                        f.layout_no_wrap(
                            format!("{label_txt}:"),
                            egui::FontId::proportional(13.0),
                            Color32::WHITE,
                        ).size().x
                    });
                    // selectable_label is button-styled: theme sets
                    // button_padding = (12.0, 6.0), adding 12px on EACH side
                    // of the text (24px total) -- previously unaccounted
                    // for, which is exactly why fields overflowed the row.
                    // item_spacing.x (8.0) is egui's automatic gap inserted
                    // between every pair of widgets in a horizontal layout;
                    // one occurs between the label and its input box, and
                    // another between this unit's input box and the next
                    // unit's label (no manual ui.add_space() needed for that
                    // -- removed from the render loop below to match).
                    let unit_w = 24.0 + label_w + 8.0 + width + 8.0;
                    if row_w + unit_w > avail_w && !rows.last().unwrap().is_empty() {
                        rows.push(Vec::new());
                        row_w = 0.0;
                    }
                    rows.last_mut().unwrap().push(i);
                    row_w += unit_w;
                }

                for row in &rows {
                    ui.horizontal(|ui| {
                        for &i in row {
                            let (tag, val) = &mut self.cfg.metadata_fields[i];
                            let spec      = field_spec(tag);
                            let label_txt = spec.map(|s| s.label).unwrap_or(tag.as_str());
                            let width     = spec.map(|s| s.width).unwrap_or(150.0);
                            let tip       = spec.map(|s| s.tip).unwrap_or("");
                            let is_sel    = sel_tag.as_deref() == Some(tag.as_str());

                            if ui.selectable_label(is_sel, format!("{label_txt}:")).clicked() {
                                new_sel = if is_sel { None } else { Some(tag.clone()) };
                            }
                            match spec.map(|s| s.kind) {
                                Some(FieldKind::Numeric { max_digits }) => {
                                    let r = ui.add(egui::TextEdit::singleline(val).desired_width(width));
                                    if r.changed() {
                                        *val = val.chars().filter(|c| c.is_ascii_digit())
                                            .take(max_digits).collect();
                                    }
                                    r.on_hover_text(tip);
                                }
                                Some(FieldKind::Decimal { min, max }) => {
                                    // CommunityRating's box accepts 0-10 instead of the
                                    // schema's real 0-5 when the user opts into rating on
                                    // a MAL/AniList-style 10 scale -- conversion happens
                                    // once, at XML-write time, not here (see worker.rs).
                                    let (min, max) = if tag.as_str() == "CommunityRating"
                                        && self.cfg.community_rating_10_scale
                                    {
                                        (0.0, 10.0)
                                    } else {
                                        (min, max)
                                    };
                                    let r = ui.add(egui::TextEdit::singleline(val)
                                        .desired_width(width)
                                        .hint_text(format!("{min:.0}-{max:.0}")));
                                    if r.changed() {
                                        let mut seen_dot = false;
                                        *val = val.chars().filter(|&c| {
                                            if c.is_ascii_digit() { true }
                                            else if c == '.' && !seen_dot { seen_dot = true; true }
                                            else { false }
                                        }).collect();
                                    }
                                    if r.lost_focus() {
                                        if let Ok(n) = val.parse::<f64>() {
                                            *val = format!("{:.1}", n.clamp(min, max));
                                        } else if !val.trim().is_empty() {
                                            val.clear();
                                        }
                                    }
                                    r.on_hover_text(tip);
                                }
                                Some(FieldKind::Enum(options)) => {
                                    egui::ComboBox::from_id_salt(format!("cb_{tag}"))
                                        .width(width)
                                        .selected_text(val.as_str())
                                        .show_ui(ui, |ui| {
                                            for opt in options {
                                                ui.selectable_value(val, opt.to_string(), *opt);
                                            }
                                        }).response.on_hover_text(tip);
                                }
                                _ => {
                                    Self::scrollable_text_edit(ui, tag, val, width)
                                        .on_hover_text(tip);
                                }
                            }
                        }
                    });
                    ui.add_space(4.0);
                }

                // Outside the measured-width grid above on purpose: this
                // toggle's width isn't accounted for in that grid's row-
                // wrapping math, and folding it in risks the exact overflow
                // bugs that math was written to fix.
                if self.cfg.metadata_fields.iter().any(|(t, _)| t == "CommunityRating") {
                    ui.add_space(2.0);
                    ui.checkbox(&mut self.cfg.community_rating_10_scale,
                        RichText::new("Rate Community Rating on a 1-10 scale (auto-converted to 0-5 in the XML)")
                            .size(11.0).color(theme::TDIM))
                        .on_hover_text(
                            "Enter your rating as given -- e.g. a MyAnimeList or AniList \
                             score out of 10. It's written to ComicInfo.xml as rating/10*5, \
                             matching the field's real 0-5 scale.");
                }

                self.meta_field_sel = new_sel;
            });

            if pending_remove {
                if let Some(tag) = self.meta_field_sel.take() {
                    self.cfg.metadata_fields.retain(|(t, _)| t != &tag);
                }
            }
            if pending_dialog.is_some() { self.dialog = pending_dialog; }

            ui.add_space(10.0);

            theme::card().show(ui, |ui| {
                theme::section_hdr(ui, "Default Summary  (Chapter 1 + fallback)");
                ui.add(egui::TextEdit::multiline(&mut self.cfg.summary)
                    .desired_rows(6).desired_width(f32::INFINITY)
                    .font(egui::FontId::new(12.0, egui::FontFamily::Monospace)));
            });

                }); // Frame
        });
    }

    fn show_rules(&mut self, ui: &mut egui::Ui) {
        // Single scrollbar for the whole tab -- individual tables are
        // never capped or independently scrollable. Each card gets an
        // equal "fair share" of the tab's height as its default size (so
        // 3 near-empty tables still look like they fill the window
        // instead of leaving a dead gap below them), but this is
        // computed once from the tab's height alone, independent of any
        // card's row count -- so a table with enough rows to need more
        // than its fair share just grows past it on its own, with zero
        // effect on the other two cards' size. (An earlier version
        // computed one shared "leftover space" pool from all 3 cards'
        // combined height, which meant growing one card shrank all
        // three back to bare minimum the moment the total stopped
        // fitting -- this avoids that entirely.)
        egui::ScrollArea::vertical().id_salt("rules_scr").show(ui, |ui| {
        egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(20.0, 16.0))
            .show(ui, |ui| {

        const CARD_PAD: f32 = 28.0;      // theme::card()'s 14px inner_margin, top + bottom
        const GAPS: f32 = 20.0;          // 2 x 10px add_space between the 3 cards
        const SAFETY_MARGIN: f32 = 10.0; // small buffer against layout rounding
        let fair_share = ((ui.available_height() - GAPS - SAFETY_MARGIN) / 3.0).max(100.0);

        // Renders one card and pads it up to fair_share if its actual
        // content (measured via ui.scope(), not estimated) is shorter --
        // never shrinks it below what its own rows need.
        let rule_card = |ui: &mut egui::Ui, add_contents: &mut dyn FnMut(&mut egui::Ui)| {
            theme::card().show(ui, |ui| {
                let r = ui.scope(|ui| add_contents(ui));
                let natural_h = r.response.rect.height() + CARD_PAD;
                let pad = (fair_share - natural_h).max(0.0);
                if pad > 0.0 { ui.add_space(pad); }
            });
        };

        rule_card(ui, &mut |ui| {
            if let Some(dlg) = Self::rule_section(
                ui, "Volume Rules   -   Chapter range -> Volume number",
                &[("Ch Start", 110.0),("Ch End", 110.0),("Volume", 110.0)],
                &mut self.cfg.volume_rules, &mut self.vol_sel, RuleTarget::Volume,
            ) { self.dialog = Some(dlg); }
        });
        ui.add_space(10.0);
        rule_card(ui, &mut |ui| {
            if let Some(dlg) = Self::rule_section(
                ui, "Date Rules   -   Volume range -> Publication Date",
                &[("Vol Start",90.0),("Vol End",90.0),("Year",70.0),("Month",70.0),("Day",70.0)],
                &mut self.cfg.date_rules, &mut self.date_sel, RuleTarget::Date,
            ) { self.dialog = Some(dlg); }
        });
        ui.add_space(10.0);
        rule_card(ui, &mut |ui| {
            if let Some(dlg) = Self::rule_section(
                ui, "Summary Rules   -   Volume range -> Custom Summary",
                &[("Vol Start",90.0),("Vol End",90.0),("Summary",560.0)],
                &mut self.cfg.summ_rules, &mut self.summ_sel, RuleTarget::Summary,
            ) { self.dialog = Some(dlg); }
        });

            }); // Frame
        }); // ScrollArea
    }

    fn show_run(&mut self, ui: &mut egui::Ui) {
        egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(20.0, 0.0))
            .show(ui, |ui| {
        ui.add_space(16.0);
        // ── Control bar ──────────────────────────────────────────────────────
        egui::Frame::none()
            .fill(theme::SURF)
            .stroke(egui::Stroke::new(1.0, theme::BDR))
            .rounding(egui::Rounding::same(8.0))
            .inner_margin(egui::Margin::symmetric(16.0, 6.0))
            .show(ui, |ui| {
                ui.allocate_exact_size(egui::vec2(ui.available_width(), 0.0), egui::Sense::hover());
                ui.horizontal(|ui| {
                    // Start
                    let start_btn = ui.add_enabled(
                        !self.running,
                        egui::Button::new(
                            RichText::new("  Start Processing  ")
                                .color(Color32::WHITE).size(13.0).strong()
                        )
                        .fill(Color32::from_rgb(0x16, 0x9a, 0x3c))
                        .rounding(egui::Rounding::same(8.0))
                        .min_size(egui::vec2(0.0, 32.0)),
                    );
                    if start_btn.clicked() { self.on_start(); }

                    ui.add_space(8.0);

                    // Stop
                    let stop_col = Color32::from_rgb(0xf8, 0x71, 0x71);
                    let stop_btn = ui.add_enabled(
                        self.running,
                        egui::Button::new(
                            RichText::new("  Stop  ").color(stop_col).size(13.0)
                        )
                        .fill(Color32::from_rgba_unmultiplied(0xf8, 0x71, 0x71, 18))
                        .stroke(egui::Stroke::new(1.5, stop_col))
                        .rounding(egui::Rounding::same(8.0))
                        .min_size(egui::vec2(0.0, 32.0)),
                    );
                    if stop_btn.clicked() {
                        use std::sync::atomic::Ordering;
                        self.stop_flag.store(true, Ordering::Relaxed);
                        self.status = "Stopping after current file...".to_string();
                    }

                    ui.add_space(16.0);

                    // Status indicators
                    if self.running {
                        ui.label(RichText::new("Processing...").color(theme::TGOOD).size(12.0));
                    }
                    if self.cfg.dry_run {
                        ui.add_space(8.0);
                        ui.label(RichText::new("DRY RUN -- files will NOT be modified")
                            .color(theme::TWARN).size(12.0).strong());
                    }

                    // Progress fraction right-aligned
                    let (done, total) = self.progress;
                    if total > 0 {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_space(4.0);
                            let pct = done as f32 / total as f32;
                            ui.label(RichText::new(format!("{done} / {total}"))
                                .color(theme::TDIM).size(11.0));
                            ui.add_space(8.0);
                            ui.label(RichText::new(format!("{}%", (pct * 100.0) as u32))
                                .color(theme::ACC2).strong().size(14.0));
                        });
                    }
                });
            });

        // ── Progress bar ─────────────────────────────────────────────────────
        let (done, total) = self.progress;
        let frac = if total > 0 { done as f32 / total as f32 } else { 0.0 };
        egui::Frame::none()
            .fill(theme::BDR)
            .inner_margin(egui::Margin::ZERO)
            .show(ui, |ui| {
                let bar_w = (ui.available_width() * frac).max(if self.running { 4.0 } else { 0.0 });
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 4.0), egui::Sense::hover()
                );
                ui.painter().rect_filled(rect, egui::Rounding::ZERO, theme::BDR);
                if bar_w > 0.0 {
                    let fill_rect = egui::Rect::from_min_size(rect.min, egui::vec2(bar_w, 4.0));
                    let col = if frac >= 1.0 { theme::TGOOD } else { theme::ACC };
                    ui.painter().rect_filled(fill_rect, egui::Rounding::ZERO, col);
                }
            });

        // ── Log header ────────────────────────────────────────────────────────
        egui::Frame::none()
            .fill(theme::SURF2)
            .stroke(egui::Stroke::new(1.0, theme::BDR))
            .rounding(egui::Rounding::same(8.0))
            .inner_margin(egui::Margin::symmetric(14.0, 6.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Log Output").color(theme::ACC2).strong().size(12.0));
                    ui.add_space(8.0);
                    ui.checkbox(&mut self.verbose, RichText::new("Verbose").color(theme::TDIM).size(11.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(
                            egui::Button::new(RichText::new("Clear").size(11.0).color(theme::TDIM))
                                .fill(Color32::TRANSPARENT)
                                .stroke(egui::Stroke::new(1.0, theme::BDR))
                                .rounding(egui::Rounding::same(4.0))
                        ).clicked() {
                            let has_content = !self.log.is_empty()
                                || !self.file_slots.is_empty()
                                || !self.log_footer.is_empty();
                            if has_content {
                                self.dialog = Some(Dialog::ConfirmClearLog);
                            }
                        }
                    });
                });
            });

        // ── Stats snapshot (before closures) ─────────────────────────────────
        let st = [
            ("Total",       self.disp_stats.total,     false),
            ("Processed",   self.disp_stats.processed, false),
            ("Renamed",     self.disp_stats.renamed,   false),
            ("Skipped",     self.disp_stats.skipped,   false),
            ("XML Updated", self.disp_stats.xml,       false),
            ("Errors",      self.disp_stats.errors,    self.disp_stats.errors > 0),
        ];
        // Stats frame real height ≈ frame padding (14*2=28) + content row
        // (~18px number + ~7px spacing + ~12px label ≈ 37px) ≈ 65px.
        // bottom_gap reserves clear empty space between the stats box and
        // the status bar below it, so the box's rounded corner is always
        // fully visible instead of running flush against the footer.
        let stats_h    = 68.0;   // now matches the smaller box (~58px real height)
        let bottom_gap = 26.0;   // explicit, more generous gap above the status bar
        let log_h = (ui.available_height() - stats_h - bottom_gap).max(60.0);

        // ── Log output ────────────────────────────────────────────────────────
        egui::Frame::none()
            .fill(theme::BG)
            .rounding(egui::Rounding::same(8.0))
            .inner_margin(egui::Margin::symmetric(14.0, 8.0))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("log_scr")
                    .max_height(log_h)
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.allocate_exact_size(egui::vec2(ui.available_width(), 0.0), egui::Sense::hover());

                        // 1. Header lines (Started/sep, resume/fresh-start notices)
                        for entry in &self.log {
                            ui.add(egui::Label::new(
                                RichText::new(&entry.text)
                                    .color(entry.level.color())
                                    .font(egui::FontId::new(12.0, egui::FontFamily::Monospace))
                            ).wrap_mode(egui::TextWrapMode::Extend));
                        }
                        // 2. Per-file blocks, in numeric order -- only files that
                        //    have actually completed are shown; gaps (files still
                        //    in progress on other threads) are simply skipped for
                        //    now and filled in once they arrive.
                        for slot in &self.file_slots {
                            if let Some(entries) = slot {
                                for entry in entries {
                                    ui.add(egui::Label::new(
                                        RichText::new(&entry.text)
                                            .color(entry.level.color())
                                            .font(egui::FontId::new(12.0, egui::FontFamily::Monospace))
                                    ).wrap_mode(egui::TextWrapMode::Extend));
                                }
                            }
                        }
                        // 3. Footer ([DONE] + separators), always last
                        for entry in &self.log_footer {
                            ui.add(egui::Label::new(
                                RichText::new(&entry.text)
                                    .color(entry.level.color())
                                    .font(egui::FontId::new(12.0, egui::FontFamily::Monospace))
                            ).wrap_mode(egui::TextWrapMode::Extend));
                        }
                    });
            });

        // ── Stats bar ─────────────────────────────────────────────────────────
        egui::Frame::none()
            .fill(theme::SURF)
            .stroke(egui::Stroke::new(1.0, theme::BDR))
            .rounding(egui::Rounding::same(8.0))
            .inner_margin(egui::Margin::symmetric(16.0, 8.0))
            .show(ui, |ui| {
                ui.allocate_exact_size(egui::vec2(ui.available_width(), 0.0), egui::Sense::hover());
                ui.horizontal(|ui| {
                    ui.style_mut().spacing.item_spacing.x = 0.0;
                    // Distribute all 6 items evenly across the full box width,
                    // instead of fixed-width columns left-packed with leftover
                    // empty space on the right.
                    let n = st.len() as f32;
                    let divider_w = 1.0;
                    let item_w  = ((ui.available_width() - divider_w * (n - 1.0)) / n).max(60.0);
                    let cell_h  = 42.0; // snug fit around content (~40px), minimal extra padding

                    for (i, &(lbl, val, is_err)) in st.iter().enumerate() {
                        let num_col = if is_err { theme::TERR }
                                      else if val > 0 { theme::TGOOD }
                                      else { theme::TMUT };
                        ui.allocate_ui_with_layout(
                            egui::vec2(item_w, cell_h),
                            egui::Layout::top_down(egui::Align::Center),
                            |ui| {
                                // Manual vertical centering: number line (~21px) +
                                // item spacing (~7px) + label line (~12px) ≈ 40px.
                                let content_h = 40.0_f32;
                                let pad = ((cell_h - content_h) / 2.0).max(0.0);
                                ui.add_space(pad);
                                ui.label(RichText::new(val.to_string())
                                    .color(num_col).strong().size(18.0));
                                ui.label(RichText::new(lbl).color(theme::TMUT).size(10.0));
                            },
                        );
                        if i < st.len() - 1 {
                            let (vl, _) = ui.allocate_exact_size(
                                egui::vec2(divider_w, cell_h), egui::Sense::hover()
                            );
                            ui.painter().rect_filled(vl, egui::Rounding::ZERO, theme::BDR);
                        }
                    }
                });
            });
            }); // Frame
    }
}