//! "Setup info" (Settings > About > COPY): a few lines about this Glint, Windows and the settings that
//! change how things behave, put on the clipboard to paste into a bug report. Never a file or
//! folder name, a pinned place or a person's own skin's name: only Glint's own words and numbers.
//!
//! Glint 1.0.24-beta.5
//! Windows 11 24H2 (build 26100.2314)
//! Skin: AMD Radeon, dark
//! ...

use serde_json::{Map, Value};

use crate::settings::{self, ViewMode};

/// What the person's machine says, gathered separately so the text can be tested.
pub struct Machine {
    /// "Windows 11 24H2 (build 26100.2314)".
    pub windows: String,
    /// "2, main one at 150%".
    pub screens: String,
    pub portable: bool,
    /// "Open folders and drives in Glint" and "Win+E opens Glint", as the registry says.
    pub default_explorer: bool,
    pub win_e: bool,
    /// How many errors the error log holds, and when the newest was ("2026-11-12 16:02").
    pub errors: (usize, Option<String>),
}

/// The lines to copy. `skins`: every skin's id and name (Settings' list), to name a built-in one.
pub fn text(values: &Map<String, Value>, skins: &[(String, String, String)], m: &Machine) -> String {
    let s = |k: &str| values.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let b = |k: &str, default: bool| values.get(k).and_then(Value::as_bool).unwrap_or(default);
    let on = |x: bool| if x { "on" } else { "off" };
    let skin_id = s("Skin");
    let skin = if skin_id.starts_with("file:") {
        "their own".to_string()
    } else {
        skins.iter().find(|(id, _, _)| *id == skin_id).map_or_else(|| if skin_id.is_empty() { "AMD Radeon".to_string() } else { skin_id.clone() }, |(_, name, _)| name.clone())
    };
    let theme = match s("Theme").as_str() {
        "light" => "light",
        "dark" => "dark",
        _ => "as Windows",
    };
    let view = ViewMode::ALL.iter().find(|(v, _)| v.key() == s("View")).map_or("Details", |(_, n)| n);
    let sort_number = values.get("SortColumn").and_then(Value::as_u64).unwrap_or(0) as u32;
    let sort = match settings::sort_key(sort_number) {
        crate::folder::SortKey::Name => "Name",
        crate::folder::SortKey::Date => "Date modified",
        crate::folder::SortKey::Type => "Type",
        crate::folder::SortKey::Size => "Size",
        crate::folder::SortKey::Created => "Date created",
        crate::folder::SortKey::Dimensions => "Picture size",
        crate::folder::SortKey::Length => "Length",
        crate::folder::SortKey::Folder => "Folder",
    };
    let order = if b("SortDescending", false) { "reversed" } else { "normal order" };
    let custom = if b("CustomSort", false) {
        if values.get("SortRecipe").is_some_and(|v| v.is_string()) { "on, own rules" } else { "on, starter rules" }
    } else {
        "off"
    };
    let startup = match s("Startup").as_str() {
        "restore" => "Tabs from last time",
        "thispc" => "This PC",
        "folder" => "A specific folder",
        _ => "Home folder",
    };
    let errors = match &m.errors {
        (0, _) => "none".to_string(),
        (n, Some(when)) => format!("{n}, newest {when}"),
        (n, None) => n.to_string(),
    };
    [
        format!("Glint {}", crate::log::VERSION),
        m.windows.clone(),
        format!("Screens: {}", m.screens),
        format!("Portable: {}", if m.portable { "yes" } else { "no" }),
        format!("Skin: {skin}, {theme}"),
        format!("View: {view}, sorted by {sort}, {order}"),
        format!("Custom order: {custom}"),
        format!("Custom views: {}", on(b("CustomViews", true))),
        format!("Open Glint to: {startup}"),
        format!("Hidden files: {}", if b("ShowHidden", false) { "shown" } else { "hidden" }),
        format!("Default file explorer: {}", on(m.default_explorer)),
        format!("Win+E opens Glint: {}", on(m.win_e)),
        format!("Errors logged: {errors}"),
    ]
    .join("\r\n")
}

/// What this machine says now (registry, screens, the error log).
pub fn machine() -> Machine {
    Machine {
        windows: windows_text(),
        screens: screens_text(),
        portable: settings::portable_now(),
        default_explorer: crate::life::default_explorer_on(),
        win_e: crate::life::win_e_on(),
        errors: errors_in(&std::fs::read_to_string(crate::log::error_log_path()).unwrap_or_default()),
    }
}

/// How many entries an error log holds (each starts "YYYY-MM-DD HH:MM:SS  Glint"), and the newest
/// one's date and time, to the minute.
pub fn errors_in(log: &str) -> (usize, Option<String>) {
    let starts: Vec<&str> = log
        .lines()
        .filter(|l| {
            let b = l.as_bytes();
            b.len() > 19 && b[4] == b'-' && b[7] == b'-' && b[10] == b' ' && b[13] == b':' && b[..4].iter().all(u8::is_ascii_digit) && l[19..].trim_start().starts_with("Glint")
        })
        .collect();
    (starts.len(), starts.last().map(|l| l[..16].to_string()))
}

/// "Windows 11 24H2 (build 26100.2314)". Windows 11 still calls itself 10 in places, so the build
/// number decides: 22000 and up is 11.
fn windows_text() -> String {
    let key = windows::core::w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion");
    let build = read_string(key, windows::core::w!("CurrentBuildNumber")).unwrap_or_default();
    let release = read_string(key, windows::core::w!("DisplayVersion")).or_else(|| read_string(key, windows::core::w!("ReleaseId"))).unwrap_or_default();
    let ubr = read_dword(key, windows::core::w!("UBR"));
    windows_line(&build, &release, ubr)
}

/// The Windows line from the registry's values.
pub fn windows_line(build: &str, release: &str, ubr: Option<u32>) -> String {
    let n: u32 = build.trim().parse().unwrap_or(0);
    let name = if n >= 22000 { "Windows 11" } else if n > 0 { "Windows 10" } else { "Windows" };
    let full = match (n, ubr) {
        (0, _) => String::from("unknown"),
        (_, Some(u)) => format!("{n}.{u}"),
        (_, None) => n.to_string(),
    };
    let release = release.trim();
    if release.is_empty() { format!("{name} (build {full})") } else { format!("{name} {release} (build {full})") }
}

fn read_string(key: windows::core::PCWSTR, name: windows::core::PCWSTR) -> Option<String> {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    let mut buf = vec![0u16; 256];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: NUL-terminated key and value names; `buf` and `size` are a valid buffer and its size.
    let read = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key, name, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut size)) };
    if read.is_err() {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

fn read_dword(key: windows::core::PCWSTR, name: windows::core::PCWSTR) -> Option<u32> {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD};
    let mut v = 0u32;
    let mut size = 4u32;
    // SAFETY: NUL-terminated names; `v` is a 4-byte buffer.
    let read = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key, name, RRF_RT_REG_DWORD, None, Some(&mut v as *mut u32 as *mut _), Some(&mut size)) };
    read.is_ok().then_some(v)
}

/// "2, main one at 150%".
fn screens_text() -> String {
    use windows::Win32::UI::HiDpi::GetDpiForSystem;
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CMONITORS};
    // SAFETY: plain queries without arguments to check.
    let (count, dpi) = unsafe { (GetSystemMetrics(SM_CMONITORS), GetDpiForSystem()) };
    format!("{}, main one at {}%", count.max(1), (dpi.max(96) * 100 + 48) / 96)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine() -> Machine {
        Machine { windows: windows_line("26100", "24H2", Some(2314)), screens: "2, main one at 100%".into(), portable: false, default_explorer: true, win_e: false, errors: (0, None) }
    }

    fn values(json: &str) -> Map<String, Value> {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn windows_11_is_told_from_10_by_its_build() {
        assert_eq!(windows_line("26100", "24H2", Some(2314)), "Windows 11 24H2 (build 26100.2314)");
        assert_eq!(windows_line("22000", "", None), "Windows 11 (build 22000)");
        assert_eq!(windows_line("19045", "22H2", Some(5011)), "Windows 10 22H2 (build 19045.5011)");
        assert_eq!(windows_line("", "", None), "Windows (build unknown)");
        assert_eq!(windows_line("junk", "x", Some(1)), "Windows x (build unknown)");
    }

    #[test]
    fn the_text_says_the_settings_that_change_behaviour_in_plain_words() {
        let skins = vec![("gunmetal".to_string(), "Gunmetal".to_string(), String::new())];
        let v = values(r#"{"Skin":"gunmetal","Theme":"dark","View":"details","SortColumn":7,"SortDescending":true,"CustomSort":true,"Startup":"restore","ShowHidden":true}"#);
        let t = text(&v, &skins, &machine());
        let lines: Vec<&str> = t.split("\r\n").collect();
        assert_eq!(lines[0], format!("Glint {}", crate::log::VERSION));
        assert!(lines.contains(&"Windows 11 24H2 (build 26100.2314)"), "{t}");
        assert!(lines.contains(&"Skin: Gunmetal, dark"), "{t}");
        assert!(lines.contains(&"View: Details, sorted by Length, reversed"), "{t}");
        assert!(lines.contains(&"Custom order: on, starter rules"), "{t}");
        assert!(lines.contains(&"Open Glint to: Tabs from last time"), "{t}");
        assert!(lines.contains(&"Hidden files: shown"), "{t}");
        assert!(lines.contains(&"Default file explorer: on") && lines.contains(&"Win+E opens Glint: off"), "{t}");
        assert!(lines.contains(&"Errors logged: none"), "{t}");
    }

    #[test]
    fn no_file_folder_or_own_skin_names_get_in() {
        let v = values(
            r#"{"Skin":"file:Moonbeam Secret","SortRecipe":"in C:\\Users\\someone\\Private ( name_first )","CustomSort":true,"StartupFolder":"C:\\Users\\someone\\Secret","Startup":"folder",
               "Pinned":["D:\\Hidden stuff"],"Favorites":["C:\\x\\diary.txt"],"Tabs":[{"Path":"C:\\Users\\someone\\Videos"}]}"#,
        );
        let mut m = machine();
        m.errors = errors_in("2026-11-12 16:02:11  Glint 1.0.24  error: Failed to read C:\\Users\\someone\\x.txt\n    at src/x.rs:1\n\n2026-11-12 16:05:00  Glint 1.0.24  panic: oh\n    at y\n");
        let t = text(&v, &[], &m);
        for secret in ["Moonbeam", "someone", "Secret", "Hidden stuff", "diary", "Videos", "Private", "x.txt", "src/"] {
            assert!(!t.contains(secret), "{secret} in:\n{t}");
        }
        assert!(t.contains("Skin: their own") && t.contains("Custom order: on, own rules") && t.contains("Open Glint to: A specific folder"), "{t}");
        assert!(t.contains("Errors logged: 2, newest 2026-11-12 16:05"), "{t}");
    }

    #[test]
    fn empty_settings_and_odd_logs_still_give_every_line() {
        let t = text(&Map::new(), &[], &machine());
        assert_eq!(t.split("\r\n").count(), 13, "{t}");
        assert!(t.contains("Skin: AMD Radeon, as Windows") && t.contains("View: Details, sorted by Name, normal order") && t.contains("Custom order: off"), "{t}");
        assert_eq!(errors_in(""), (0, None));
        assert_eq!(errors_in("nonsense\n2026-xx\n    at 2026-11-12 16:02:11  Glint"), (0, None));
        assert_eq!(errors_in("2026-11-12 16:02:11  Glint 1  error: a\n"), (1, Some("2026-11-12 16:02".into())));
    }
}
