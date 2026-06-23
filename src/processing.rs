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
fn re_title()    -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)^(?:Ep\.?|Episode|Ch\.?|Chapter|Vol\.?|Volume)\s*\d+(?:\.\d+)?\s*[-:]\s*(.+)").unwrap()) }
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

// ── Title extraction ──────────────────────────────────────────────────────────
pub fn extract_title_from_filename(filename: &str) -> Option<String> {
    let stem = Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    re_title().captures(stem)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string())
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
            result.push(format!("2{}", s[last..m.start()].to_lowercase()));
        }
        let ns = m.as_str();
        if ns.contains('.') {
            let f: f64 = ns.parse().unwrap_or(0.0);
            result.push(format!("0{:020.6}", f));
        } else {
            let i: u64 = ns.parse().unwrap_or(0);
            result.push(format!("1{:020}", i));
        }
        last = m.end();
    }
    if last < s.len() {
        result.push(format!("2{}", s[last..].to_lowercase()));
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
/// than emitting it empty. Tags are sorted into canonical schema order
/// regardless of insertion order.
pub fn build_comic_info_xml(data: &HashMap<String, String>) -> String {
    let mut entries: Vec<(&String, &String)> = data.iter().collect();
    entries.sort_by_key(|(tag, _)| canonical_index(tag));

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
pub fn write_comic_info_to_cbz(path: &Path, xml: &str) -> std::io::Result<()> {
    use std::io::{Read, Write};
    use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

    let tmp = path.with_extension("cbz_tmp");
    {
        let src = std::fs::File::open(path)?;
        let mut archive = ZipArchive::new(src)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        let dst = std::fs::File::create(&tmp)?;
        let mut writer = ZipWriter::new(dst);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

        for i in 0..archive.len() {
            let mut f = archive.by_index(i)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            if f.name() == "ComicInfo.xml" { continue; }
            let name = f.name().to_owned();
            writer.start_file(&name, opts)
                  .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            writer.write_all(&buf)?;
        }
        writer.start_file("ComicInfo.xml", opts)
              .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        writer.write_all(xml.as_bytes())?;
        writer.finish()
              .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
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