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
    Arc, Mutex, LazyLock,
};

// Chapter/volume number extraction pattern, shared across every file in a
// batch instead of being recompiled per file inside process_one -- compiled
// once, on first use, then reused for the rest of the run (and any later
// run in the same process).
static NUM_RE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\d+(?:\.\d+)?").unwrap());

// ── Channel messages ──────────────────────────────────────────────────────────
#[derive(Debug)]
pub enum WorkerMsg {
    Log      { text: String, level: LogLevel },
    /// Several lines sent together as one channel message so they can never
    /// be interleaved by another thread's messages arriving in between --
    /// used to keep one file's whole report (detection + verbose + result)
    /// contiguous even though multiple files are processed in parallel.
    /// `idx` is the file's position in the originally sorted file list, so
    /// the UI can slot each block into its correct numeric order even
    /// though files complete in an arbitrary order across threads.
    LogBatch { idx: usize, lines: Vec<(String, LogLevel)> },
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
    // Output mode: false (default) overwrites the original in place; true
    // writes a new file instead and leaves the original completely
    // untouched. output_same_path/output_path determine the destination
    // folder when write_new_cbz is on.
    pub write_new_cbz:    bool,
    pub output_same_path: bool,
    pub output_path:      String,
    // Before overwriting a CBZ in place (only relevant when write_new_cbz is
    // off, since write_new_cbz never touches the original at all), copy the
    // untouched original into a "backups" subfolder first. Mirrors
    // AppSettings::backup_before_overwrite -- see state.rs for why this
    // lives outside AppConfig.
    pub backup_before_overwrite: bool,
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
    // Constant metadata as a dynamic tag->value map (e.g. "Series" -> "...").
    // Built from AppConfig::metadata_fields; the exact set of tags present
    // is entirely user-chosen via Add Tag / Remove in the Metadata tab.
    pub metadata_fields:  HashMap<String, String>,
    // CommunityRating is authored 0-10 (matches AppConfig::community_
    // rating_10_scale) and converted to the schema's real 0-5 scale once,
    // right when metadata_fields is turned into the per-file dict below.
    pub community_rating_10_scale: bool,
    pub tag_order:        Vec<String>,
    pub summary:          String,
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

    // Shared across all threads: written exactly once, the first time this
    // run actually has something to log to error_log_file. Runs with zero
    // errors/warnings never touch the file at all, so it stays lean instead
    // of accumulating an empty header for every successful run.
    let header_written = Arc::new(AtomicBool::new(false));
    let run_ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

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
                let hdr_c  = Arc::clone(&header_written);
                let run_ts_c = run_ts.clone();
                let path   = path.clone();
                s.spawn(move |_| {
                    if stop_c.load(Ordering::Relaxed) { return; }
                    process_one(
                        &path, &cfg_c, &tx_c, None,
                        auto_pad, total, invalid_sep, &st_c, &dc_c, &fim_c,
                        &hdr_c, &run_ts_c,
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
            &header_written, &run_ts,
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
    header_written: &AtomicBool,
    run_ts:  &str,
) {
    let file = path.file_name().unwrap_or_default().to_string_lossy().to_string();
    // Position in the originally sorted file list -- used as the sort key
    // so the UI can slot this file's log block into correct numeric order
    // regardless of which thread finishes processing it first.
    let file_idx = fim.get(&file).copied().unwrap_or(total);

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

    // Buffer every line for THIS file and flush it as one atomic LogBatch at
    // the end, instead of sending each line individually -- otherwise, since
    // multiple files are processed concurrently across rayon threads, lines
    // from different files interleave at the per-line level based on OS
    // thread scheduling, scrambling the log into an unreadable mix.
    let mut batch: Vec<(String, LogLevel)> = Vec::new();

    let mode_str = detect_file_type(&file);
    if cfg.verbose {
        batch.push((format!("  -  {file}  ->  {mode_str}"), LogLevel::Dim));
    }

    // Extract number from filename. Compiled once for the whole run (see
    // NUM_RE below) rather than per file -- this function runs once per
    // CBZ in the batch, across parallel rayon threads, so re-compiling the
    // same pattern every time was a redundant allocation per file for no
    // benefit (the pattern never changes).
    let num_m  = match NUM_RE.find(&file) {
        Some(m) => m,
        None => {
            let mut result = vec![(format!("[WARN] no number found - skipping: {file}"), LogLevel::Warn)];
            result.extend(batch);
            flush_batch(tx, file_idx, result);
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

    if cfg.verbose {
        let pad_note = if number != orig_num { format!("{orig_num} -> {number} (zero-padded)") }
                       else { format!("{number} (no padding)") };
        batch.push((format!(
            "       number={pad_note}  prefix=\"{}\" (mode={})",
            prefix_word.trim(), cfg.prefix_mode
        ), LogLevel::Dim));
    }

    // Original filename text (no extension), sanitized for filesystem
    // safety. Used as the fallback for BOTH the new filename and the XML
    // Title below when there's no reliable JSON title -- so the two always
    // agree instead of Title silently reverting to a bare "{prefix}
    // {number}" while the filename keeps whatever descriptive text the
    // original name had.
    let orig_stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(file.as_str());
    let orig_stem_safe = sanitize_filename(orig_stem);

    // Title lookup: only a non-empty value from chapter_titles.json /
    // volume_titles.json counts as a reliable title. With no titles file at
    // all (or an empty one), or this specific chapter/volume missing or
    // blank in it, the app makes no attempt to guess a title from the
    // filename -- the "New filename" step below keeps the original name
    // untouched (sanitized only), and ComicInfo.xml's Title matches it.
    let titles = if mode_str == "volume" { &cfg.volume_titles } else { &cfg.chapter_titles };
    // Both chapter_titles and volume_titles are populated from the same
    // single titles JSON file (see app.rs::make_worker_cfg) -- there's no
    // longer a separate chapter_titles.json/volume_titles.json to name
    // distinctly here.
    let titles_kind = "titles JSON";
    let json_entry = titles.get(&orig_num).or_else(|| titles.get(&number));
    let has_reliable_title = json_entry.map_or(false, |t| !t.is_empty());

    // Only warn when a titles file was actually configured (the map has
    // *some* entries) -- running with no titles file at all is the
    // expected, silent default, not something worth flagging per file.
    if !titles.is_empty() && !has_reliable_title {
        let reason = if json_entry.is_some() { "empty title" } else { "no entry" };
        let msg = format!(
            "  [WARN] {file}  -  {reason} for \"{orig_num}\" in {titles_kind}, using filename as-is"
        );
        batch.push((msg.clone(), LogLevel::Warn));
        append_error_log(&cfg.error_log_file, &msg, header_written, run_ts);
    }

    let title_source: &str;
    let raw_title: String = if has_reliable_title {
        title_source = titles_kind;
        json_entry.unwrap().clone()
    } else {
        title_source = "fallback -- using original filename as title";
        orig_stem_safe.clone()
    };

    if cfg.verbose {
        batch.push((format!(
            "       title=\"{raw_title}\"  (source: {title_source})"
        ), LogLevel::Dim));
    }

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

    let sep = get_separator(&prefix_word, cfg.use_csep, &cfg.csep);
    let after_finale = matches!(cfg.finale_index, Some(fi) if file_idx > fi);

    // ── Build XML title ───────────────────────────────────────────────────────
    let xml_title = build_xml_title(
        &orig_num, &number, &base, &sep, &labelled_title, &raw_title,
        mode_str, is_decimal, after_finale, has_reliable_title, cfg,
    );

    // ── Metadata dict ─────────────────────────────────────────────────────────
    // Start from the user's chosen constant-metadata tags (whatever set they
    // configured via Add Tag / Remove), then layer Title/Number/Volume/
    // Summary on top -- those four are always computed by this app's own
    // logic, never part of the user-editable constant set.
    let mut md: HashMap<String, String> = cfg.metadata_fields.clone();
    if cfg.community_rating_10_scale {
        if let Some(v) = md.get_mut("CommunityRating") {
            if let Ok(n) = v.parse::<f64>() {
                *v = format!("{:.1}", (n / 10.0 * 5.0).clamp(0.0, 5.0));
            }
        }
    }
    md.insert("Title".to_string(),  xml_title.clone());
    md.insert("Number".to_string(), number.clone());

    // Volume
    let volume = if cfg.use_vol {
        if mode_str == "volume" { Some(number.clone()) }
        else { find_volume(&orig_num, &cfg.volume_rules) }
    } else { None };
    if let Some(ref v) = volume { md.insert("Volume".to_string(), v.clone()); }

    if cfg.verbose {
        let vol_note = if !cfg.use_vol {
            "disabled".to_string()
        } else {
            match &volume {
                Some(v) if mode_str == "volume" => format!("{v} (file is a volume)"),
                Some(v) => format!("{v} (matched a volume rule)"),
                None    => "none (no rule matched)".to_string(),
            }
        };
        batch.push((format!("       volume={vol_note}"), LogLevel::Dim));
    }

    // Date from volume-date rules
    let mut date_matched = false;
    if cfg.use_vol_date {
        if let Some(ref v) = volume {
            if let Some((y, m, d)) = find_date(v, &cfg.date_rules) {
                md.insert("Year".to_string(),  y.to_string());
                md.insert("Month".to_string(), m.to_string());
                md.insert("Day".to_string(),   d.to_string());
                date_matched = true;
            }
        }
    } else if let Some(date_str) = cfg.dates_json.get(&orig_num) {
        if let Ok(dt) = chrono::NaiveDate::parse_from_str(date_str, "%b %d, %Y") {
            md.insert("Year".to_string(),  dt.year().to_string());
            md.insert("Month".to_string(), dt.month().to_string());
            md.insert("Day".to_string(),   dt.day().to_string());
            date_matched = true;
        }
    }

    if cfg.verbose {
        let date_note = if cfg.use_vol_date {
            if date_matched { format!("{}-{}-{}  (matched a date rule)", md["Year"], md["Month"], md["Day"]) }
            else { "no rule matched".to_string() }
        } else if date_matched {
            format!("{}-{}-{}  (from episode_dates.json)", md["Year"], md["Month"], md["Day"])
        } else {
            "not found in episode_dates.json".to_string()
        };
        batch.push((format!("       date={date_note}"), LogLevel::Dim));
    }

    // Summary from volume-summary rules
    let mut summ_matched = false;
    if mode_str == "chapter" && orig_num == "1" {
        md.insert("Summary".to_string(), cfg.summary.clone());
    } else if cfg.use_vol_summ {
        if let Some(ref v) = volume {
            if let Some(s) = find_summary(v, &cfg.summ_rules) {
                md.insert("Summary".to_string(), s);
                summ_matched = true;
            } else {
                md.insert("Summary".to_string(), cfg.summary.clone());
            }
        } else {
            // No volume rule matched this chapter at all -- still fall back
            // to the default rather than leaving Summary unset.
            md.insert("Summary".to_string(), cfg.summary.clone());
        }
    } else {
        md.insert("Summary".to_string(), cfg.summary.clone());
    }

    if cfg.verbose {
        let summ_note = if mode_str == "chapter" && orig_num == "1" {
            "default summary (chapter 1)".to_string()
        } else if !cfg.use_vol_summ {
            "disabled (using default summary)".to_string()
        } else if summ_matched {
            "matched a summary rule".to_string()
        } else {
            "no rule matched (using default summary)".to_string()
        };
        batch.push((format!("       summary={summ_note}"), LogLevel::Dim));
    }

    let xml_content = build_comic_info_xml(&md, &cfg.tag_order);

    // ── New filename ──────────────────────────────────────────────────────────
    let new_name = if has_reliable_title {
        let safe_t = sanitize_filename(&raw_title);
        let fname_sep = if cfg.use_csep && !invalid_sep {
            format!(" {} ", cfg.csep.trim())
        } else {
            " - ".to_string()
        };
        format!("{base}{fname_sep}{safe_t}.cbz")
    } else {
        // No reliable title: keep the original filename exactly as given,
        // only sanitizing characters the filesystem can't store. No
        // prefix/number/separator reconstruction here at all -- that
        // guessing is exactly what points 1/2 asked to remove.
        format!("{orig_stem_safe}.cbz")
    };

    // Destination differs by output mode:
    // - default (write_new_cbz=false): same folder as the source, then
    //   renamed in place after writing -- unchanged from every previous
    //   version of this app.
    // - write_new_cbz=true, custom folder (output_path): the user-chosen
    //   folder, written directly.
    // - write_new_cbz=true, "same folder as source" (output_same_path):
    //   a subfolder INSIDE the source folder, not the source folder
    //   itself. Writing directly into the source folder used the exact
    //   same name-computation as the in-place-rename path, so whenever
    //   the computed name happened to equal the original filename (the
    //   common case -- no reliable title means the name doesn't change
    //   at all), the "new" file silently overwrote the original in
    //   place, defeating the entire point of write_new_cbz ("don't
    //   overwrite the original file"). A subfolder makes that name
    //   collision structurally impossible rather than relying on names
    //   happening to differ.
    let output_dir = if cfg.write_new_cbz && !cfg.output_same_path {
        PathBuf::from(&cfg.output_path)
    } else if cfg.write_new_cbz {
        path.parent().unwrap_or(Path::new(".")).join("output")
    } else {
        path.parent().unwrap_or(Path::new(".")).to_path_buf()
    };
    let new_path = output_dir.join(&new_name);
    // counter computed after bump_done (see below)

    // ── Write / Dry-run ───────────────────────────────────────────────────────
    if cfg.dry_run {
        stats.lock().unwrap().processed += 1;
        // `pos` (completion order) still drives the progress bar via bump_done's
        // Progress message; the TEXT shown here uses file_idx+1 instead, so the
        // displayed number matches each file's fixed position in the sorted
        // list -- consistent with how blocks are now rendered in sort order,
        // rather than jumping around based on which thread finished first.
        let _pos = bump_done(tx, done_c, total, stats);
        let dest_note = if cfg.write_new_cbz {
            format!("{}", new_path.display())
        } else {
            new_name.clone()
        };
        let mut result = vec![
            (format!("  [DRY] [{}/{total}]  {file}  ->  {dest_note}", file_idx + 1), LogLevel::Warn),
            (format!("           XML title: {xml_title}"), LogLevel::Dim),
        ];
        result.extend(batch);
        flush_batch(tx, file_idx, result);
        return;
    } else if cfg.write_new_cbz {
        // Write directly to the final destination with its correct name in
        // one step. The source file is never opened for writing, renamed,
        // or modified in any way.
        match write_comic_info_xml_to(path, &new_path, &xml_content) {
            Ok(()) => { stats.lock().unwrap().xml_updated += 1; }
            Err(e) => {
                let msg = format!("  [ERR] {file}  -  {e}");
                let mut result = vec![(msg.clone(), LogLevel::Err)];
                result.extend(batch);
                flush_batch(tx, file_idx, result);
                append_error_log(&cfg.error_log_file, &msg, header_written, run_ts);
                stats.lock().unwrap().errors += 1;
                bump_done(tx, done_c, total, stats);
                return;
            }
        }
        mark_done(&cfg.progress_file, &file);
    } else {
        // Backup-before-overwrite: copy the still-untouched original into a
        // "backups" subfolder beside it before doing anything destructive.
        // This is a safety net the user explicitly opted into, so a failed
        // backup skips the file as an error rather than silently
        // overwriting without it -- fixing the "safety" toggle to actually
        // fail closed instead of quietly no-op'ing on a copy error.
        if cfg.backup_before_overwrite {
            let backup_dir  = path.parent().unwrap_or(Path::new(".")).join("backups");
            let backup_path = backup_dir.join(&file);
            if let Err(e) = std::fs::create_dir_all(&backup_dir)
                .and_then(|_| std::fs::copy(path, &backup_path).map(|_| ()))
            {
                let msg = format!("  [ERR] {file}  -  backup failed, skipped: {e}");
                let mut result = vec![(msg.clone(), LogLevel::Err)];
                result.extend(batch);
                flush_batch(tx, file_idx, result);
                append_error_log(&cfg.error_log_file, &msg, header_written, run_ts);
                stats.lock().unwrap().errors += 1;
                bump_done(tx, done_c, total, stats);
                return;
            }
        }
        match write_comic_info_to_cbz(path, &xml_content) {
            Ok(()) => { stats.lock().unwrap().xml_updated += 1; }
            Err(e) => {
                let msg = format!("  [ERR] {file}  -  {e}");
                let mut result = vec![(msg.clone(), LogLevel::Err)];
                result.extend(batch);
                flush_batch(tx, file_idx, result);
                append_error_log(&cfg.error_log_file, &msg, header_written, run_ts);
                stats.lock().unwrap().errors += 1;
                bump_done(tx, done_c, total, stats);
                return;
            }
        }
        if path != new_path && !new_path.exists() {
            match std::fs::rename(path, &new_path) {
                Ok(())  => { stats.lock().unwrap().renamed += 1; }
                Err(e)  => {
                    let msg = format!("  [WARN] {file}  -  rename failed: {e}");
                    batch.push((msg.clone(), LogLevel::Warn));
                    // The in-app log clears every session; persist this so a
                    // rename failure isn't lost the moment the app closes,
                    // same as write failures already are.
                    append_error_log(&cfg.error_log_file, &msg, header_written, run_ts);
                }
            }
        } else if new_path.exists() && path != new_path {
            stats.lock().unwrap().rename_skipped += 1;
        }
        mark_done(&cfg.progress_file, &file);
    }

    stats.lock().unwrap().processed += 1;
    // `pos` (completion order across threads) still drives the progress bar
    // via bump_done's Progress message. The displayed counter below uses
    // file_idx+1 instead -- the file's fixed position in the sorted list --
    // so it matches the order blocks actually render in, rather than
    // jumping around based on which thread happened to finish first.
    let _pos = bump_done(tx, done_c, total, stats);
    let ctr = format!("[{}/{total}]", file_idx + 1);

    let mut result = if new_name != file {
        vec![
            (format!("  [OK] {ctr}  {file}"), LogLevel::Ok),
            (format!("           ->  {new_name}"), LogLevel::Renamed),
        ]
    } else {
        vec![(format!("  [OK] {ctr}  {new_name}"), LogLevel::Ok)]
    };
    result.extend(batch);
    flush_batch(tx, file_idx, result);
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
    has_reliable_title: bool,
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

    // No reliable title (no titles file, or this entry missing/blank in
    // one) → raw_title is already the sanitized original filename text (or
    // the bare base, if the filename had nothing beyond the number/prefix)
    // -- use it directly rather than gluing base+sep+labelled together,
    // which would double up anything already present in raw_title.
    if !has_reliable_title { return raw_title.to_string(); }

    // Normal: "Episode N - Title"
    format!("{base}{sep}{labelled}")
}

// ── Helpers ───────────────────────────────────────────────────────────────────
fn logq(tx: &Sender<WorkerMsg>, text: String, level: LogLevel) {
    let _ = tx.send(WorkerMsg::Log { text, level });
}

/// Send every buffered line for one file as a single channel message, so
/// they arrive at the UI thread as one contiguous block instead of being
/// interleaved with another thread's lines mid-file. `idx` lets the UI
/// slot this block into its correct numeric position.
fn flush_batch(tx: &Sender<WorkerMsg>, idx: usize, batch: Vec<(String, LogLevel)>) {
    if !batch.is_empty() {
        let _ = tx.send(WorkerMsg::LogBatch { idx, lines: batch });
    }
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

fn append_error_log(log: &Path, msg: &str, header_written: &AtomicBool, run_ts: &str) {
    // swap() returns the PREVIOUS value -- exactly one thread among any
    // number running concurrently will see `false` here and write the
    // header; every other thread (this run or future calls this run) sees
    // `true` and skips it. This avoids a separate check-then-write race.
    if !header_written.swap(true, Ordering::SeqCst) {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
            let _ = writeln!(f, "=== Run started {run_ts} ===");
        }
    }
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
        let _ = writeln!(f, "[{ts}] {msg}");
    }
}