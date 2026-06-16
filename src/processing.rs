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

// ── XML builder ───────────────────────────────────────────────────────────────
const XML_ORDER: &[&str] = &[
    "Title","Series","Number","Volume","Writer","Penciller",
    "Publisher","LanguageISO","AlternateSeries","Web","Genre",
    "Rating","Year","Month","Day","Count","Summary",
];

fn escape_xml(s: &str) -> String {
    s.replace('&',  "&amp;")
     .replace('<',  "&lt;")
     .replace('>',  "&gt;")
     .replace('"',  "&quot;")
     .replace('\'', "&apos;")
}

pub fn build_comic_info_xml(
    data: &HashMap<&str, String>,
    custom_fields: &[Vec<String>],
) -> String {
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<ComicInfo>\n");
    for &tag in XML_ORDER {
        // Skip Volume if empty
        if tag == "Volume" {
            if data.get("Volume").map_or(true, |v| v.is_empty()) { continue; }
        }
        let val = data.get(tag).map(|s| s.as_str()).unwrap_or("");
        xml.push_str(&format!("  <{tag}>{}</{tag}>\n", escape_xml(val)));
    }
    let order_set: std::collections::HashSet<&str> = XML_ORDER.iter().copied().collect();
    for field in custom_fields {
        if field.len() >= 2 {
            let name = field[0].trim();
            if !name.is_empty() && !order_set.contains(name) {
                xml.push_str(&format!("  <{name}>{}</{name}>\n", escape_xml(field[1].trim())));
            }
        }
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
