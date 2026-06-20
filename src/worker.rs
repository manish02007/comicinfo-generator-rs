//! Background processing thread — parses filenames, builds ComicInfo.xml, rewrites CBZs.
//!
//! Rayon is used for normal files (parallel).  Decimal-chapter files run
//! sequentially so the GUI can ask the user for a label without racing.

use crate::processing::*;
use crate::state::{LogLevel, RunStats};
use chrono::Datelike;
use std::sync::atomic::{AtomicBool, Ordering};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{
    mpsc::{Receiver, Sender},
    Arc, Mutex,
};

// ── Channel messages ──────────────────────────────────────────────────────────
#[derive(Debug)]
pub enum WorkerMsg {
    Log      { text: String, level: LogLevel },
    Progress { done: usize,  total: usize },
    Stats    { stats: RunStats },
    /// Worker needs a decimal-chapter label choice from the UI thread.
    DecimalRequest { filename: String, raw_title: String },
    Done     { stats: RunStats },
}

#[derive(Debug)]
pub enum UiMsg {
    DecimalResponse { result: String },
}

// ── Runtime config (owned by worker thread) ───────────────────────────────────
pub struct WorkerConfig {
    pub dry_run:          bool,
    pub use_vol:          bool,
    pub use_vol_date:     bool,
    pub use_vol_summ:     bool,
    pub prefix_mode:      String,
    pub custom_pfx:       String,
    pub post_finale_mode: String,   // "strip" | "keep"
    pub use_csep:         bool,
    pub csep:             String,
    pub zero_pad:         bool,
    pub pad_width:        usize,
    pub series:           String,
    pub writer:           String,
    pub penciller:        String,
    pub publisher:        String,
    pub language:         String,
    pub alt_series:       String,
    pub web:              String,
    pub genre:            String,
    pub rating:           String,
    pub year:             String,
    pub month:            String,
    pub day:              String,
    pub count:            String,
    pub summary:          String,
    pub custom_fields:    Vec<Vec<String>>,
    pub volume_rules:     Vec<Vec<String>>,
    pub date_rules:       Vec<Vec<String>>,
    pub summ_rules:       Vec<Vec<String>>,
    pub chapter_titles:   HashMap<String, String>,
    pub volume_titles:    HashMap<String, String>,
    pub dates_json:       HashMap<String, String>,
    pub max_workers:      usize,
    pub processed_files:  HashSet<String>,
    pub resume_mode:      bool,
    pub finale_index:     Option<usize>,
    pub finale_number:    Option<String>,
    pub final_ch_mode:    String,   // "final" | "normal"
    pub progress_file:    PathBuf,
    pub error_log_file:   PathBuf,
    pub verbose:          bool,
}

// ── Entry point (spawned in its own OS thread) ────────────────────────────────
pub fn run(
    cbz_files: Vec<PathBuf>,
    cfg:       WorkerConfig,
    tx:        Sender<WorkerMsg>,
    ui_rx:     Receiver<UiMsg>,
    stop:      Arc<AtomicBool>,
) {
    let cfg  = Arc::new(cfg);
    let stats = Arc::new(Mutex::new(RunStats::default()));
    let done_c = Arc::new(Mutex::new(0usize));
    let total   = cbz_files.len();
    let invalid_sep = is_sep_invalid_for_filename(&cfg.csep);

    let file_index_map: HashMap<String, usize> = cbz_files.iter()
        .enumerate()
        .map(|(i, p)| (p.file_name().unwrap_or_default().to_string_lossy().to_string(), i))
        .collect();
    let fim = Arc::new(file_index_map);

    // Auto-detect pad width
    let auto_pad = if cfg.zero_pad { detect_padding(&cbz_files) } else { None };

    // Split by decimal / normal
    let is_dec = |f: &PathBuf| is_decimal_file(&f.file_name().unwrap_or_default().to_string_lossy());
    let (decimal_files, normal_files): (Vec<_>, Vec<_>) = cbz_files.iter()
        .cloned()
        .partition(is_dec);

    let sep = "-".repeat(60);
    let ts = chrono::Local::now().format("%H:%M:%S");
    logq(&tx, sep.clone(), LogLevel::Sep);
    logq(&tx, format!(
        "  [START] {ts}  -  {total} files  ({} normal  -  {} decimal)",
        normal_files.len(), decimal_files.len()
    ), LogLevel::Head);
    logq(&tx, sep.clone(), LogLevel::Sep);

    // ── Parallel: normal files ────────────────────────────────────────────────
    // Rayon's ThreadPool::scope lets each task OWN a cloned Sender,
    // avoiding the Sender: !Sync issue with par_iter().for_each.
    {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(cfg.max_workers.max(1))
            .build()
            .unwrap_or_else(|_| rayon::ThreadPoolBuilder::new().build().unwrap());

        pool.scope(|s| {
            for path in &normal_files {
                if stop.load(Ordering::Relaxed) { break; }
                let tx_c   = tx.clone();
                let cfg_c  = Arc::clone(&cfg);
                let st_c   = Arc::clone(&stats);
                let dc_c   = Arc::clone(&done_c);
                let fim_c  = Arc::clone(&fim);
                let stop_c = Arc::clone(&stop);
                let path   = path.clone();
                s.spawn(move |_| {
                    if stop_c.load(Ordering::Relaxed) { return; }
                    process_one(
                        &path, &cfg_c, &tx_c, None,
                        auto_pad, total, invalid_sep, &st_c, &dc_c, &fim_c,
                    );
                });
            }
        });
    }

    // ── Sequential: decimal files (need GUI dialog) ───────────────────────────
    for path in &decimal_files {
        if stop.load(Ordering::Relaxed) { break; }
        process_one(
            path, &cfg, &tx, Some(&ui_rx),
            auto_pad, total, invalid_sep, &stats, &done_c, &fim,
        );
    }

    // ── Done ──────────────────────────────────────────────────────────────────
    let final_stats = stats.lock().unwrap().clone();
    let _ = tx.send(WorkerMsg::Done { stats: final_stats });
}

// ── Process one CBZ file ──────────────────────────────────────────────────────
fn process_one(
    path:    &Path,
    cfg:     &WorkerConfig,
    tx:      &Sender<WorkerMsg>,
    ui_rx:   Option<&Receiver<UiMsg>>,
    auto_pad: Option<usize>,
    total:   usize,
    invalid_sep: bool,
    stats:   &Arc<Mutex<RunStats>>,
    done_c:  &Arc<Mutex<usize>>,
    fim:     &Arc<HashMap<String, usize>>,
) {
    let file = path.file_name().unwrap_or_default().to_string_lossy().to_string();

    // Resume check
    if cfg.resume_mode && cfg.processed_files.contains(&file) {
        let mut dc = done_c.lock().unwrap();
        *dc += 1;
        let s = stats.lock().unwrap().clone();
        let _ = tx.send(WorkerMsg::Progress { done: *dc, total });
        let _ = tx.send(WorkerMsg::Stats { stats: s });
        return;
    }

    { let mut s = stats.lock().unwrap(); s.total += 1; }

    let mode_str = detect_file_type(&file);
    if cfg.verbose {
        logq(tx, format!("  -  {file}  ->  {mode_str}"), LogLevel::Dim);
    }

    // Extract number from filename
    let num_re = regex::Regex::new(r"\d+(?:\.\d+)?").unwrap();
    let num_m  = match num_re.find(&file) {
        Some(m) => m,
        None => {
            logq(tx, format!("[WARN] no number found - skipping: {file}"), LogLevel::Warn);
            bump_done(tx, done_c, total, stats);
            return;
        }
    };

    let orig_num   = num_m.as_str().to_string();
    let is_decimal = orig_num.contains('.');
    let mut number = orig_num.clone();

    // Zero-pad
    if cfg.zero_pad && !is_decimal {
        if let Ok(n) = orig_num.parse::<u64>() {
            let pw = auto_pad.unwrap_or(cfg.pad_width);
            number = format!("{n:0>pw$}");
        }
    }

    let prefix_word = get_prefix(&file, &cfg.prefix_mode, &cfg.custom_pfx);
    let base        = format!("{} {}", prefix_word.trim(), number);

    // Title lookup
    let titles    = if mode_str == "volume" { &cfg.volume_titles } else { &cfg.chapter_titles };
    let raw_title = titles.get(&orig_num)
        .or_else(|| titles.get(&number))
        .cloned()
        .or_else(|| extract_title_from_filename(&file))
        .unwrap_or_else(|| base.clone());

    // Decimal: ask the UI for a label choice
    let labelled_title = if is_decimal && mode_str != "volume" {
        if let Some(rx) = ui_rx {
            let _ = tx.send(WorkerMsg::DecimalRequest {
                filename:  file.clone(),
                raw_title: raw_title.clone(),
            });
            match rx.recv() {
                Ok(UiMsg::DecimalResponse { result }) => result,
                _ => raw_title.clone(),
            }
        } else {
            raw_title.clone()
        }
    } else {
        raw_title.clone()
    };

    let sep      = get_separator(&prefix_word, cfg.use_csep, &cfg.csep);
    let file_idx = fim.get(&file).copied();
    let after_finale = matches!(
        (cfg.finale_index, file_idx),
        (Some(fi), Some(idx)) if idx > fi
    );

    // ── Build XML title ───────────────────────────────────────────────────────
    let xml_title = build_xml_title(
        &orig_num, &number, &base, &sep, &labelled_title, &raw_title,
        mode_str, is_decimal, after_finale, cfg,
    );

    // ── Metadata dict ─────────────────────────────────────────────────────────
    let mut md: HashMap<&str, String> = HashMap::new();
    md.insert("Title",           xml_title.clone());
    md.insert("Number",          number.clone());
    md.insert("Series",          cfg.series.clone());
    md.insert("Writer",          cfg.writer.clone());
    md.insert("Penciller",       cfg.penciller.clone());
    md.insert("Publisher",       cfg.publisher.clone());
    md.insert("LanguageISO",     cfg.language.clone());
    md.insert("AlternateSeries", cfg.alt_series.clone());
    md.insert("Web",             cfg.web.clone());
    md.insert("Genre",           cfg.genre.clone());
    md.insert("Rating",          cfg.rating.clone());
    md.insert("Year",            cfg.year.clone());
    md.insert("Month",           cfg.month.clone());
    md.insert("Day",             cfg.day.clone());
    md.insert("Count",           cfg.count.clone());
    md.insert("Summary",         cfg.summary.clone());

    // Volume
    let volume = if cfg.use_vol {
        if mode_str == "volume" { Some(number.clone()) }
        else { find_volume(&orig_num, &cfg.volume_rules) }
    } else { None };
    if let Some(ref v) = volume { md.insert("Volume", v.clone()); }

    // Date from volume-date rules
    if cfg.use_vol_date {
        if let Some(ref v) = volume {
            if let Some((y, m, d)) = find_date(v, &cfg.date_rules) {
                md.insert("Year",  y.to_string());
                md.insert("Month", m.to_string());
                md.insert("Day",   d.to_string());
            }
        }
    } else if let Some(date_str) = cfg.dates_json.get(&orig_num) {
        if let Ok(dt) = chrono::NaiveDate::parse_from_str(date_str, "%b %d, %Y") {
            md.insert("Year",  dt.year().to_string());
            md.insert("Month", dt.month().to_string());
            md.insert("Day",   dt.day().to_string());
        }
    }

    // Summary from volume-summary rules
    if mode_str == "chapter" && orig_num == "1" {
        md.insert("Summary", cfg.summary.clone());
    } else if cfg.use_vol_summ {
        if let Some(ref v) = volume {
            let s = find_summary(v, &cfg.summ_rules).unwrap_or_else(|| cfg.summary.clone());
            md.insert("Summary", s);
        }
    }

    let xml_content = build_comic_info_xml(&md, &cfg.custom_fields);

    // ── New filename ──────────────────────────────────────────────────────────
    let safe_t = sanitize_filename(&raw_title);
    let fname_sep = if cfg.use_csep && !invalid_sep {
        format!(" {} ", cfg.csep.trim())
    } else {
        " - ".to_string()
    };
    let new_name = if raw_title == base {
        format!("{base}.cbz")
    } else {
        format!("{base}{fname_sep}{safe_t}.cbz")
    };
    let new_path = path.parent().unwrap_or(Path::new(".")).join(&new_name);
    // counter computed after bump_done (see below)

    // ── Write / Dry-run ───────────────────────────────────────────────────────
    if cfg.dry_run {
        stats.lock().unwrap().processed += 1;
        let pos = bump_done(tx, done_c, total, stats);
        logq(tx, format!("  [DRY] [{pos}/{total}]  {file}  ->  {new_name}"), LogLevel::Warn);
        logq(tx, format!("           XML title: {xml_title}"), LogLevel::Dim);
        return;
    } else {
        match write_comic_info_to_cbz(path, &xml_content) {
            Ok(()) => { stats.lock().unwrap().xml_updated += 1; }
            Err(e) => {
                let msg = format!("  [ERR] {file}  -  {e}");
                logq(tx, msg.clone(), LogLevel::Err);
                append_error_log(&cfg.error_log_file, &msg);
                stats.lock().unwrap().errors += 1;
                bump_done(tx, done_c, total, stats);
                return;
            }
        }
        if path != new_path && !new_path.exists() {
            match std::fs::rename(path, &new_path) {
                Ok(())  => { stats.lock().unwrap().renamed += 1; }
                Err(e)  => logq(tx, format!("  [WARN] rename failed: {e}"), LogLevel::Warn),
            }
        } else if new_path.exists() && path != new_path {
            stats.lock().unwrap().rename_skipped += 1;
        }
        mark_done(&cfg.progress_file, &file);
    }

    stats.lock().unwrap().processed += 1;
    // bump_done increments the counter and returns the new position —
    // this is the only correct way to get the counter in parallel code.
    let pos = bump_done(tx, done_c, total, stats);
    let ctr = format!("[{pos}/{total}]");

    if new_name != file {
        logq(tx, format!("  [OK] {ctr}  {file}"), LogLevel::Ok);
        logq(tx, format!("           ->  {new_name}"), LogLevel::Renamed);
    } else {
        logq(tx, format!("  [OK] {ctr}  {new_name}"), LogLevel::Ok);
    }
}

// ── XML title builder (extracted for clarity) ─────────────────────────────────
fn build_xml_title(
    orig_num:     &str,
    _number:      &str,
    base:         &str,
    sep:          &str,
    labelled:     &str,
    raw_title:    &str,
    mode:         &str,
    is_decimal:   bool,
    after_finale: bool,
    cfg:          &WorkerConfig,
) -> String {
    // Special: ch 0 = keep raw title
    if orig_num == "0" { return raw_title.to_string(); }

    // Special: named finale chapter
    if let Some(ref fin_num) = cfg.finale_number {
        if orig_num == fin_num && mode == "chapter" && cfg.chapter_titles.contains_key(orig_num) {
            return if cfg.final_ch_mode == "final" {
                format!("Final Chapter: {labelled}")
            } else {
                format!("{base}{sep}{labelled}")
            };
        }
    }

    // Post-finale: strip prefix if configured
    if after_finale && cfg.post_finale_mode == "strip" {
        return if is_decimal && mode != "volume" {
            labelled.to_string()
        } else {
            raw_title.to_string()
        };
    }

    // Decimal chapter with a label → just the label
    if is_decimal && mode == "chapter" {
        return labelled.to_string();
    }

    // No title beyond the base → keep base
    if raw_title == base { return base.to_string(); }

    // Normal: "Episode N - Title"
    format!("{base}{sep}{labelled}")
}

// ── Helpers ───────────────────────────────────────────────────────────────────
fn logq(tx: &Sender<WorkerMsg>, text: String, level: LogLevel) {
    let _ = tx.send(WorkerMsg::Log { text, level });
}

fn bump_done(tx: &Sender<WorkerMsg>, done_c: &Arc<Mutex<usize>>, total: usize, stats: &Arc<Mutex<RunStats>>) -> usize {
    let dc = { let mut dc = done_c.lock().unwrap(); *dc += 1; *dc };
    let s  = stats.lock().unwrap().clone();
    let _ = tx.send(WorkerMsg::Progress { done: dc, total });
    let _ = tx.send(WorkerMsg::Stats { stats: s });
    dc
}

fn mark_done(progress_file: &Path, filename: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(progress_file) {
        let _ = writeln!(f, "{filename}");
    }
}

fn append_error_log(log: &Path, msg: &str) {
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
        let _ = writeln!(f, "[{ts}] {msg}");
    }
}