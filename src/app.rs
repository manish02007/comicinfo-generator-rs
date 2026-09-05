use crate::{processing::*, state::*, theme, worker::{UiMsg, WorkerConfig, WorkerMsg}};
use eframe::egui::{self, Color32, RichText};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{atomic::AtomicBool, mpsc, Arc};

// ── Single File Mode: ComicInfo tags this app understands beyond the
// generic COMICINFO_FIELDS registry -- Title/Number/Volume/Summary are
// deliberately excluded from that registry (see processing.rs) because
// batch mode derives them from filenames/rules rather than free-typing
// them, but Single File Mode has no filename-parsing pipeline to derive
// them from, so all four need to be ordinary editable tags here. Series is
// NOT listed -- it's already a normal COMICINFO_FIELDS entry.
const SFM_EXTRA_KNOWN_TAGS: &[&str] = &["Title", "Number", "Volume", "Summary"];

// Shared reserved buffer subtracted from available_height() before handing
// it to a ScrollArea as an explicit max_height, in both sfm_file_tree and
// sfm_editor_editing_ui -- see either call site's comment for why this
// needs to be one shared constant rather than each panel picking its own
// value.
const SFM_SCROLL_BOTTOM_GAP: f32 = 14.0;

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
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
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

// ── Single File Mode ──────────────────────────────────────────────────────────
/// One entry in the left-side file tree.
#[derive(Debug, Clone)]
pub struct SfmFileEntry {
    pub path:    PathBuf,
    pub name:    String,
    pub is_cbz:  bool,
}

/// A single tag row in the ComicInfo.xml editor, in on-screen order.
/// Foreign (unrecognized) tags carry the same shape as known ones --
/// field_spec(&tag) returning None at render time is what flags a row as
/// foreign, rather than a separate variant/flag duplicated here, so a tag
/// can never disagree with the registry about whether it's known.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct SfmTagRow {
    pub tag:   String,
    pub value: String,
    // Stable identity for egui_dnd's drag-and-drop reordering, independent
    // of `tag`/`value` -- matches the existing Tag Order dialog's own
    // pattern of giving egui_dnd something that doesn't change identity
    // just because the user edited a field's text.
    pub id:    u64,
}

/// What the right-hand panel is currently showing for the selected file.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum SfmPanelState {
    /// Nothing selected yet (no file opened, or folder opened but empty).
    #[default]
    Empty,
    /// Selected file has no ComicInfo.xml -- offering to create one.
    NoComicInfo,
    /// Selected file's ComicInfo.xml is loaded and being edited.
    Editing,
    /// The archive itself couldn't be read (corrupt zip, not a real CBZ).
    LoadError(String),
}

/// Where a pending file-switch (or mode-exit) in Single File Mode is headed,
/// once the user resolves any unsaved-changes prompt. A plain
/// Option<usize> can't distinguish "no navigation pending" from "pending
/// navigation is to leave the mode" (index None is a valid destination in
/// its own right, not the same as no destination at all).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SfmNavTarget {
    File(usize),
    ExitMode,
}

/// One undo/redo step: the full tag list for `file` immediately before
/// the change this entry represents. Whole-list snapshots rather than a
/// diff -- at word-level granularity (see SfmState::pending_undo_word)
/// the history is short enough that snapshot cost is a non-issue, and
/// snapshots can never drift out of sync with reality the way a
/// hand-rolled diff/patch representation could if a future edit here
/// missed a case (add/remove/reorder all naturally fall out of "the
/// whole list looked like this" with no extra bookkeeping).
#[derive(Debug, Clone)]
pub struct SfmUndoEntry {
    pub file:   PathBuf,
    pub before: Vec<SfmTagRow>,
    pub after:  Vec<SfmTagRow>,
}

/// All Single File Mode state, held on ComicInfoApp only while
/// settings.single_file_mode is true. Kept as one sub-struct rather than
/// flattened onto ComicInfoApp directly so it's obvious at a glance which
/// fields belong to this mode versus the batch-processing tabs, and so
/// resetting/clearing it on mode-exit is one assignment instead of
/// resetting a dozen scattered fields individually.
#[derive(Debug, Clone, Default)]
pub struct SfmState {
    // The single file or folder the user opened. Some(dir) for a folder
    // (even a folder containing exactly one file) vs Some(file) with no
    // sibling entries for a directly-opened single file -- root itself
    // isn't shown in the tree, only `files` is.
    pub root:            Option<PathBuf>,
    pub files:            Vec<SfmFileEntry>,
    pub selected:          Option<usize>,
    pub panel:              SfmPanelState,
    // The tags currently shown/edited in the right panel.
    pub tags:                 Vec<SfmTagRow>,
    // Snapshot of `tags` exactly as loaded (or as last saved), for dirty-
    // checking and for Discard to revert to. Not kept in sync on every
    // keystroke -- only refreshed on load and on successful save.
    pub loaded_tags:            Vec<SfmTagRow>,
    // Foreign tags detected on the currently-loaded file (field_spec(tag)
    // == None), surfaced to the user once per load rather than silently.
    pub foreign_tags_notice:      Option<Vec<String>>,
    // Monotonic counter for SfmTagRow::id -- see its doc comment.
    pub next_row_id:                u64,
    // The navigation the user requested while the current selection had
    // unsaved changes -- holds the pending target while the
    // Save/Discard/Cancel prompt (Dialog::SfmUnsavedChanges) is showing.
    pub pending_nav:                   Option<SfmNavTarget>,
    // Snapshot of the WHOLE tag list taken just before the word currently
    // being typed started, held until the word finishes (a boundary
    // character is typed, or the field loses focus) and gets pushed to
    // the undo stack as one step -- see sfm_editor_editing_ui's per-row
    // rendering. None means no word is currently in progress anywhere in
    // this file's editor. Dropped without committing on file switch,
    // same as everything else in-progress on the old file (a half-typed
    // word doesn't need its own undo step once you've navigated away
    // from it -- the file-switch save/discard prompt already covers
    // whether that edit is kept at all).
    pub pending_undo_word:               Option<Vec<SfmTagRow>>,
    // Snapshot to apply once a pending navigation (see pending_nav)
    // resolves -- set when Ctrl+Z/Y needs to jump to a different file
    // that also requires the Save/Discard/Cancel prompt first (auto-
    // save-on-focus-change is off and the current file has unsaved
    // changes). The file switch itself happens via the normal
    // pending_nav machinery; this is only the extra "and then also apply
    // this undo/redo step" instruction layered on top of it. None in
    // every other case -- undo/redo targeting the already-open file, or
    // a different file that didn't need the prompt, apply immediately
    // with no deferral.
    pub pending_undo_apply:              Option<Vec<SfmTagRow>>,
    // Whether the "Add Tag" floating menu (a plain egui::Window, not a
    // real egui::ComboBox -- this app's pinned egui 0.29 predates
    // ComboBox::close_behavior/the Popup rewrite that would let a
    // combobox stay open after a selection, so a small manually-
    // positioned window is used instead) is currently showing. Stays
    // true across multiple tag picks in a row (each pick just pushes a
    // row, it doesn't touch this flag) and is only set false by an
    // explicit click outside the menu's own rect.
    pub add_tag_menu_open:                bool,
}

impl SfmState {
    pub fn dirty(&self) -> bool {
        self.tags.iter().map(|r| (&r.tag, &r.value)).collect::<Vec<_>>()
            != self.loaded_tags.iter().map(|r| (&r.tag, &r.value)).collect::<Vec<_>>()
    }
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
    /// Detailed explanation for a tab or card's "?" help button.
    HelpText { title: String, body: String },
    /// Single File Mode: switching to a different file, or leaving the
    /// mode entirely, while the currently-selected file has unsaved tag
    /// edits. The actual destination lives in SfmState::pending_nav
    /// (set before this dialog opens) rather than duplicated here.
    SfmUnsavedChanges,
}

#[derive(Debug, Clone)]
pub enum PathPick { Folder, TitlesJson, DateJson, LoadConfig, SaveConfig(String), ImportMeta, OutputPath,
    SfmFile, SfmFolder }

#[derive(Default, Clone)]
pub struct DisplayStats {
    pub total: usize, pub processed: usize, pub renamed: usize,
    pub skipped: usize, pub xml: usize, pub errors: usize,
}

// ── Main struct ───────────────────────────────────────────────────────────────
pub struct ComicInfoApp {
    pub cfg:   AppConfig,
    pub tab:   Tab,
    // Last frame's tab, and when the switch to it happened -- used to
    // compute the tab-fade-in animation's progress directly from elapsed
    // wall-clock time (see update()), rather than relying on egui's
    // animate_bool_with_time, whose per-Id memory only starts at 0.0 the
    // very first time that Id is ever queried. Since a tab's Id would
    // already be "settled" at 1.0 from any earlier visit, later visits
    // showed no animation at all -- this timer-based approach sidesteps
    // that by tracking the switch ourselves, same pattern already used
    // for the theme cross-fade's Transition struct.
    pub prev_tab: Tab,
    pub tab_switched_at: std::time::Instant,
    // Same pattern, one level up: fades in whichever mode (Single File
    // Mode vs batch tabs) just became active, on the frame
    // settings.single_file_mode actually changes. Only the freshly-
    // active mode's own content fades in -- there's no attempt to
    // cross-fade the outgoing mode out at the same time, since the two
    // are structurally different layouts (Single File Mode adds a
    // SidePanel batch mode doesn't have at all), and briefly rendering
    // both together to cross-fade risks real layout/overlap glitches
    // that a simple fade-in for the incoming side avoids entirely.
    pub prev_single_file_mode: bool,
    pub mode_switched_at: std::time::Instant,
    // Whether self.dialog was Some(_) last frame, and when it most
    // recently transitioned from None to Some -- drives the same
    // fade-in treatment as tabs for popup/dialog windows (Notice,
    // Confirm Reset, the "?" help windows, Settings, etc). Dialog
    // carries variant data that doesn't cleanly derive PartialEq/Hash
    // (Vec<PathBuf>, HashSet<String>, ...), so this only tracks the
    // None/Some transition rather than which specific dialog is
    // showing -- sufficient since dialogs are modal (only one shown at
    // a time) and this app never swaps directly from one dialog type
    // to another without a None frame in between.
    pub dialog_was_open: bool,
    pub dialog_opened_at: std::time::Instant,
    // Fade-out companions to the fade-in fields above. last_dialog holds
    // onto the most recently shown dialog's data during its close
    // animation -- self.dialog itself is already None by the time
    // closing starts (that's exactly how a close is detected), so
    // something has to keep the content around to keep rendering it
    // while it fades. dialog_closing/dialog_closed_at drive that timer.
    pub last_dialog: Option<Dialog>,
    pub dialog_closing: bool,
    pub dialog_closed_at: std::time::Instant,
    pub sep_preview: String,
    pub status:      String,
    pub verbose:     bool,
    // App-wide settings (backup-before-overwrite, completion sound) --
    // persisted separately from AppConfig, see state.rs::AppSettings.
    pub settings:      AppSettings,
    pub settings_open: bool,
    pub settings_was_open: bool,
    pub settings_opened_at: std::time::Instant,
    pub settings_closing: bool,
    pub settings_closed_at: std::time::Instant,
    // The Settings toolbar button's own on-screen rect, recorded every
    // frame show_toolbar renders it. show_settings_window reads this to
    // exclude the button itself from its click-outside-closes check --
    // same reasoning as the Add Tag menu's own click-outside handling:
    // without excluding the button, the very click that just opened
    // Settings would also register as "outside the window" (since the
    // window doesn't exist yet on the frame the button is clicked) and
    // immediately start closing it again.
    pub settings_btn_rect: egui::Rect,
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
    // Single File Mode's file tree + editor state. Only meaningfully
    // populated while settings.single_file_mode is true; left at its
    // Default when the mode isn't active rather than allocated lazily, so
    // toggling the mode never needs a None-vs-Some(default) distinction.
    pub sfm: SfmState,
    // Single File Mode undo/redo, shared across every file (not reset on
    // file switch or re-entering the mode -- lives on ComicInfoApp
    // directly rather than inside SfmState, which DOES get reset on
    // switch/exit, precisely so history survives both). `undo_cursor` is
    // the index of the next entry Ctrl+Z would apply (one past the most
    // recent applied undo, so Ctrl+Y re-applies it); a fresh edit made
    // after undoing truncates everything from undo_cursor onward, same
    // as any standard undo/redo stack.
    pub sfm_undo_stack:  Vec<SfmUndoEntry>,
    pub sfm_undo_cursor: usize,
}

impl ComicInfoApp {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        theme::setup_style(&cc.egui_ctx);
        let mut app = Self {
            cfg:         AppConfig::default(),
            tab:         Tab::default(),
            prev_tab:    Tab::default(),
            tab_switched_at: std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(1))
                .unwrap_or_else(std::time::Instant::now),
            prev_single_file_mode: false,
            mode_switched_at: std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(1))
                .unwrap_or_else(std::time::Instant::now),
            dialog_was_open: false,
            dialog_opened_at: std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(1))
                .unwrap_or_else(std::time::Instant::now),
            last_dialog: None,
            dialog_closing: false,
            dialog_closed_at: std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(1))
                .unwrap_or_else(std::time::Instant::now),
            sep_preview: String::new(),
            status:      "Ready.".to_string(),
            verbose:     false,
            settings:      AppSettings::default(),
            settings_open: false,
            settings_was_open: false,
            settings_opened_at: std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(1))
                .unwrap_or_else(std::time::Instant::now),
            settings_closing: false,
            settings_closed_at: std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(1))
                .unwrap_or_else(std::time::Instant::now),
            settings_btn_rect: egui::Rect::ZERO, // corrected next frame once show_toolbar renders it
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
            sfm: SfmState::default(),
            sfm_undo_stack: Vec::new(),
            sfm_undo_cursor: 0,
        };
        app.load_autosave();
        app.load_settings();
        app.sfm_restore_last_session();
        // Apply the saved theme choice now, instantly (no cross-fade --
        // that's reserved for user-triggered switches after startup, not
        // the app's first frame). theme::set_theme always animates, so
        // this calls setup_style directly instead to avoid a startup flash
        // of the default theme before the saved one takes over.
        theme::apply_theme_immediately(app.settings.theme);
        theme::setup_style(&cc.egui_ctx);
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
                // Legacy flat metadata fields (series, writer, publisher, ...)
                // predate the dynamic metadata_fields list (v0.3.0-beta.1)
                // and were never named AppConfig struct fields, so the
                // deserialize above can't see them -- serde's container-level
                // #[serde(default)] fills a missing metadata_fields key with
                // AppConfig::default()'s pre-populated starter list (Series,
                // Writer, ... with empty values), NOT an empty Vec, which is
                // why the inner logic below overwrites a matching tag rather
                // than skipping it as "already present". Import already
                // migrates these via legacy_field_alias/field_spec; Load
                // needs the same migration since it's just as valid a
                // source of an old config file, reusing that logic here
                // rather than duplicating it.
                if let Ok(raw) = serde_json::from_str::<serde_json::Value>(&data) {
                    if let Some(map) = raw.as_object() {
                        for (key, val) in map.iter().filter_map(|(k, v)| v.as_str().map(|s| (k, s))) {
                            if key.eq_ignore_ascii_case("Summary") {
                                continue; // Summary already has its own named AppConfig field
                            }
                            let resolved = field_spec(key).map(|s| (s.tag, false))
                                .or_else(|| Self::legacy_field_alias(key));
                            if let Some((tag, is_legacy_rating)) = resolved {
                                if is_legacy_rating {
                                    cfg.community_rating_10_scale = true;
                                }
                                // AppConfig::default().metadata_fields (used
                                // by serde's container-level #[serde(default)]
                                // to fill in the WHOLE metadata_fields list
                                // when it's absent from the JSON, which it
                                // always is in this old flat-field format)
                                // pre-populates Series/Writer/Publisher/...
                                // with EMPTY values. So `tag` frequently
                                // already exists here as an empty starter
                                // placeholder, not as a real prior value --
                                // overwrite it in that case instead of
                                // treating "tag exists" as "already handled,
                                // skip", which silently discarded the real
                                // value being migrated in.
                                if let Some(entry) = cfg.metadata_fields.iter_mut().find(|(t, _)| t == tag) {
                                    entry.1 = val.to_string();
                                } else {
                                    cfg.metadata_fields.push((tag.to_string(), val.to_string()));
                                }
                            }
                        }
                    }
                }
                let loaded_version = cfg.config_version;
                cfg.config_version = CURRENT_CONFIG_VERSION;
                self.cfg = cfg;
                self.rebuild_sep_preview();
                let fname = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                self.status = format!("Loaded: {fname}");
                // Every successful load gets a confirmation popup, not
                // just version mismatches -- previously a normal,
                // matching-version load only updated the small dim
                // status-bar text, which is easy to miss entirely,
                // especially for something as consequential as replacing
                // the whole current session (which Load's own tooltip
                // now explicitly warns about). The version-mismatch cases
                // still get their specific warning, appended to this same
                // confirmation instead of being the only time a dialog
                // appears at all.
                let mut msg = format!("Loaded '{fname}'.\n\nThe entire session was replaced with this file's settings.");
                // A version mismatch means this config predates (or postdates)
                // a structural change to AppConfig -- serde's #[serde(default)]
                // already prevented a hard load failure, but fields that were
                // renamed/restructured since then won't have carried over.
                // Surface that explicitly rather than letting it look like
                // silently "lost" data with no explanation.
                if loaded_version < CURRENT_CONFIG_VERSION {
                    msg.push_str(&format!(
                        "\n\nThis was saved with an older version of this app \
                         (config v{loaded_version} vs current v{CURRENT_CONFIG_VERSION}). \
                         Some fields may not have carried over if the config \
                         format changed since then -- worth double-checking the \
                         Metadata tab before running."
                    ));
                } else if loaded_version > CURRENT_CONFIG_VERSION {
                    msg.push_str(&format!(
                        "\n\nThis was saved with a NEWER version of this app \
                         (config v{loaded_version} vs current v{CURRENT_CONFIG_VERSION}). \
                         Some fields may not load correctly -- consider updating \
                         the app if you run into issues."
                    ));
                }
                self.dialog = Some(Dialog::Notice(msg));
            } else {
                let fname = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                self.dialog = Some(Dialog::Notice(format!(
                    "Could not load '{fname}': the file isn't a valid config \
                     (it may be a different kind of JSON file, or corrupted)."
                )));
            }
        } else {
            self.dialog = Some(Dialog::Notice(format!(
                "Could not read file:\n{}", path.display()
            )));
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
                        // Merge, not replace: this used to be `self.cfg =
                        // cfg`, which silently did a full session
                        // wipe-and-replace -- identical to what the Load
                        // button does, but with none of Load's version-
                        // mismatch warning, and while the person clicking
                        // "Import" reasonably expects an ADDITIVE merge
                        // into their current session (that's the entire
                        // reason Import exists as a separate button from
                        // Load). Only the structural, non-metadata-field
                        // parts are pulled in here -- rules, paths, and
                        // prefix/separator/mode settings, each only if the
                        // incoming file actually has something in it.
                        // Metadata FIELDS are deliberately left to the
                        // flat-field scan below (which already runs
                        // unconditionally after this block) rather than
                        // merged here too, to avoid applying them twice.
                        if !cfg.volume_rules.is_empty() {
                            imported.push(("Volume Rules".to_string(), format!("{} rule(s)", cfg.volume_rules.len())));
                            self.cfg.volume_rules = cfg.volume_rules;
                        }
                        if !cfg.date_rules.is_empty() {
                            imported.push(("Date Rules".to_string(), format!("{} rule(s)", cfg.date_rules.len())));
                            self.cfg.date_rules = cfg.date_rules;
                        }
                        if !cfg.summ_rules.is_empty() {
                            imported.push(("Summary Rules".to_string(), format!("{} rule(s)", cfg.summ_rules.len())));
                            self.cfg.summ_rules = cfg.summ_rules;
                        }
                        if !cfg.folder.trim().is_empty() {
                            imported.push(("CBZ Folder".to_string(), cfg.folder.clone()));
                            self.cfg.folder = cfg.folder;
                        }
                        if !cfg.titles_json.trim().is_empty() {
                            imported.push(("Titles JSON".to_string(), cfg.titles_json.clone()));
                            self.cfg.titles_json = cfg.titles_json;
                        }
                        if !cfg.date_json.trim().is_empty() {
                            imported.push(("Episode Dates JSON".to_string(), cfg.date_json.clone()));
                            self.cfg.date_json = cfg.date_json;
                        }
                        self.rebuild_sep_preview();
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
                    if let Some(m) = re_anynum().find(s) {
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
                PathPick::SfmFile =>
                    rfd::FileDialog::new().add_filter("CBZ Comic Archive", &["cbz"]).pick_file(),
                PathPick::SfmFolder => rfd::FileDialog::new().pick_folder(),
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
            Some(PathPick::SfmFile)   => self.sfm_open_path(path, false),
            Some(PathPick::SfmFolder) => self.sfm_open_path(path, true),
            None => {}
        }
    }

    // ── Single File Mode ──────────────────────────────────────────────────────
    // Opens a single file or a folder into the SFM file tree. For a single
    // file, the tree shows just that one entry (still routed through the
    // same list-and-select machinery as a folder, rather than a separate
    // one-file code path, so selection/loading/saving behave identically
    // either way). For a folder, every direct child file is listed (not
    // just .cbz -- see SfmFileEntry::is_cbz) and non-.cbz entries are
    // rendered greyed out and unselectable rather than filtered out
    // entirely, so the user can see everything that's actually there.
    fn sfm_open_path(&mut self, path: PathBuf, is_folder: bool) {
        self.sfm = SfmState::default();
        self.sfm.root = Some(path.clone());

        let mut entries: Vec<SfmFileEntry> = if is_folder {
            std::fs::read_dir(&path)
                .map(|rd| rd.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_file())
                    .map(|p| {
                        let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
                        let is_cbz = p.extension().map_or(false, |x| x.eq_ignore_ascii_case("cbz"));
                        SfmFileEntry { path: p, name, is_cbz }
                    })
                    .collect())
                .unwrap_or_default()
        } else {
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let is_cbz = path.extension().map_or(false, |x| x.eq_ignore_ascii_case("cbz"));
            vec![SfmFileEntry { path: path.clone(), name, is_cbz }]
        };
        entries.sort_by(|a, b| natural_sort_key(&a.name).cmp(&natural_sort_key(&b.name)));
        self.sfm.files = entries;

        // Auto-select the first .cbz entry, if any, so opening a folder
        // drops the user straight into the editor instead of an empty
        // right panel they then have to click into themselves.
        let first_cbz = self.sfm.files.iter().position(|f| f.is_cbz);
        if let Some(idx) = first_cbz {
            self.sfm.selected = Some(idx);
            self.sfm_load_selected();
        }

        // Remember this as the session to restore next time Single File
        // Mode is entered/the app is relaunched -- see
        // sfm_restore_last_session. Persisted here (the one place a new
        // root gets opened) rather than duplicated at every call site.
        self.settings.sfm_last_root = Some(path);
        self.settings.sfm_last_is_folder = is_folder;
        self.sfm_persist_selection();
    }

    /// Re-opens whatever file/folder was open in Single File Mode last
    /// time (persisted via sfm_open_path/sfm_request_select/
    /// sfm_resolve_pending_nav), so leaving the mode or quitting the app
    /// entirely doesn't drop the user back to an empty file tree. Called
    /// once at startup, after load_settings -- restoring here rather than
    /// lazily on first mode-entry means the file tree is already correct
    /// the instant the user flips the Settings toggle on, no visible pop.
    fn sfm_restore_last_session(&mut self) {
        let Some(root) = self.settings.sfm_last_root.clone() else { return };
        if !root.exists() {
            // Show once, then clear the stale remembered path so this
            // notice doesn't keep reappearing on every future launch.
            self.dialog = Some(Dialog::Notice(format!(
                "The Single File Mode item you had open last time is no \
                 longer there:\n{}", root.display()
            )));
            self.settings.sfm_last_root = None;
            self.settings.sfm_last_selected = None;
            self.save_settings();
            return;
        }

        let is_folder = self.settings.sfm_last_is_folder;
        self.sfm_open_path(root, is_folder);

        // sfm_open_path already auto-selected the first .cbz entry (and
        // re-saved settings using that as sfm_last_selected) -- if a
        // different file was actually selected last time, switch to it
        // now instead. A missing/renamed remembered file just leaves the
        // auto-selected first entry in place rather than erroring, since
        // the folder itself did resolve fine.
        if let Some(want) = self.settings.sfm_last_selected.clone() {
            if let Some(idx) = self.sfm.files.iter().position(|f| f.path == want) {
                if Some(idx) != self.sfm.selected {
                    self.sfm.selected = Some(idx);
                    self.sfm_load_selected();
                    self.sfm_persist_selection();
                }
            }
        }
    }

    /// Requests a switch to `idx` in the file tree. Goes through the
    /// unsaved-changes guard: if the currently-loaded file has edits that
    /// haven't been saved, this opens the confirmation dialog instead of
    /// switching immediately, and the actual switch happens once that
    /// dialog resolves (see render_dialogs' Dialog::SfmUnsavedChanges arm).
    fn sfm_request_select(&mut self, idx: usize) {
        if Some(idx) == self.sfm.selected { return; }
        if self.sfm.dirty() {
            if self.settings.sfm_autosave_on_focus_change {
                self.sfm_save_current();
                self.sfm_switch_to(idx);
            } else {
                self.sfm.pending_nav = Some(SfmNavTarget::File(idx));
                // A plain click is never an undo/redo action -- clear any
                // stale pending_undo_apply left over from an earlier
                // cancelled Ctrl+Z/Y attempt, so THIS navigation's Save/
                // Discard doesn't incorrectly re-apply that old step.
                self.sfm.pending_undo_apply = None;
                self.dialog = Some(Dialog::SfmUnsavedChanges);
            }
        } else {
            self.sfm_switch_to(idx);
        }
    }

    /// Actually performs a selection change: sets `selected`, loads the
    /// new file's tags, and persists it as the file to restore next
    /// session. Assumes any unsaved-changes handling (prompt, or
    /// auto-save) for whatever was previously selected has already
    /// happened -- this is the "just do it" step every switch path
    /// (direct click when clean, auto-save-then-switch, Save/Discard
    /// resolving the prompt, undo/redo jumping files) funnels through.
    fn sfm_switch_to(&mut self, idx: usize) {
        self.sfm.selected = Some(idx);
        self.sfm_load_selected();
        self.sfm_persist_selection();
    }

    /// Saves whichever file is currently self.sfm.selected as the one to
    /// restore next time (see sfm_restore_last_session). Small and called
    /// from every place selected actually changes, rather than
    /// duplicating the same three-field settings update at each of those
    /// call sites.
    fn sfm_persist_selection(&mut self) {
        self.settings.sfm_last_selected = self.sfm.selected
            .and_then(|i| self.sfm.files.get(i))
            .map(|f| f.path.clone());
        self.save_settings();
    }

    /// Loads (or re-loads) ComicInfo.xml for whichever file is currently
    /// self.sfm.selected, populating the right panel. Called after a
    /// selection change, and after a successful Save (to re-derive
    /// loaded_tags from what's now actually on disk rather than trusting
    /// the in-memory tags are byte-identical to what got written).
    fn sfm_load_selected(&mut self) {
        self.sfm.foreign_tags_notice = None;
        self.sfm.tags.clear();
        self.sfm.loaded_tags.clear();

        let Some(idx) = self.sfm.selected else { self.sfm.panel = SfmPanelState::Empty; return };
        let Some(entry) = self.sfm.files.get(idx) else { self.sfm.panel = SfmPanelState::Empty; return };
        if !entry.is_cbz {
            // Shouldn't normally be reachable (non-.cbz rows are
            // unselectable in the UI), but if selected is somehow left
            // pointing at one -- e.g. after a folder re-scan reorders
            // entries -- fail safe into Empty rather than trying to read
            // a file that was never a real archive.
            self.sfm.panel = SfmPanelState::Empty;
            return;
        }

        match read_comic_info_from_cbz(&entry.path) {
            Ok(ComicInfoReadResult::Missing) => {
                self.sfm.panel = SfmPanelState::NoComicInfo;
            }
            Ok(ComicInfoReadResult::Found(pairs)) => {
                let foreign: Vec<String> = pairs.iter()
                    .map(|(t, _)| t.clone())
                    .filter(|t| field_spec(t).is_none() && !SFM_EXTRA_KNOWN_TAGS.contains(&t.as_str()))
                    .collect();
                let rows: Vec<SfmTagRow> = pairs.into_iter().map(|(tag, value)| {
                    let id = self.sfm.next_row_id;
                    self.sfm.next_row_id += 1;
                    SfmTagRow { tag, value, id }
                }).collect();
                self.sfm.tags = rows.clone();
                self.sfm.loaded_tags = rows;
                self.sfm.foreign_tags_notice = if foreign.is_empty() { None } else { Some(foreign) };
                self.sfm.panel = SfmPanelState::Editing;
            }
            Err(e) => {
                self.sfm.panel = SfmPanelState::LoadError(e.to_string());
            }
        }
    }

    /// Creates a fresh ComicInfo.xml for the selected file with just the
    /// two defaults specified for this feature (Title = filename without
    /// extension, Series = containing folder's name) and switches
    /// straight into the editor with those two rows pre-populated. Does
    /// NOT write anything to disk yet -- same as loading an existing file,
    /// this only takes effect once the user hits Save, so a user who opens
    /// "Add ComicInfo.xml" and then navigates away without saving loses
    /// nothing and the file is untouched.
    fn sfm_create_default(&mut self) {
        let Some(idx) = self.sfm.selected else { return };
        let Some(entry) = self.sfm.files.get(idx).cloned() else { return };

        let title = entry.path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let series = entry.path.parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let mut rows = Vec::new();
        for (tag, value) in [("Title", title), ("Series", series)] {
            let id = self.sfm.next_row_id;
            self.sfm.next_row_id += 1;
            rows.push(SfmTagRow { tag: tag.to_string(), value, id });
        }
        self.sfm.tags = rows;
        // loaded_tags stays empty (not a clone of tags) so dirty() reads
        // true immediately -- this is genuinely unsaved new content, not
        // a no-op edit, and should prompt like any other unsaved change
        // if the user tries to navigate away without saving.
        self.sfm.loaded_tags = Vec::new();
        self.sfm.foreign_tags_notice = None;
        self.sfm.panel = SfmPanelState::Editing;
    }

    /// Writes the current editor's tags to the selected file's
    /// ComicInfo.xml, in place, reusing the exact same
    /// build_comic_info_xml + write_comic_info_xml_to path the batch
    /// pipeline uses -- Single File Mode never needs its own writer.
    fn sfm_save_current(&mut self) {
        let Some(idx) = self.sfm.selected else { return };
        let Some(entry) = self.sfm.files.get(idx).cloned() else { return };

        let data: HashMap<String, String> = self.sfm.tags.iter()
            .map(|r| (r.tag.clone(), r.value.clone())).collect();
        // Order rows by their current on-screen position (post drag-
        // reorder), not canonical/registry order -- build_comic_info_xml
        // sorts by tag_rank against this list, so handing it the tags in
        // exactly their current UI order makes the written XML match
        // what's shown on screen.
        let order: Vec<String> = self.sfm.tags.iter().map(|r| r.tag.clone()).collect();
        let xml = build_comic_info_xml(&data, &order);

        match write_comic_info_to_cbz(&entry.path, &xml) {
            Ok(()) => {
                self.sfm.loaded_tags = self.sfm.tags.clone();
                self.status = format!("Saved ComicInfo.xml in {}", entry.name);
            }
            Err(e) => {
                self.dialog = Some(Dialog::Notice(format!("Couldn't save {}: {e}", entry.name)));
            }
        }
    }

    /// Carries out whatever navigation was waiting on the unsaved-changes
    /// prompt (see Dialog::SfmUnsavedChanges), once the user has resolved
    /// it via Save or Discard. Not called for Cancel -- see that arm's
    /// comment for why pending_nav is deliberately left set in that case.
    fn sfm_resolve_pending_nav(&mut self) {
        match self.sfm.pending_nav.take() {
            Some(SfmNavTarget::File(idx)) => {
                self.sfm_switch_to(idx);
                // If this navigation was actually undo/redo jumping to a
                // different file (deferred past the Save/Discard/Cancel
                // prompt -- see sfm_goto_undo_entry), apply that step's
                // target tags now that the switch itself is done.
                if let Some(tags) = self.sfm.pending_undo_apply.take() {
                    self.sfm_apply_undo_tags(tags);
                }
            }
            Some(SfmNavTarget::ExitMode) => {
                self.settings.single_file_mode = false;
                self.save_settings();
                // Deliberately NOT reset to SfmState::default() here --
                // see the Settings toggle's identical comment on why:
                // this lets toggling back into Single File Mode later in
                // the same session restore the same file tree/selection
                // instead of an empty one. The dirty tag edits that
                // triggered this whole prompt are already gone either
                // way (Save wrote them to disk and refreshed
                // loaded_tags; Discard is handled by simply never having
                // written them) -- what's kept here is just which
                // root/file was open, not any unsaved content.
            }
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

    // Archives the previous run's file_slots content and log_footer into
    // self.log (a plain, ever-growing Vec, unlike file_slots) before
    // anything for the NEW run is added to any of these three. Must be
    // called before the first thing a new run pushes to self.log (e.g.
    // "[Fresh start]" in the ResumeSession dialog, or check_finale's own
    // notices) -- calling it from inside start_worker was too late, since
    // that runs a full frame after "[Fresh start]" is pushed for the
    // Start Fresh path, which put the archived previous-run content AFTER
    // the new run's own "[Fresh start]" marker instead of before it.
    //
    // file_slots/log_footer themselves are NOT reset here -- that still
    // happens in start_worker, immediately before the worker thread
    // actually spawns, sized to that specific run's file count (their
    // whole design: index-stable slots for parallel workers to write
    // results back into in the right numeric order, which only works
    // when sized to exactly the run in question).
    fn archive_previous_run_log(&mut self) {
        if !self.file_slots.is_empty() || !self.log_footer.is_empty() {
            for slot in self.file_slots.drain(..) {
                if let Some(entries) = slot {
                    self.log.extend(entries);
                }
            }
            if self.log_footer.is_empty() {
                // The previous run was stopped/interrupted before
                // WorkerMsg::Done ever fired (log_footer is only ever
                // populated there), so there's no [DONE] block -- and
                // therefore no trailing separator -- to carry over.
                // Without this, an interrupted run's archived content
                // would run directly into the next run's with no visual
                // break at all.
                self.log.push(LogEntry { text: "-".repeat(60), level: LogLevel::Sep });
            } else {
                // log_footer's own [DONE] block already ends with a
                // LogLevel::Sep separator line (see the WorkerMsg::Done
                // handler), so appending it here already leaves the
                // previous run's content ending on a clean divider -- no
                // extra separator needed on top of that.
                self.log.append(&mut self.log_footer);
            }
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
        self.archive_previous_run_log();
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
            if let Some(m) = re_anynum().find(&n) {
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
    // Fade-out design: a dialog's own click-handler logic (e.g. "if yes {
    // self.reset_all() } else if !cancel { self.dialog = Some(...) }")
    // is left completely untouched -- when a dialog decides to close, it
    // simply stops re-setting self.dialog, exactly as before, leaving it
    // None after this fn's `.take()`. What's new is what happens with
    // that None: rather than the window vanishing that same frame, this
    // function keeps rendering the LAST dialog it saw (self.last_dialog)
    // for a further ~150ms with opacity ticking down to 0 via
    // dialog_closing/dialog_closed_at, and only drops last_dialog for
    // real once that timer completes.
    //
    // ui.disable() while disable_ui is true stops a fading dialog's
    // buttons from reporting NEW clicks -- but it does NOT stop each
    // arm's existing "nothing was clicked, stay open" fallback branch
    // (the `else { self.dialog = Some(...) }` part) from firing on every
    // one of those disabled fade-out frames, since "disabled" and
    // "wasn't clicked" both just mean .clicked() == false to that
    // branch. Left alone, that fallback re-sets self.dialog every single
    // closing frame, which flips now_open back to true next frame and
    // restarts the whole open/close cycle -- the dialog never actually
    // closes, it just fades out partway and snaps back. Fix: after the
    // match below runs (so the fade-out still renders and any in-
    // progress per-dialog state is preserved for those frames), if this
    // was a closing frame, self.dialog is forced back to None regardless
    // of what the arm just wrote -- the close was already decided the
    // frame dialog_closing became true, and nothing after that should
    // be able to undo it.
    fn render_dialogs(&mut self, ctx: &egui::Context) {
        const DIALOG_FADE_SECS: f32 = 0.15;
        let now_open = self.dialog.is_some();

        if now_open {
            if !self.dialog_was_open {
                self.dialog_opened_at = std::time::Instant::now();
            }
            self.dialog_was_open = true;
            self.dialog_closing = false;
            self.last_dialog = self.dialog.take();
        } else if self.dialog_was_open {
            // Was open last frame, None now: the exact frame it closed.
            self.dialog_was_open = false;
            self.dialog_closing = true;
            self.dialog_closed_at = std::time::Instant::now();
        } else if self.dialog_closing
            && self.dialog_closed_at.elapsed().as_secs_f32() >= DIALOG_FADE_SECS
        {
            self.dialog_closing = false;
            self.last_dialog = None;
        }

        let Some(dlg) = self.last_dialog.clone() else { return };

        let dialog_opacity = if self.dialog_closing {
            let raw = (self.dialog_closed_at.elapsed().as_secs_f32() / DIALOG_FADE_SECS).clamp(0.0, 1.0);
            ctx.request_repaint();
            1.0 - (1.0 - (1.0 - raw).powi(3)) // fading 1 -> 0, mirrors the fade-in curve
        } else {
            let raw = (self.dialog_opened_at.elapsed().as_secs_f32() / DIALOG_FADE_SECS).clamp(0.0, 1.0);
            if raw < 1.0 { ctx.request_repaint(); }
            1.0 - (1.0 - raw).powi(3) // ease-out cubic, fading 0 -> 1
        };
        let disable_ui = self.dialog_closing;
        let was_closing_this_frame = self.dialog_closing;

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

                // Overlap check against every OTHER rule in the same list
                // (the row being edited is excluded by index, so re-saving
                // it unchanged -- or just editing an unrelated field on it
                // -- never flags itself). find_volume/find_date/
                // find_summary all return the FIRST matching rule and
                // never look further, so an overlapping/duplicate rule
                // isn't a harmless duplicate -- it's a rule that can
                // NEVER fire, silently, exactly like the empty-range bug
                // fixed earlier. Only checked when range_valid, since
                // there's nothing meaningful to compare yet otherwise.
                let existing_rules: &[Vec<String>] = match s.target {
                    RuleTarget::Volume  => &self.cfg.volume_rules,
                    RuleTarget::Date    => &self.cfg.date_rules,
                    RuleTarget::Summary => &self.cfg.summ_rules,
                };
                let overlap = range_valid && Self::rule_range_overlaps(&s.values, existing_rules, s.row_idx);

                // Volume Rules specifically: two DIFFERENT, non-
                // overlapping chapter ranges both producing the same
                // output Volume number (e.g. "1-4 -> Vol 1" and
                // "5-8 -> Vol 1") don't create a silently-dead rule the
                // way an overlapping range does -- find_volume still
                // returns the right value for whichever chapters are
                // actually being looked up. But it's still nonsensical: a
                // volume number should represent one specific, contiguous
                // stretch of chapters, not two disconnected ones. Only
                // meaningful for Volume Rules -- Date/Summary Rules
                // repeating a Year or Summary across different volume
                // ranges is completely normal (e.g. two volumes released
                // in the same year) and shouldn't be flagged.
                let duplicate_volume = range_valid && !overlap
                    && matches!(s.target, RuleTarget::Volume)
                    && Self::volume_value_duplicated(&s.values, existing_rules, s.row_idx);

                let rule_dialog_title = if s.is_new { "Add Rule" } else { "Edit Rule" };
                egui::Window::new(rule_dialog_title)
                    .id(egui::Id::new("rule_add_edit_dialog"))
                    .title_bar(false)
                    .resizable(true).collapsible(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .frame(theme::dialog_window_frame(dialog_opacity))
                    .show(ctx, |ui| {
                        ui.set_opacity(dialog_opacity);
                        if disable_ui { ui.disable(); }
                        theme::window_titlebar(ui, rule_dialog_title);
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
                                ui.label(RichText::new(display_label).color(theme::TXT()));
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
                            ui.label(RichText::new(msg).color(theme::TERR()).size(11.0));
                        } else if overlap {
                            ui.add_space(4.0);
                            let start_lbl = s.labels.first().map(String::as_str).unwrap_or("Start");
                            let end_lbl = s.labels.get(1).map(String::as_str).unwrap_or("End");
                            ui.label(RichText::new(format!(
                                "This {start_lbl}/{end_lbl} range overlaps another rule -- \
                                 whichever rule comes first will always be used, and this one \
                                 will never take effect."
                            )).color(theme::TERR()).size(11.0));
                        } else if duplicate_volume {
                            ui.add_space(4.0);
                            ui.label(RichText::new(
                                "This Volume number is already used by a different chapter range. \
                                 Each volume number should map to one chapter range."
                            ).color(theme::TERR()).size(11.0));
                        }
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.add(theme::btn_primary("  Save  ")).clicked() && range_valid && !overlap && !duplicate_volume {
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
                    // false, or the range overlapped another rule -- the
                    // dialog simply stays open, same as clicking neither
                    // button, so the person can fix the fields and try
                    // again rather than losing their input.
                    self.dialog = Some(Dialog::EditRule(s));
                }
            }

            Dialog::Decimal(mut ds) => {
                let mut ok = false;
                egui::Window::new("Decimal Chapter")
                    .title_bar(false)
                    .resizable(false).collapsible(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .frame(theme::dialog_window_frame(dialog_opacity))
                    .show(ctx, |ui| {
                        ui.set_opacity(dialog_opacity);
                        if disable_ui { ui.disable(); }
                        theme::window_titlebar(ui, "Decimal Chapter");
                        ui.label(RichText::new("Decimal Chapter Detected").color(theme::TWARN()).strong().size(13.0));
                        ui.separator();
                        ui.label(RichText::new(format!("File:  {}", ds.filename)).color(theme::TDIM()).size(11.0));
                        ui.label(RichText::new(format!("Title: {}", ds.raw_title)).color(theme::TXT()));
                        ui.add_space(6.0);
                        let rt = ds.raw_title.clone();
                        for (v, lbl) in [(1u8, rt.as_str()), (2,"Bonus Manga"), (3,"Bonus Chapter"), (4,"Extra Chapter"), (5,"Custom ->")] {
                            ui.radio_value(&mut ds.choice, v, lbl);
                        }
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("   Prefix:").color(theme::TDIM()));
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
                    .title_bar(false)
                    .resizable(false).collapsible(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0,0.0])
                    .frame(theme::dialog_window_frame(dialog_opacity))
                    .show(ctx, |ui| {
                        ui.set_opacity(dialog_opacity);
                        if disable_ui { ui.disable(); }
                        theme::window_titlebar(ui, "Previous Session Found");
                        ui.label(format!("{count} files already processed in a previous run."));
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.add(theme::btn_primary("Resume")).clicked() { choice=1; }
                            ui.add_space(4.0);
                            if ui.add(theme::btn_secondary("Start Fresh")).clicked() { choice=2; }
                            ui.add_space(4.0);
                            if ui.add(egui::Button::new("Cancel").fill(theme::SURF3())).clicked() { choice=-1; }
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
                    .title_bar(false)
                    .resizable(false).collapsible(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0,0.0])
                    .frame(theme::dialog_window_frame(dialog_opacity))
                    .show(ctx, |ui| {
                        ui.set_opacity(dialog_opacity);
                        if disable_ui { ui.disable(); }
                        theme::window_titlebar(ui, "Final Chapter Detected");
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
                egui::Window::new("Notice").title_bar(false).resizable(false).collapsible(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0,0.0])
                    .frame(theme::dialog_window_frame(dialog_opacity)).show(ctx, |ui| {
                        ui.set_opacity(dialog_opacity);
                        if disable_ui { ui.disable(); }
                        theme::window_titlebar(ui, "Notice");
                        ui.label(&msg); ui.add_space(8.0);
                        if ui.add(theme::btn_secondary("  OK  ")).clicked() { ok_clicked = true; }
                    });
                if !ok_clicked { self.dialog = Some(Dialog::Notice(msg)); }
            }

            Dialog::HelpText { title, body } => {
                let mut ok_clicked = false;
                egui::Window::new(&title)
                    .id(egui::Id::new("help_text_dialog"))
                    .title_bar(false)
                    .resizable(true).collapsible(false)
                    .default_width(440.0)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .frame(theme::dialog_window_frame(dialog_opacity)).show(ctx, |ui| {
                        // dialog_opacity/disable_ui were skipped here for a
                        // while to isolate a reported "closes then
                        // immediately reopens" bug that seemed specific to
                        // this dialog. It wasn't specific to this dialog --
                        // it was the same fade-out re-persist bug every
                        // dialog had (see the comment at the top of
                        // render_dialogs), just harder to notice elsewhere
                        // since most other dialogs don't sit directly over
                        // the "?" button that opens them. Now that the
                        // real fix is in place, this can render like every
                        // other dialog again.
                        ui.set_opacity(dialog_opacity);
                        if disable_ui { ui.disable(); }
                        theme::window_titlebar(ui, &title);
                        egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                            ui.label(RichText::new(&body).size(12.5));
                        });
                        ui.add_space(8.0);
                        if ui.add(theme::btn_secondary("  Close  ")).clicked() { ok_clicked = true; }
                    });
                if !ok_clicked { self.dialog = Some(Dialog::HelpText { title, body }); }
            }

            Dialog::ConfirmReset => {
                let mut yes = false; let mut cancel = false;
                egui::Window::new("Confirm Reset").title_bar(false).resizable(false).collapsible(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0,0.0])
                    .frame(theme::dialog_window_frame(dialog_opacity)).show(ctx, |ui| {
                        ui.set_opacity(dialog_opacity);
                        if disable_ui { ui.disable(); }
                        theme::window_titlebar(ui, "Confirm Reset");
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
                egui::Window::new("Confirm Remove").title_bar(false).resizable(false).collapsible(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0,0.0])
                    .frame(theme::dialog_window_frame(dialog_opacity)).show(ctx, |ui| {
                        ui.set_opacity(dialog_opacity);
                        if disable_ui { ui.disable(); }
                        theme::window_titlebar(ui, "Confirm Remove");
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
                egui::Window::new("Clear Log").title_bar(false).resizable(false).collapsible(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0,0.0])
                    .frame(theme::dialog_window_frame(dialog_opacity)).show(ctx, |ui| {
                        ui.set_opacity(dialog_opacity);
                        if disable_ui { ui.disable(); }
                        theme::window_titlebar(ui, "Clear Log");
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
                    .title_bar(false)
                    .resizable(false).collapsible(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .frame(theme::dialog_window_frame(dialog_opacity))
                    .show(ctx, |ui| {
                        ui.set_opacity(dialog_opacity);
                        if disable_ui { ui.disable(); }
                        theme::window_titlebar(ui, "Empty Metadata Fields");
                        ui.label(RichText::new(
                            "These fields are empty and will be blank in every generated file:"
                        ).color(theme::TXT()));
                        ui.add_space(6.0);
                        egui::Frame::none()
                            .fill(theme::SURF3())
                            .rounding(egui::Rounding::same(4.0))
                            .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                            .show(ui, |ui| {
                                ui.label(RichText::new(fields.join(", "))
                                    .color(theme::TWARN()).strong());
                            });
                        ui.add_space(8.0);
                        ui.label(RichText::new(
                            "You can continue anyway, or go back to the Metadata tab and fill them in first."
                        ).color(theme::TDIM()).size(11.0));
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
                    .title_bar(false)
                    .resizable(true).collapsible(false)
                    .min_width(320.0)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .frame(theme::dialog_window_frame(dialog_opacity))
                    .show(ctx, |ui| {
                        ui.set_opacity(dialog_opacity);
                        if disable_ui { ui.disable(); }
                        theme::window_titlebar(ui, "Add Metadata Tag");
                        ui.label(RichText::new("Choose a field to add:")
                            .color(theme::TDIM()).size(11.0));
                        ui.add_space(6.0);

                        if available.is_empty() {
                            ui.label(RichText::new("All available fields have already been added.")
                                .color(theme::TDIM()).size(12.0));
                        } else {
                            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                                for spec in &available {
                                    let resp = ui.add(
                                        egui::Button::new(RichText::new(spec.label).color(theme::TXT()).size(12.0))
                                            .fill(theme::SURF3())
                                            .stroke(egui::Stroke::new(1.0_f32, theme::BDR()))
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
                                .title_bar(false)
                                .resizable(true).collapsible(false)
                                .min_width(300.0)
                                .default_pos(egui::pos2(360.0, 120.0))
                                .frame(theme::dialog_window_frame(dialog_opacity))
                                .show(ctx, |ui| {
                                    ui.set_opacity(dialog_opacity);
                                    if disable_ui { ui.disable(); }
                                    theme::window_titlebar(ui, "Tag Order");
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
                    .title_bar(false)
                    .resizable(true)
                    .collapsible(false)
                    .min_width(480.0)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .frame(theme::dialog_window_frame(dialog_opacity))
                    .show(ctx, |ui| {
                        ui.set_opacity(dialog_opacity);
                        if disable_ui { ui.disable(); }
                        theme::window_titlebar(ui, "Import Successful");
                        // Header
                        ui.label(
                            RichText::new(format!("{} field(s) imported from  {filename}", items.len()))
                                .color(theme::TGOOD()).strong().size(13.0),
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
                                                    .color(theme::ACC2())
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
                                                    .color(theme::TXT())
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
                                .color(theme::TDIM())
                                .size(11.0),
                        );
                        ui.add_space(6.0);
                        if ui.add(theme::btn_primary("  OK  ")).clicked() { close = true; }
                    });

                if !close {
                    self.dialog = Some(Dialog::ImportResult { filename, items });
                }
            }

            Dialog::SfmUnsavedChanges => {
                let mut save = false; let mut discard = false; let mut cancel = false;
                let filename = self.sfm.selected
                    .and_then(|i| self.sfm.files.get(i))
                    .map(|f| f.name.clone())
                    .unwrap_or_default();
                egui::Window::new("Unsaved Changes").title_bar(false).resizable(false).collapsible(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0,0.0])
                    .frame(theme::dialog_window_frame(dialog_opacity)).show(ctx, |ui| {
                        ui.set_opacity(dialog_opacity);
                        if disable_ui { ui.disable(); }
                        theme::window_titlebar(ui, "Unsaved Changes");
                        ui.label(format!("{filename} has unsaved ComicInfo.xml changes."));
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.add(theme::btn_primary("  Save  ")).clicked() { save = true; }
                            ui.add_space(4.0);
                            if ui.add(theme::btn_secondary("  Discard  ")).clicked() { discard = true; }
                            ui.add_space(4.0);
                            if ui.add(theme::btn_secondary("  Cancel  ")).clicked() { cancel = true; }
                        });
                    });
                // Cancel leaves pending_nav set on purpose -- Cancel means
                // "stay here for now," not "forget I tried to navigate."
                // A subsequent click on the same target (or another one)
                // just re-opens this same prompt via sfm_request_select,
                // same as if the first attempt never happened.
                if save {
                    self.sfm_save_current();
                    self.sfm_resolve_pending_nav();
                } else if discard {
                    // Revert just the unsaved edit (tags back to
                    // loaded_tags, whatever was last actually on disk) --
                    // NOT a full self.sfm reset. Needed now that neither
                    // sfm_resolve_pending_nav's ExitMode arm nor the
                    // Settings toggle wipe self.sfm to a blank default on
                    // mode-exit anymore (kept intact instead, so toggling
                    // back into Single File Mode later in the session
                    // restores the same file tree/selection) -- without
                    // this explicit revert, "Discard" would do nothing
                    // to the actual dirty tags at all, and they'd still
                    // be sitting there dirty after leaving the mode.
                    self.sfm.tags = self.sfm.loaded_tags.clone();
                    self.sfm.pending_undo_word = None;
                    self.sfm_resolve_pending_nav();
                } else if !cancel {
                    self.dialog = Some(Dialog::SfmUnsavedChanges);
                }
            }
        }

        // See the comment at the top of this function: whatever the arm
        // above just wrote into self.dialog doesn't get to count while
        // we were fading out this frame -- the close already happened.
        if was_closing_this_frame {
            self.dialog = None;
        }
    }

    // ── Keyboard shortcuts ────────────────────────────────────────────────────
    pub fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        // Single File Mode gets its own, unrelated set of shortcuts
        // (Ctrl+S saves the currently-edited file, Ctrl+Z/Y undo/redo
        // tag edits) rather than sharing any of the batch-mode ones
        // below, which all act on self.cfg -- a job config SFM has no
        // connection to. Most important for Ctrl+S specifically: a user
        // editing tags in SFM pressing Ctrl+S out of habit should save
        // their tag edits, not pop a "Save Config" file dialog for a job
        // they're not even looking at.
        if self.settings.single_file_mode {
            ctx.input_mut(|i| {
                if i.consume_key(egui::Modifiers::CTRL, egui::Key::S) {
                    if self.sfm.dirty() { self.sfm_save_current(); }
                }
                if i.consume_key(egui::Modifiers::CTRL, egui::Key::Z) { self.sfm_undo(); }
                // Ctrl+Shift+Z is the correct redo binding on every
                // platform: it's macOS's actual standard (Cmd+Shift+Z --
                // and per egui's own docs, Modifiers::CTRL already means
                // "Ctrl on Win/Linux, Cmd on Mac" at the input-reporting
                // level, so this one binding covers both automatically,
                // no separate mac_cmd check needed), and it's also a
                // widely-supported secondary on Windows/Linux apps.
                // Ctrl+Y is kept alongside it as the more common primary
                // on Windows/Linux specifically (Y is not any part of
                // Mac's redo convention, so this is purely an addition,
                // never a conflict with the line above).
                if i.consume_key(egui::Modifiers::CTRL.plus(egui::Modifiers::SHIFT), egui::Key::Z)
                    || i.consume_key(egui::Modifiers::CTRL, egui::Key::Y)
                {
                    self.sfm_redo();
                }
            });
            return;
        }
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
        theme::advance_transition(ctx);

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
            .frame(egui::Frame::none().fill(theme::SURF()).stroke(egui::Stroke::new(1.0_f32, theme::BDR()))
                .inner_margin(egui::Margin::symmetric(8.0, 8.0)))
            .show(ctx, |ui| self.show_toolbar(ui));
        egui::TopBottomPanel::bottom("statusbar")
            .frame(egui::Frame::none().fill(theme::BG()).stroke(egui::Stroke::new(1.0_f32, theme::BDR()))
                .inner_margin(egui::Margin::symmetric(12.0, 6.0)))
            .show(ctx, |ui| self.show_statusbar(ui));

        // Same pattern as the tab-content fade below: elapsed wall-clock
        // time since the switch was detected, not egui's per-Id
        // animate_bool_with_time (whose memory would already read
        // "settled at 1.0" the second time a mode is revisited, showing
        // no animation at all after the first switch -- exactly the bug
        // that pattern already avoids for tabs). Detected once per
        // frame, here, before either mode's own panels render, so both
        // branches below see the same fresh elapsed/opacity value.
        if self.settings.single_file_mode != self.prev_single_file_mode {
            self.prev_single_file_mode = self.settings.single_file_mode;
            self.mode_switched_at = std::time::Instant::now();
        }
        const MODE_FADE_SECS: f32 = 0.18;
        let mode_elapsed = self.mode_switched_at.elapsed().as_secs_f32();
        let mode_raw = (mode_elapsed / MODE_FADE_SECS).clamp(0.0, 1.0);
        if mode_raw < 1.0 { ctx.request_repaint(); }
        let mode_opacity = 1.0 - (1.0 - mode_raw).powi(3); // ease-out cubic, same curve as the tab fade

        // Tabbar only applies to the batch-processing tabs; skip it
        // entirely in Single File Mode rather than show it disabled, since
        // its tabs have no meaning there.
        if !self.settings.single_file_mode {
            egui::TopBottomPanel::top("tabbar")
                .frame(egui::Frame::none().fill(theme::SURF()).stroke(egui::Stroke::new(1.0_f32, theme::BDR()))
                    .inner_margin(egui::Margin::symmetric(14.0, 8.0)))
                .show(ctx, |ui| { ui.set_opacity(mode_opacity); self.show_tabbar(ui); });
        }

        if self.settings.single_file_mode {
            self.show_single_file_mode(ctx, mode_opacity);
            return;
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(theme::BG()))
            .show(ctx, |ui| {
                // Fade the incoming tab's content in over ~180ms rather
                // than a hard cut. Progress is computed directly from
                // elapsed wall-clock time since the last detected switch
                // (self.tab != self.prev_tab resets tab_switched_at to
                // now), the same pattern already used for the theme
                // cross-fade's Transition struct -- this sidesteps
                // egui's animate_bool_with_time entirely, whose per-Id
                // memory only starts at 0.0 the very first time that Id
                // is ever queried; every later revisit to an
                // already-seen tab would just recall its
                // previously-settled 1.0 with no animation, which is
                // why the fade appeared to stop working after the first
                // pass through the tabs. Only the tab's own content
                // fades; the panel background (filled above) stays
                // fully opaque throughout so there's no flash of the
                // window behind it.
                if self.tab != self.prev_tab {
                    self.prev_tab = self.tab;
                    self.tab_switched_at = std::time::Instant::now();
                }
                const FADE_SECS: f32 = 0.18;
                let elapsed = self.tab_switched_at.elapsed().as_secs_f32();
                let raw = (elapsed / FADE_SECS).clamp(0.0, 1.0);
                // ease-out cubic, same easing curve as the theme cross-fade
                let opacity = 1.0 - (1.0 - raw).powi(3);
                // Combined with mode_opacity (computed once above, before
                // this whole branch) rather than used alone: switching
                // FROM Single File Mode back into batch mode should fade
                // the newly-active tab in regardless of which specific
                // tab it happens to land on, not just when the tab
                // itself changes. Multiplying is correct either way --
                // both terms settle to 1.0 once their own transition
                // finishes, so whichever one is still mid-fade is what
                // dims the result.
                ui.set_opacity(opacity * mode_opacity);
                if raw < 1.0 {
                    ctx.request_repaint();
                }

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
                .size(14.0).color(theme::TXT()).strong());

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);
                let settings_closed_this_frame = self.settings_closing
                    && self.settings_closed_at.elapsed().as_secs_f32() < 0.05;
                let settings_btn = ui.add(theme::btn_secondary("Settings"));
                self.settings_btn_rect = settings_btn.rect;
                if settings_btn.on_hover_text("App settings").clicked()
                    && !settings_closed_this_frame
                {
                    self.settings_open = true;
                }
                // Reset All / Import / Load / Save all act on self.cfg,
                // the batch-processing job config -- there's nothing for
                // them to do in Single File Mode (which has its own Save,
                // scoped to the one file being edited, in the editor
                // panel itself), and leaving them visible would invite
                // clicking "Save" expecting it to save tag edits it has
                // no connection to.
                if !self.settings.single_file_mode {
                    ui.add_space(10.0);
                    if ui.add(theme::btn_danger("Reset All")).on_hover_text("Clear all settings (Ctrl+R)").clicked() {
                        self.dialog = Some(Dialog::ConfirmReset);
                    }
                    ui.add_space(6.0);
                    if ui.add(theme::btn_secondary("Import")).on_hover_text("Merge metadata/rules from a .py or .json into the CURRENT session -- never replaces anything not in the file. (Ctrl+I)").clicked() {
                        self.start_pick(PathPick::ImportMeta);
                    }
                    ui.add_space(4.0);
                    if ui.add(theme::btn_secondary("Load")).on_hover_text("Load a saved config file, REPLACING the entire current session. (Ctrl+O)").clicked() {
                        self.start_pick(PathPick::LoadConfig);
                    }
                    ui.add_space(4.0);
                    if ui.add(theme::btn_secondary("Save")).on_hover_text("Save config file (Ctrl+S)").clicked() {
                        let n = self.smart_filename();
                        self.start_pick(PathPick::SaveConfig(n));
                    }
                    ui.add_space(20.0);
                    ui.label(RichText::new("Ctrl+S / O / I / R").color(theme::TMUT()).size(10.0));
                }
            });
        });
    }

    fn show_statusbar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // Status dot — drawn as a vector circle (never depends on font glyph coverage)
            let dot_col = if self.running { theme::TGOOD() }
                          else if self.status.contains("error") || self.status.contains("Error") { theme::TERR() }
                          else { theme::TMUT() };
            let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
            ui.painter().circle_filled(dot_rect.center(), 3.5, dot_col);
            ui.add_space(6.0);
            ui.label(RichText::new(&self.status).color(theme::TDIM()).size(11.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new("ComicInfo Generator  -  Rust Edition")
                    .color(theme::TMUT()).size(10.0));
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
                            .color(if active { theme::ON_ACCENT() } else { theme::TDIM() })
                    )
                    .fill(if active { theme::ACC() } else { Color32::TRANSPARENT })
                    .stroke(egui::Stroke::new(1.0_f32,
                        if active { theme::ACC() } else { theme::BDR() }))
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
        const SETTINGS_FADE_SECS: f32 = 0.15;

        if !self.settings_open && !self.settings_closing {
            self.settings_was_open = false;
            return;
        }
        if self.settings_open && !self.settings_was_open {
            self.settings_opened_at = std::time::Instant::now();
            self.settings_closing = false;
        }
        if self.settings_open {
            self.settings_was_open = true;
        }

        // If a previous frame's close button click started the closing
        // fade, keep counting that down; once it completes, actually
        // clear settings_was_open so a future re-open starts a fresh
        // fade-in rather than reading stale state.
        if self.settings_closing
            && self.settings_closed_at.elapsed().as_secs_f32() >= SETTINGS_FADE_SECS
        {
            self.settings_closing = false;
            self.settings_was_open = false;
            self.settings_open = false;
            return;
        }

        let settings_opacity = if self.settings_closing {
            let raw = (self.settings_closed_at.elapsed().as_secs_f32() / SETTINGS_FADE_SECS).clamp(0.0, 1.0);
            ctx.request_repaint();
            1.0 - (1.0 - (1.0 - raw).powi(3))
        } else {
            let raw = (self.settings_opened_at.elapsed().as_secs_f32() / SETTINGS_FADE_SECS).clamp(0.0, 1.0);
            if raw < 1.0 { ctx.request_repaint(); }
            1.0 - (1.0 - raw).powi(3)
        };
        let disable_settings_ui = self.settings_closing;

        // `open` starts true whenever we're still meant to be showing
        // (either genuinely open, or mid-closing-fade) -- if the
        // titlebar's close button flips it to false THIS frame, that's
        // the signal to start the closing fade rather than vanishing
        // immediately.
        let mut open = true;
        let settings_window_resp = egui::Window::new("Settings")
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .anchor(egui::Align2::RIGHT_TOP, [-16.0, 64.0])
            .frame(theme::dialog_window_frame(settings_opacity))
            .show(ctx, |ui| {
                ui.set_opacity(settings_opacity);
                if disable_settings_ui { ui.disable(); }
                theme::window_titlebar_with_close(ui, "Settings", &mut open);
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
                theme::section_hdr(ui, "Mode");
                {
                    // Same segmented two-button look as the Theme picker
                    // right below (reusing its exact row-width math: see
                    // that block's own comment on why item_spacing.x is
                    // subtracted, not added, when splitting the row in
                    // half for two side-by-side buttons).
                    let row_w = ui.available_width();
                    let btn_w = (row_w - ui.spacing().item_spacing.x) / 2.0;
                    let sfm_on_now = self.settings.single_file_mode;
                    ui.horizontal(|ui| {
                        for (label, is_sfm, tip) in [
                            ("Single File Mode", true,
                                "Switches to a file-tree + ComicInfo.xml editor for \
                                 working on one file (or a folder of files) at a \
                                 time, instead of the normal batch-processing tabs."),
                            ("Batch Mode", false,
                                "The normal batch-processing tabs: process a whole \
                                 folder of CBZ files at once."),
                        ] {
                            let selected = sfm_on_now == is_sfm;
                            let mut btn = egui::Button::new(
                                RichText::new(label).size(12.0)
                                    .color(if selected { theme::ON_ACCENT() } else { theme::TXT() })
                            )
                            .min_size(egui::vec2(btn_w, 26.0));
                            btn = if selected {
                                btn.fill(theme::ACC())
                            } else {
                                btn.fill(theme::SURF3()).stroke(egui::Stroke::new(1.0_f32, theme::BDR()))
                            };
                            if ui.add(btn).on_hover_text(tip).clicked() && !selected {
                                let sfm_on = is_sfm;
                                if !sfm_on && self.sfm.dirty() {
                                    if self.settings.sfm_autosave_on_focus_change {
                                        // Same as any other focus-change save with
                                        // this setting on -- silently save instead of
                                        // prompting, then proceed with the exit.
                                        self.sfm_save_current();
                                        self.settings.single_file_mode = sfm_on;
                                        self.save_settings();
                                        // self.sfm is deliberately left as-is here --
                                        // NOT reset to SfmState::default() -- so
                                        // toggling back into Single File Mode later
                                        // in this same session finds the same file
                                        // tree/selection exactly as it was, instead
                                        // of an empty tree that then has to be
                                        // re-derived from settings.sfm_last_* (which
                                        // only really matters across a full app
                                        // restart, not a same-session mode toggle).
                                    } else {
                                        // Leaving the mode with unsaved edits on the
                                        // current file -- same Save/Discard/Cancel
                                        // prompt a file-to-file switch would show, not
                                        // a silent discard. Neither button's selected
                                        // state changes yet (both read from
                                        // self.settings.single_file_mode, untouched
                                        // below) until the prompt resolves.
                                        self.sfm.pending_nav = Some(SfmNavTarget::ExitMode);
                                        self.sfm.pending_undo_apply = None;
                                        self.dialog = Some(Dialog::SfmUnsavedChanges);
                                    }
                                } else {
                                    self.settings.single_file_mode = sfm_on;
                                    self.save_settings();
                                    // Same reasoning as the autosave branch above:
                                    // self.sfm is intentionally NOT reset here either
                                    // when turning the mode off, so it's still intact
                                    // if/when the mode is turned back on later this
                                    // session. Entering the mode (sfm_on == true) with
                                    // an empty self.sfm (first time this session) is
                                    // handled by sfm_restore_last_session below.
                                    if sfm_on && self.sfm.root.is_none() {
                                        self.sfm_restore_last_session();
                                    }
                                }
                            }
                        }
                    });
                    ui.add_space(4.0);
                    if self.settings.single_file_mode {
                        if ui.checkbox(&mut self.settings.sfm_autosave_on_focus_change,
                            RichText::new("Auto-save on file switch").size(12.0))
                            .on_hover_text(
                                "When switching to a different file (or leaving Single \
                                 File Mode) with unsaved changes, save them automatically \
                                 instead of asking each time.")
                            .changed()
                        {
                            self.save_settings();
                        }
                    }
                }

                ui.add_space(10.0);
                theme::section_hdr(ui, "Theme");
                let current = theme::current_choice();
                // egui already inserts item_spacing.x (8px, set in
                // theme::setup_style) between widgets placed side by side
                // in a ui.horizontal() -- this must be subtracted from the
                // row width budget here, not added again on top of it,
                // or the two buttons' combined width overflows what the
                // window actually sized itself for. Combined with this
                // window's RIGHT_TOP anchor, that overflow pushed the
                // whole window leftward past the screen edge to
                // accommodate the wider-than-budgeted row.
                let row_w = ui.available_width();
                let btn_w = (row_w - ui.spacing().item_spacing.x) / 2.0;
                for pair in theme::ThemeChoice::ALL.chunks(2) {
                    ui.horizontal(|ui| {
                        for &choice in pair.iter() {
                            let selected = choice == current;
                            let mut btn = egui::Button::new(
                                RichText::new(choice.label()).size(12.0)
                                    .color(if selected { theme::ON_ACCENT() } else { theme::TXT() })
                            )
                            .min_size(egui::vec2(btn_w, 26.0));
                            btn = if selected {
                                btn.fill(theme::ACC())
                            } else {
                                btn.fill(theme::SURF3()).stroke(egui::Stroke::new(1.0_f32, theme::BDR()))
                            };
                            if ui.add(btn).clicked() && !selected {
                                theme::set_theme(choice);
                                self.settings.theme = choice;
                                self.save_settings();
                            }
                        }
                    });
                    ui.add_space(4.0);
                }

                ui.add_space(6.0);
                theme::section_hdr(ui, "Notifications");
                if ui.checkbox(&mut self.settings.play_sound_on_completion,
                    RichText::new("Play a sound when a run finishes").size(12.0))
                    .changed()
                {
                    self.save_settings();
                }

                // About is static info, not a preference, so it's set off
                // with a real divider (rather than the section_hdr accent
                // bar the toggle groups above use) to read as a distinct
                // kind of content rather than a third settable option.
                ui.add_space(14.0);
                ui.separator();
                ui.add_space(8.0);

                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("ComicInfo Generator")
                        .size(13.0).color(theme::TXT()).strong());
                    ui.add_space(2.0);
                    ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                        .size(11.0).color(theme::TDIM()));
                    ui.add_space(6.0);
                    ui.hyperlink_to(
                        RichText::new("GitHub").size(11.5).color(theme::ACC2()),
                        env!("CARGO_PKG_REPOSITORY"),
                    );
                });
            });

        // Click-outside-closes, same behavior as the Add Tag menu:
        // check for a click this frame that landed outside both the
        // window's own rect and the toolbar button that opens it
        // (excluding the button avoids the very click that just opened
        // Settings also registering as "outside" and immediately
        // closing it again the same frame -- the window doesn't exist
        // yet on the frame the button is clicked, so without this
        // exclusion every open would self-close instantly). Skipped
        // entirely while already mid-closing-fade (disable_settings_ui)
        // -- a stray click during that brief fade shouldn't matter, the
        // close is already in progress.
        if !disable_settings_ui {
            if let Some(resp) = &settings_window_resp {
                let window_rect = resp.response.rect;
                let clicked_outside = ctx.input(|i| i.pointer.any_click())
                    && ctx.input(|i| i.pointer.interact_pos())
                        .map(|pos| !window_rect.contains(pos) && !self.settings_btn_rect.contains(pos))
                        .unwrap_or(false);
                if clicked_outside {
                    open = false;
                }
            }
        }

        if !open && !self.settings_closing {
            // The close button was clicked this frame (while genuinely
            // open, not already mid-fade-out) -- start the fade-out
            // instead of closing immediately. self.settings_open stays
            // true for now; the guard clause at the top of this fn
            // keeps it rendering (non-interactively, fading) via
            // settings_closing until that timer completes.
            self.settings_closing = true;
            self.settings_closed_at = std::time::Instant::now();
        } else if open {
            self.settings_open = true;
        }
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
    //
    // Known cosmetic limitation, left as-is rather than chasing further:
    // egui's own maintainer has confirmed ScrollArea isn't customizable
    // enough from application code to stop it from relocating a nested
    // scrollbar to stay "visible" when the outer (vertical) ScrollArea
    // has scrolled this field's own row out of the visible viewport --
    // that's deliberate on egui's part for other situations (e.g. a wide
    // table in a narrow container), but here it can occasionally show
    // this field's scrollbar hovering at the wrong vertical position
    // once the field itself has scrolled out of view. Several attempts
    // at a fully custom (non-ScrollArea) replacement each introduced a
    // worse, harder-to-diagnose regression instead of fixing this
    // cleanly, so this simpler version -- accepting the occasional
    // stray-bar glitch -- is the one actually being kept.
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

    // Small circular "?" button placed next to a card's title, used
    // consistently across every tab. Returns true on click; callers open
    // Dialog::HelpText with their own title/body text.
    fn help_btn(ui: &mut egui::Ui) -> bool {
        ui.add(
            egui::Button::new(RichText::new("?").size(11.0).color(theme::TDIM()).strong())
                .fill(Color32::TRANSPARENT)
                .stroke(egui::Stroke::new(1.0_f32, theme::BDR()))
                .rounding(egui::Rounding::same(9.0))
                .min_size(egui::vec2(18.0, 18.0))
        ).on_hover_text("What does this do?").clicked()
    }

    fn path_row(ui: &mut egui::Ui, label: &str, val: &mut String, tip: &str) -> bool {
        let mut clicked = false;
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.add_sized([152.0, 26.0], egui::Label::new(
                RichText::new(label).color(theme::TDIM()).size(12.0)
            ));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(
                    egui::Button::new(RichText::new("Browse").size(11.5).color(theme::TXT()))
                        .fill(theme::SURF3())
                        .stroke(egui::Stroke::new(1.0_f32, theme::BDR()))
                        .rounding(egui::Rounding::same(5.0))
                        .min_size(egui::vec2(74.0, 26.0))
                ).clicked() { clicked = true; }
                let resp = ui.add(
                    egui::TextEdit::singleline(val)
                        .font(egui::FontId::new(12.0, egui::FontFamily::Monospace))
                        .hint_text(RichText::new("Browse or type a path...").color(theme::TMUT()))
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
        ui.painter().rect_filled(hr, egui::Rounding::same(4.0), theme::SURF3());
        let mut cx = hr.left() + 6.0;
        for (name, w) in cols {
            ui.painter().text(
                egui::pos2(cx, hr.center().y),
                egui::Align2::LEFT_CENTER, *name,
                egui::FontId::new(11.5, egui::FontFamily::Proportional), theme::ACC2(),
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
            let bg = if is_sel { theme::ACC() } else if i%2==0 { theme::SURF2() } else { theme::ROW_ALT() };
            let tc = if is_sel { theme::ON_ACCENT() } else { theme::TXT() };
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
            .color(theme::TDIM()).size(11.0));
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
                    let color = if is_active { theme::TXT() } else { theme::TDIM() };
                    // Whole row is the handle -- there's nothing else
                    // interactive in it to conflict with.
                    handle.ui(ui, |ui| {
                        egui::Frame::none()
                            .fill(theme::SURF3())
                            .stroke(egui::Stroke::new(1.0_f32, theme::BDR()))
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
    // Checks whether `values`' [Start, End] range (indices 0 and 1)
    // overlaps any OTHER rule's range in `existing`. `skip_idx` excludes
    // the row currently being edited (None when adding a fresh rule),
    // so re-saving a rule unchanged -- or editing some other field on it
    // -- never flags itself as overlapping its own prior range.
    // Malformed existing rows (can't parse as numbers) are skipped
    // rather than treated as a match, matching how find_volume/find_date/
    // find_summary already tolerate bad data elsewhere.
    // Checks whether `values`' output Volume number (index 2) is already
    // produced by a DIFFERENT rule in `existing` -- specific to Volume
    // Rules, where the range represents chapters and column 2 is the
    // resulting volume number. `skip_idx` excludes the row being edited,
    // same as rule_range_overlaps. Compared as trimmed strings, not
    // parsed as numbers: "1" and "01" are treated as different values
    // deliberately, since zero-padding is a display choice elsewhere in
    // this app (Zero-Padding setting), not something this equality check
    // should second-guess or normalize.
    fn volume_value_duplicated(values: &[String], existing: &[Vec<String>], skip_idx: Option<usize>) -> bool {
        let Some(new_vol) = values.get(2).map(|v| v.trim()) else { return false; };
        if new_vol.is_empty() { return false; }
        existing.iter().enumerate().any(|(i, row)| {
            if Some(i) == skip_idx { return false; }
            row.get(2).map(|v| v.trim()) == Some(new_vol)
        })
    }

    fn rule_range_overlaps(values: &[String], existing: &[Vec<String>], skip_idx: Option<usize>) -> bool {
        let (Some(new_lo), Some(new_hi)) = (
            values.first().and_then(|v| v.trim().parse::<f64>().ok()),
            values.get(1).and_then(|v| v.trim().parse::<f64>().ok()),
        ) else { return false; };
        // Normalize in case Start > End was typed -- the overlap check
        // itself shouldn't depend on entry order.
        let (new_lo, new_hi) = (new_lo.min(new_hi), new_lo.max(new_hi));

        existing.iter().enumerate().any(|(i, row)| {
            if Some(i) == skip_idx { return false; }
            let Some(lo) = row.first().and_then(|v| v.trim().parse::<f64>().ok()) else { return false; };
            let Some(hi) = row.get(1).and_then(|v| v.trim().parse::<f64>().ok()) else { return false; };
            let (lo, hi) = (lo.min(hi), lo.max(hi));
            new_lo <= hi && lo <= new_hi
        })
    }

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
            ui.label(RichText::new(title).color(theme::ACC2()).strong().size(12.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // "?" added FIRST so it lands at the true right edge of
                // the row -- in a right-to-left layout, the first widget
                // added claims the rightmost position, and each
                // subsequent widget packs further left. Adding it last
                // (as a previous pass here mistakenly left it) puts it at
                // the LEFT edge of this button group instead -- closer to
                // the title than to the window's actual right edge,
                // which is the bug being fixed here.
                if Self::help_btn(ui) {
                    let body = match target {
                        RuleTarget::Volume => "Maps a range of CHAPTER numbers to a single Volume number -- e.g. chapters 1 through 10 all belong to Volume 1.\n\n\
                            Ch Start / Ch End: the inclusive chapter range this rule covers (works with decimal chapters too, e.g. 5.5).\n\n\
                            Volume: the volume number written into ComicInfo.xml for every chapter in that range, when \"Include volume number in metadata\" is on (Processing tab).\n\n\
                            Rules are checked in order, and the FIRST matching range wins -- so overlapping ranges, or two different ranges both producing the same Volume number, are blocked at Save time, since either one means a rule can silently never take effect or a volume number stops representing one specific stretch of chapters.".to_string(),
                        RuleTarget::Date => "Maps a range of VOLUME numbers to a publication date -- e.g. volumes 1 through 1 (a single volume) were published on a specific Year/Month/Day.\n\n\
                            Vol Start / Vol End: the inclusive volume range this rule covers. Tick \"Vol Start and Vol End are the same\" in Add/Edit Rule to enter just one volume number instead of a range.\n\n\
                            Year / Month / Day: the publication date written into ComicInfo.xml for every chapter belonging to a volume in that range, when \"Use volume date rules for publication\" is on (Processing tab).\n\n\
                            This is the VOLUME-based counterpart to Episode Dates JSON (Paths tab), which maps individual chapter numbers to dates instead -- use whichever matches how your source actually publishes.".to_string(),
                        RuleTarget::Summary => "Maps a range of VOLUME numbers to a custom Summary -- e.g. volume 1 gets its own specific summary text, separate from every other volume.\n\n\
                            Vol Start / Vol End: the inclusive volume range this rule covers. Tick \"Vol Start and Vol End are the same\" in Add/Edit Rule to enter just one volume number instead of a range.\n\n\
                            Summary: the text written into ComicInfo.xml's Summary field for every chapter belonging to a volume in that range, when \"Use per-volume summary rules\" is on (Processing tab). Takes priority over the Default Summary (Metadata tab) for volumes it covers.".to_string(),
                    };
                    pending = Some(Dialog::HelpText { title: title.to_string(), body });
                }
                ui.add_space(2.0);
                if ui.add(egui::Button::new(RichText::new("Remove").size(11.0).color(theme::TERR())).fill(Color32::TRANSPARENT).stroke(egui::Stroke::new(1.0_f32, theme::BDR())).rounding(egui::Rounding::same(5.0)).min_size(egui::vec2(0.0,24.0))).clicked() {
                    if let Some(idx) = *sel {
                        if idx < rows.len() { rows.remove(idx); }
                        *sel = None;
                    } else if !rows.is_empty() {
                        pending = Some(Dialog::ConfirmClearAllRules(target));
                    }
                }
                ui.add_space(2.0);
                if ui.add(egui::Button::new(RichText::new("Edit").size(11.0).color(theme::ACC2())).fill(Color32::TRANSPARENT).stroke(egui::Stroke::new(1.0_f32, theme::BDR())).rounding(egui::Rounding::same(5.0)).min_size(egui::vec2(0.0,24.0))).clicked() {
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
                if ui.add(egui::Button::new(RichText::new("Add").size(11.0).color(theme::TGOOD())).fill(Color32::TRANSPARENT).stroke(egui::Stroke::new(1.0_f32, theme::BDR())).rounding(egui::Rounding::same(5.0)).min_size(egui::vec2(0.0,24.0))).clicked() {
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
        let mut help = None::<(&str, &str)>;
        egui::ScrollArea::vertical().id_salt("paths_scr").show(ui, |ui| {
            egui::Frame::none()
                .inner_margin(egui::Margin::symmetric(20.0, 16.0))
                .show(ui, |ui| {

            theme::card().show(ui, |ui| {
                let mut clicked = false;
                theme::section_hdr_with_help(ui, "File Paths", &mut clicked);
                if clicked {
                    help = Some(("File Paths", "CBZ Folder is the only required field -- everything else is optional.\n\n\
                        CBZ Folder: the folder containing the .cbz comic archive files you want to process. All .cbz files directly inside this folder are picked up when you click Start Processing (subfolders are not scanned).\n\n\
                        Titles JSON: an optional file mapping chapter or volume numbers to titles, e.g. {\"1\": \"The Beginning\", \"2\": \"Rising Action\"}. If a number in this file matches a chapter/volume being processed, that title is used in both the renamed filename and the XML Title field. If left blank, or a specific number isn't in the file, the original filename is kept as-is (sanitized only) and used as the title. This same file is used whether you're numbering by chapter, episode, or volume.\n\n\
                        Episode Dates JSON: an optional file mapping chapter/episode numbers directly to a publication date, e.g. {\"1\": \"Jul 25, 2019\"}. This is a per-chapter alternative to the Date Rules table on the Rules tab (which maps VOLUME ranges to dates) -- use whichever matches how your source actually publishes dates."));
                }
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
                let mut clicked = false;
                theme::section_hdr_with_help(ui, "Output Mode", &mut clicked);
                if clicked {
                    help = Some(("Output Mode", "Controls whether the original .cbz files are modified, or left untouched with new files written instead.\n\n\
                        Write new CBZ (unchecked by default): when OFF, the original .cbz is renamed and its ComicInfo.xml is updated in place -- the safest option to leave off if you want to keep working the same way this app has always worked.\n\n\
                        When ON, a completely new .cbz file is written and the original is never modified, renamed, or deleted.\n\n\
                        Subfolder next to source: the new files are written into an \"output\" folder created inside the same folder as your originals. This folder is created automatically if it doesn't exist. Writing new files directly into the source folder isn't offered as an option: if a new file's computed name ever happened to match an original's name, it would silently overwrite it -- exactly what this feature exists to prevent.\n\n\
                        Custom folder: choose any folder you like for the new files, via Browse or by typing a path directly."));
                }
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
                                RichText::new("Output Folder:").color(theme::TDIM()).size(12.0)
                            ));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add(
                                    egui::Button::new(RichText::new("Browse").size(11.5).color(theme::TXT()))
                                        .fill(theme::SURF3())
                                        .stroke(egui::Stroke::new(1.0_f32, theme::BDR()))
                                        .rounding(egui::Rounding::same(5.0))
                                        .min_size(egui::vec2(74.0, 26.0))
                                ).clicked() {
                                    self.start_pick(PathPick::OutputPath);
                                }
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.cfg.output_path)
                                        .font(egui::FontId::new(12.0, egui::FontFamily::Monospace))
                                        .hint_text(RichText::new("Browse or type a folder path...").color(theme::TMUT()))
                                        .desired_width(f32::INFINITY)
                                ).on_hover_text("New CBZ files are written here instead of the source folder.");
                            });
                        });
                        if self.cfg.output_path.trim().is_empty() {
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.add_space(20.0);
                                ui.label(RichText::new("Choose a folder, or switch back to \"Subfolder next to source\".")
                                    .color(theme::TWARN()).size(11.0));
                            });
                        }
                    }
                }
            });

                }); // Frame
        });
        if let Some((title, body)) = help {
            self.dialog = Some(Dialog::HelpText { title: title.to_string(), body: body.to_string() });
        }
    }

    fn show_processing(&mut self, ui: &mut egui::Ui) {
        let mut help = None::<(&str, &str)>;
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
                        let mut clicked = false;
                        theme::section_hdr_with_help(ui, "Mode & Volume Metadata", &mut clicked);
                        if clicked {
                            help = Some(("Mode & Volume Metadata", "Mode is a shortcut that sets the 3 checkboxes below it -- it has no other effect and isn't itself saved into the XML.\n\n\
                                Manga: turns ON all 3 checkboxes below (the usual case for series organized into volumes).\n\n\
                                Manhwa / Manhua: turns OFF all 3 checkboxes below (manhwa/manhua are typically published as standalone chapters with no volume structure).\n\n\
                                You can still flip any of the 3 checkboxes individually afterward -- picking a Mode is just a fast starting point, not a locked setting.\n\n\
                                Include volume number in metadata: writes a Volume field into ComicInfo.xml, using whatever the Volume Rules table (Rules tab) maps the current chapter to.\n\n\
                                Use volume date rules for publication: overrides the Year/Month/Day fields with whatever the Date Rules table (Rules tab) maps the current volume to, instead of using Episode Dates JSON or leaving them blank.\n\n\
                                Use per-volume summary rules: overrides the Summary field with whatever the Summary Rules table (Rules tab) maps the current volume to, instead of the Default Summary on the Metadata tab."));
                        }
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
                        let mut clicked = false;
                        theme::section_hdr_with_help(ui, "Title Separator", &mut clicked);
                        if clicked {
                            help = Some(("Title Separator", "Controls the text placed between the number and the title in the renamed filename and the XML Title -- e.g. the \" - \" in \"Episode 40 - My Title\".\n\n\
                                By default, this is \" - \" for Episode/Volume prefixes and \": \" for Chapter (matching how each is conventionally written). Override separator replaces this with whatever you type in the Separator box.\n\n\
                                Avoid characters that aren't valid in filenames: / \\ : * ? \" < > |\n\n\
                                The Preview box below always shows exactly what the final filename would look like with your current settings, updating live as you change Separator, Number Prefix, or Zero-Padding."));
                        }
                        if ui.checkbox(&mut self.cfg.csep_on,
                            RichText::new("Override separator").size(12.0))
                            .on_hover_text("Replaces the default ' - ' or ': ' between number and title.")
                            .changed() {
                            self.rebuild_sep_preview();
                        }
                        ui.horizontal(|ui| {
                            ui.add_space(20.0);
                            ui.label(RichText::new("Separator:").color(theme::TXT()).size(12.0));
                            let r = ui.add_enabled(self.cfg.csep_on,
                                egui::TextEdit::singleline(&mut self.cfg.csep).desired_width(80.0))
                                .on_hover_text("e.g. \"-\" or \"~\"   (avoid  / \\ : * ? \" < > |  - invalid in filenames)");
                            if r.changed() { self.rebuild_sep_preview(); }
                        });
                        ui.add_space(4.0);
                        egui::Frame::none().fill(theme::SURF3()).rounding(egui::Rounding::same(4.0))
                            .inner_margin(egui::Margin::symmetric(8.0, 4.0)).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Preview:").color(theme::TDIM()).size(11.0));
                                ui.label(RichText::new(&self.sep_preview).color(theme::ACC2())
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
                        let mut clicked = false;
                        theme::section_hdr_with_help(ui, "Processing Settings", &mut clicked);
                        if clicked {
                            help = Some(("Processing Settings", "Max Workers: how many files are processed in parallel. Higher values finish a batch faster on multi-core machines, but with heavy disk or antivirus activity, a very high number can sometimes be slower than a moderate one -- 4 is a reasonable default, and there's rarely a benefit to going far beyond your CPU's core count.\n\n\
                                Dry Run: runs through every step -- reading files, computing new names, resolving titles/dates/summaries from your rules -- and reports exactly what WOULD happen, without modifying, renaming, or writing anything. Use this to sanity-check your rules and settings before committing to a real run.\n\n\
                                Log directory / Open Folder: every run appends to a log file in this folder, useful for reviewing exactly what happened after the fact, especially for a large batch."));
                        }
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Max Workers:").color(theme::TDIM()).size(12.0));
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
                            .color(theme::TMUT()).size(11.0));
                        ui.add_space(4.0);
                        if ui.add(
                            egui::Button::new(RichText::new("Open Folder").size(11.0).color(theme::TDIM()))
                                .fill(Color32::TRANSPARENT)
                                .stroke(egui::Stroke::new(1.0_f32, theme::BDR()))
                                .rounding(egui::Rounding::same(4.0))
                                .min_size(egui::vec2(0.0, 20.0))
                        ).on_hover_text("Open the folder containing progress and error logs for past runs.").clicked() {
                            let _ = std::fs::create_dir_all(&log_path);
                            Self::open_in_file_manager(&log_path);
                        }
                    });
                    ui.add_space(12.0);
                });

                let rc = &mut cols[1];
                egui::Frame::none().outer_margin(egui::Margin { left:8.0, right:20.0, ..Default::default() }).show(rc, |ui| {
                    // Prefix mode + Post-finale -- merged into one card
                    // since post-finale behaviour is a refinement of the
                    // same numbering scheme, and Post-Finale Behaviour
                    // alone was a one-dropdown sliver next to much taller
                    // neighbors.
                    theme::card().show(ui, |ui| {
                        let mut clicked = false;
                        theme::section_hdr_with_help(ui, "Number Prefix", &mut clicked);
                        if clicked {
                            help = Some(("Number Prefix", "Controls the word placed before the chapter/episode/volume number in the renamed filename and the XML Title -- the \"Episode\" in \"Episode 40\".\n\n\
                                Auto-detect from filename: looks at each file's own original name and picks Episode, Chapter, or Volume based on which word (or an abbreviation like \"Ep.\"/\"Ch.\"/\"Vol.\") already appears in it.\n\n\
                                Always: Episode / Chapter / Volume: forces every file to use that same word, regardless of what the original filename says.\n\n\
                                Custom: uses whatever text you type into Custom text instead of Episode/Chapter/Volume -- useful for series with non-standard numbering (e.g. \"Break\", \"Part\", \"Side Story\").\n\n\
                                Post-Finale Behaviour (below the divider): once a chapter is marked as the finale (via the Finale Chapter Detected prompt shown when starting a run), this controls how chapters AFTER it are numbered.\n\n\
                                strip: removes the number prefix entirely from post-finale chapters, leaving just their title -- useful for side stories or bonus chapters that come after the main story ends.\n\n\
                                keep: continues numbering post-finale chapters normally, as if nothing changed."));
                        }
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
                            ui.label(RichText::new("Custom text:").color(theme::TXT()).size(12.0));
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
                            ui.label(RichText::new("After finale:").color(theme::TXT()).size(12.0));
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
                        let mut clicked = false;
                        theme::section_hdr_with_help(ui, "Zero-Padding", &mut clicked);
                        if clicked {
                            help = Some(("Zero-Padding", "Pads chapter/episode/volume numbers with leading zeros -- e.g. \"1\" becomes \"01\" with a width of 2.\n\n\
                                Width sets how many digits the number is padded to. A number that's already at or beyond that width is left unchanged (e.g. width 2 leaves \"123\" as \"123\", it never truncates).\n\n\
                                Decimal chapter numbers (e.g. \"5.5\") are never padded, since padding a fraction doesn't have a sensible meaning -- they're written exactly as found.\n\n\
                                This only affects the DISPLAYED number in the filename and Title -- it has no effect on which Rules-tab range a chapter falls into, since those are matched by the actual numeric value, not its padded text."));
                        }
                        if ui.checkbox(&mut self.cfg.zero_pad, RichText::new("Zero-pad numbers  (e.g. 01, 02 ...)").size(12.0)).changed() {
                            self.rebuild_sep_preview();
                        }
                        ui.horizontal(|ui| {
                            ui.add_space(20.0);
                            ui.add_enabled(self.cfg.zero_pad, egui::Label::new(RichText::new("Width:").color(theme::TXT()).size(12.0)));
                            if ui.add_enabled(self.cfg.zero_pad, egui::DragValue::new(&mut self.cfg.pad_width).range(1..=5)).changed() {
                                self.rebuild_sep_preview();
                            }
                        });
                    });
                    ui.add_space(12.0);
                });
            });
        });
        if let Some((title, body)) = help {
            self.dialog = Some(Dialog::HelpText { title: title.to_string(), body: body.to_string() });
        }
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
                        .color(theme::TXT()).strong().size(12.5));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Added FIRST so it lands at the far right -- a
                        // right-to-left layout packs widgets starting
                        // from the right edge in the order they're
                        // added, so adding "?" last (as this was left,
                        // mistakenly, from an earlier pass) put it at the
                        // LEFT edge of this button group instead of the
                        // window's true right edge.
                        if Self::help_btn(ui) {
                            pending_dialog = Some(Dialog::HelpText {
                                title: "Constant Metadata".to_string(),
                                body: "These fields are written into EVERY CBZ's ComicInfo.xml exactly as shown here -- Series, Writer, Genre, and so on don't usually change chapter to chapter, so they're set once here rather than per-file.\n\n\
                                    Add Tag: adds another ComicInfo v2.1 field to this list (only fields not already added are offered).\n\n\
                                    Remove: removes the currently-selected field below (click a field's name to select it first).\n\n\
                                    Tag Order: opens a separate window where you can drag fields into whatever order you want them written to the XML -- purely cosmetic for most readers, but some tools care about tag order.\n\n\
                                    Community Rating specifically has its own \"1-10 scale\" checkbox next to it when added -- tick it to enter a MyAnimeList/AniList-style score out of 10, which is automatically converted to the ComicInfo schema's real 0-5 scale on write.\n\n\
                                    A field left blank here is simply omitted from the XML entirely, rather than being written as an empty tag.".to_string(),
                            });
                        }
                        ui.add_space(2.0);
                        if ui.add(
                            egui::Button::new(RichText::new("Remove").size(11.0).color(theme::TERR()))
                                .fill(Color32::TRANSPARENT)
                                .stroke(egui::Stroke::new(1.0_f32, theme::BDR()))
                                .rounding(egui::Rounding::same(5.0))
                                .min_size(egui::vec2(0.0, 24.0))
                        ).on_hover_text("Remove the selected field below.").clicked() {
                            pending_remove = true;
                        }
                        ui.add_space(2.0);
                        if ui.add(
                            egui::Button::new(RichText::new("Add Tag").size(11.0).color(theme::TGOOD()))
                                .fill(Color32::TRANSPARENT)
                                .stroke(egui::Stroke::new(1.0_f32, theme::BDR()))
                                .rounding(egui::Rounding::same(5.0))
                                .min_size(egui::vec2(0.0, 24.0))
                        ).on_hover_text("Add another ComicInfo field.").clicked() {
                            pending_dialog = Some(Dialog::AddMetadataTag);
                        }
                        ui.add_space(2.0);
                        if ui.add(
                            egui::Button::new(RichText::new("Tag Order").size(11.0).color(theme::ACC()))
                                .fill(Color32::TRANSPARENT)
                                .stroke(egui::Stroke::new(1.0_f32, theme::BDR()))
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
                            .size(11.0).color(theme::TDIM()))
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
            ui.add_space(10.0);

            theme::card().show(ui, |ui| {
                let mut clicked = false;
                theme::section_hdr_with_help(ui, "Default Summary  (Chapter 1 + fallback)", &mut clicked);
                if clicked {
                    pending_dialog = Some(Dialog::HelpText {
                        title: "Default Summary".to_string(),
                        body: "This text is used as the Summary field for Chapter 1, and as a fallback for any other chapter/volume that doesn't get a more specific summary from the Summary Rules table (Rules tab).\n\n\
                            If \"Use per-volume summary rules\" is on (Processing tab) and a chapter's volume has a matching entry in Summary Rules, that takes priority over this text. Otherwise, every chapter uses this Default Summary as-is.\n\n\
                            Leave this blank if you'd rather have no Summary at all for chapters not covered by a Summary Rule.".to_string(),
                    });
                }
                ui.add(egui::TextEdit::multiline(&mut self.cfg.summary)
                    .desired_rows(6).desired_width(f32::INFINITY)
                    .font(egui::FontId::new(12.0, egui::FontFamily::Monospace)));
            });

            if pending_dialog.is_some() { self.dialog = pending_dialog; }

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
        // effect on the other two cards' size.
        //
        // outer_available_h is captured right here, before entering
        // ScrollArea::vertical() or any nested Frame. This is the one
        // point where ui.available_height() reliably reports the tab's
        // real visible height: a plain ScrollArea::vertical() defaults to
        // an unbounded (max_height = f32::INFINITY) content area by
        // design, so available_height() called from INSIDE it reports
        // something tied to that unbounded sizing instead of the real
        // viewport -- a well-known egui quirk (available_width/height not
        // reliably accounting for a containing panel's own margins from
        // inside nested containers).
        let outer_available_h = ui.available_height();

        // This value is used both for the Frame below and for the height
        // subtraction above it -- so the two can never silently drift
        // apart the way separate hardcoded constants did in earlier
        // attempts at this fix. (Read back via Margin's own l/r/t/b
        // fields intentionally avoided here: those fields' exact numeric
        // type isn't worth depending on across egui versions.)
        let outer_margin_v: f32 = 16.0; // top + bottom, each side

        egui::ScrollArea::vertical().id_salt("rules_scr").show(ui, |ui| {
        egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(20.0, outer_margin_v))
            .show(ui, |ui| {

        const CARD_PAD: f32 = 28.0; // theme::card()'s 14px inner_margin, top + bottom
        const GAPS: f32 = 20.0;     // 2 x 10px add_space between the 3 cards
        let fair_share = ((outer_available_h - 2.0 * outer_margin_v
                            - GAPS - 3.0 * CARD_PAD) / 3.0).max(100.0);
        let fair_share = fair_share - 12.5;

        // Renders one card and pads it up to fair_share if its actual
        // content (measured via ui.scope(), not estimated) is shorter --
        // never shrinks it below what its own rows need.
        let rule_card = |ui: &mut egui::Ui, add_contents: &mut dyn FnMut(&mut egui::Ui)| {
            theme::card().show(ui, |ui| {
                let r = ui.scope(|ui| add_contents(ui));
                let natural_h = r.response.rect.height();
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
            .fill(theme::SURF())
            .stroke(egui::Stroke::new(1.0_f32, theme::BDR()))
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
                        .stroke(egui::Stroke::new(1.5_f32, stop_col))
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
                        ui.label(RichText::new("Processing...").color(theme::TGOOD()).size(12.0));
                    }
                    if self.cfg.dry_run {
                        ui.add_space(8.0);
                        ui.label(RichText::new("DRY RUN -- files will NOT be modified")
                            .color(theme::TWARN()).size(12.0).strong());
                    }

                    // Right-aligned group: "?" and, once a run has
                    // started, the progress fraction. Both live in ONE
                    // right-to-left sub-layout added LAST in this row --
                    // verified directly (a standalone layout test) that
                    // adding a right-to-left sub-layout FIRST in a
                    // left-to-right horizontal does NOT claim the row's
                    // true right edge; it instead pushes everything added
                    // afterward (Start Processing included) to the
                    // right, which is the opposite of what's wanted here.
                    // Added last, "?" first within it, correctly lands at
                    // the far right with the progress numbers sitting
                    // cleanly to its left, no overlap, Start Processing
                    // unaffected at the left edge.
                    let (done, total) = self.progress;
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if Self::help_btn(ui) {
                            self.dialog = Some(Dialog::HelpText {
                                title: "Run".to_string(),
                                body: "Start Processing: begins processing every .cbz file in the CBZ Folder (Paths tab), applying your Constant Metadata, Rules, and every other setting from the other tabs.\n\n\
                                    If a previous run on this same folder was interrupted, you'll be asked whether to resume from where it left off or start fresh.\n\n\
                                    Stop: cancels an in-progress run after the file currently being processed finishes -- already-completed files are not undone.\n\n\
                                    Dry Run (Processing tab): if enabled, this run only reports what WOULD happen without actually modifying, renaming, or writing any files.\n\n\
                                    Verbose (Log Output, below): shows additional detail in the log, such as per-file processing steps that are otherwise summarized.\n\n\
                                    Clear (Log Output, below): erases everything currently shown in the log. This cannot be undone, and you'll be asked to confirm first.\n\n\
                                    The log keeps every previous run's output as you start new ones, growing downward -- scroll up to see earlier runs.".to_string(),
                            });
                        }
                        if total > 0 {
                            ui.add_space(8.0);
                            let pct = done as f32 / total as f32;
                            ui.label(RichText::new(format!("{}%", (pct * 100.0) as u32))
                                .color(theme::ACC2()).strong().size(14.0));
                            ui.add_space(8.0);
                            ui.label(RichText::new(format!("{done} / {total}"))
                                .color(theme::TDIM()).size(11.0));
                        }
                    });
                });
            });

        // ── Progress bar ─────────────────────────────────────────────────────
        let (done, total) = self.progress;
        let frac = if total > 0 { done as f32 / total as f32 } else { 0.0 };
        egui::Frame::none()
            .fill(theme::BDR())
            .inner_margin(egui::Margin::ZERO)
            .show(ui, |ui| {
                let bar_w = (ui.available_width() * frac).max(if self.running { 4.0 } else { 0.0 });
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 4.0), egui::Sense::hover()
                );
                ui.painter().rect_filled(rect, egui::Rounding::ZERO, theme::BDR());
                if bar_w > 0.0 {
                    let fill_rect = egui::Rect::from_min_size(rect.min, egui::vec2(bar_w, 4.0));
                    let col = if frac >= 1.0 { theme::TGOOD() } else { theme::ACC() };
                    ui.painter().rect_filled(fill_rect, egui::Rounding::ZERO, col);
                }
            });

        // ── Log header ────────────────────────────────────────────────────────
        egui::Frame::none()
            .fill(theme::SURF2())
            .stroke(egui::Stroke::new(1.0_f32, theme::BDR()))
            .rounding(egui::Rounding::same(8.0))
            .inner_margin(egui::Margin::symmetric(14.0, 6.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Log Output").color(theme::ACC2()).strong().size(12.0));
                    ui.add_space(8.0);
                    ui.checkbox(&mut self.verbose, RichText::new("Verbose").color(theme::TDIM()).size(11.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(
                            egui::Button::new(RichText::new("Clear").size(11.0).color(theme::TDIM()))
                                .fill(Color32::TRANSPARENT)
                                .stroke(egui::Stroke::new(1.0_f32, theme::BDR()))
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
            .fill(theme::BG())
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
            .fill(theme::SURF())
            .stroke(egui::Stroke::new(1.0_f32, theme::BDR()))
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
                        let num_col = if is_err { theme::TERR() }
                                      else if val > 0 { theme::TGOOD() }
                                      else { theme::TMUT() };
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
                                ui.label(RichText::new(lbl).color(theme::TMUT()).size(10.0));
                            },
                        );
                        if i < st.len() - 1 {
                            let (vl, _) = ui.allocate_exact_size(
                                egui::vec2(divider_w, cell_h), egui::Sense::hover()
                            );
                            ui.painter().rect_filled(vl, egui::Rounding::ZERO, theme::BDR());
                        }
                    }
                });
            });
            }); // Frame
    }

    // ── Single File Mode ──────────────────────────────────────────────────────
    // Code-editor-style layout: a file tree on the left, the ComicInfo.xml
    // editor for whichever file is selected on the right. Entirely
    // separate from the batch-processing tabs' CentralPanel -- called
    // directly from update() instead of going through show_tabbar/the
    // Tab enum, since none of that machinery applies here.
    fn show_single_file_mode(&mut self, ctx: &egui::Context, opacity: f32) {
        // The file tree and editor each render as their own theme::card()
        // -- same rounded-box-with-border language every other panel in
        // this app already uses -- rather than styling the SidePanel/
        // CentralPanel frames directly. egui's panel frames sit flush
        // against the window edge and each other with no visible gap,
        // so rounding them individually wouldn't read as two separated
        // boxes the way an inner card with breathing room around it
        // does. The panels themselves stay unstyled/transparent and
        // exist only for the resizable-width layout mechanics.
        //
        // To make each card's painted box actually reach the full height
        // of its panel (not shrink to fit its content, leaving bare
        // background below it): per egui's own maintainer, the correct
        // pattern is ui.allocate_space(ui.available_size()) as the LAST
        // thing inside the frame's content closure, not something set on
        // the outer ui beforehand -- that claims the remaining space from
        // inside the frame's own content_ui, so the frame measures out to
        // the full size and paints its background/border around all of
        // it, all the way down.
        // Both panels use the exact same symmetric margin -- kept
        // deliberately simple (egui::Margin::symmetric, not a per-side
        // struct literal) since this project's pinned egui 0.29 takes
        // f32 margins the way symmetric()/same() already do throughout
        // this file, but Margin's own field types have changed across
        // egui versions (newer releases use i8 with a separate MarginF32
        // for floats), so constructing one field-by-field isn't
        // something to risk without being able to compile-check it here.
        // Same value on both panels fixes the top/bottom gap looking
        // inconsistent between the two boxes; using a smaller value than
        // before (8.0) also shrinks the doubled-up gap between the two
        // boxes (tree's right margin + editor's left margin stack
        // together, so that gap reads wider than a single outer edge
        // even when every margin is identical) down closer to what a
        // single edge looks like.
        let panel_margin = egui::Margin::symmetric(8.0, 8.0);

        egui::SidePanel::left("sfm_file_tree")
            .frame(egui::Frame::none().fill(theme::BG()).inner_margin(panel_margin))
            .resizable(true)
            // The default resize-drag indicator: a vertical line drawn at
            // the panel boundary regardless of any Frame styling applied
            // to the panel or its content, since it's a separate visual
            // element from the panel's own frame. The card already gives
            // a visible border of its own, so this extra line just reads
            // as a stray duplicate right next to it.
            .show_separator_line(false)
            .default_width(240.0)
            .width_range(160.0..=420.0)
            .show(ctx, |ui| {
                ui.set_opacity(opacity);
                theme::card().show(ui, |ui| {
                    self.sfm_file_tree(ui);
                    ui.allocate_space(ui.available_size());
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(theme::BG()).inner_margin(panel_margin))
            .show(ctx, |ui| {
                ui.set_opacity(opacity);
                theme::card().show(ui, |ui| {
                    self.sfm_editor_panel(ui);
                    ui.allocate_space(ui.available_size());
                });
            });
    }

    fn sfm_editor_panel(&mut self, ui: &mut egui::Ui) {
        match self.sfm.panel.clone() {
            SfmPanelState::Empty => {
                ui.vertical_centered(|ui| {
                    ui.add_space(60.0);
                    ui.label(RichText::new("No file selected").size(14.0).color(theme::TDIM()));
                });
            }
            SfmPanelState::LoadError(msg) => {
                ui.vertical_centered(|ui| {
                    ui.add_space(60.0);
                    ui.label(RichText::new("Couldn't read this file").size(14.0).color(theme::TERR()));
                    ui.add_space(6.0);
                    ui.label(RichText::new(msg).size(11.0).color(theme::TDIM()));
                });
            }
            SfmPanelState::NoComicInfo => {
                let name = self.sfm.selected.and_then(|i| self.sfm.files.get(i))
                    .map(|f| f.name.clone()).unwrap_or_default();
                ui.vertical_centered(|ui| {
                    ui.add_space(60.0);
                    ui.label(RichText::new(format!("{name} has no ComicInfo.xml"))
                        .size(14.0).color(theme::TXT()));
                    ui.add_space(4.0);
                    ui.label(RichText::new("Title and Series will be filled in from the filename and folder.")
                        .size(11.0).color(theme::TDIM()));
                    ui.add_space(12.0);
                    if ui.add(theme::btn_primary("  Add ComicInfo.xml  ")).clicked() {
                        self.sfm_create_default();
                    }
                });
            }
            SfmPanelState::Editing => self.sfm_editor_editing_ui(ui),
        }
    }

    fn sfm_editor_editing_ui(&mut self, ui: &mut egui::Ui) {
        let name = self.sfm.selected.and_then(|i| self.sfm.files.get(i))
            .map(|f| f.name.clone()).unwrap_or_default();
        let dirty = self.sfm.dirty();
        // Snapshot at the very top of the frame, before any widget below
        // can mutate self.sfm.tags. If this frame turns out to start a
        // brand new word (pending_undo_word was None coming in), this IS
        // the correct "before" state for that word's eventual undo
        // step -- whatever the tags looked like before anything this
        // frame touched them. If a word was already in progress from an
        // earlier frame, this snapshot is simply discarded in favor of
        // the one already held in pending_undo_word.
        let frame_start_snapshot = self.sfm.tags.clone();

        ui.horizontal(|ui| {
            ui.label(RichText::new(&name).size(15.0).color(theme::TXT()).strong());
            if dirty {
                ui.label(RichText::new("(unsaved)").size(11.0).color(theme::TWARN()));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let save_btn = ui.add_enabled(dirty, theme::btn_primary("  Save  "));
                if save_btn.clicked() {
                    self.sfm_save_current();
                }
                ui.add_space(6.0);
                // Tags already present in this file's own tag list are
                // excluded from the add-menu -- same "can't add a
                // duplicate" rule the batch-mode metadata field picker
                // (Dialog::AddMetadataTag) already follows.
                let existing: HashSet<String> = self.sfm.tags.iter().map(|r| r.tag.clone()).collect();
                let addable_now: Vec<&'static str> = COMICINFO_FIELDS.iter()
                    .map(|f| f.tag)
                    .chain(SFM_EXTRA_KNOWN_TAGS.iter().copied())
                    .filter(|t| !existing.contains(*t))
                    .collect();
                let add_btn = ui.add_enabled(!addable_now.is_empty(), theme::btn_secondary("  Add Tag  "));
                if add_btn.clicked() {
                    self.sfm.add_tag_menu_open = !self.sfm.add_tag_menu_open;
                }
                if self.sfm.add_tag_menu_open {
                    self.sfm_add_tag_menu(ui.ctx(), add_btn.rect, frame_start_snapshot.clone());
                }
            });
        });
        ui.add_space(4.0);

        if let Some(foreign) = &self.sfm.foreign_tags_notice {
            let list = foreign.join(", ");
            ui.label(
                RichText::new(format!(
                    "This file has {} tag{} not recognized by this app: {list}. \
                     They're shown below and will be kept if you save.",
                    foreign.len(), if foreign.len() == 1 { "" } else { "s" }
                ))
                .size(11.0).color(theme::TWARN())
            );
            ui.add_space(6.0);
        }

        ui.add_space(4.0);

        let mut remove_id: Option<u64> = None;
        // Detected inside the show_vec closure below (word boundaries per
        // row) but can't be acted on there -- show_vec holds self.sfm.tags
        // mutably borrowed one row at a time for the whole closure, so
        // there's no way to also touch self.sfm_undo_stack or
        // self.sfm.pending_undo_word (which needs the FULL tags list, not
        // one row) from inside it. Collected here, applied after
        // show_vec returns and the borrow ends.
        let mut word_boundary_hit = false;
        // Explicit max_height rather than letting the ScrollArea
        // auto-size to "fill remaining space" (the default with no
        // max_height set): egui has a known bug (emilk/egui#3385) where
        // a single-axis ScrollArea sizing itself this way overflows a
        // few pixels past its container's own bottom margin instead of
        // clipping to it -- read available_height() right here, after
        // everything above (header, notice, add-tag row) has already
        // been laid out this frame, so it reflects the genuine remaining
        // room, then hand that to the ScrollArea explicitly so it clips
        // correctly instead of hitting that overflow path at all.
        //
        // The reserved buffer here (SFM_SCROLL_BOTTOM_GAP, shared with
        // sfm_file_tree's identical fix) needs to be the same fixed
        // value in both places rather than each computed independently:
        // this panel and the file tree panel have different amounts of
        // content above their own ScrollArea (header/notice/add-tag row
        // here; folder label/separator there), and whether either one's
        // list is actually long enough to need a scrollbar varies too --
        // both affect exactly how many pixels egui's own overflow bug
        // eats. A small buffer (4.0) that happened to look right in one
        // panel didn't reliably match the other; a larger, shared
        // constant is a deliberate hedge against that variance rather
        // than a value tuned to one specific screenshot.
        let remaining_height = (ui.available_height() - SFM_SCROLL_BOTTOM_GAP).max(0.0);
        // ScrollStyle::solid() as the base, not just flipping `floating`
        // on the default: Default::default() for ScrollStyle is actually
        // floating(), so setting only .floating = false on top of it
        // leaves every other field (bar_width, margins, ...) at
        // floating()'s wider values -- still solid-positioned (no more
        // Remove-button overlap) but visually just as thick as before.
        // solid() itself already defaults to a slim 6.0-point bar with no
        // hover-driven growth (that hover-expand behavior is explicitly
        // a floating-only concept per egui's own docs, so it isn't
        // available here -- kept a fixed slim width instead of
        // reintroducing float-over-content just to get that effect
        // back). Scoped with ui.scope so this only affects these two SFM
        // ScrollAreas rather than changing scrollbar behavior app-wide.
        ui.scope(|ui| {
            let mut scroll_style = egui::style::ScrollStyle::solid();
            // Correction: bar_outer_margin is the gap between the bar
            // and the CONTAINER's edge (increasing it pushes the bar
            // left, away from the edge -- the opposite of what's wanted
            // here). bar_inner_margin is the actual gap between the
            // CONTENT (Remove buttons) and the bar, which is what needed
            // increasing from solid()'s default of 4.0 to push the bar
            // away from the buttons and closer to the card's edge.
            scroll_style.bar_inner_margin = 10.0;
            ui.style_mut().spacing.scroll = scroll_style;
            egui::ScrollArea::vertical().id_salt("sfm_tags_scroll").max_height(remaining_height).show(ui, |ui| {
            egui_dnd::dnd(ui, "sfm_tags_dnd")
                .show_vec(&mut self.sfm.tags, |ui, row, handle, _state| {
                    let spec = field_spec(&row.tag);
                    let label = spec.map(|s| s.label).unwrap_or(row.tag.as_str());
                    let is_foreign = spec.is_none() && !SFM_EXTRA_KNOWN_TAGS.contains(&row.tag.as_str());

                    ui.horizontal(|ui| {
                        handle.ui(ui, |ui| {
                            ui.label(RichText::new("::").size(14.0).color(theme::TDIM()));
                        });
                        ui.add_space(4.0);
                        let name_color = if is_foreign { theme::TWARN() } else { theme::TXT() };
                        ui.label(RichText::new(label).size(12.0).color(name_color).monospace())
                            .on_hover_text(if is_foreign {
                                "Not recognized by this app -- kept as-is on save.".to_string()
                            } else {
                                spec.map(|s| s.tip.to_string()).unwrap_or_default()
                            });
                        ui.add_space(6.0);

                        match spec.map(|s| s.kind) {
                            Some(FieldKind::Enum(options)) => {
                                let before = row.value.clone();
                                egui::ComboBox::from_id_salt(("sfm_tag_cb", row.id))
                                    .width(220.0)
                                    .selected_text(row.value.as_str())
                                    .show_ui(ui, |ui| {
                                        for opt in options {
                                            ui.selectable_value(&mut row.value, opt.to_string(), *opt);
                                        }
                                    });
                                // A dropdown pick is always a complete,
                                // atomic choice -- there's no partial
                                // "word in progress" concept for an enum
                                // field the way there is for free text,
                                // so every change here is its own
                                // immediate undo step.
                                if row.value != before { word_boundary_hit = true; }
                            }
                            _ => {
                                // Summary has no FieldSpec (it's one of
                                // SFM_EXTRA_KNOWN_TAGS, excluded from the
                                // registry -- see that const's own
                                // comment), so it would otherwise fall
                                // through to the same flat 230.0 default
                                // every other unspecified tag gets. That
                                // reads as cramped for what's normally a
                                // full paragraph -- 560.0 matches the
                                // width the batch-mode Summary Rules
                                // column already uses for the same field
                                // elsewhere in this app, rather than
                                // picking a new number. (A multi-line
                                // text area was also tried for Summary
                                // specifically, but reverted: it
                                // correlated with a large, unexplained
                                // empty gap appearing at the top of the
                                // WHOLE tag list, on every file --
                                // including ones without a Summary row
                                // at all once one had ever been rendered
                                // in the session -- and the actual cause
                                // was never conclusively identified
                                // despite ruling out egui_dnd's handle
                                // logic, the card-fill allocate_space
                                // call, and stale scroll position. Back
                                // to the single-line scrollable box,
                                // confirmed working, until a real root
                                // cause is found.
                                let width = spec.map(|s| s.width)
                                    .unwrap_or(if row.tag == "Summary" { 560.0 } else { 230.0 });
                                let resp = Self::scrollable_text_edit(ui, &row.tag, &mut row.value, width);
                                if resp.changed() {
                                    // A boundary character just landed at
                                    // the end of the value (the character
                                    // just typed, since TextEdit only
                                    // grows/shrinks from wherever the
                                    // cursor is -- checking the tail is a
                                    // reasonable proxy without needing
                                    // the actual cursor position or a
                                    // full diff against the previous
                                    // value) -- or focus just left the
                                    // field entirely, which always ends
                                    // whatever word was in progress
                                    // regardless of the character at the
                                    // cursor.
                                    let ends_word = row.value.ends_with(|c: char| c.is_whitespace())
                                        || resp.lost_focus();
                                    if ends_word { word_boundary_hit = true; }
                                }
                            }
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(egui::Button::new(RichText::new("Remove").size(11.0).color(theme::TDIM())))
                                .clicked()
                            {
                                remove_id = Some(row.id);
                            }
                        });
                    });
                    ui.add_space(4.0);
                });
            });
        });
        if let Some(id) = remove_id {
            self.sfm.tags.retain(|r| r.id != id);
            // Removing a row is its own immediate, atomic undo step, same
            // reasoning as an enum dropdown pick -- there's no sense in
            // which "delete this tag" is part of a word in progress.
            self.sfm_push_undo(frame_start_snapshot.clone());
        } else if word_boundary_hit {
            // Commit whichever snapshot represents "before this word":
            // one already in progress from an earlier frame, or (if none
            // was) the one taken at the top of this same frame -- see
            // frame_start_snapshot's comment above for why that's
            // correct even for a word that starts and ends in one frame.
            let before = self.sfm.pending_undo_word.take().unwrap_or(frame_start_snapshot);
            self.sfm_push_undo(before);
        } else if self.sfm.tags != frame_start_snapshot {
            // Something changed this frame without crossing a text-edit
            // word boundary. Two real cases land here: mid-word typing
            // (track as in-progress, same as before), or a drag-reorder
            // (every row's id+value pair is still present, just in a
            // different order) -- which has no "word in progress"
            // concept at all and should commit immediately, same as
            // add/remove/enum-pick, not wait for a boundary that will
            // never come.
            let same_rows_different_order = {
                let mut a: Vec<(u64, &String)> = self.sfm.tags.iter().map(|r| (r.id, &r.value)).collect();
                let mut b: Vec<(u64, &String)> = frame_start_snapshot.iter().map(|r| (r.id, &r.value)).collect();
                a.sort_by_key(|(id, _)| *id);
                b.sort_by_key(|(id, _)| *id);
                a == b
            };
            if same_rows_different_order {
                self.sfm_push_undo(frame_start_snapshot);
            } else if self.sfm.pending_undo_word.is_none() {
                self.sfm.pending_undo_word = Some(frame_start_snapshot);
            }
        }
    }

    /// Renders the floating "Add Tag" menu (a plain egui::Window used
    /// as a manually-positioned popup -- see add_tag_menu_open's own
    /// comment for why a real ComboBox couldn't do this on egui 0.29)
    /// anchored just under `anchor_rect` (the Add Tag button's own
    /// rect). Clicking a tag adds it and leaves the menu open so more
    /// tags can be added in one go; the menu only closes via an
    /// explicit click outside both the window and the button itself.
    fn sfm_add_tag_menu(&mut self, ctx: &egui::Context, anchor_rect: egui::Rect, frame_start_snapshot: Vec<SfmTagRow>) {
        let existing: HashSet<String> = self.sfm.tags.iter().map(|r| r.tag.clone()).collect();
        let mut addable: Vec<&'static str> = COMICINFO_FIELDS.iter()
            .map(|f| f.tag)
            .chain(SFM_EXTRA_KNOWN_TAGS.iter().copied())
            .filter(|t| !existing.contains(*t))
            .collect();
        addable.sort();
        if addable.is_empty() {
            // Every addable tag has already been added since the menu
            // was opened (e.g. the user added the last remaining one
            // this same session) -- nothing left to show, so close
            // automatically rather than leave an empty floating window
            // up with nothing to click.
            self.sfm.add_tag_menu_open = false;
            return;
        }

        let mut menu_rect = anchor_rect; // fallback if the Window is somehow never actually shown below
        let mut to_add: Option<&'static str> = None;
        egui::Window::new("sfm_add_tag_menu")
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            // pivot(RIGHT_TOP) + the button's own bottom-RIGHT corner:
            // the menu's right edge stays aligned with the button's
            // right edge and grows leftward, regardless of how wide its
            // content (the longest tag label) ends up making it. With
            // the default pivot (LEFT_TOP) anchored to the button's
            // bottom-left instead, a menu wider than the button drifts
            // further left the wider it gets, which is what was making
            // it look unanchored/disconnected from the button that
            // opened it.
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(anchor_rect.right_bottom() + egui::vec2(0.0, 4.0))
            .frame(theme::card())
            .show(ctx, |ui| {
                const MENU_WIDTH: f32 = 200.0;
                ui.set_min_width(MENU_WIDTH);
                // Same slim, non-overlapping scrollbar style used for
                // the file tree and tag list elsewhere in Single File
                // Mode -- solid() (reserves its own width, doesn't
                // overlay on top of the tag labels) rather than the
                // default floating style, which read as thick and was
                // sitting on top of the last visible label.
                ui.scope(|ui| {
                    ui.style_mut().spacing.scroll = egui::style::ScrollStyle::solid();
                    // max_width matched to MENU_WIDTH (not left to grow
                    // to the window's own width independently), and
                    // each label given that same explicit width via
                    // min_size -- without both of these, a
                    // selectable_label only ever claims as much width
                    // as its own text needs, leaving a gap of bare card
                    // background between the shortest labels and the
                    // scrollbar sitting at the window's true right edge.
                    egui::ScrollArea::vertical().max_height(280.0).max_width(MENU_WIDTH).show(ui, |ui| {
                        for tag in &addable {
                            let label = field_spec(tag).map(|s| s.label).unwrap_or(tag);
                            let resp = ui.add_sized(
                                egui::vec2(ui.available_width(), 22.0),
                                egui::SelectableLabel::new(false, label),
                            );
                            if resp.clicked() {
                                to_add = Some(tag);
                            }
                        }
                    });
                });
                menu_rect = ui.min_rect();
            });

        if let Some(tag) = to_add {
            let id = self.sfm.next_row_id;
            self.sfm.next_row_id += 1;
            self.sfm.tags.push(SfmTagRow { tag: tag.to_string(), value: String::new(), id });
            // Adding a row is its own atomic undo step, same reasoning
            // as Remove and an enum pick. Deliberately does NOT close
            // the menu (add_tag_menu_open untouched) -- see this
            // function's own doc comment.
            self.sfm_push_undo(frame_start_snapshot);
        }

        // Close on a click that's outside both the menu window and the
        // button that opened it (excluding the button avoids the same
        // click that just toggled the menu open from also immediately
        // registering as an outside-click and closing it again in the
        // same frame).
        let clicked_outside = ctx.input(|i| i.pointer.any_click())
            && ctx.input(|i| i.pointer.interact_pos())
                .map(|pos| !menu_rect.contains(pos) && !anchor_rect.contains(pos))
                .unwrap_or(false);
        if clicked_outside {
            self.sfm.add_tag_menu_open = false;
        }
    }

    /// Pushes `before` (the tag list as it was immediately before the
    /// change that just happened) onto the undo stack for whichever file
    /// is currently selected, truncating any redo entries ahead of the
    /// cursor first -- standard undo/redo semantics: a fresh edit made
    /// after undoing invalidates whatever redo history existed past that
    /// point, same as any text editor.
    /// Records one completed change: `before` is the tag list as it was
    /// immediately prior, `self.sfm.tags` (read at call time) is the
    /// "after" -- undo applies `before`, redo applies `after`, perfectly
    /// symmetric. Truncates any redo entries ahead of the cursor first --
    /// standard undo/redo semantics: a fresh edit made after undoing
    /// invalidates whatever redo history existed past that point, same
    /// as any text editor.
    fn sfm_push_undo(&mut self, before: Vec<SfmTagRow>) {
        let Some(idx) = self.sfm.selected else { return };
        let Some(entry) = self.sfm.files.get(idx) else { return };
        self.sfm_undo_stack.truncate(self.sfm_undo_cursor);
        self.sfm_undo_stack.push(SfmUndoEntry {
            file: entry.path.clone(),
            before,
            after: self.sfm.tags.clone(),
        });
        self.sfm_undo_cursor = self.sfm_undo_stack.len();
    }

    /// Ctrl+Z. Applies entry[cursor - 1].before, switching to whichever
    /// file it belongs to first if that isn't the one currently open (per
    /// the answered design question: if the current file also has
    /// unsaved changes, auto-save-on-focus-change decides whether that's
    /// a silent save-then-jump or the usual prompt -- a save doesn't
    /// invalidate the ability to undo further back later, since undo
    /// operates on the stack's own snapshots, not on dirty-vs-saved
    /// state).
    fn sfm_undo(&mut self) {
        if self.sfm_undo_cursor == 0 { return; }
        let entry = self.sfm_undo_stack[self.sfm_undo_cursor - 1].clone();
        // Whatever word/edit was in progress on the CURRENT file doesn't
        // get its own undo step just because undo was pressed mid-word --
        // it's simply abandoned in favor of jumping to the target state.
        self.sfm.pending_undo_word = None;
        self.sfm_undo_cursor -= 1;
        let target_file = entry.file.clone();
        let tags = entry.before;
        self.sfm_goto_undo_entry(target_file, tags);
    }

    /// Ctrl+Y. Mirror of sfm_undo: applies entry[cursor].after and moves
    /// the cursor forward instead of back.
    fn sfm_redo(&mut self) {
        if self.sfm_undo_cursor >= self.sfm_undo_stack.len() { return; }
        let entry = self.sfm_undo_stack[self.sfm_undo_cursor].clone();
        self.sfm.pending_undo_word = None;
        self.sfm_undo_cursor += 1;
        let target_file = entry.file.clone();
        let tags = entry.after;
        self.sfm_goto_undo_entry(target_file, tags);
    }

    /// Shared by undo and redo: gets onto `target_file` (immediately if
    /// it's already the open file or the switch needs no prompt, or via
    /// the deferred pending_undo_apply + pending_nav path if a prompt is
    /// needed first), then applies `tags`.
    fn sfm_goto_undo_entry(&mut self, target_file: PathBuf, tags: Vec<SfmTagRow>) {
        let already_open = self.sfm.selected
            .and_then(|i| self.sfm.files.get(i))
            .map(|f| f.path == target_file)
            .unwrap_or(false);

        if already_open {
            self.sfm_apply_undo_tags(tags);
            return;
        }

        let Some(target_idx) = self.sfm.files.iter().position(|f| f.path == target_file) else {
            // The undone/redone edit belongs to a file that isn't in the
            // CURRENTLY open folder's tree at all (a different folder or
            // single file was opened since) -- nothing sensible to jump
            // to, so just drop this step rather than silently doing
            // nothing with no explanation.
            self.dialog = Some(Dialog::Notice(
                "Can't jump to that change -- the file it belongs to isn't open right now.".to_string()
            ));
            return;
        };

        if self.sfm.dirty() {
            if self.settings.sfm_autosave_on_focus_change {
                self.sfm_save_current();
                self.sfm_switch_to(target_idx);
                self.sfm_apply_undo_tags(tags);
            } else {
                self.sfm.pending_nav = Some(SfmNavTarget::File(target_idx));
                self.sfm.pending_undo_apply = Some(tags);
                self.dialog = Some(Dialog::SfmUnsavedChanges);
            }
        } else {
            self.sfm_switch_to(target_idx);
            self.sfm_apply_undo_tags(tags);
        }
    }

    /// Replaces the editor's current tags with `tags` (an undo/redo
    /// target state) without touching loaded_tags -- deliberately, so
    /// dirty() correctly reads true when the undone/redone state differs
    /// from what's actually saved on disk, same as any other edit.
    fn sfm_apply_undo_tags(&mut self, tags: Vec<SfmTagRow>) {
        self.sfm.tags = tags;
    }

    fn sfm_file_tree(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.add(theme::btn_secondary("Open File")).clicked() {
                self.start_pick(PathPick::SfmFile);
            }
            if ui.add(theme::btn_secondary("Open Folder")).clicked() {
                self.start_pick(PathPick::SfmFolder);
            }
        });
        ui.add_space(8.0);

        if let Some(root) = &self.sfm.root {
            let label = root.file_name().map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.to_string_lossy().into_owned());
            ui.label(RichText::new(label).size(11.0).color(theme::TDIM()).italics());
            ui.add_space(6.0);
        }
        ui.separator();
        ui.add_space(4.0);

        if self.sfm.files.is_empty() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("Open a file or folder to get started.")
                    .size(12.0).color(theme::TDIM()));
            });
            return;
        }

        // See sfm_editor_editing_ui's identical fix and
        // SFM_SCROLL_BOTTOM_GAP's own comment for why max_height is set
        // explicitly here (emilk/egui#3385) using that same shared
        // constant rather than a value picked independently for this
        // panel.
        let remaining_height = (ui.available_height() - SFM_SCROLL_BOTTOM_GAP).max(0.0);
        // See sfm_editor_editing_ui's identical fix and comment: uses the
        // full ScrollStyle::solid() preset (slim, fixed-width bar with no
        // hover growth) rather than just flipping `floating` on the
        // default style, which would leave the bar at its wider
        // floating()-preset dimensions. Scoped so it only affects this
        // one ScrollArea.
        ui.scope(|ui| {
            // See sfm_editor_editing_ui's corrected comment: this needed
            // bar_inner_margin (gap between content and bar), not
            // bar_outer_margin (gap between bar and container edge,
            // which pushes the wrong direction).
            let mut scroll_style = egui::style::ScrollStyle::solid();
            scroll_style.bar_inner_margin = 10.0;
            ui.style_mut().spacing.scroll = scroll_style;
            egui::ScrollArea::vertical().id_salt("sfm_tree_scroll").max_height(remaining_height).show(ui, |ui| {
            // Collected rather than acted on inline: iterating
            // self.sfm.files while also wanting to call
            // self.sfm_request_select(idx) (which needs &mut self, i.e.
            // a second mutable borrow of self while files is already
            // borrowed via self.sfm.files.iter()) doesn't borrow-check.
            let mut clicked_idx = None;
            for (idx, entry) in self.sfm.files.iter().enumerate() {
                let selected = self.sfm.selected == Some(idx);
                let enabled = entry.is_cbz;
                let color = if !enabled {
                    theme::TMUT()
                } else if selected {
                    theme::ON_ACCENT()
                } else {
                    theme::TXT()
                };
                let btn = egui::Button::new(RichText::new(&entry.name).size(12.0).color(color))
                    .fill(if selected { theme::ACC() } else { Color32::TRANSPARENT })
                    .min_size(egui::vec2(ui.available_width(), 24.0));
                let resp = ui.add_enabled(enabled, btn);
                let was_clicked = resp.clicked();
                if enabled {
                    resp.on_hover_text(if selected { "Currently open".to_string() }
                        else { "Click to open".to_string() });
                } else {
                    resp.on_hover_text("Not a .cbz file");
                }
                if was_clicked {
                    clicked_idx = Some(idx);
                }
            }
            if let Some(idx) = clicked_idx {
                self.sfm_request_select(idx);
            }
            });
        });
    }
}