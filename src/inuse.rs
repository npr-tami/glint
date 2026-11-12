//! Which programs have a file open, so "it's open in another program" can name them:
//! "“photo.jpg” is open in Paint." Windows' Restart Manager knows (the same thing installers use
//! to say "close these programs first"). For a folder, the files in it are asked about (at most
//! [`MOST_FILES`]), since a folder is "in use" when something inside it is open.
//!
//! Asking takes a moment (tens of milliseconds), so it's only done after something has already
//! failed because an item was in use, never before.

use crate::folder::wide_path;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::System::RestartManager::{RmEndSession, RmGetList, RmRegisterResources, RmStartSession, CCH_RM_SESSION_KEY, RM_PROCESS_INFO};

/// The most files of a folder that are asked about.
const MOST_FILES: usize = 2000;

/// The ERROR_MORE_DATA answer of RmGetList: the list didn't fit.
const ERROR_MORE_DATA: u32 = 234;

/// The names of the programs that have `path` (or, for a folder, something in it) open, each once,
/// in the order Windows gives them. Empty when Windows can't tell (or nothing has it open).
pub fn programs(path: &str) -> Vec<String> {
    let files = files_of(path);
    if files.is_empty() {
        return Vec::new();
    }
    let wides: Vec<Vec<u16>> = files.iter().map(|f| wide_path(f)).collect();
    let ptrs: Vec<PCWSTR> = wides.iter().map(|w| PCWSTR(w.as_ptr())).collect();
    let mut session = 0u32;
    let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
    // SAFETY: `key` has room for the session key and its NUL; `session` is written by the call.
    if unsafe { RmStartSession(&mut session, None, PWSTR(key.as_mut_ptr())) }.0 != 0 {
        return Vec::new();
    }
    let mut names: Vec<String> = Vec::new();
    // SAFETY: the strings in `ptrs` point into `wides`, which outlives the calls; the session is
    // ended below whatever happens.
    unsafe {
        if RmRegisterResources(session, Some(&ptrs), None, None).0 == 0 {
            let mut infos: Vec<RM_PROCESS_INFO> = vec![RM_PROCESS_INFO::default(); 8];
            // A program may open the file between the two calls: a few tries.
            for _ in 0..3 {
                let (mut needed, mut count, mut reasons) = (0u32, infos.len() as u32, 0u32);
                let r = RmGetList(session, &mut needed, &mut count, Some(infos.as_mut_ptr()), &mut reasons).0;
                if r == ERROR_MORE_DATA {
                    infos = vec![RM_PROCESS_INFO::default(); needed as usize + 4];
                    continue;
                }
                if r == 0 {
                    for info in &infos[..(count as usize).min(infos.len())] {
                        let name = app_name(info);
                        if !name.is_empty() && !names.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                            names.push(name);
                        }
                    }
                }
                break;
            }
        }
        let _ = RmEndSession(session);
    }
    names
}

/// A program's name as Windows shows it ("Paint", "Microsoft Word"); its service name for a
/// service without one.
fn app_name(info: &RM_PROCESS_INFO) -> String {
    let text = |w: &[u16]| String::from_utf16_lossy(&w[..w.iter().position(|&c| c == 0).unwrap_or(w.len())]).trim().to_string();
    let name = text(&info.strAppName);
    if name.is_empty() { text(&info.strServiceShortName) } else { name }
}

/// The files to ask about: the file itself, or the files in a folder and its subfolders (at most
/// MOST_FILES, the nearest first).
fn files_of(path: &str) -> Vec<String> {
    let Ok(meta) = std::fs::metadata(path) else { return Vec::new() };
    if !meta.is_dir() {
        return vec![path.to_string()];
    }
    let mut files = Vec::new();
    let mut dirs = std::collections::VecDeque::from([std::path::PathBuf::from(path)]);
    while let Some(dir) = dirs.pop_front() {
        let Ok(items) = std::fs::read_dir(&dir) else { continue };
        for item in items.flatten() {
            let Ok(kind) = item.file_type() else { continue };
            if kind.is_symlink() {
                continue; // (a junction could lead anywhere, or round in a circle)
            }
            if kind.is_dir() {
                dirs.push_back(item.path());
            } else {
                files.push(item.path().to_string_lossy().into_owned());
                if files.len() >= MOST_FILES {
                    return files;
                }
            }
        }
    }
    files
}

/// "Paint", "Paint and Word", "Paint, Word and Excel" (and "3 other programs" past three).
pub fn list_text(programs: &[String]) -> String {
    match programs {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        [a, b, c] => format!("{a}, {b} and {c}"),
        [a, b, rest @ ..] => format!("{a}, {b} and {} other programs", rest.len()),
    }
}

/// The end of a "Failed to …" line for an item that is open somewhere: "“photo.jpg” is open in
/// Paint. Close it there and try again." - or None when Windows can't say which program.
pub fn open_in(path: &str) -> Option<String> {
    let found = programs(path);
    if found.is_empty() {
        return None;
    }
    let name = crate::folder::file_name(path);
    let is_folder = std::fs::metadata(path).is_ok_and(|m| m.is_dir());
    Some(if is_folder {
        format!("Something in \u{201c}{name}\u{201d} is open in {}. Close it there and try again.", list_text(&found))
    } else {
        format!("\u{201c}{name}\u{201d} is open in {}. Close it there and try again.", list_text(&found))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("glint-test-inuse-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_list_reads_like_a_sentence() {
        let s = |v: &[&str]| list_text(&v.iter().map(|x| x.to_string()).collect::<Vec<_>>());
        assert_eq!(s(&[]), "");
        assert_eq!(s(&["Paint"]), "Paint");
        assert_eq!(s(&["Paint", "Word"]), "Paint and Word");
        assert_eq!(s(&["Paint", "Word", "Excel"]), "Paint, Word and Excel");
        assert_eq!(s(&["Paint", "Word", "Excel", "Notepad", "VLC"]), "Paint, Word and 3 other programs");
    }

    #[test]
    fn nothing_is_said_about_a_file_nobody_has_open_or_that_is_gone() {
        let d = temp("free");
        let f = d.join("free.txt");
        std::fs::write(&f, "x").unwrap();
        assert_eq!(open_in(&f.to_string_lossy()), None);
        assert_eq!(open_in(&d.join("gone.txt").to_string_lossy()), None);
        assert!(programs("").is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_folder_asks_about_its_files_in_subfolders_too_up_to_a_limit() {
        let d = temp("tree");
        std::fs::create_dir_all(d.join("a").join("b")).unwrap();
        std::fs::write(d.join("top.txt"), "x").unwrap();
        std::fs::write(d.join("a").join("b").join("deep.txt"), "x").unwrap();
        let mut files = files_of(&d.to_string_lossy());
        files.sort();
        assert_eq!(files.len(), 2, "{files:?}");
        assert!(files.iter().any(|f| f.ends_with("deep.txt")));
        assert_eq!(files_of(&d.join("top.txt").to_string_lossy()).len(), 1);
        assert!(files_of(&d.join("nothing").to_string_lossy()).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A file this test itself holds open is reported as open in this program (on real Windows;
    /// under Wine the Restart Manager isn't there, and nothing is said, which is fine too).
    #[test]
    fn a_file_held_open_is_named_or_left_unsaid() {
        let d = temp("held");
        let f = d.join("held.txt");
        let mut file = std::fs::File::create(&f).unwrap();
        file.write_all(b"x").unwrap();
        let said = open_in(&f.to_string_lossy());
        if let Some(said) = said {
            assert!(said.starts_with("\u{201c}held.txt\u{201d} is open in "), "{said}");
            assert!(said.ends_with(". Close it there and try again."), "{said}");
        }
        drop(file);
        let _ = std::fs::remove_dir_all(&d);
    }
}
