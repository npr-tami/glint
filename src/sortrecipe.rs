//! The custom sort order: rules the person writes, like  -newest_first  -folder_first  -image -biggest
//! -exe_last  (sort syntax v3).
//!
//! - **Groups** (-folder, -updated, -image, -png…) are the kinds of things, in the order they should
//!   come; `_first` / `_last` put one at the very top or bottom. Each item goes in the most exact
//!   group that fits it (a name rule beats a time, which beats -hidden, a size, one file type, a kind
//!   and -folder). Things that fit no group go where -other is, or after the other groups.
//! - **Order rules** (-newest, -biggest, -a_to_z…; `_first` / `_last` say which end, `_by_day…` round
//!   dates) order things in every group - or in one group, when written after it on its line.
//! - `in Downloads:` (or a full path) starts rules for that folder only.
//! - **Forgiving**: the dash is optional, spaces work instead of underscores (`newest first`), and
//!   plain words mean rules (date, size, type, name, folders). The text stays as typed; it's only
//!   read this way.
//!
//! Reading never fails: mistakes become [`Message`]s, with the place in the text and, for a typo, the
//! fix ("Did you mean -newest?").

use std::cmp::Ordering;
use std::sync::{Arc, Mutex};

use crate::folder::{self, Entry};

/// A note about the rules: a mistake (that rule is skipped) or a tip.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    /// The line, counted from 1 (0: not about one line).
    pub line: usize,
    pub text: String,
    pub error: bool,
    /// Where in the text it is about (byte offsets), to underline it.
    pub span: Option<(usize, usize)>,
    /// What to put there instead (clicking the hint does it).
    pub fix: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    Normal,
    First,
    Last,
}

/// What an order rule compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    Modified,
    Created,
    Size,
    Name,
    Type,
    Extension,
}

/// How far dates are rounded (`_by_day`…): things in the same day count as equal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Grain {
    Day,
    Week,
    Month,
    Year,
}

impl Grain {
    fn name(self) -> &'static str {
        match self {
            Grain::Day => "day",
            Grain::Week => "week",
            Grain::Month => "month",
            Grain::Year => "year",
        }
    }
    fn parse(s: &str) -> Option<Grain> {
        Some(match s {
            "day" => Grain::Day,
            "week" => Grain::Week,
            "month" => Grain::Month,
            "year" => Grain::Year,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
struct Order {
    key: Key,
    descending: bool,
    grain: Option<Grain>,
    text: String,
}

/// What a group takes.
#[derive(Clone, Debug)]
enum Takes {
    Folders,
    Files,
    Nothing,
    Hidden,
    Empty,
    /// Changed (or, `created`, added) in the last this many hours. Files only.
    Since { hours: f64, created: bool },
    Today,
    Yesterday,
    Older { hours: f64 },
    Bigger(u64),
    Smaller(u64),
    NameStarts(String),
    NameEnds(String),
    NameHas(String),
    Kind(&'static [&'static str]),
    Ext(String),
}

#[derive(Clone, Debug)]
struct Group {
    name: String,
    label: String,
    exactness: f64,
    place: Place,
    takes: Takes,
    orders: Vec<Order>,
}

#[derive(Clone, Debug, Default)]
struct Rules {
    groups: Vec<Group>,
    orders: Vec<Order>,
    line: usize,
}

/// The time now, and how far local time is from UTC, to tell which day something was changed.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    /// FILETIME (100 ns steps since 1601, UTC).
    pub now: u64,
    /// Local time minus UTC, in 100 ns steps.
    pub offset: i64,
}

const HOUR: f64 = 36_000_000_000.0;
const DAY: i64 = 864_000_000_000;

impl Clock {
    /// The time now on this computer.
    pub fn now() -> Clock {
        use windows::Win32::Foundation::FILETIME;
        use windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
        use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
        // SAFETY: no arguments / valid in and out pointers to plain structs.
        unsafe {
            let utc = GetSystemTimeAsFileTime();
            let mut local = FILETIME::default();
            let now = ((utc.dwHighDateTime as u64) << 32) | utc.dwLowDateTime as u64;
            let offset = if FileTimeToLocalFileTime(&utc, &mut local).is_ok() {
                (((local.dwHighDateTime as u64) << 32) | local.dwLowDateTime as u64) as i64 - now as i64
            } else {
                0
            };
            Clock { now, offset }
        }
    }

    /// The local day (days since 1 January 1601, a Monday).
    fn day(&self, t: u64) -> i64 {
        (t as i64 + self.offset).div_euclid(DAY)
    }

    /// A time rounded down to its day, week (from Monday), month or year, as a number to compare.
    fn round(&self, t: u64, grain: Option<Grain>) -> i64 {
        let Some(g) = grain else { return t as i64 };
        let day = self.day(t);
        match g {
            Grain::Day => day,
            Grain::Week => day.div_euclid(7),
            Grain::Month | Grain::Year => {
                let (y, m) = year_month(day);
                if g == Grain::Month {
                    y * 12 + m
                } else {
                    y
                }
            }
        }
    }
}

/// The year and month of a day counted from 1 January 1601.
fn year_month(day1601: i64) -> (i64, i64) {
    // Days since 1 March of year 0 (the usual civil-calendar trick), from days since 1970.
    let z = day1601 - 134_774 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m)
}

// ---------------------------------------------------------------- the words (also the guide)

/// Groups, the kinds of things: (rule, a few words about it).
pub const GROUP_RULES: &[(&str, &str)] = &[
    ("-folder", "folders"),
    ("-updated", "changed in the last 24 hours"),
    ("-image", "pictures"),
    ("-video", "videos"),
    ("-audio", "music and sounds"),
    ("-document", "pdf, Word, Excel, txt…"),
    ("-archive", "zip, rar, 7z, iso…"),
    ("-program", "programs and installers"),
    ("-code", "code files"),
    ("-font", "fonts"),
    ("-shortcut", "shortcuts and web links"),
    ("-hidden", "hidden things"),
    ("-other", "everything else"),
];

/// Order rules: (rule, a few words about it).
pub const ORDER_RULES: &[(&str, &str)] = &[
    ("-newest", "newest changed first"),
    ("-oldest", "oldest changed first"),
    ("-biggest", "largest first"),
    ("-smallest", "smallest first"),
    ("-newest_added", "newest added first"),
    ("-oldest_added", "oldest added first"),
    ("-a_to_z", "by name, A to Z"),
    ("-z_to_a", "by name, Z to A"),
    ("-by_type", "by type, A to Z"),
    ("-by_extension", "by extension"),
];

/// Groups with a number or a word in them: (rule, a few words about it).
pub const EXACT_RULES: &[(&str, &str)] = &[
    ("-today", "changed today"),
    ("-yesterday", "changed yesterday"),
    ("-added", "added in the last day"),
    ("-updated_3_days", "changed in 3 days"),
    ("-older_than_1_year", "not changed for a year"),
    ("-bigger_than_100mb", "files over 100 MB"),
    ("-smaller_than_1mb", "files under 1 MB"),
    ("-empty", "empty files"),
    ("-name_has_", "names with a word"),
    ("-name_starts_with_", "names starting with…"),
    ("-name_ends_with_", "names ending with…"),
];

/// The words that can follow "in" for a folder rule.
pub const PLACE_NAMES: &[&str] = &["Downloads", "Desktop", "Documents", "Pictures", "Music", "Videos", "Home"];

const KINDS: &[(&str, &[&str])] = &[
    ("image", &["jpg", "jpeg", "png", "gif", "bmp", "webp", "heic", "heif", "tif", "tiff", "svg", "ico", "raw", "cr2", "nef", "arw", "dng", "psd", "avif", "jfif"]),
    ("video", &["mp4", "mkv", "mov", "avi", "wmv", "webm", "m4v", "flv", "mpg", "mpeg", "3gp", "ts"]),
    ("audio", &["mp3", "wav", "flac", "aac", "m4a", "ogg", "wma", "opus", "mid", "midi", "aiff"]),
    ("document", &["pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "txt", "rtf", "md", "csv", "epub", "one"]),
    ("archive", &["zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "xz", "iso", "cab", "img", "vhd", "vhdx"]),
    ("program", &["exe", "msi", "bat", "cmd", "ps1", "com", "appx", "msix", "appinstaller", "jar", "vbs", "scr"]),
    (
        "code",
        &[
            "cs", "js", "ts", "tsx", "jsx", "py", "java", "c", "cpp", "h", "hpp", "go", "rs", "rb", "php", "html", "htm", "css", "scss", "json", "xml", "yml", "yaml", "sh", "sln", "csproj",
            "lua", "kt", "swift", "sql", "toml", "ini",
        ],
    ),
    ("font", &["ttf", "otf", "woff", "woff2", "fon"]),
    ("shortcut", &["lnk", "url"]),
];

/// More file types people sort by, so they're never mistaken for a misspelt rule.
const OTHER_TYPES: &[&str] = &[
    "dll", "sys", "log", "tmp", "bak", "dat", "bin", "cfg", "conf", "torrent", "apk", "dmg", "pkg", "deb", "rpm", "blend", "obj", "fbx", "stl", "gltf", "glb", "ai", "eps", "indd", "sketch",
    "fig", "xd", "srt", "vtt", "ass", "sub", "nfo", "cue", "crdownload", "part", "db", "sqlite", "ics", "vcf", "eml", "msg", "pst", "key", "pages", "numbers", "mobi", "azw3", "djvu",
    "kdbx", "pem", "crt", "reg", "inf", "ova", "vmdk", "aep", "prproj", "kra", "xcf", "clip", "drawio", "ipynb", "pyc", "docm", "xlsm", "pptm", "dotx", "xltx", "m4b", "mka", "mpc", "ape",
    "wv", "dsf", "rw2", "orf", "raf", "pef", "srw", "exr", "hdr", "dds", "tga", "jxl", "msp", "msu", "cer", "p12", "pfx", "asc", "sig", "sha256", "md5", "patch", "diff", "lock", "env",
    "gitignore",
];

/// Other ways to write a rule, and the rule they mean (without _first, _last or _by_…).
const ALIASES: &[(&str, &str)] = &[
    ("folders", "folder"),
    ("dir", "folder"),
    ("dirs", "folder"),
    ("directory", "folder"),
    ("directories", "folder"),
    ("files", "file"),
    ("others", "other"),
    ("rest", "other"),
    ("the_rest", "other"),
    ("everything", "other"),
    ("everything_else", "other"),
    ("images", "image"),
    ("picture", "image"),
    ("pictures", "image"),
    ("photo", "image"),
    ("photos", "image"),
    ("pics", "image"),
    ("videos", "video"),
    ("movie", "video"),
    ("movies", "video"),
    ("music", "audio"),
    ("sound", "audio"),
    ("sounds", "audio"),
    ("song", "audio"),
    ("songs", "audio"),
    ("documents", "document"),
    ("docs", "document"),
    ("archives", "archive"),
    ("compressed", "archive"),
    ("programs", "program"),
    ("app", "program"),
    ("apps", "program"),
    ("installer", "program"),
    ("installers", "program"),
    ("shortcuts", "shortcut"),
    ("links", "shortcut"),
    ("fonts", "font"),
    ("recent", "updated"),
    ("recently_updated", "updated"),
    ("recently_changed", "updated"),
    ("changed", "updated"),
    ("edited", "updated"),
    ("last_24_hours", "updated"),
    ("this_week", "updated_7_days"),
    ("updated_this_week", "updated_7_days"),
    ("last_7_days", "updated_7_days"),
    ("this_month", "updated_30_days"),
    ("updated_this_month", "updated_30_days"),
    ("last_30_days", "updated_30_days"),
    ("updated_today", "today"),
    ("changed_today", "today"),
    ("updated_yesterday", "yesterday"),
    ("recently_added", "added"),
    ("downloaded", "added"),
    ("recently_downloaded", "added"),
    ("latest", "newest"),
    ("date", "newest"),
    ("modified", "newest"),
    ("newest_changed", "newest"),
    ("oldest_changed", "oldest"),
    ("largest", "biggest"),
    ("size", "biggest"),
    ("bigger", "biggest"),
    ("smaller", "smallest"),
    ("name", "a_to_z"),
    ("by_name", "a_to_z"),
    ("alphabetical", "a_to_z"),
    ("a_z", "a_to_z"),
    ("az", "a_to_z"),
    ("name_reverse", "z_to_a"),
    ("z_a", "z_to_a"),
    ("za", "z_to_a"),
    ("type", "by_type"),
    ("types", "by_type"),
    ("extension", "by_extension"),
    ("ext", "by_extension"),
    ("by_ext", "by_extension"),
    ("created", "newest_added"),
    ("created_newest", "newest_added"),
    ("newest_created", "newest_added"),
    ("created_oldest", "oldest_added"),
    ("oldest_created", "oldest_added"),
];

/// Order rules by their word: what they compare, and whether the larger value comes first.
const ORDER_WORDS: &[(&str, Key, bool)] = &[
    ("newest", Key::Modified, true),
    ("oldest", Key::Modified, false),
    ("newest_added", Key::Created, true),
    ("oldest_added", Key::Created, false),
    ("biggest", Key::Size, true),
    ("smallest", Key::Size, false),
    ("a_to_z", Key::Name, false),
    ("z_to_a", Key::Name, true),
    ("by_type", Key::Type, false),
    ("by_extension", Key::Extension, false),
];

const SIMPLE_GROUPS: &[&str] = &["folder", "other", "hidden", "updated", "added", "today", "yesterday", "empty"];

fn alias(word: &str) -> &str {
    ALIASES.iter().find(|(a, _)| *a == word).map_or(word, |(_, r)| r)
}

fn order_word(word: &str) -> Option<(Key, bool)> {
    ORDER_WORDS.iter().find(|(w, ..)| *w == word).map(|&(_, k, d)| (k, d))
}

fn kind(word: &str) -> Option<&'static [&'static str]> {
    KINDS.iter().find(|(k, _)| *k == word).map(|(_, list)| *list)
}

/// A file type Glint knows (so it's never taken for a misspelt rule).
pub fn known_type(word: &str) -> bool {
    OTHER_TYPES.contains(&word) || KINDS.iter().any(|(_, list)| list.contains(&word))
}

/// Every rule word (without _first, _last or _by_…) and file type, for "Did you mean…".
fn rule_words() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = ORDER_WORDS.iter().map(|(w, ..)| *w).collect();
    v.extend(SIMPLE_GROUPS);
    v.extend(KINDS.iter().map(|(k, _)| *k));
    v.extend(ALIASES.iter().map(|(a, _)| *a));
    v.extend(["updated_3_days", "added_3_days", "older_than_1_year", "bigger_than_100mb", "smaller_than_1mb"]);
    v.extend(KINDS.iter().flat_map(|(_, list)| list.iter().copied()));
    v.extend(OTHER_TYPES);
    let mut seen = std::collections::HashSet::new();
    v.retain(|w| seen.insert(*w));
    v
}

/// The option closest to a misspelt word, or None when none is close (at most
/// 2 changes, or a third of the word's length).
pub fn closest<'a>(word: &str, options: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let w: Vec<char> = word.to_lowercase().chars().collect();
    let mut best: Option<(&str, usize)> = None;
    for o in options {
        let d = distance(&w, &o.to_lowercase().chars().collect::<Vec<_>>());
        if best.is_none_or(|(_, b)| d < b) {
            best = Some((o, d));
        }
    }
    best.filter(|&(_, d)| d <= 2.max(w.len() / 3)).map(|(o, _)| o)
}

fn distance(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + usize::from(a[i - 1] != b[j - 1]));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// A rule as written, the way it's read: no dash, lower case, `-` as `_`.
fn normalize(raw: &str) -> String {
    raw.trim_start_matches(['-', '–', '—']).trim().to_lowercase().replace('-', "_")
}

/// Takes _first/_last and _by_day… off the end (in either order).
fn split(word: &str) -> (String, Place, Option<Grain>) {
    let mut w = word.to_string();
    let (mut place, mut grain) = (Place::Normal, None);
    for _ in 0..2 {
        if grain.is_none() {
            if let Some(i) = w.rfind("_by_") {
                if i > 0 {
                    if let Some(g) = Grain::parse(&w[i + 4..]) {
                        grain = Some(g);
                        w.truncate(i);
                        continue;
                    }
                }
            }
        }
        if place == Place::Normal {
            if let Some(base) = w.strip_suffix("_first").filter(|b| !b.is_empty()) {
                w = base.to_string();
                place = Place::First;
                continue;
            }
            if let Some(base) = w.strip_suffix("_last").filter(|b| !b.is_empty()) {
                w = base.to_string();
                place = Place::Last;
                continue;
            }
        }
        break;
    }
    (w, place, grain)
}

fn ending(place: Place, grain: Option<Grain>) -> String {
    let mut s = grain.map_or(String::new(), |g| format!("_by_{}", g.name()));
    s.push_str(match place {
        Place::First => "_first",
        Place::Last => "_last",
        Place::Normal => "",
    });
    s
}

/// Whether a rule as written is an order rule (the editor colours those differently).
pub fn is_order_word(raw: &str) -> bool {
    let (word, _, _) = split(&normalize(raw));
    order_word(alias(&word)).is_some()
}

/// What one word means.
enum Meaning {
    Order(Order),
    Group(Group),
    /// A word Glint recognises, written wrong (-updated_3_dys): what to write instead.
    Explained(String),
    /// _by_… on something that isn't a date order.
    BadGrain(String),
    Unknown,
}

/// What a rule word (normalised) means.
fn meaning(word: &str, written: &str) -> Meaning {
    let (rule, place, grain) = split(word);
    let rule = alias(&rule).to_string();
    if let Some((key, desc)) = order_word(&rule) {
        let descending = desc ^ (place == Place::Last);
        let grain = grain.filter(|_| matches!(key, Key::Modified | Key::Created));
        return Meaning::Order(Order { key, descending, grain, text: order_text(key, descending, grain) });
    }
    if let Some(g) = grain {
        return Meaning::BadGrain(format!("\"{written}\": _by_{} goes with a date order rule, like -newest_by_{}.", g.name(), g.name()));
    }
    match make_group(&rule, written) {
        Ok(Some(mut g)) => {
            g.place = place;
            Meaning::Group(g)
        }
        Ok(None) => Meaning::Unknown,
        Err(text) => Meaning::Explained(text),
    }
}

fn group(name: impl Into<String>, label: impl Into<String>, exactness: f64, takes: Takes) -> Group {
    Group { name: name.into(), label: label.into(), exactness, place: Place::Normal, takes, orders: Vec::new() }
}

/// A shorter time is more exact: the last 2 hours beats the last 24 hours, which beats the last week.
fn date_exactness(hours: f64) -> f64 {
    50.0 + 10.0 / (1.0 + hours / 24.0)
}

fn type_group(ext: &str) -> Group {
    group(format!(".{ext}"), format!(".{ext} files"), 30.0, Takes::Ext(ext.to_string()))
}

/// The group a word names: Ok(None) when it names none, Err when it's a rule Glint knows written
/// wrong (and what to write).
fn make_group(w: &str, written: &str) -> Result<Option<Group>, String> {
    let since = |name: String, hours: f64, label: String, created: bool| group(name, label, date_exactness(hours), Takes::Since { hours, created });
    match w {
        "folder" => return Ok(Some(group("folder", "folders", 10.0, Takes::Folders))),
        "file" => return Ok(Some(group("file", "files", 10.0, Takes::Files))),
        "other" => return Ok(Some(group("other", "everything else", 0.0, Takes::Nothing))),
        "hidden" => return Ok(Some(group("hidden", "hidden", 40.0, Takes::Hidden))),
        "empty" => return Ok(Some(group("empty", "empty files", 36.0, Takes::Empty))),
        // Dates take files only: a folder's date changes whenever anything inside it does.
        "updated" => return Ok(Some(since("updated".into(), 24.0, "updated in the last 24 hours".into(), false))),
        "added" => return Ok(Some(since("added".into(), 24.0, "added in the last 24 hours".into(), true))),
        "today" => return Ok(Some(group("today", "changed today", date_exactness(24.0), Takes::Today))),
        "yesterday" => return Ok(Some(group("yesterday", "changed yesterday", date_exactness(24.0), Takes::Yesterday))),
        _ => {}
    }
    // -.blend (a file type with a dot)
    if let Some(ext) = w.strip_prefix('.') {
        if ext.is_empty() || !ext.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err(format!("{written} isn't a file type. Write the letters after the dot, like -.blend"));
        }
        return Ok(Some(type_group(ext)));
    }
    // -updated_3_days, -added_2_hours, -older_than_1_year
    for kind in ["older_than", "updated", "added"] {
        let Some(rest) = w.strip_prefix(kind) else { continue };
        let rest = match rest.strip_prefix('_') {
            Some(r) => r,
            None if rest.is_empty() && kind == "older_than" => "",
            None => continue,
        };
        let span = time_span(rest);
        let Some((n, unit)) = span else {
            let example = if kind == "older_than" { "1_year" } else { "3_days" };
            return Err(format!("{written}: write a number and hours, days, weeks, months or years, like -{kind}_{example}."));
        };
        let hours = n as f64
            * match unit {
                'h' => 1.0,
                'd' => 24.0,
                'w' => 24.0 * 7.0,
                'm' => 24.0 * 30.0,
                _ => 24.0 * 365.0,
            };
        let unit_name = format!(
            "{}{}",
            match unit {
                'h' => "hour",
                'd' => "day",
                'w' => "week",
                'm' => "month",
                _ => "year",
            },
            if n == 1 { "" } else { "s" }
        );
        if kind == "older_than" {
            return Ok(Some(group(format!("older:{hours}"), format!("older than {n} {unit_name}"), 45.0 + (hours / 87_600.0).min(4.0), Takes::Older { hours })));
        }
        let shown = if n == 1 { unit_name } else { format!("{n} {unit_name}") };
        return Ok(Some(since(format!("{kind}:{hours}"), hours, format!("{kind} in the last {shown}"), kind == "added")));
    }
    // -bigger_than_100mb, -smaller_than_1mb
    for kind in ["bigger", "larger", "smaller"] {
        let Some(rest) = w.strip_prefix(kind).and_then(|r| r.strip_prefix("_than")) else { continue };
        if !rest.is_empty() && !rest.starts_with('_') {
            continue;
        }
        let Some((number, unit, digits)) = size_of(rest.trim_start_matches('_')) else {
            return Err(format!("{written}: write a size, like -{kind}_than_100mb (kb, mb or gb)."));
        };
        let factor = match unit.as_str() {
            "kb" => 1024.0,
            "mb" => 1024.0 * 1024.0,
            "gb" => 1024.0 * 1024.0 * 1024.0,
            "tb" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
            _ => 1.0,
        };
        let bytes = (number * factor).min((u64::MAX / 2) as f64) as u64;
        let shown = format!("{digits} {}", if unit.starts_with('b') { "bytes".to_string() } else { unit.to_uppercase() });
        let log = (bytes.max(1) as f64).log10();
        // A bigger "bigger than" (and a smaller "smaller than") is more exact.
        return Ok(Some(if kind == "smaller" {
            group(format!("smaller:{bytes}"), format!("smaller than {shown}"), 35.0 + (20.0 - log) / 100.0, Takes::Smaller(bytes))
        } else {
            group(format!("bigger:{bytes}"), format!("bigger than {shown}"), 35.0 + log / 100.0, Takes::Bigger(bytes))
        }));
    }
    // -name_has_screenshot, -name_starts_with_img, -name_ends_with_final
    for how in ["has", "contains", "starts_with", "ends_with"] {
        let Some(rest) = w.strip_prefix("name_").and_then(|r| r.strip_prefix(how)) else { continue };
        if !rest.is_empty() && !rest.starts_with('_') {
            continue;
        }
        let how = if how == "contains" { "has" } else { how };
        let text = rest.strip_prefix('_').unwrap_or("");
        if text.is_empty() {
            let what = if how == "has" { "has in it".to_string() } else { how.replace('_', " ") };
            return Err(format!("{written}: write what the name {what} after it, like -name_{how}_invoice."));
        }
        let shown = text.replace('_', " ");
        return Ok(Some(match how {
            "starts_with" => group(format!("name^{text}"), format!("names starting with \"{shown}\""), 70.0, Takes::NameStarts(text.into())),
            "ends_with" => group(format!("name${text}"), format!("names ending with \"{shown}\""), 70.0, Takes::NameEnds(text.into())),
            _ => group(format!("name~{text}"), format!("names with \"{shown}\""), 70.0, Takes::NameHas(text.into())),
        }));
    }
    if let Some(list) = kind(w) {
        return Ok(Some(group(w, kind_label(w), 20.0, Takes::Kind(list))));
    }
    // A file type without the dot: -exe, -png, -torrent
    if known_type(w) {
        return Ok(Some(type_group(w)));
    }
    Ok(None)
}

/// "3_days", "2h", "1_year" -> (3, 'd')...; None when it isn't a time.
fn time_span(s: &str) -> Option<(u32, char)> {
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 4 {
        return None;
    }
    let unit = s[digits.len()..].trim_start_matches('_');
    let u = match unit {
        "hour" | "hours" | "h" => 'h',
        "day" | "days" | "d" => 'd',
        "week" | "weeks" | "w" => 'w',
        "month" | "months" => 'm',
        "year" | "years" | "y" => 'y',
        _ => return None,
    };
    let n: u32 = digits.parse().ok()?;
    (n > 0).then_some((n, u))
}

/// "100mb", "1.5_gb", "200" (MB) -> (number, unit, the number as written).
fn size_of(s: &str) -> Option<(f64, String, String)> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    if digits.is_empty() || !digits.starts_with(|c: char| c.is_ascii_digit()) || digits.ends_with('.') || digits.matches('.').count() > 1 {
        return None;
    }
    let unit = s[digits.len()..].trim_start_matches('_');
    let unit = match unit {
        "" => "mb",
        "b" | "byte" | "bytes" => "b",
        "kb" | "mb" | "gb" | "tb" => unit,
        _ => return None,
    };
    Some((digits.parse().ok()?, unit.to_string(), digits))
}

fn kind_label(kind: &str) -> String {
    match kind {
        "image" => "pictures",
        "video" => "videos",
        "audio" => "music and sounds",
        "document" => "documents",
        "archive" => "archives",
        "program" => "programs",
        "font" => "fonts",
        "shortcut" => "shortcuts",
        other => other,
    }
    .to_string()
}

fn order_text(key: Key, descending: bool, grain: Option<Grain>) -> String {
    let text = match key {
        Key::Modified => if descending { "newest first" } else { "oldest first" },
        Key::Created => if descending { "newest added first" } else { "oldest added first" },
        Key::Size => if descending { "biggest first" } else { "smallest first" },
        Key::Type => if descending { "by type, Z to A" } else { "by type" },
        Key::Extension => if descending { "by extension, Z to A" } else { "by extension" },
        Key::Name => if descending { "Z to A" } else { "A to Z" },
    };
    match grain {
        Some(g) => format!("{text} (by {})", g.name()),
        None => text.to_string(),
    }
}

fn key_name(key: Key) -> &'static str {
    match key {
        Key::Modified => "date changed",
        Key::Created => "date added",
        Key::Size => "size",
        Key::Type => "type",
        Key::Extension => "extension",
        Key::Name => "name",
    }
}

/// A place name (Downloads…) or a full path, as a folder path.
pub fn place_path(label: &str) -> Option<String> {
    use windows::Win32::UI::Shell::{FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music, FOLDERID_Pictures, FOLDERID_Profile, FOLDERID_Videos};
    let id = match label.to_lowercase().as_str() {
        "downloads" => Some(FOLDERID_Downloads),
        "desktop" => Some(FOLDERID_Desktop),
        "documents" => Some(FOLDERID_Documents),
        "pictures" => Some(FOLDERID_Pictures),
        "music" => Some(FOLDERID_Music),
        "videos" => Some(FOLDERID_Videos),
        "home" => Some(FOLDERID_Profile),
        _ => None,
    };
    if let Some(path) = id.and_then(|id| crate::shell::known_folder(&id)) {
        return Some(path);
    }
    let path = folder::normalize(label);
    let b = path.as_bytes();
    (path.starts_with("\\\\") || (b.get(1) == Some(&b':') && b.get(2) == Some(&b'\\'))).then_some(path)
}

fn same_path(a: &str, b: &str) -> bool {
    a.trim_end_matches('\\').eq_ignore_ascii_case(b.trim_end_matches('\\'))
}

// ---------------------------------------------------------------- reading

/// One word of a line: where it is in the text, and how it was written.
#[derive(Clone, Debug)]
struct Token<'a> {
    text: &'a str,
    start: usize,
    end: usize,
}

/// A recipe, read.
#[derive(Clone, Debug, Default)]
pub struct Recipe {
    pub messages: Vec<Message>,
    everywhere: Rules,
    folders: Vec<(String, String, Rules)>,
}

impl Recipe {
    /// Reads a recipe. Never fails: mistakes become `messages`.
    pub fn parse(text: &str) -> Recipe {
        let mut r = Recipe::default();
        // Which rules the lines go into: None for every folder, Some(i) for folders[i], and a
        // throwaway set under a folder Glint doesn't know.
        let mut current: Option<Option<usize>> = Some(None);
        let mut scratch = Rules::default();
        let mut offset = 0;
        for (i, raw_line) in text.split('\n').enumerate() {
            let line_no = i + 1;
            let line_start = offset;
            offset += raw_line.len() + 1;
            let content = strip_note(raw_line.trim_end_matches('\r'));
            let lead = content.len() - content.trim_start().len();
            let line = content.trim();
            if line.is_empty() {
                continue;
            }
            let at = line_start + lead;
            // "in Downloads:" - rules for one folder (the colon may be left out).
            if let Some(rest) = strip_in(line) {
                let label = rest.trim_end_matches(':').trim().trim_matches('"').to_string();
                let span = Some((at, at + line.len()));
                match place_path(&label).filter(|_| !label.is_empty()) {
                    Some(path) => {
                        r.folders.push((path, label, Rules { line: line_no, ..Rules::default() }));
                        current = Some(Some(r.folders.len() - 1));
                    }
                    None => {
                        let suggestion = closest(&label, PLACE_NAMES.iter().copied());
                        let hint = suggestion.map_or(String::new(), |s| format!(" Did you mean \"{s}\"?"));
                        r.messages.push(Message {
                            line: line_no,
                            text: format!("\"{label}\" isn't a folder Glint knows.{hint} Write one of {}, or a full path like in C:\\Users\\me\\Work:", PLACE_NAMES.join(", ")),
                            error: true,
                            span,
                            fix: suggestion.map(|s| format!("in {s}:")),
                        });
                        current = None; // rules below it are still checked, but not used
                    }
                }
                continue;
            }
            if line.ends_with(':') {
                let name = line.trim_end_matches(':').trim();
                r.messages.push(Message {
                    line: line_no,
                    text: format!("To give a folder its own rules, start the line with \"in\": in {name}:"),
                    error: true,
                    span: Some((at, at + line.len())),
                    fix: Some(format!("in {name}:")),
                });
                continue;
            }
            let tokens = tokenize(line, at);
            let mut messages = Vec::new();
            let rules = match current {
                Some(None) => &mut r.everywhere,
                Some(Some(i)) => &mut r.folders[i].2,
                None => &mut scratch,
            };
            read_line(&tokens, line_no, rules, &mut messages);
            r.messages.extend(messages);
        }
        let mut tips = Vec::new();
        check_useless(&r.everywhere, "", &mut tips);
        for (_, label, rules) in &r.folders {
            if rules.groups.is_empty() && rules.orders.is_empty() {
                tips.push(Message { line: rules.line, text: format!("\"in {label}:\" has no rules under it yet."), error: false, span: None, fix: None });
            }
            check_useless(rules, &format!(" (in {label})"), &mut tips);
        }
        r.messages.extend(tips);
        r
    }

    #[cfg(test)]
    pub fn has_errors(&self) -> bool {
        self.messages.iter().any(|m| m.error)
    }

    fn rules_for(&self, folder: &str) -> &Rules {
        self.folders.iter().find(|(p, ..)| same_path(p, folder)).map_or(&self.everywhere, |(_, _, r)| r)
    }

    /// The name as written of the "in …:" rules this folder gets, if any.
    pub fn folder_label(&self, folder: &str) -> Option<&str> {
        self.folders.iter().find(|(p, ..)| same_path(p, folder)).map(|(_, l, _)| l.as_str())
    }

    /// Whether the rules for this folder have groups (the preview then shows group headings).
    pub fn has_groups(&self, folder: &str) -> bool {
        !self.rules_for(folder).groups.is_empty()
    }

    /// The preview's heading for an item's group: its name, and its own order if it has one
    /// ("pictures  ·  biggest first").
    pub fn group_heading(&self, e: &Entry, folder: &str, clock: &Clock) -> String {
        match group_of(self.rules_for(folder), e, clock).1 {
            None => "everything else".into(),
            Some(g) if g.orders.is_empty() => g.label.clone(),
            Some(g) => format!("{}  \u{00b7}  {}", g.label, g.orders.iter().map(|o| o.text.as_str()).collect::<Vec<_>>().join(", then ")),
        }
    }

    /// An item's group name ("folders", ".exe files"…).
    #[cfg(test)]
    pub fn group_name(&self, e: &Entry, folder: &str, clock: &Clock) -> String {
        group_of(self.rules_for(folder), e, clock).1.map_or_else(|| "everything else".into(), |g| g.label.clone())
    }

    /// Which group each item goes in, as a number: the same number, the same group.
    pub fn group_number(&self, e: &Entry, folder: &str, clock: &Clock) -> i64 {
        group_of(self.rules_for(folder), e, clock).0
    }

    /// Sorts `view` (indexes into `entries`) the way the rules for `folder` say.
    pub fn sort(&self, entries: &[Entry], view: &mut [u32], folder: &str, clock: &Clock) {
        let rules = self.rules_for(folder);
        let mut keyed: Vec<(i64, Option<usize>, u32)> = view
            .iter()
            .map(|&i| {
                let (pos, g) = group_index(rules, &entries[i as usize], clock);
                (pos, g, i)
            })
            .collect();
        keyed.sort_by(|&(pa, ga, a), &(pb, _, b)| {
            let (a, b) = (&entries[a as usize], &entries[b as usize]);
            pa.cmp(&pb)
                .then_with(|| {
                    // The same place, so the same group: its own order rules first, then the ones
                    // for every group.
                    let own = ga.map_or(&[][..], |g| &rules.groups[g].orders[..]);
                    own.iter().chain(&rules.orders).map(|o| compare(o, a, b, clock)).find(|c| c.is_ne()).unwrap_or(Ordering::Equal)
                })
                .then_with(|| folder::natural_cmp(&a.lower, &b.lower))
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| folder::result_folder(a).to_lowercase().cmp(&folder::result_folder(b).to_lowercase()))
        });
        for (slot, (_, _, i)) in view.iter_mut().zip(keyed) {
            *slot = i;
        }
    }
}

/// "in Downloads:" -> "Downloads:" (any capitals for "in").
fn strip_in(line: &str) -> Option<&str> {
    let head = line.get(..3)?;
    (head.eq_ignore_ascii_case("in ") || head.eq_ignore_ascii_case("in\t")).then(|| line[3..].trim_start()).filter(|r| !r.is_empty())
}

/// Notes start with # (or //) and are left out.
fn strip_note(line: &str) -> &str {
    let mut end = line.len();
    for mark in ["#", "//"] {
        let mut from = 0;
        while let Some(i) = line[from..].find(mark).map(|i| i + from) {
            if i == 0 || line[..i].ends_with(char::is_whitespace) {
                end = end.min(i);
                break;
            }
            from = i + mark.len();
        }
    }
    &line[..end]
}

/// The words of a line, split at spaces, commas and semicolons, with where each one is.
fn tokenize(line: &str, at: usize) -> Vec<Token<'_>> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in line.char_indices().chain(std::iter::once((line.len(), ' '))) {
        let sep = c.is_whitespace() || c == ',' || c == ';';
        match (sep, start) {
            (true, Some(s)) => {
                out.push(Token { text: &line[s..i], start: at + s, end: at + i });
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    out
}

fn has_dash(raw: &str) -> bool {
    raw.starts_with(['-', '–', '—'])
}

/// How many of the tokens from `i` make one rule: words written with spaces instead of `_`
/// ("newest first", "bigger than 100 mb") join up when together they make a rule. A word with a
/// dash always starts a new rule. A name rule takes one word of text.
fn joined_len(tokens: &[Token], i: usize) -> usize {
    let mut most = 1;
    while i + most < tokens.len() && most < 6 && !has_dash(tokens[i + most].text) {
        most += 1;
    }
    for n in (2..=most).rev() {
        let word = tokens[i..i + n].iter().map(|t| normalize(t.text)).collect::<Vec<_>>().join("_");
        if let Meaning::Order(_) | Meaning::Group(_) = meaning(&word, &word) {
            // "name has screenshot newest" is "name has screenshot", then "newest".
            let name_words = ["name_has_", "name_contains_", "name_starts_with_", "name_ends_with_"];
            if let Some(prefix) = name_words.iter().find(|p| word.starts_with(*p)) {
                let used = prefix.matches('_').count() + 1;
                if n != used {
                    continue;
                }
            }
            return n;
        }
    }
    // A misspelt rule with its ending written apart ("newst first"): one word, so the fix
    // ("newest_first") takes both.
    let next = |k: usize| tokens.get(i + k).filter(|t| !has_dash(t.text)).map(|t| normalize(t.text));
    match (next(1).as_deref(), next(2).as_deref()) {
        (Some("first" | "last"), _) => 2,
        (Some("by"), Some("day" | "week" | "month" | "year")) => 3,
        _ => 1,
    }
}

/// One line. An order rule written after a group on the same line is for that group only
/// (-image -biggest); one written before any group, or on its own line, is for every group.
fn read_line(tokens: &[Token], line: usize, into: &mut Rules, messages: &mut Vec<Message>) {
    let mut line_groups: Vec<usize> = Vec::new();
    let mut last_was_order = false;
    let mut i = 0;
    while i < tokens.len() {
        let n = joined_len(tokens, i);
        let (start, end) = (tokens[i].start, tokens[i + n - 1].end);
        let word = tokens[i..i + n].iter().map(|t| normalize(t.text)).collect::<Vec<_>>().join("_");
        let raw: String = tokens[i..i + n].iter().map(|t| t.text).collect::<Vec<_>>().join(" ");
        let dash = has_dash(tokens[i].text);
        i += n;
        if word.is_empty() {
            continue;
        }
        let written = format!("-{word}");
        let span = Some((start, end));
        let say = |messages: &mut Vec<Message>, text: String, error: bool, fix: Option<String>| messages.push(Message { line, text, error, span, fix });
        match meaning(&word, &written) {
            Meaning::Order(o) => {
                let (_, _, grain) = split(&word);
                if let (Some(g), None) = (grain, o.grain) {
                    say(messages, format!("{written}: _by_{} only works on dates (-newest_by_{}), so it was left out.", g.name(), g.name()), false, None);
                }
                if line_groups.is_empty() {
                    add_order(&mut into.orders, o, &written, "", line, span, messages);
                } else {
                    for &g in &line_groups {
                        let label = format!(" for {}", into.groups[g].label);
                        add_order(&mut into.groups[g].orders, o.clone(), &written, &label, line, span, messages);
                    }
                }
                last_was_order = true;
            }
            Meaning::Group(g) => {
                if last_was_order {
                    line_groups.clear(); // -folder -a_to_z -image -biggest: each group gets its own
                }
                line_groups.push(add_group(into, g, &written, line, span, messages));
                last_was_order = false;
            }
            Meaning::Explained(text) | Meaning::BadGrain(text) => say(messages, text, true, None),
            Meaning::Unknown => {
                let (rule, place, grain) = split(&word);
                let words = rule_words();
                let close = closest(&rule, words.iter().copied());
                let plain = !rule.is_empty() && rule.len() <= 8 && rule.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
                if close.is_none() && plain && n == 1 {
                    // Not a rule: a file type (-blend).
                    say(messages, format!("{written} means files ending in .{rule}."), false, None);
                    let mut g = type_group(&rule);
                    g.place = place;
                    if last_was_order {
                        line_groups.clear();
                    }
                    line_groups.push(add_group(into, g, &written, line, span, messages));
                    last_was_order = false;
                    continue;
                }
                match close {
                    Some(c) => {
                        let fixed = format!("{c}{}", ending(place, grain));
                        say(messages, format!("\"{}\" isn't a rule. Did you mean -{fixed}?", raw.trim_start_matches(['-', '–', '—'])), true, Some(if dash { format!("-{fixed}") } else { fixed }));
                    }
                    None => say(messages, format!("\"{written}\" isn't a rule. The guide lists them (for files ending in .{rule}, write -.{rule})."), true, None),
                }
            }
        }
    }
}

fn add_group(into: &mut Rules, g: Group, written: &str, line: usize, span: Option<(usize, usize)>, messages: &mut Vec<Message>) -> usize {
    if let Some(i) = into.groups.iter().position(|x| x.name == g.name) {
        messages.push(Message { line, text: format!("{written}: {} already have a line above, so this one is left out.", into.groups[i].label), error: false, span, fix: None });
        return i;
    }
    into.groups.push(g);
    into.groups.len() - 1
}

fn add_order(into: &mut Vec<Order>, o: Order, written: &str, for_whom: &str, line: usize, span: Option<(usize, usize)>, messages: &mut Vec<Message>) {
    if into.iter().any(|x| x.key == o.key) {
        let text = format!("{written}: there's already an order rule for the {}{for_whom} before it, so this one never decides anything.", key_name(o.key));
        messages.push(Message { line, text, error: false, span, fix: None });
        return;
    }
    into.push(o);
}

/// Tips for rules that can't make a difference.
fn check_useless(rules: &Rules, place: &str, out: &mut Vec<Message>) {
    let mut check = |orders: &[Order], whose: String| {
        // An exact date first: two items almost never share the same second, so later rules hardly
        // ever decide.
        if orders.len() > 1 && matches!(orders[0].key, Key::Modified | Key::Created) && orders[0].grain.is_none() {
            let first = &orders[0];
            let word = match (first.key, first.descending) {
                (Key::Created, true) => "newest_added",
                (Key::Created, false) => "oldest_added",
                (_, true) => "newest",
                (_, false) => "oldest",
            };
            out.push(Message {
                line: rules.line,
                text: format!("Tip{place}{whose}: -{word} is exact to the second, so the order rules after it almost never decide. -{word}_by_day puts each day together and lets the next rule order it."),
                error: false,
                span: None,
                fix: None,
            });
        }
    };
    check(&rules.orders, String::new());
    for g in &rules.groups {
        check(&g.orders, format!(" ({})", g.label));
    }
}

// ---------------------------------------------------------------- using it

/// A name as the name rules compare it: lower case, spaces and dashes as _.
fn plain(name: &str) -> String {
    name.to_lowercase().replace([' ', '-'], "_")
}

fn takes(g: &Group, e: &Entry, clock: &Clock) -> bool {
    let dir = e.is_container();
    let since = |t: u64, hours: f64| t as f64 >= clock.now as f64 - hours * HOUR;
    match &g.takes {
        Takes::Folders => dir,
        Takes::Files => !dir,
        Takes::Nothing => false,
        Takes::Hidden => e.is_hidden(),
        Takes::Empty => !dir && e.size == 0,
        Takes::Since { hours, created } => !dir && since(if *created { e.created } else { e.modified }, *hours),
        Takes::Today => !dir && e.modified != 0 && clock.day(e.modified) == clock.day(clock.now),
        Takes::Yesterday => !dir && e.modified != 0 && clock.day(e.modified) == clock.day(clock.now) - 1,
        Takes::Older { hours } => !dir && !since(e.modified, *hours),
        Takes::Bigger(b) => !dir && e.size > *b,
        Takes::Smaller(b) => !dir && e.size < *b,
        Takes::NameStarts(t) => plain(&e.name).starts_with(t.as_str()),
        Takes::NameEnds(t) => {
            let stem = if dir || e.ext.is_empty() { e.name.as_str() } else { &e.name[..e.name.len() - e.ext.len() - 1] };
            plain(stem).ends_with(t.as_str())
        }
        Takes::NameHas(t) => plain(&e.name).contains(t.as_str()),
        Takes::Kind(list) => !dir && list.contains(&e.ext.as_str()),
        Takes::Ext(x) => !dir && e.ext == *x,
    }
}

/// Where an item goes (smaller comes first: the _first groups, the others in the order written,
/// things that fit nothing, the _last groups) and its group (None: it fits no group and there's
/// no -other).
fn group_index(rules: &Rules, e: &Entry, clock: &Clock) -> (i64, Option<usize>) {
    let mut best: Option<usize> = None;
    for (i, g) in rules.groups.iter().enumerate() {
        if best.is_none_or(|b| g.exactness > rules.groups[b].exactness) && takes(g, e, clock) {
            best = Some(i);
        }
    }
    let best = match best.or_else(|| rules.groups.iter().position(|g| g.name == "other")) {
        Some(b) => b,
        None => return (199_999, None), // after the other groups, before the _last ones
    };
    let tier = match rules.groups[best].place {
        Place::First => 0,
        Place::Normal => 100_000,
        Place::Last => 200_000,
    };
    (tier + best as i64, Some(best))
}

fn group_of<'a>(rules: &'a Rules, e: &Entry, clock: &Clock) -> (i64, Option<&'a Group>) {
    let (pos, g) = group_index(rules, e, clock);
    (pos, g.map(|g| &rules.groups[g]))
}

fn compare(o: &Order, a: &Entry, b: &Entry, clock: &Clock) -> Ordering {
    let c = match o.key {
        Key::Modified => clock.round(a.modified, o.grain).cmp(&clock.round(b.modified, o.grain)),
        Key::Created => clock.round(a.created, o.grain).cmp(&clock.round(b.created, o.grain)),
        Key::Size => a.size.cmp(&b.size),
        Key::Type => a.type_name.to_lowercase().cmp(&b.type_name.to_lowercase()),
        Key::Extension => a.ext.cmp(&b.ext),
        Key::Name => folder::natural_cmp(&a.lower, &b.lower),
    };
    if o.descending {
        c.reverse()
    } else {
        c
    }
}

/// The recipe last read, kept so every tab shares one (reading again only when the text changed).
static CACHE: Mutex<Option<(String, Arc<Recipe>)>> = Mutex::new(None);

/// The recipe for this text, read once.
pub fn cached(text: &str) -> Arc<Recipe> {
    let mut cache = CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((t, r)) = cache.as_ref() {
        if t == text {
            return r.clone();
        }
    }
    let r = Arc::new(Recipe::parse(text));
    *cache = Some((text.to_string(), r.clone()));
    r
}

/// What a new custom sort order starts with: explained, and a useful order in itself.
pub const STARTER: &str = "# My sort order. Lines starting with # are notes.
# Write the kinds of things in the order you want them.
# _first / _last put one at the very top / bottom.

-newest_first            # newest first in every group
-folder_first            # folders at the very top
-updated                 # files changed in the last 24 hours
-document
-image -biggest_first    # pictures, the biggest first
-video
-program_last            # programs at the very bottom

# Just for Downloads: everything newest first, all mixed.
in Downloads:
-newest_first
";

/// "Start from…": ready-made sort orders (name, text).
pub const PRESETS: &[(&str, &str)] = &[
    ("Downloads: newest first, installers last", "-a_to_z\n-folder_first\n\nin Downloads:\n-newest_first\n-program_last\n"),
    ("Photos: pictures newest first", "-a_to_z\n-folder_first\n-image -newest_first\n-video -newest_first\n"),
    ("Code project: folders, then by type", "-folder_first\n-by_type\n-a_to_z\n"),
    ("Plain A\u{2013}Z", "-a_to_z\n"),
    ("The starter, with notes", STARTER),
];

/// The examples (kept so they're checked to read without mistakes).
#[cfg(test)]
pub const EXAMPLES: &[(&str, &str)] = &[
    ("Folders first, then A to Z", "-a_to_z\n-folder_first"),
    ("Folders first, then newest first", "-newest_first\n-folder_first"),
    ("Newest first, everything mixed", "-newest_first"),
    ("Just-changed files on top", "-newest_first\n-updated_first\n-folder"),
    ("Recent, programs, pictures, videos", "-newest_first\n-updated\n-exe\n-image\n-video"),
    ("PNGs before other pictures", "-a_to_z\n-folder_first\n-png\n-image"),
    ("Programs at the bottom", "-newest_first\n-folder_first\n-program_last"),
    ("Pictures biggest first, the rest newest first", "-newest_first\n-folder_first\n-image -biggest_first"),
    ("Screenshots together", "-newest_first\n-folder_first\n-name_has_screenshot"),
    ("Big files at the bottom", "-a_to_z\n-folder_first\n-bigger_than_100mb_last"),
    ("Old files at the bottom", "-newest_first\n-folder_first\n-older_than_1_year_last"),
    ("By day, A to Z within each day", "-newest_by_day\n-a_to_z\n-folder_first"),
    ("Biggest first", "-biggest_first\n-folder_first"),
    ("By type, then A to Z", "-by_type\n-a_to_z\n-folder_first"),
    ("Hidden things at the bottom", "-a_to_z\n-folder_first\n-hidden_last"),
    ("Downloads newest first, elsewhere folders first", "-a_to_z\n-folder_first\n\nin Downloads:\n-newest_first"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::folder::EntryKind;

    /// 7 October 2026, 12:00 UTC, as a FILETIME.
    const NOW: u64 = 134_358_480_000_000_000;
    const H: u64 = 36_000_000_000;
    const D: u64 = 24 * H;

    fn clock() -> Clock {
        Clock { now: NOW, offset: 0 }
    }

    fn entry(name: &str, dir: bool, modified: u64, size: u64) -> Entry {
        let ext = if dir { String::new() } else { folder::extension(name) };
        Entry {
            name: name.into(),
            lower: name.to_lowercase(),
            kind: if dir { EntryKind::Folder } else { EntryKind::File },
            size,
            modified,
            created: modified,
            attributes: 0,
            type_name: Arc::from(name.to_string()),
            ext,
            target: None,
            free: 0,
        }
    }

    fn f(name: &str, modified: u64, size: u64) -> Entry {
        entry(name, false, modified, size)
    }
    fn d(name: &str, modified: u64) -> Entry {
        entry(name, true, modified, 0)
    }

    fn items() -> Vec<Entry> {
        vec![
            f("b.exe", NOW - H, 500),
            f("a.txt", NOW - 2 * H, 1),
            d("Zeta", NOW - D),
            f("c.pdf", NOW - 3 * D, 1),
            d("Alpha", NOW - 5 * D),
            f("z.exe", NOW - 2 * D, 100),
            f("m.txt", NOW - 3 * H, 1),
            f("p.png", NOW - 4 * D, 300),
            f("q.jpg", NOW - 6 * D, 900),
            f("s.png", NOW - 5 * H, 50),
        ]
    }

    fn sorted_in(recipe: &str, folder: &str) -> Vec<String> {
        let entries = items();
        let mut view: Vec<u32> = (0..entries.len() as u32).collect();
        Recipe::parse(recipe).sort(&entries, &mut view, folder, &clock());
        view.iter().map(|&i| entries[i as usize].name.clone()).collect()
    }

    fn order(recipe: &str) -> String {
        sorted_in(recipe, "C:\\t").join(",")
    }

    fn name_of(recipe: &str, e: &Entry) -> String {
        Recipe::parse(recipe).group_name(e, "C:\\t", &clock())
    }

    fn errors(recipe: &str) -> Vec<Message> {
        Recipe::parse(recipe).messages.into_iter().filter(|m| m.error).collect()
    }

    // Every kind of rule, case by case.

    #[test]
    fn the_users_own_example() {
        const MINE: &str = "-newest_first\n-updated\n-exe\n-image\n-video";
        assert!(Recipe::parse(MINE).messages.is_empty(), "{:?}", Recipe::parse(MINE).messages);
        assert_eq!(order(MINE), "b.exe,a.txt,m.txt,s.png,z.exe,p.png,q.jpg,Zeta,c.pdf,Alpha");
        assert_eq!(order("-a_to_z\n-folder\n-png\n-image"), "Alpha,Zeta,p.png,s.png,q.jpg,a.txt,b.exe,c.pdf,m.txt,z.exe");
        assert_eq!(order("-folder\n-image\n-updated"), "Alpha,Zeta,p.png,q.jpg,a.txt,b.exe,m.txt,s.png,c.pdf,z.exe");
        assert_eq!(name_of("-updated\n-folder", &d("Fresh", NOW - 5 * 60 * 10_000_000)), "folders");
        assert_eq!(order("-folder\n-a_to_z"), "Alpha,Zeta,a.txt,b.exe,c.pdf,m.txt,p.png,q.jpg,s.png,z.exe");
        assert_eq!(order("-folder\n-file\n-name"), order("-folder\n-a_to_z"));
        assert_eq!(sorted_in("-newest_first_by_day\n-a_to_z", "C:\\t")[..4], ["a.txt", "b.exe", "m.txt", "s.png"]);
        assert_eq!(sorted_in("-today\n-folder", "C:\\t")[..5], ["a.txt", "b.exe", "m.txt", "s.png", "Alpha"]);
        let blend = Recipe::parse("-blend");
        assert!(!blend.has_errors());
        assert_eq!(blend.group_name(&f("scene.blend", NOW, 1), "C:\\t", &clock()), ".blend files");
        assert!(errors("-imgae").iter().any(|m| m.text.contains("-image")), "a typo isn't taken as a file type");
        assert!(!Recipe::parse(STARTER).has_errors());
        for (name, text) in EXAMPLES.iter().chain(PRESETS) {
            assert!(!Recipe::parse(text).has_errors(), "{name}: {:?}", errors(text));
        }
        assert!(GROUP_RULES.iter().all(|(r, _)| *r != "-file"));
    }

    #[test]
    fn first_and_last() {
        assert_eq!(order("-a_to_z\n-folder\n-image_first\n-exe_last"), "p.png,q.jpg,s.png,Alpha,Zeta,a.txt,c.pdf,m.txt,b.exe,z.exe");
        assert!(!Recipe::parse("-document_first\n-image_last").has_errors());
        assert_eq!(order("-biggest"), "q.jpg,b.exe,p.png,z.exe,s.png,a.txt,c.pdf,m.txt,Alpha,Zeta");
        assert_eq!(order("-biggest_first"), order("-biggest"));
        assert_eq!(order("-biggest_last"), "Alpha,Zeta,a.txt,c.pdf,m.txt,s.png,z.exe,p.png,b.exe,q.jpg");
        assert_eq!(order("-smallest"), order("-biggest_last"));
        assert_eq!(order("-newest_last"), order("-oldest"));
        assert_ne!(order("-newest_last"), order("-newest"));
    }

    #[test]
    fn an_order_after_a_group_is_that_groups() {
        const PER_GROUP: &str = "-newest_first\n-folder -a_to_z\n-image -biggest";
        assert_eq!(order(PER_GROUP), "Alpha,Zeta,q.jpg,p.png,s.png,b.exe,a.txt,m.txt,z.exe,c.pdf");
        assert_eq!(Recipe::parse(PER_GROUP).group_heading(&f("x.jpg", NOW, 1), "C:\\t", &clock()), "pictures  \u{00b7}  biggest first");
        assert_eq!(order("-newest_first -folder -exe"), order("-newest_first\n-folder\n-exe"));
    }

    #[test]
    fn being_exact() {
        assert_eq!(name_of("-updated_3_days", &f("c.pdf", NOW - 3 * D, 1)), "updated in the last 3 days");
        assert_eq!(name_of("-updated_3_days", &f("old.pdf", NOW - 4 * D, 1)), "everything else");
        assert_eq!(name_of("-updated_2_hours\n-updated", &f("x.txt", NOW - H, 1)), "updated in the last 2 hours");
        assert_eq!(name_of("-updated\n-updated_2_hours", &f("x.txt", NOW - H, 1)), "updated in the last 2 hours");
        assert_eq!(name_of("-older_than_1_year", &f("old.txt", NOW - 400 * D, 1)), "older than 1 year");
        assert_eq!(name_of("-image\n-bigger_than_200b", &f("q.jpg", NOW, 900)), "bigger than 200 bytes");
        assert_eq!(name_of("-png\n-name_has_screenshot", &f("Screenshot 2026-09-30.png", NOW - 9 * D, 1)), "names with \"screenshot\"");
        assert_ne!(name_of("-name_starts_with_img", &f("IMG_2041.jpg", NOW, 1)), "everything else");
        let mut copy = f("copy.txt", NOW - 90 * D, 1);
        copy.created = NOW - H;
        assert_eq!(name_of("-added", &copy), "added in the last 24 hours");
        assert!(errors("-updated_3_dys").iter().any(|m| m.text.contains("3_days")));
        assert!(errors("-bigger_than_lots").iter().any(|m| m.text.contains("100mb")));
        assert!(errors("-documnet_first").iter().any(|m| m.text.contains("-document_first")), "a typo keeps the _first");
        assert!(is_order_word("-biggest_last") && is_order_word("-newest_first_by_day") && !is_order_word("-image_last"));
    }

    #[test]
    fn mistakes_are_explained() {
        let bad = Recipe::parse("-folder\n-newst_first\nin Nowhere Special:\n-name");
        assert!(bad.messages.iter().any(|m| m.line == 2 && m.error && m.text.contains("newest_first")));
        assert!(bad.messages.iter().any(|m| m.line == 3 && m.error));
        assert!(Recipe::parse("-folder\n-.blend\n-zip").messages.is_empty());
    }

    #[test]
    fn rules_for_one_folder() {
        let downloads = place_path("Downloads").unwrap();
        let r = "-a_to_z\n-folder\nin Downloads:\n-newest_first";
        assert_eq!(sorted_in(r, &downloads)[0], "b.exe");
        assert_eq!(sorted_in(r, "C:\\t")[0], "Alpha");
        assert_eq!(Recipe::parse(r).folder_label(&downloads), Some("Downloads"));
        // The colon may be left out; a full path works too.
        assert_eq!(sorted_in("-a_to_z\n-folder\nin C:\\t\n-newest_first", "C:\\T")[0], "b.exe");
    }

    // Friendlier: the same rules, written more freely.

    #[test]
    fn forgiving_words() {
        assert_eq!(order("newest first\nfolders first"), order("-newest_first\n-folder_first"));
        assert!(Recipe::parse("newest first\nfolders first").messages.is_empty());
        assert_eq!(order("date"), order("-newest"));
        assert_eq!(order("size"), order("-biggest"));
        assert_eq!(order("folders\nname"), order("-folder\n-a_to_z"));
        assert_eq!(order("-folder\ntype"), order("-folder\n-by_type"));
        assert_eq!(order("-image biggest"), order("-image -biggest"));
        assert_eq!(order("pictures last, a to z"), order("-image_last -a_to_z"));
        assert_eq!(name_of("bigger than 200 b", &f("q.jpg", NOW, 900)), "bigger than 200 bytes");
        assert_eq!(name_of("updated 3 days", &f("c.pdf", NOW - 3 * D, 1)), "updated in the last 3 days");
        // A name rule takes one word; the next is a rule of its own.
        let r = Recipe::parse("name has screenshot newest");
        assert!(r.messages.is_empty(), "{:?}", r.messages);
        assert_eq!(r.group_heading(&f("Screenshot 1.png", NOW, 1), "C:\\t", &clock()), "names with \"screenshot\"  \u{00b7}  newest first");
    }

    #[test]
    fn typos_say_where_and_how_to_fix() {
        let text = "-folder\nnewst";
        let r = Recipe::parse(text);
        let m = r.messages.iter().find(|m| m.error).unwrap();
        let (a, b) = m.span.unwrap();
        assert_eq!(&text[a..b], "newst");
        assert_eq!(m.fix.as_deref(), Some("newest"));
        assert!(m.text.contains("Did you mean -newest?"), "{}", m.text);
        // The ending written apart goes with it.
        let text = "-folder\nnewst first";
        let r = Recipe::parse(text);
        let errors: Vec<&Message> = r.messages.iter().filter(|m| m.error).collect();
        assert_eq!(errors.len(), 1, "{errors:?}");
        let (a, b) = errors[0].span.unwrap();
        assert_eq!(&text[a..b], "newst first");
        assert_eq!(errors[0].fix.as_deref(), Some("newest_first"));
        let text = "-folder -imgae_last";
        let m = Recipe::parse(text).messages.into_iter().find(|m| m.error).unwrap();
        assert_eq!(&text[m.span.unwrap().0..m.span.unwrap().1], "-imgae_last");
        assert_eq!(m.fix.as_deref(), Some("-image_last"));
    }

    #[test]
    fn notes_and_dates() {
        assert_eq!(strip_note("-folder  # folders"), "-folder  ");
        assert_eq!(strip_note("-name_has_c#"), "-name_has_c#");
        assert_eq!(strip_note("// all of it"), "");
        assert_eq!(year_month(clock().day(NOW)), (2026, 10));
        // 1 January 1601 was a Monday: weeks start on Mondays.
        let monday = clock().day(NOW) - 2; // 7 October 2026 is a Wednesday
        assert_eq!(monday.div_euclid(7), clock().day(NOW).div_euclid(7));
        assert_ne!((monday - 1).div_euclid(7), monday.div_euclid(7));
    }
}
