use regex::Regex;
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;
use unicode_normalization::UnicodeNormalization;

// ── Compiled regexes (lazy, thread-safe) ─────────────────────────────────────
fn re_volume()   -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)\b(vol|volume|v)\s*\d+").unwrap()) }
fn re_chapter()  -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)\b(ch|chapter)\s*\d+").unwrap()) }
fn re_episode()  -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)\b(ep|episode)\s*\d+").unwrap()) }
fn re_decimal()  -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\d+\.\d+").unwrap()) }
fn re_anynum()   -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\d+(?:\.\d+)?").unwrap()) }
fn re_intonly()  -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\b\d+\b").unwrap()) }
fn re_vol_kw()   -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)\b(vol(?:ume)?)\b").unwrap()) }
fn re_ch_kw()    -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)\b(ch(?:apter)?|ch\.)\b").unwrap()) }
fn re_spaces()   -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\s{2,}").unwrap()) }
fn re_dashes()   -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"[-\u2013\u2014]{2,}").unwrap()) }
fn re_unsafe()   -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#"[<>:"/\\|?*]"#).unwrap()) }
fn re_ctrls()    -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"[\n\r\t]").unwrap()) }
fn re_nat_split()-> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(\d+\.\d+|\d+)").unwrap()) }

// ── File-type detection ───────────────────────────────────────────────────────
pub fn detect_file_type(filename: &str) -> &'static str {
    let n = filename.to_lowercase();
    if re_volume().is_match(&n)  { return "volume";  }
    if re_chapter().is_match(&n) { return "chapter"; }
    if re_episode().is_match(&n) { return "episode"; }
    if re_decimal().is_match(&n) { return "chapter"; }
    if re_anynum().is_match(&n)  { return "chapter"; }
    "unknown"
}

pub fn is_decimal_file(filename: &str) -> bool {
    re_decimal().is_match(filename)
}

// ── Prefix ────────────────────────────────────────────────────────────────────
pub fn get_prefix(filename: &str, mode: &str, custom: &str) -> String {
    match mode {
        "custom"  => (if custom.is_empty() { "Episode" } else { custom }).to_string(),
        "episode" => "Episode".to_string(),
        "chapter" => "Chapter".to_string(),
        "volume"  => "Volume".to_string(),
        _ /* auto */ => {
            if re_vol_kw().is_match(filename) { "Volume".to_string() }
            else if re_ch_kw().is_match(filename) { "Chapter".to_string() }
            else { "Episode".to_string() }
        }
    }
}

// ── Separator ─────────────────────────────────────────────────────────────────
pub fn get_separator(prefix: &str, use_custom: bool, custom_sep: &str) -> String {
    if use_custom && !custom_sep.is_empty() {
        return format!(" {} ", custom_sep);
    }
    let p = prefix.to_lowercase();
    if p.contains("chapter") || p.contains("ch.") || p.contains("volume") || p.contains("vol.") {
        ": ".to_string()
    } else {
        " - ".to_string()
    }
}

// ── Filename sanitisation ─────────────────────────────────────────────────────
pub fn sanitize_filename(name: &str) -> String {
    let n: String = name.nfkc().collect();
    let n = n.replace('<', "(").replace('>', ")").replace('"', "'");
    let n = re_unsafe().replace_all(&n, "").to_string();
    let n = re_ctrls().replace_all(&n, "").to_string();
    let n = re_spaces().replace_all(&n, " ").to_string();
    let n = re_dashes().replace_all(&n, "-").to_string();
    n.trim().trim_end_matches('.').to_string()
}

// ── Natural sort key ──────────────────────────────────────────────────────────
pub fn natural_sort_key(s: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut last = 0usize;
    for m in re_nat_split().find_iter(s) {
        if m.start() > last {
            result.push(format!("1{}", s[last..m.start()].to_lowercase()));
        }
        let ns = m.as_str();
        let f: f64 = ns.parse().unwrap_or(0.0);
        // Single unified numeric encoding regardless of whether the source
        // token was written as an integer ("2") or a decimal ("2.5").
        // Previously, decimals used prefix "0" and integers used prefix
        // "1", which meant EVERY decimal sorted before EVERY integer at
        // the same position regardless of actual value -- "2.5" sorted
        // before "2", and even before "100". Encoding both uniformly as
        // zero-padded fixed-point values compares correctly by magnitude.
        result.push(format!("0{:020.6}", f));
        last = m.end();
    }
    if last < s.len() {
        result.push(format!("1{}", s[last..].to_lowercase()));
    }
    result
}

// ── Rule lookups ──────────────────────────────────────────────────────────────
pub fn find_volume(number: &str, rules: &[Vec<String>]) -> Option<String> {
    let num: f64 = number.parse().ok()?;
    for rule in rules {
        if rule.len() < 3 { continue; }
        let lo: f64 = rule[0].parse().unwrap_or(f64::MAX);
        let hi: f64 = rule[1].parse().unwrap_or(f64::MIN);
        if lo <= num && num <= hi { return Some(rule[2].clone()); }
    }
    None
}

pub fn find_date(vol_num: &str, rules: &[Vec<String>]) -> Option<(i32, i32, i32)> {
    let v: f64 = vol_num.parse().ok()?;
    for rule in rules {
        if rule.len() < 5 { continue; }
        let lo: f64 = rule[0].parse().unwrap_or(f64::MAX);
        let hi: f64 = rule[1].parse().unwrap_or(f64::MIN);
        if lo <= v && v <= hi {
            let y: i32 = rule[2].parse().unwrap_or(0);
            let m: i32 = rule[3].parse().unwrap_or(0);
            let d: i32 = rule[4].parse().unwrap_or(0);
            return Some((y, m, d));
        }
    }
    None
}

pub fn find_summary(vol_num: &str, rules: &[Vec<String>]) -> Option<String> {
    let v: f64 = vol_num.parse().ok()?;
    for rule in rules {
        if rule.len() < 3 { continue; }
        let lo: f64 = rule[0].parse().unwrap_or(f64::MAX);
        let hi: f64 = rule[1].parse().unwrap_or(f64::MIN);
        if lo <= v && v <= hi { return Some(rule[2].clone()); }
    }
    None
}

// ── Auto-detect zero-padding width ───────────────────────────────────────────
pub fn detect_padding(files: &[std::path::PathBuf]) -> Option<usize> {
    let widths: Vec<usize> = files.iter().filter_map(|f| {
        let name = f.file_name()?.to_str()?;
        re_intonly().find(name).filter(|m| !m.as_str().is_empty()).map(|m| m.as_str().len())
    }).collect();
    widths.into_iter().max()
}

// ── ComicInfo v2.1 schema registry ─────────────────────────────────────────────
// Source: https://github.com/anansi-project/comicinfo (the de-facto standard
// used by Komga, Kavita, ComicTagger, ComicRack, etc).
//
// Excluded deliberately:
//   Title, Number, Volume  -- computed per-file by this app's own logic
//                             (filename parsing + rules), not constant metadata.
//   Pages                  -- a structured per-page array, not a simple value;
//                             out of scope for a "constant metadata" editor.
//   Summary                -- kept as its own dedicated multi-line section in
//                             the UI (chapter-1 override + fallback logic),
//                             not part of the dynamic field list.

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FieldKind {
    Text,
    /// Digits only, truncated/filtered to at most `max_digits` characters.
    Numeric { max_digits: usize },
    /// Decimal in [min, max], one fractional digit (matches CommunityRating).
    Decimal { min: f64, max: f64 },
    /// Fixed set of allowed values, rendered as a dropdown.
    Enum(&'static [&'static str]),
}

#[derive(Debug, Clone, Copy)]
pub struct FieldSpec {
    /// Exact XML tag name -- must match the schema exactly.
    pub tag:   &'static str,
    /// Friendlier label shown in the UI when it differs from the tag name.
    pub label: &'static str,
    pub kind:  FieldKind,
    /// Suggested input box width in points.
    pub width: f32,
    /// Hover tooltip, adapted from the Anansi Project's schema documentation.
    pub tip:   &'static str,
}

pub const COMICINFO_FIELDS: &[FieldSpec] = &[
    FieldSpec { tag: "Series", label: "Series", kind: FieldKind::Text, width: 230.0,
        tip: "Title of the series the book is part of." },
    FieldSpec { tag: "Count", label: "Count", kind: FieldKind::Numeric { max_digits: 4 }, width: 70.0,
        tip: "Total number of chapters/volumes/issues in the series." },
    FieldSpec { tag: "AlternateSeries", label: "Alt. Series", kind: FieldKind::Text, width: 230.0,
        tip: "Some books are part of cross-over story arcs and can specify an alternate series." },
    FieldSpec { tag: "AlternateNumber", label: "Alt. Number", kind: FieldKind::Text, width: 100.0,
        tip: "Number of the book within its alternate series." },
    FieldSpec { tag: "AlternateCount", label: "Alt. Count", kind: FieldKind::Numeric { max_digits: 4 }, width: 70.0,
        tip: "Total number of books in the alternate series." },
    FieldSpec { tag: "SeriesGroup", label: "Series Group", kind: FieldKind::Text, width: 230.0,
        tip: "A larger collection or imprint grouping this series belongs to." },
    FieldSpec { tag: "Notes", label: "Notes", kind: FieldKind::Text, width: 300.0,
        tip: "Free text field, usually used to store information about the application that created the file." },
    FieldSpec { tag: "Year", label: "Year", kind: FieldKind::Numeric { max_digits: 4 }, width: 70.0,
        tip: "Default publication year." },
    FieldSpec { tag: "Month", label: "Month", kind: FieldKind::Numeric { max_digits: 2 }, width: 50.0,
        tip: "Default publication month (1-12)." },
    FieldSpec { tag: "Day", label: "Day", kind: FieldKind::Numeric { max_digits: 2 }, width: 50.0,
        tip: "Default publication day." },
    FieldSpec { tag: "Writer", label: "Writer", kind: FieldKind::Text, width: 230.0,
        tip: "Person or organization responsible for creating the scenario." },
    FieldSpec { tag: "Penciller", label: "Penciller", kind: FieldKind::Text, width: 230.0,
        tip: "Person or organization responsible for drawing the art." },
    FieldSpec { tag: "Inker", label: "Inker", kind: FieldKind::Text, width: 230.0,
        tip: "Person or organization responsible for inking the pencil art." },
    FieldSpec { tag: "Colorist", label: "Colorist", kind: FieldKind::Text, width: 230.0,
        tip: "Person or organization responsible for applying color to drawings." },
    FieldSpec { tag: "Letterer", label: "Letterer", kind: FieldKind::Text, width: 230.0,
        tip: "Person or organization responsible for drawing text and speech bubbles." },
    FieldSpec { tag: "CoverArtist", label: "Cover Artist", kind: FieldKind::Text, width: 230.0,
        tip: "Person or organization responsible for drawing the cover art." },
    FieldSpec { tag: "Editor", label: "Editor", kind: FieldKind::Text, width: 230.0,
        tip: "Person or organization revising or elucidating the content." },
    FieldSpec { tag: "Translator", label: "Translator", kind: FieldKind::Text, width: 230.0,
        tip: "Person or organization responsible for translating the text." },
    FieldSpec { tag: "Publisher", label: "Publisher", kind: FieldKind::Text, width: 230.0,
        tip: "Publisher names, comma-separated." },
    FieldSpec { tag: "Imprint", label: "Imprint", kind: FieldKind::Text, width: 230.0,
        tip: "Publishing imprint -- a brand or label under the main publisher." },
    FieldSpec { tag: "Genre", label: "Genre", kind: FieldKind::Text, width: 230.0,
        tip: "Genres, comma-separated." },
    FieldSpec { tag: "Tags", label: "Tags", kind: FieldKind::Text, width: 230.0,
        tip: "Free-form tags, comma-separated -- distinct from Genre." },
    FieldSpec { tag: "Web", label: "Web", kind: FieldKind::Text, width: 480.0,
        tip: "Space-separated URLs for the series." },
    FieldSpec { tag: "PageCount", label: "Page Count", kind: FieldKind::Numeric { max_digits: 5 }, width: 70.0,
        tip: "Total number of pages in the book." },
    FieldSpec { tag: "LanguageISO", label: "Language ISO", kind: FieldKind::Text, width: 70.0,
        tip: "ISO code: \"en\", \"ja\", \"ko\" ..." },
    FieldSpec { tag: "Format", label: "Format", kind: FieldKind::Text, width: 150.0,
        tip: "Format of the book, e.g. \"Digital\", \"Annual\", \"TPB\"." },
    FieldSpec { tag: "BlackAndWhite", label: "Black & White", kind: FieldKind::Enum(&["Unknown","No","Yes"]), width: 120.0,
        tip: "Whether the book is printed in black and white." },
    FieldSpec { tag: "Manga", label: "Manga", kind: FieldKind::Enum(&["Unknown","No","Yes","YesAndRightToLeft"]), width: 170.0,
        tip: "Whether the book is a manga, and if it reads right-to-left." },
    FieldSpec { tag: "Characters", label: "Characters", kind: FieldKind::Text, width: 480.0,
        tip: "Characters appearing in the book, comma-separated." },
    FieldSpec { tag: "Teams", label: "Teams", kind: FieldKind::Text, width: 480.0,
        tip: "Teams appearing in the book, comma-separated." },
    FieldSpec { tag: "Locations", label: "Locations", kind: FieldKind::Text, width: 480.0,
        tip: "Locations appearing in the book, comma-separated." },
    FieldSpec { tag: "ScanInformation", label: "Scan Information", kind: FieldKind::Text, width: 300.0,
        tip: "Free text field, usually used to store information about who scanned the book." },
    FieldSpec { tag: "StoryArc", label: "Story Arc", kind: FieldKind::Text, width: 230.0,
        tip: "The story arc this book belongs to. Multiple values are comma-separated." },
    FieldSpec { tag: "StoryArcNumber", label: "Story Arc Number", kind: FieldKind::Text, width: 120.0,
        tip: "Position within the story arc's reading order. Multiple values are comma-separated." },
    FieldSpec { tag: "AgeRating", label: "Age Rating", kind: FieldKind::Enum(&[
            "Unknown","Adults Only 18+","Early Childhood","Everyone","Everyone 10+",
            "G","Kids to Adults","M","MA15+","Mature 17+","PG","R18+",
            "Rating Pending","Teen","X18+",
        ]), width: 170.0,
        tip: "Content advisory rating, e.g. \"Teen\" or \"Mature 17+\"." },
    FieldSpec { tag: "CommunityRating", label: "Community Rating", kind: FieldKind::Decimal { min: 0.0, max: 5.0 }, width: 70.0,
        tip: "Community/quality score, 0-5 (e.g. a MyAnimeList score of 7.6/10 -> 3.8)." },
    FieldSpec { tag: "MainCharacterOrTeam", label: "Main Character/Team", kind: FieldKind::Text, width: 230.0,
        tip: "The principal character or team the book focuses on." },
    FieldSpec { tag: "Review", label: "Review", kind: FieldKind::Text, width: 300.0,
        tip: "A review or critique of the book." },
    FieldSpec { tag: "GTIN", label: "GTIN", kind: FieldKind::Text, width: 150.0,
        tip: "Global Trade Item Number -- barcode/ISBN-style identifier." },
];

/// Canonical schema order (matches the official XSD sequence) used to sort
/// the user's chosen fields before writing XML, regardless of the order
/// they were added in the UI -- keeps output valid against strict readers
/// that expect element order to match the schema sequence.
const CANONICAL_ORDER: &[&str] = &[
    "Title","Series","Number","Count","Volume",
    "AlternateSeries","AlternateNumber","AlternateCount",
    "Summary","Notes","Year","Month","Day",
    "Writer","Penciller","Inker","Colorist","Letterer","CoverArtist","Editor","Translator",
    "Publisher","Imprint","Genre","Tags","Web","PageCount","LanguageISO","Format",
    "BlackAndWhite","Manga","Characters","Teams","Locations","ScanInformation",
    "StoryArc","StoryArcNumber","SeriesGroup","AgeRating","CommunityRating",
    "MainCharacterOrTeam","Review","GTIN",
];

pub fn canonical_index(tag: &str) -> usize {
    CANONICAL_ORDER.iter().position(|&t| t == tag).unwrap_or(usize::MAX)
}

/// The built-in schema order, owned and ready to seed `AppConfig::tag_order`
/// or to reset a user's customized order back to the default.
pub fn default_tag_order() -> Vec<String> {
    CANONICAL_ORDER.iter().map(|s| s.to_string()).collect()
}

/// Rank of `tag` within a possibly user-customized order (see the Tag Order
/// dialog). A tag missing from `order` -- e.g. a saved order from before a
/// new field existed in the registry -- falls back to its position in the
/// built-in canonical order, offset to sort after every tag the user has
/// explicitly placed rather than being silently tied with every other
/// unknown tag at `usize::MAX`.
pub fn tag_rank(tag: &str, order: &[String]) -> usize {
    order.iter().position(|t| t == tag)
        .unwrap_or_else(|| CANONICAL_ORDER.len() + canonical_index(tag))
}

/// Looks up a field's spec (kind, width, tooltip) by its exact tag name.
pub fn field_spec(tag: &str) -> Option<&'static FieldSpec> {
    COMICINFO_FIELDS.iter().find(|f| f.tag == tag)
}

fn escape_xml(s: &str) -> String {
    s.replace('&',  "&amp;")
     .replace('<',  "&lt;")
     .replace('>',  "&gt;")
     .replace('"',  "&quot;")
     .replace('\'', "&apos;")
}

/// Builds ComicInfo.xml from a dynamic tag->value map. Only tags actually
/// present in `data` are emitted -- removing a field from the constant
/// metadata box means its tag no longer appears in the XML at all, rather
/// than emitting it empty. Tags are sorted by `order` (see the Tag Order
/// dialog / `AppConfig::tag_order`), which defaults to canonical schema
/// order but can be customized per-config.
pub fn build_comic_info_xml(data: &HashMap<String, String>, order: &[String]) -> String {
    let mut entries: Vec<(&String, &String)> = data.iter().collect();
    entries.sort_by_key(|(tag, _)| tag_rank(tag, order));

    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<ComicInfo>\n");
    for (tag, val) in entries {
        // Volume is only emitted when it actually has a value -- most
        // chapter-type entries have no volume at all.
        if tag == "Volume" && val.trim().is_empty() { continue; }
        xml.push_str(&format!("  <{tag}>{}</{tag}>\n", escape_xml(val)));
    }
    xml.push_str("</ComicInfo>\n");
    xml
}

// ── CBZ write ─────────────────────────────────────────────────────────────────
/// Reads `src_path`'s CBZ contents, replaces ComicInfo.xml, and writes the
/// result to `dest_path`. When `dest_path == src_path` this overwrites the
/// original in place (the original behavior); when they differ, the source
/// is left completely untouched and a new file is written at `dest_path`
/// instead -- used by the "write new CBZ" output mode. Creates `dest_path`'s
/// parent directory if it doesn't exist yet.
pub fn write_comic_info_xml_to(src_path: &Path, dest_path: &Path, xml: &str) -> std::io::Result<()> {
    use std::io::Write;
    use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let tmp = dest_path.with_extension("cbz_tmp");
    {
        let src = std::fs::File::open(src_path)?;
        let mut archive = ZipArchive::new(src)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        let dst = std::fs::File::create(&tmp)?;
        let mut writer = ZipWriter::new(dst);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

        for i in 0..archive.len() {
            let f = archive.by_index(i)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            if f.name() == "ComicInfo.xml" { continue; }
            // raw_copy_file streams the entry's existing (already-compressed
            // or already-stored) bytes straight into the destination archive
            // via one internal io::copy, rather than this function reading
            // the whole entry -- a full manga page image, sometimes several
            // MB -- into a Vec<u8> first. That buffering happened once per
            // entry regardless, but with rayon running multiple CBZs in
            // parallel (max_workers threads), peak memory scaled with
            // worker count x largest single image in the batch. This also
            // preserves whatever compression the source entry already had
            // instead of forcing it through Stored -- never a regression,
            // since raw_copy_file only ever copies bytes, never re-compresses.
            writer.raw_copy_file(f)
                  .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        }
        writer.start_file("ComicInfo.xml", opts)
              .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        writer.write_all(xml.as_bytes())?;
        writer.finish()
              .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    }
    std::fs::rename(&tmp, dest_path)?;
    Ok(())
}

/// In-place overwrite -- thin wrapper around write_comic_info_xml_to for the
/// default (and previously only) behavior.
pub fn write_comic_info_to_cbz(path: &Path, xml: &str) -> std::io::Result<()> {
    write_comic_info_xml_to(path, path, xml)
}

// ── Safe JSON load (chapter/volume/date title maps) ───────────────────────────
pub fn safe_json_load(path: &str) -> HashMap<String, String> {
    if path.is_empty() { return HashMap::new(); }
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<HashMap<String, String>>(&s).ok())
        .unwrap_or_default()
}

// ── Separator validity check ──────────────────────────────────────────────────
pub fn is_sep_invalid_for_filename(sep: &str) -> bool {
    sep.chars().any(|c| matches!(c, ':' | '/' | '\\' | '|' | '?' | '*' | '<' | '>' | '"'))
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Unit tests
// ═══════════════════════════════════════════════════════════════════════════════
// The UI has been exercised extensively by hand across many iterations, but
// the actual parsing/rule-matching logic underneath it -- the part that
// silently produces wrong output rather than visibly breaking -- has only
// ever been tested indirectly, by running real batches. These tests target
// exactly the boundary conditions most likely to harbor an off-by-one or
// silently-wrong-match bug: range edges, decimal vs. integer ordering, and
// the auto-detection heuristics.
#[cfg(test)]
mod tests {
    use super::*;

    // ── detect_file_type ───────────────────────────────────────────────────
    #[test]
    fn detect_type_volume_keyword() {
        assert_eq!(detect_file_type("Volume 3.cbz"), "volume");
        assert_eq!(detect_file_type("vol 3.cbz"), "volume");
        assert_eq!(detect_file_type("v3.cbz"), "volume");
    }

    #[test]
    fn detect_type_chapter_keyword() {
        assert_eq!(detect_file_type("Chapter 12.cbz"), "chapter");
        assert_eq!(detect_file_type("ch 12.cbz"), "chapter");
    }

    #[test]
    fn detect_type_episode_keyword() {
        assert_eq!(detect_file_type("Episode 5.cbz"), "episode");
        assert_eq!(detect_file_type("ep 5.cbz"), "episode");
    }

    #[test]
    fn detect_type_decimal_with_no_keyword_is_chapter() {
        // No vol/ch/ep keyword, but has a decimal number -- treated as chapter.
        assert_eq!(detect_file_type("Series 2.5.cbz"), "chapter");
    }

    #[test]
    fn detect_type_bare_number_is_chapter() {
        assert_eq!(detect_file_type("Series 5.cbz"), "chapter");
    }

    #[test]
    fn detect_type_no_number_is_unknown() {
        assert_eq!(detect_file_type("Cover.cbz"), "unknown");
    }

    #[test]
    fn detect_type_volume_keyword_takes_priority_over_bare_number() {
        // Even though "5" alone would be "chapter", the Vol keyword wins.
        assert_eq!(detect_file_type("My Series Vol 5.cbz"), "volume");
    }

    // ── is_decimal_file ──────────────────────────────────────────────────────
    #[test]
    fn decimal_file_detection() {
        assert!(is_decimal_file("Episode 2.5.cbz"));
        assert!(!is_decimal_file("Episode 2.cbz"));
        assert!(!is_decimal_file("Episode.cbz"));
    }

    // ── get_prefix ───────────────────────────────────────────────────────────
    #[test]
    fn prefix_auto_detects_volume_keyword() {
        assert_eq!(get_prefix("My Series Vol 5.cbz", "auto", ""), "Volume");
    }

    #[test]
    fn prefix_auto_detects_chapter_keyword() {
        assert_eq!(get_prefix("My Series Ch 5.cbz", "auto", ""), "Chapter");
    }

    #[test]
    fn prefix_auto_falls_back_to_episode() {
        // No vol/ch keyword anywhere in the filename -- defaults to Episode.
        assert_eq!(get_prefix("My Series 5.cbz", "auto", ""), "Episode");
    }

    #[test]
    fn prefix_explicit_modes_override_filename_content() {
        // Even though the filename says "Vol", explicit mode wins.
        assert_eq!(get_prefix("My Series Vol 5.cbz", "chapter", ""), "Chapter");
        assert_eq!(get_prefix("My Series.cbz", "episode", ""), "Episode");
        assert_eq!(get_prefix("My Series.cbz", "volume", ""), "Volume");
    }

    #[test]
    fn prefix_custom_mode_uses_given_text() {
        assert_eq!(get_prefix("My Series.cbz", "custom", "Break"), "Break");
    }

    #[test]
    fn prefix_custom_mode_falls_back_to_episode_if_blank() {
        assert_eq!(get_prefix("My Series.cbz", "custom", ""), "Episode");
    }

    // ── find_volume (boundary conditions) ────────────────────────────────────
    #[test]
    fn find_volume_matches_within_range() {
        let rules = vec![
            vec!["1".into(), "3.5".into(), "1".into()],
            vec!["4".into(), "8.5".into(), "2".into()],
        ];
        assert_eq!(find_volume("1", &rules), Some("1".to_string()));
        assert_eq!(find_volume("3.5", &rules), Some("1".to_string())); // upper bound inclusive
        assert_eq!(find_volume("4", &rules), Some("2".to_string()));   // lower bound inclusive
        assert_eq!(find_volume("8.5", &rules), Some("2".to_string()));
    }

    #[test]
    fn find_volume_no_match_outside_all_ranges() {
        let rules = vec![vec!["1".into(), "3.5".into(), "1".into()]];
        assert_eq!(find_volume("3.6", &rules), None);
        assert_eq!(find_volume("100", &rules), None);
    }

    #[test]
    fn find_volume_handles_decimal_chapter_inside_range() {
        let rules = vec![vec!["1".into(), "3.5".into(), "1".into()]];
        assert_eq!(find_volume("2.5", &rules), Some("1".to_string()));
    }

    #[test]
    fn find_volume_invalid_number_returns_none() {
        let rules = vec![vec!["1".into(), "3.5".into(), "1".into()]];
        assert_eq!(find_volume("not_a_number", &rules), None);
    }

    #[test]
    fn find_volume_malformed_rule_row_is_skipped() {
        // A row with fewer than 3 columns must not panic -- just skip it.
        let rules = vec![vec!["1".into(), "2".into()]];
        assert_eq!(find_volume("1", &rules), None);
    }

    // ── find_date ─────────────────────────────────────────────────────────────
    #[test]
    fn find_date_matches_within_range() {
        let rules = vec![vec!["1".into(), "1".into(), "2020".into(), "6".into(), "16".into()]];
        assert_eq!(find_date("1", &rules), Some((2020, 6, 16)));
    }

    #[test]
    fn find_date_no_match_returns_none() {
        let rules = vec![vec!["1".into(), "1".into(), "2020".into(), "6".into(), "16".into()]];
        assert_eq!(find_date("2", &rules), None);
    }

    // ── find_summary ──────────────────────────────────────────────────────────
    #[test]
    fn find_summary_matches_within_range() {
        let rules = vec![vec!["1".into(), "3".into(), "Test summary".into()]];
        assert_eq!(find_summary("2", &rules), Some("Test summary".to_string()));
    }

    #[test]
    fn find_summary_no_match_returns_none() {
        let rules = vec![vec!["1".into(), "3".into(), "Test summary".into()]];
        assert_eq!(find_summary("4", &rules), None);
    }

    // ── natural_sort_key ──────────────────────────────────────────────────────
    #[test]
    fn natural_sort_orders_numbers_not_lexicographically() {
        // Plain string sort would put "10" before "2"; natural sort must not.
        let mut files = vec!["Episode 10.cbz", "Episode 2.cbz", "Episode 1.cbz"];
        files.sort_by(|a, b| natural_sort_key(a).cmp(&natural_sort_key(b)));
        assert_eq!(files, vec!["Episode 1.cbz", "Episode 2.cbz", "Episode 10.cbz"]);
    }

    #[test]
    fn natural_sort_places_decimal_chapter_between_integers() {
        let mut files = vec!["Episode 3.cbz", "Episode 2.5.cbz", "Episode 2.cbz"];
        files.sort_by(|a, b| natural_sort_key(a).cmp(&natural_sort_key(b)));
        assert_eq!(files, vec!["Episode 2.cbz", "Episode 2.5.cbz", "Episode 3.cbz"]);
    }

    #[test]
    fn natural_sort_handles_zero_padded_numbers_equivalently() {
        // "Episode 02" and "Episode 2" should compare as the same number.
        let mut files = vec!["Episode 02.cbz", "Episode 1.cbz"];
        files.sort_by(|a, b| natural_sort_key(a).cmp(&natural_sort_key(b)));
        assert_eq!(files, vec!["Episode 1.cbz", "Episode 02.cbz"]);
    }

    // ── sanitize_filename ─────────────────────────────────────────────────────
    #[test]
    fn sanitize_strips_filesystem_unsafe_characters() {
        let result = sanitize_filename(r#"Title: "Quoted" / Slashed * Star?"#);
        assert!(!result.contains(['/', '*', '?']));
    }

    #[test]
    fn sanitize_converts_angle_brackets_to_parens() {
        let result = sanitize_filename("Title <bracketed>");
        assert_eq!(result, "Title (bracketed)");
    }

    #[test]
    fn sanitize_collapses_repeated_spaces() {
        let result = sanitize_filename("Too    many     spaces");
        assert!(!result.contains("  "));
    }

    #[test]
    fn sanitize_trims_trailing_dot_and_whitespace() {
        let result = sanitize_filename("Trailing dot.   ");
        assert!(!result.ends_with('.'));
        assert!(!result.ends_with(' '));
    }

    // ── is_sep_invalid_for_filename ────────────────────────────────────────────
    #[test]
    fn separator_validity() {
        assert!(is_sep_invalid_for_filename(":"));
        assert!(is_sep_invalid_for_filename("/"));
        assert!(!is_sep_invalid_for_filename("-"));
        assert!(!is_sep_invalid_for_filename("~"));
    }

    // ── ComicInfo schema registry ─────────────────────────────────────────────
    #[test]
    fn canonical_order_known_tags_come_before_unknown() {
        assert!(canonical_index("Series") < canonical_index("Writer"));
        assert_eq!(canonical_index("NotARealTag"), usize::MAX);
    }

    #[test]
    fn field_spec_lookup_finds_known_tags() {
        assert!(field_spec("Series").is_some());
        assert!(field_spec("CommunityRating").is_some());
        assert!(field_spec("NotARealTag").is_none());
    }

    #[test]
    fn every_canonical_order_entry_except_per_file_fields_has_a_spec() {
        // Title/Number/Volume/Summary are deliberately excluded from
        // COMICINFO_FIELDS (computed per-file or handled separately) --
        // every other canonical tag must have a real FieldSpec, or the
        // Add Tag picker can never offer it (this caught a real bug once:
        // "Count" existed in CANONICAL_ORDER but had no FieldSpec entry).
        let excluded = ["Title", "Number", "Volume", "Summary"];
        for &tag in CANONICAL_ORDER {
            if excluded.contains(&tag) { continue; }
            assert!(field_spec(tag).is_some(), "tag '{tag}' is in CANONICAL_ORDER but missing from COMICINFO_FIELDS");
        }
    }

    #[test]
    fn every_comicinfo_field_has_a_canonical_position() {
        for spec in COMICINFO_FIELDS {
            assert_ne!(
                canonical_index(spec.tag), usize::MAX,
                "tag '{}' is in COMICINFO_FIELDS but missing from CANONICAL_ORDER", spec.tag
            );
        }
    }

    // ── build_comic_info_xml ──────────────────────────────────────────────────
    #[test]
    fn xml_builder_escapes_special_characters() {
        let mut data = HashMap::new();
        data.insert("Series".to_string(), "Tom & Jerry's \"Big\" Day <2>".to_string());
        let xml = build_comic_info_xml(&data, &default_tag_order());
        assert!(xml.contains("&amp;"));
        assert!(xml.contains("&apos;"));
        assert!(xml.contains("&quot;"));
        assert!(xml.contains("&lt;2&gt;"));
        assert!(!xml.contains("Tom & Jerry")); // raw & must not survive unescaped
    }

    #[test]
    fn xml_builder_omits_empty_volume_tag() {
        let mut data = HashMap::new();
        data.insert("Title".to_string(), "Episode 1".to_string());
        data.insert("Volume".to_string(), "".to_string());
        let xml = build_comic_info_xml(&data, &default_tag_order());
        assert!(!xml.contains("<Volume>"));
    }

    #[test]
    fn xml_builder_sorts_into_canonical_order_regardless_of_insertion_order() {
        // Insert Writer before Series -- output must still have Series first,
        // matching the schema sequence, not insertion order.
        let mut data = HashMap::new();
        data.insert("Writer".to_string(), "A".to_string());
        data.insert("Series".to_string(), "B".to_string());
        let xml = build_comic_info_xml(&data, &default_tag_order());
        let series_pos = xml.find("<Series>").unwrap();
        let writer_pos  = xml.find("<Writer>").unwrap();
        assert!(series_pos < writer_pos);
    }

    #[test]
    fn xml_builder_only_emits_tags_actually_present() {
        let mut data = HashMap::new();
        data.insert("Series".to_string(), "Only This".to_string());
        let xml = build_comic_info_xml(&data, &default_tag_order());
        assert!(xml.contains("<Series>"));
        assert!(!xml.contains("<Writer>"));
        assert!(!xml.contains("<Genre>"));
    }

    #[test]
    fn xml_builder_respects_a_custom_tag_order() {
        // Same two tags as the canonical-order test above, but with a custom
        // order that deliberately reverses their usual relationship -- proves
        // the Tag Order feature actually reaches the XML output, not just the
        // UI's own display sort.
        let mut data = HashMap::new();
        data.insert("Writer".to_string(), "A".to_string());
        data.insert("Series".to_string(), "B".to_string());
        let custom_order = vec!["Writer".to_string(), "Series".to_string()];
        let xml = build_comic_info_xml(&data, &custom_order);
        let series_pos = xml.find("<Series>").unwrap();
        let writer_pos  = xml.find("<Writer>").unwrap();
        assert!(writer_pos < series_pos);
    }

    #[test]
    fn xml_builder_falls_back_to_canonical_order_for_tags_missing_from_a_custom_order() {
        // A custom order saved before a field existed in the registry (or
        // simply never touched) shouldn't crash or silently tie every
        // unlisted tag together -- unlisted tags keep their relative
        // canonical ordering, appended after everything explicitly ordered.
        let mut data = HashMap::new();
        data.insert("Series".to_string(), "A".to_string());
        data.insert("Writer".to_string(), "B".to_string());
        data.insert("Genre".to_string(), "C".to_string());
        let custom_order = vec!["Genre".to_string()]; // Series/Writer not listed
        let xml = build_comic_info_xml(&data, &custom_order);
        let genre_pos  = xml.find("<Genre>").unwrap();
        let series_pos = xml.find("<Series>").unwrap();
        let writer_pos = xml.find("<Writer>").unwrap();
        assert!(genre_pos < series_pos, "explicitly-ordered tag should sort first");
        assert!(series_pos < writer_pos, "unlisted tags keep canonical relative order");
    }
}