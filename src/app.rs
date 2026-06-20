use crate::{processing::*, state::*, theme, worker::{UiMsg, WorkerConfig, WorkerMsg}};
use eframe::egui::{self, Color32, RichText};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{atomic::AtomicBool, mpsc, Arc};

// ── Autosave ──────────────────────────────────────────────────────────────────
fn autosave_path() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".comicinfo_autosave.json")
}

// ── Tabs ──────────────────────────────────────────────────────────────────────
#[derive(Clone, Copy, PartialEq, Default)]
pub enum Tab { #[default] Paths, Processing, Metadata, Rules, Run }

// ── Rule-edit dialog state ────────────────────────────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RuleTarget { Volume, Date, Summary, CustomField }

#[derive(Debug, Clone)]
pub struct RuleEditState {
    pub target:  RuleTarget,
    pub row_idx: Option<usize>,
    pub labels:  Vec<String>,
    pub values:  Vec<String>,
    pub is_new:  bool,
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
    /// Shows the list of fields imported from a .py or .json metadata file
    ImportResult { filename: String, items: Vec<(String, String)> },
}

#[derive(Debug, Clone)]
pub enum PathPick { Folder, ChJson, VolJson, DateJson, LoadConfig, SaveConfig(String), ImportMeta }

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
    // Table selection
    pub vol_sel:  Option<usize>,
    pub date_sel: Option<usize>,
    pub summ_sel: Option<usize>,
    pub cust_sel: Option<usize>,
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
            vol_sel:     None, date_sel: None, summ_sel: None, cust_sel: None,
            dialog:      None,
            pick_kind:   None, pick_rx: None,
            running:     false,
            stop_flag:   Arc::new(AtomicBool::new(false)),
            worker_rx:   None, ui_tx: None,
            progress:    (0, 0),
            disp_stats:  DisplayStats::default(),
            log:         Vec::new(),
            pending_start: None,
            last_save:   std::time::Instant::now(),
        };
        app.load_autosave();
        app.rebuild_sep_preview();
        app
    }

    // ── Autosave ──────────────────────────────────────────────────────────────
    fn autosave(&self) {
        if let Ok(s) = serde_json::to_string_pretty(&self.cfg) {
            let _ = std::fs::write(autosave_path(), s);
        }
    }
    fn load_autosave(&mut self) {
        if let Ok(data) = std::fs::read_to_string(autosave_path()) {
            if let Ok(cfg) = serde_json::from_str::<AppConfig>(&data) {
                self.cfg = cfg;
                self.status = "Session restored.".to_string();
            }
        }
    }

    // ── Config ────────────────────────────────────────────────────────────────
    fn save_config(&self, path: &Path) {
        if let Ok(s) = serde_json::to_string_pretty(&self.cfg) { let _ = std::fs::write(path, s); }
    }
    fn load_config(&mut self, path: &Path) {
        if let Ok(data) = std::fs::read_to_string(path) {
            if let Ok(cfg) = serde_json::from_str::<AppConfig>(&data) {
                self.cfg = cfg; self.rebuild_sep_preview();
                self.status = format!("Loaded: {}", path.file_name().unwrap_or_default().to_string_lossy());
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

            pairs
        } else {
            // ── JSON file parser ──────────────────────────────────────────
            let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) else {
                self.dialog = Some(Dialog::Notice(
                    "Could not parse file as JSON.\nFor Python files use a .py extension.".to_string()
                ));
                return;
            };

            // Full app config?
            if let Some(map) = json.as_object() {
                if map.contains_key("folder") || map.contains_key("prefix_mode") {
                    if let Ok(cfg) = serde_json::from_str::<AppConfig>(&data) {
                        self.cfg = cfg;
                        self.rebuild_sep_preview();
                        self.dialog = Some(Dialog::ImportResult {
                            filename: fname.clone(),
                            items: vec![("Config".to_string(), "Full session config loaded".to_string())],
                        });
                        return;
                    }
                }
            }

            // Flat metadata dict
            json.as_object()
                .map(|map| {
                    map.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default()
        };

        // ── Apply extracted key-value pairs to config ─────────────────────
        for (key, val) in &kv_pairs {
            let display_val = if val.len() > 80 {
                format!("{}...", &val[..77])
            } else {
                val.clone()
            };
            match key.as_str() {
                "Series"          => { self.cfg.series     = val.clone(); imported.push(("Series".into(),          display_val)); }
                "Writer"          => { self.cfg.writer     = val.clone(); imported.push(("Writer".into(),          display_val)); }
                "Penciller"       => { self.cfg.penciller  = val.clone(); imported.push(("Penciller".into(),       display_val)); }
                "Publisher"       => { self.cfg.publisher  = val.clone(); imported.push(("Publisher".into(),       display_val)); }
                "LanguageISO"     => { self.cfg.language   = val.clone(); imported.push(("Language ISO".into(),    display_val)); }
                "AlternateSeries" => { self.cfg.alt_series = val.clone(); imported.push(("Alt. Series".into(),     display_val)); }
                "Web"             => { self.cfg.web        = val.clone(); imported.push(("Web".into(),             display_val)); }
                "Genre"           => { self.cfg.genre      = val.clone(); imported.push(("Genre".into(),           display_val)); }
                "Rating"          => { self.cfg.rating     = val.clone(); imported.push(("Rating".into(),          display_val)); }
                "Year"            => { self.cfg.year       = val.clone(); imported.push(("Year".into(),            display_val)); }
                "Month"           => { self.cfg.month      = val.clone(); imported.push(("Month".into(),           display_val)); }
                "Day"             => { self.cfg.day        = val.clone(); imported.push(("Day".into(),             display_val)); }
                "Count"           => { self.cfg.count      = val.clone(); imported.push(("Count".into(),           display_val)); }
                "Summary"         => { self.cfg.summary    = val.clone(); imported.push(("Summary".into(),         display_val)); }
                _ => {} // unknown field — silently skip
            }
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
    fn reset_all(&mut self) {
        self.cfg = AppConfig::default(); self.rebuild_sep_preview();
        self.status = "Reset to defaults.".to_string();
    }
    fn smart_filename(&self) -> String {
        let src = if !self.cfg.folder.is_empty() {
            Path::new(&self.cfg.folder).file_name().unwrap_or_default().to_string_lossy().into_owned()
        } else { self.cfg.series.clone() };
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
                PathPick::Folder     => rfd::FileDialog::new().pick_folder(),
                PathPick::ImportMeta |
                PathPick::ChJson | PathPick::VolJson | PathPick::DateJson =>
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
            Some(PathPick::ChJson)    => self.cfg.ch_json   = path.to_string_lossy().into(),
            Some(PathPick::VolJson)   => self.cfg.vol_json  = path.to_string_lossy().into(),
            Some(PathPick::DateJson)  => self.cfg.date_json = path.to_string_lossy().into(),
            Some(PathPick::LoadConfig) => self.load_config(&path),
            Some(PathPick::SaveConfig(_)) => {
                self.save_config(&path);
                self.status = format!("Saved: {}", path.file_name().unwrap_or_default().to_string_lossy());
            }
            Some(PathPick::ImportMeta) => self.import_meta(&path),
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
                Ok(WorkerMsg::LogBatch(lines)) => {
                    // All of one file's lines arrive together in one message;
                    // push them in one go so they stay contiguous in the log
                    // even though other files' batches may interleave with
                    // this one at the message level (unavoidable with true
                    // parallel processing, but each file's own block is safe).
                    self.log.extend(lines.into_iter().map(|(text, level)| LogEntry { text, level }));
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
                    self.disp_stats = DisplayStats {
                        total: stats.total, processed: stats.processed,
                        renamed: stats.renamed, skipped: stats.rename_skipped,
                        xml: stats.xml_updated, errors: stats.errors,
                    };
                    let sep = "-".repeat(60);
                    let ts  = chrono::Local::now().format("%H:%M:%S");
                    self.log.push(LogEntry { text: sep.clone(),                             level: LogLevel::Sep });
                    let (msg, lvl, st) = if stats.errors > 0 {
                        (format!("  [DONE] {ts}  -  {} errors", stats.errors), LogLevel::Warn,
                         format!("Done  -  {} error(s).", stats.errors))
                    } else {
                        (format!("  [DONE] {ts}  -  {} processed  -  {} renamed  -  0 errors",
                                 stats.processed, stats.renamed), LogLevel::Ok, "Done.".to_string())
                    };
                    self.log.push(LogEntry { text: msg, level: lvl });
                    self.log.push(LogEntry { text: sep, level: LogLevel::Sep });
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
        WorkerConfig {
            dry_run: self.cfg.dry_run,
            use_vol: self.cfg.use_vol, use_vol_date: self.cfg.use_vol_date, use_vol_summ: self.cfg.use_vol_summ,
            prefix_mode: self.cfg.prefix_mode.as_str().to_string(),
            custom_pfx:  self.cfg.custom_pfx.clone(),
            post_finale_mode: match self.cfg.post_finale { PostFinale::Strip => "strip", PostFinale::Keep => "keep" }.to_string(),
            use_csep: self.cfg.csep_on, csep: self.cfg.csep.clone(),
            zero_pad: self.cfg.zero_pad, pad_width: self.cfg.pad_width,
            series: self.cfg.series.clone(), writer: self.cfg.writer.clone(),
            penciller: self.cfg.penciller.clone(), publisher: self.cfg.publisher.clone(),
            language: self.cfg.language.clone(), alt_series: self.cfg.alt_series.clone(),
            web: self.cfg.web.clone(), genre: self.cfg.genre.clone(),
            rating: self.cfg.rating.clone(), year: self.cfg.year.clone(),
            month: self.cfg.month.clone(), day: self.cfg.day.clone(),
            count: self.cfg.count.clone(), summary: self.cfg.summary.clone(),
            custom_fields: self.cfg.custom_fields.clone(),
            volume_rules:  self.cfg.volume_rules.clone(),
            date_rules:    self.cfg.date_rules.clone(),
            summ_rules:    self.cfg.summ_rules.clone(),
            chapter_titles: safe_json_load(&self.cfg.ch_json),
            volume_titles:  safe_json_load(&self.cfg.vol_json),
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
        let titles = safe_json_load(&self.cfg.ch_json);
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
                egui::Window::new(if s.is_new { "Add Rule" } else { "Edit Rule" })
                    .resizable(true).collapsible(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        egui::Grid::new("re_g").num_columns(2).spacing([8.0,6.0]).show(ui, |ui| {
                            for (i, lbl) in s.labels.iter().enumerate() {
                                ui.label(RichText::new(lbl.as_str()).color(theme::TXT));
                                if lbl.to_lowercase().contains("summary") {
                                    ui.add(egui::TextEdit::multiline(&mut s.values[i]).desired_rows(4).desired_width(420.0));
                                } else {
                                    ui.add(egui::TextEdit::singleline(&mut s.values[i]).desired_width(280.0));
                                }
                                ui.end_row();
                            }
                        });
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.add(theme::btn_primary("  Save  ")).clicked() { saved = true; }
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
                        (RuleTarget::CustomField, None)    => self.cfg.custom_fields.push(vals),
                        (RuleTarget::CustomField, Some(i)) => { if i < self.cfg.custom_fields.len() { self.cfg.custom_fields[i] = vals; } }
                    }
                } else if !cancelled { self.dialog = Some(Dialog::EditRule(s)); }
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
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Helper widgets (static)
// ═══════════════════════════════════════════════════════════════════════════════
impl ComicInfoApp {


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
        id:      &str,
        cols:    &[(&str, f32)],
        rows:    &[Vec<String>],
        sel:     &mut Option<usize>,
        height:  f32,
    ) -> bool { // returns true if a row was double-clicked
        let mut dblclk = false;
        let total_w: f32 = cols.iter().map(|(_,w)| w).sum::<f32>() + 12.0;

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

        // Rows
        egui::ScrollArea::vertical()
            .id_salt(id).max_height(height).auto_shrink([false,false])
            .show(ui, |ui| {
                ui.set_min_width(total_w);
                for (i, row) in rows.iter().enumerate() {
                    let is_sel = *sel == Some(i);
                    let bg = if is_sel { theme::ACC } else if i%2==0 { theme::SURF2 } else { theme::ROW_ALT };
                    let tc = if is_sel { Color32::WHITE } else { theme::TXT };
                    let row_h = 24.0;
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width().max(total_w), row_h), egui::Sense::click());
                    if resp.clicked()        { *sel = Some(i); }
                    if resp.double_clicked() { *sel = Some(i); dblclk = true; }
                    if ui.is_rect_visible(rect) {
                        ui.painter().rect_filled(rect, egui::Rounding::ZERO, bg);
                        let mut cx = rect.left() + 6.0;
                        for (j, (_, w)) in cols.iter().enumerate() {
                            let txt = row.get(j).map(|s| s.as_str()).unwrap_or("");
                            // Clip text to column — rect must have positive dims or egui panics
                            let clip = egui::Rect::from_min_size(
                                egui::pos2(cx, rect.top()),
                                egui::vec2((w - 4.0).max(1.0), row_h.max(1.0)),
                            );
                            ui.painter().with_clip_rect(clip).text(
                                egui::pos2(cx, rect.center().y),
                                egui::Align2::LEFT_CENTER, txt,
                                egui::FontId::new(12.0, egui::FontFamily::Monospace), tc,
                            );
                            cx += w;
                        }
                    }
                }
            });
        dblclk
    }

    /// Reusable rule section: title + Add/Edit/Remove buttons + table.
    fn rule_section(
        ui:     &mut egui::Ui,
        title:  &str,
        id:     &str,
        cols:   &[(&str, f32)],
        rows:   &mut Vec<Vec<String>>,
        sel:    &mut Option<usize>,
        height: f32,
        target: RuleTarget,
    ) -> Option<Dialog> {
        let mut pending: Option<Dialog> = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).color(theme::ACC2).strong().size(12.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::new(RichText::new("Remove").size(11.0).color(theme::TERR)).fill(Color32::TRANSPARENT).stroke(egui::Stroke::new(1.0, theme::BDR)).rounding(egui::Rounding::same(5.0)).min_size(egui::vec2(0.0,24.0))).clicked() {
                    if let Some(idx) = *sel { if idx < rows.len() { rows.remove(idx); } *sel = None; }
                }
                ui.add_space(2.0);
                if ui.add(egui::Button::new(RichText::new("Edit").size(11.0).color(theme::ACC2)).fill(Color32::TRANSPARENT).stroke(egui::Stroke::new(1.0, theme::BDR)).rounding(egui::Rounding::same(5.0)).min_size(egui::vec2(0.0,24.0))).clicked() {
                    if let Some(idx) = *sel {
                        if let Some(row) = rows.get(idx) {
                            pending = Some(Dialog::EditRule(RuleEditState {
                                target, row_idx: Some(idx), is_new: false,
                                labels: cols.iter().map(|(h,_)| h.to_string()).collect(),
                                values: padded(row, cols.len()),
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
                    }));
                }
            });
        });
        let dbl = Self::table(ui, id, cols, rows, sel, height);
        if dbl {
            if let Some(idx) = *sel {
                if let Some(row) = rows.get(idx) {
                    pending = Some(Dialog::EditRule(RuleEditState {
                        target, row_idx: Some(idx), is_new: false,
                        labels: cols.iter().map(|(h,_)| h.to_string()).collect(),
                        values: padded(row, cols.len()),
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
                .inner_margin(egui::Margin::symmetric(20.0, 20.0))
                .show(ui, |ui| {

            theme::card().show(ui, |ui| {
                theme::section_hdr(ui, "File Paths");
                if Self::path_row(ui, "CBZ Folder:", &mut self.cfg.folder, "Folder containing the .cbz files.") {
                    self.start_pick(PathPick::Folder);
                }
                if Self::path_row(ui, "Chapter Titles JSON:", &mut self.cfg.ch_json, r#"{"1":"Title","2":"..."}"#) {
                    self.start_pick(PathPick::ChJson);
                }
                if Self::path_row(ui, "Volume Titles JSON:", &mut self.cfg.vol_json, r#"{"1":"Vol 1 Title"}"#) {
                    self.start_pick(PathPick::VolJson);
                }
                if Self::path_row(ui, "Episode Dates JSON:", &mut self.cfg.date_json, r#"{"1":"Jul 25, 2019"}"#) {
                    self.start_pick(PathPick::DateJson);
                }
            });
            ui.add_space(14.0);

            theme::card().show(ui, |ui| {
                theme::section_hdr(ui, "Processing Settings");
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Max Workers:").color(theme::TDIM).size(12.0));
                    ui.add_space(4.0);
                    ui.add(egui::DragValue::new(&mut self.cfg.workers)
                        .range(1..=32).speed(0.1));
                    ui.add_space(28.0);
                    ui.checkbox(&mut self.cfg.dry_run,
                        RichText::new("Dry Run  -  preview only, no files modified").size(12.0));
                });
                ui.add_space(6.0);
                let log_path = std::env::current_dir().unwrap_or_default().join("logs");
                ui.label(RichText::new(format!("Log directory: {}", log_path.display()))
                    .color(theme::TMUT).size(11.0));
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
                    // Mode
                    theme::card().show(ui, |ui| {
                        theme::section_hdr(ui, "Mode");
                        ui.horizontal(|ui| {
                            let was = self.cfg.mode.clone();
                            ui.radio_value(&mut self.cfg.mode, ComicMode::Manga, "Manga")
                                .on_hover_text("Turns ON all Volume Metadata options (default for most manga).");
                            ui.add_space(8.0);
                            ui.radio_value(&mut self.cfg.mode, ComicMode::Manhwa, "Manhwa / Manhua")
                                .on_hover_text("Turns OFF all Volume Metadata options (no volumes in manhwa).");
                            if self.cfg.mode != was {
                                let is_m = matches!(self.cfg.mode, ComicMode::Manga);
                                self.cfg.use_vol = is_m; self.cfg.use_vol_date = is_m; self.cfg.use_vol_summ = is_m;
                            }
                        });
                    });
                    ui.add_space(10.0);
                    // Volume metadata
                    theme::card().show(ui, |ui| {
                        theme::section_hdr(ui, "Volume Metadata");
                        ui.checkbox(&mut self.cfg.use_vol, RichText::new("Include volume number in metadata").size(12.0))
                            .on_hover_text("Enables Volume field in ComicInfo.xml. Disable for manhwa.");
                        ui.checkbox(&mut self.cfg.use_vol_date, RichText::new("Use volume date rules for publication").size(12.0))
                            .on_hover_text("Overrides Year/Month/Day from Date Rules table. Disable for manhwa.");
                        ui.checkbox(&mut self.cfg.use_vol_summ, RichText::new("Use per-volume summary rules").size(12.0))
                            .on_hover_text("Overrides Summary from Summary Rules table. Disable for manhwa.");
                    });
                    ui.add_space(10.0);
                    // Post-finale
                    theme::card().show(ui, |ui| {
                        theme::section_hdr(ui, "Post-Finale Behaviour");
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
                        ui.checkbox(&mut self.cfg.zero_pad, RichText::new("Zero-pad numbers  (e.g. 01, 02 ...)").size(12.0));
                        ui.horizontal(|ui| {
                            ui.add_space(20.0);
                            ui.add_enabled(self.cfg.zero_pad, egui::Label::new(RichText::new("Width:").color(theme::TXT).size(12.0)));
                            ui.add_enabled(self.cfg.zero_pad, egui::DragValue::new(&mut self.cfg.pad_width).range(1..=5));
                        });
                    });
                });

                let rc = &mut cols[1];
                egui::Frame::none().outer_margin(egui::Margin { left:8.0, right:20.0, ..Default::default() }).show(rc, |ui| {
                    // Prefix mode
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
                });
            });
        });
    }

    fn show_metadata(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().id_salt("meta_scr").show(ui, |ui| {
            egui::Frame::none()
                .inner_margin(egui::Margin::symmetric(20.0, 16.0))
                .show(ui, |ui| {

            theme::card().show(ui, |ui| {
                theme::section_hdr(ui, "Constant Metadata  (applied to every CBZ)");
                // Split into two equal columns; each has its own label+field grid
                // so field widths are independent of label text length.
                let lbl = |t: &str| RichText::new(t).color(theme::TXT).size(12.0);
                let (s, w, pe, pu, l, al, g, r, y, mo, d, c) = (
                    &mut self.cfg.series,   &mut self.cfg.writer,
                    &mut self.cfg.penciller,&mut self.cfg.publisher,
                    &mut self.cfg.language, &mut self.cfg.alt_series,
                    &mut self.cfg.genre,    &mut self.cfg.rating,
                    &mut self.cfg.year,     &mut self.cfg.month,
                    &mut self.cfg.day,      &mut self.cfg.count,
                );
                ui.columns(2, |cols| {
                    egui::Grid::new("mgl").num_columns(2).spacing([8.0,6.0])
                        .min_col_width(0.0).show(&mut cols[0], |ui| {
                            ui.label(lbl("Series:"));
                            ui.add(egui::TextEdit::singleline(s).desired_width(f32::INFINITY))
                                .on_hover_text("Comic series title."); ui.end_row();
                            ui.label(lbl("Penciller:"));
                            ui.add(egui::TextEdit::singleline(pe).desired_width(f32::INFINITY))
                                .on_hover_text("Penciller / illustrator."); ui.end_row();
                            ui.label(lbl("Language ISO:"));
                            ui.add(egui::TextEdit::singleline(l).desired_width(f32::INFINITY))
                                .on_hover_text("ISO code: \"en\", \"ja\", \"ko\" ..."); ui.end_row();
                            ui.label(lbl("Genre:"));
                            ui.add(egui::TextEdit::singleline(g).desired_width(f32::INFINITY))
                                .on_hover_text("Genres, comma-separated."); ui.end_row();
                            ui.label(lbl("Year:"));
                            ui.add(egui::TextEdit::singleline(y).desired_width(f32::INFINITY))
                                .on_hover_text("Default publication year."); ui.end_row();
                            ui.label(lbl("Day:"));
                            ui.add(egui::TextEdit::singleline(d).desired_width(f32::INFINITY))
                                .on_hover_text("Default publication day."); ui.end_row();
                        });
                    egui::Grid::new("mgr").num_columns(2).spacing([8.0,6.0])
                        .min_col_width(0.0).show(&mut cols[1], |ui| {
                            ui.label(lbl("Writer:"));
                            ui.add(egui::TextEdit::singleline(w).desired_width(f32::INFINITY))
                                .on_hover_text("Script writer / author."); ui.end_row();
                            ui.label(lbl("Publisher:"));
                            ui.add(egui::TextEdit::singleline(pu).desired_width(f32::INFINITY))
                                .on_hover_text("Publisher names, comma-separated."); ui.end_row();
                            ui.label(lbl("Alt. Series:"));
                            ui.add(egui::TextEdit::singleline(al).desired_width(f32::INFINITY))
                                .on_hover_text("Original / alternate series title."); ui.end_row();
                            ui.label(lbl("Rating:"));
                            ui.add(egui::TextEdit::singleline(r).desired_width(f32::INFINITY))
                                .on_hover_text("Score, e.g. 7.7"); ui.end_row();
                            ui.label(lbl("Month:"));
                            ui.add(egui::TextEdit::singleline(mo).desired_width(f32::INFINITY))
                                .on_hover_text("Default publication month."); ui.end_row();
                            ui.label(lbl("Count:"));
                            ui.add(egui::TextEdit::singleline(c).desired_width(f32::INFINITY))
                                .on_hover_text("Total chapter / volume count."); ui.end_row();
                        });
                });
                // Web — full width below both columns
                ui.horizontal(|ui| {
                    ui.label(lbl("Web:"));
                    ui.add(egui::TextEdit::singleline(&mut self.cfg.web)
                        .desired_width(f32::INFINITY))
                        .on_hover_text("Space-separated URLs for the series.");
                });
            });
            ui.add_space(10.0);

            theme::card().show(ui, |ui| {
                if let Some(dlg) = Self::rule_section(
                    ui, "Custom XML Fields", "cf",
                    &[("Field Name", 200.0), ("Value", 500.0)],
                    &mut self.cfg.custom_fields,
                    &mut self.cust_sel, 110.0, RuleTarget::CustomField,
                ) { self.dialog = Some(dlg); }
            });
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
        // Divide available height evenly among the 3 rule sections.
        // Each section header+buttons = ~38px, frame margin = ~24px, gaps = ~20px.
        let overhead_per_section = 38.0 + 24.0;
        let total_overhead       = 3.0 * overhead_per_section + 2.0 * 10.0 + 32.0 + 16.0; // gaps + frame margin
        let table_h = ((ui.available_height() - total_overhead) / 3.0).max(80.0);

        egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(20.0, 8.0))
            .show(ui, |ui| {

        theme::card().show(ui, |ui| {
            if let Some(dlg) = Self::rule_section(
                ui, "Volume Rules   -   Chapter range -> Volume number", "vr",
                &[("Ch Start", 110.0),("Ch End", 110.0),("Volume", 110.0)],
                &mut self.cfg.volume_rules, &mut self.vol_sel, table_h, RuleTarget::Volume,
            ) { self.dialog = Some(dlg); }
        });
        ui.add_space(10.0);
        theme::card().show(ui, |ui| {
            if let Some(dlg) = Self::rule_section(
                ui, "Date Rules   -   Volume range -> Publication Date", "dr",
                &[("Vol Start",90.0),("Vol End",90.0),("Year",70.0),("Month",70.0),("Day",70.0)],
                &mut self.cfg.date_rules, &mut self.date_sel, table_h, RuleTarget::Date,
            ) { self.dialog = Some(dlg); }
        });
        ui.add_space(10.0);
        theme::card().show(ui, |ui| {
            if let Some(dlg) = Self::rule_section(
                ui, "Summary Rules   -   Volume range -> Custom Summary", "sr",
                &[("Vol Start",90.0),("Vol End",90.0),("Summary",560.0)],
                &mut self.cfg.summ_rules, &mut self.summ_sel, table_h, RuleTarget::Summary,
            ) { self.dialog = Some(dlg); }
        });

            }); // Frame
    }

    fn show_run(&mut self, ui: &mut egui::Ui) {
        egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(20.0, 0.0))
            .show(ui, |ui| {
        ui.add_space(10.0);
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
                        ).clicked() { self.log.clear(); }
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
                        for entry in &self.log {
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