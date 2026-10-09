//! Small doors into Windows itself: opening files in their app, File Explorer, the clipboard, the
//! user's known folders (Desktop, Downloads...) - and starting background threads, which most of
//! these need. Also "Open with", the terminal, and notes from background jobs for the status bar.

use windows::core::{GUID, PCWSTR};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ};
use windows::Win32::UI::Shell::{
    SHGetKnownFolderPath, ShellExecuteW, FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music,
    FOLDERID_Pictures, FOLDERID_Profile, FOLDERID_Videos, KF_FLAG_DONT_VERIFY,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::folder::{parent, wide};
// For the notes, "Open with", the terminal and Explorer with several items (below `copy_text`).
use std::sync::Mutex;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::Storage::FileSystem::{GetFileAttributesW, SearchPathW, INVALID_FILE_ATTRIBUTES};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    SHOpenFolderAndSelectItems, SHOpenWithDialog, SHParseDisplayName, FOLDERID_LocalAppData, FOLDERID_ProgramFiles, FOLDERID_System, OAIF_ALLOW_REGISTRATION,
    OAIF_EXEC, OPENASINFO,
};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
// "shell:" places typed in the path bar (`shell_folder`).
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::{
    IKnownFolderManager, KnownFolderManager, FOLDERID_CommonPrograms, FOLDERID_CommonStartMenu, FOLDERID_CommonStartup, FOLDERID_Favorites,
    FOLDERID_Fonts, FOLDERID_ProgramData, FOLDERID_Programs, FOLDERID_Recent, FOLDERID_RoamingAppData, FOLDERID_SendTo,
    FOLDERID_StartMenu, FOLDERID_Startup, FOLDERID_Templates, FOLDERID_Windows,
};

use crate::folder::file_name;
use crate::log;
use crate::settings::Terminal;

/// Runs `job` on a new background thread. Returns false (and drops the job) if Windows can't give
/// us a thread right now - the caller carries on without it instead of the whole app stopping.
pub fn background(name: &str, job: impl FnOnce() + Send + 'static) -> bool {
    std::thread::Builder::new().name(name.to_string()).spawn(job).is_ok()
}

/// COM for a background thread (the shell needs it for icons, type names and opening files),
/// released when dropped. Make one at the top of the thread.
pub struct ComGuard(bool);

impl ComGuard {
    pub fn init() -> ComGuard {
        // SAFETY: pairs with CoUninitialize in Drop, on the same thread.
        ComGuard(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }.is_ok())
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: matches the successful CoInitializeEx in `init`.
            unsafe { CoUninitialize() };
        }
    }
}

/// Opens a file with its app (like double-clicking it in Explorer). Runs on its own thread, so a
/// slow app start or an "open with" question never freezes Glint.
pub fn open(path: &str) {
    let path = path.to_string();
    background("open file", move || {
        let _com = ComGuard::init();
        let dir = parent(&path).unwrap_or_default();
        let (file_w, dir_w, verb) = (wide(&path), wide(&dir), wide("open"));
        // SAFETY: all strings are NUL-terminated and live across the call.
        unsafe {
            ShellExecuteW(None, PCWSTR(verb.as_ptr()), PCWSTR(file_w.as_ptr()), PCWSTR::null(), PCWSTR(dir_w.as_ptr()), SW_SHOWNORMAL);
        }
    });
}

/// Opens File Explorer at `path` - with the item selected when `select` is true.
pub fn show_in_explorer(path: &str, select: bool) {
    let args = if path.is_empty() {
        String::new() // This PC: plain Explorer
    } else if select {
        format!("/select,\"{path}\"")
    } else {
        format!("\"{path}\"")
    };
    background("show in Explorer", move || {
        // ShellExecute needs COM on the calling thread.
        let _com = ComGuard::init();
        let (exe, args_w, verb) = (wide("explorer.exe"), wide(&args), wide("open"));
        // SAFETY: all strings are NUL-terminated and live across the call.
        unsafe {
            ShellExecuteW(None, PCWSTR(verb.as_ptr()), PCWSTR(exe.as_ptr()), PCWSTR(args_w.as_ptr()), PCWSTR::null(), SW_SHOWNORMAL);
        }
    });
}

/// Puts text on the clipboard. Returns false if the clipboard was busy.
pub fn copy_text(owner: HWND, text: &str) -> bool {
    let data = wide(text);
    let bytes = data.len() * 2;
    // SAFETY: the clipboard is opened and always closed; the global memory block is either handed
    // to the clipboard (which then owns it) or freed by us.
    unsafe {
        if OpenClipboard(Some(owner)).is_err() {
            return false;
        }
        let mut ok = false;
        if EmptyClipboard().is_ok() {
            if let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, bytes) {
                let dst = GlobalLock(mem) as *mut u16;
                if !dst.is_null() {
                    std::ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len());
                    let _ = GlobalUnlock(mem);
                    ok = SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(mem.0))).is_ok();
                }
                if !ok {
                    let _ = GlobalFree(Some(HGLOBAL(mem.0)));
                }
            }
        }
        let _ = CloseClipboard();
        ok
    }
}

// ------------------------------------------------------------ notes from background jobs

/// Posted to the main window when a background job has a note for the status bar (see [`tell`]).
pub const WM_APP_NOTE: u32 = WM_APP + 21;

static NOTES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// From any thread: a short note for the status bar ("Couldn't open a terminal here"). The window
/// shows it when it gets WM_APP_NOTE (see [`take_notes`]).
pub fn tell(owner: HWND, text: String) {
    NOTES.lock().unwrap_or_else(|e| e.into_inner()).push(text);
    // SAFETY: posting to a window handle is safe even if the window is gone (the call just fails).
    unsafe {
        let _ = PostMessageW(Some(owner), WM_APP_NOTE, WPARAM(0), LPARAM(0));
    }
}

/// The notes waiting for the status bar, oldest first.
pub fn take_notes() -> Vec<String> {
    std::mem::take(&mut *NOTES.lock().unwrap_or_else(|e| e.into_inner()))
}

/// A window handle that can travel to another thread (there it's only ever posted to or used as
/// the owner of a Windows dialog, both safe even if the window is gone).
#[derive(Clone, Copy, Debug)]
pub struct Owner(isize);

impl Owner {
    pub fn new(hwnd: HWND) -> Owner {
        Owner(hwnd.0 as isize)
    }
    pub fn hwnd(self) -> HWND {
        HWND(self.0 as *mut _)
    }
}

// ------------------------------------------------------------ Open with, Explorer, terminal

/// Windows' "Open with" window for one file, on its own thread (it waits for the person). The
/// app picked opens the file. `owner` gets a note if Windows can't show it.
pub fn open_with(owner: Owner, path: &str) {
    let path = path.to_string();
    let started = background("open with", move || {
        let _com = ComGuard::init();
        let file = wide(&path);
        let info = OPENASINFO { pcszFile: PCWSTR(file.as_ptr()), pcszClass: PCWSTR::null(), oaifInFlags: OAIF_ALLOW_REGISTRATION | OAIF_EXEC };
        // SAFETY: `file` is NUL-terminated and outlives the call.
        let shown = unsafe { SHOpenWithDialog(Some(owner.hwnd()), &info) };
        if let Err(e) = shown {
            // 0x800704C7: the person closed it without choosing.
            if e.code().0 as u32 != 0x800704C7 {
                log::line(&format!("open with {path:?}: {e}"));
                // The older way, through the "openas" command (older Windows, Wine).
                let verb = wide("openas");
                // SAFETY: as in `open`.
                let r = unsafe {
                    ShellExecuteW(Some(owner.hwnd()), PCWSTR(verb.as_ptr()), PCWSTR(file.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL)
                };
                if r.0 as isize <= 32 {
                    tell(owner.hwnd(), "Windows couldn't show the \u{201c}Open with\u{201d} window.".to_string());
                }
            }
        }
    });
    if !started {
        tell(owner.hwnd(), "Windows couldn't start \u{201c}Open with\u{201d} right now.".to_string());
    }
}

/// Opens File Explorer with these items (all in one folder) selected - all of them, where
/// `explorer /select` takes only one. Falls back to `show_in_explorer` for the first.
pub fn show_items_in_explorer(paths: Vec<String>) {
    let Some(first) = paths.first().cloned() else { return };
    if paths.len() == 1 {
        show_in_explorer(&first, true);
        return;
    }
    background("show in Explorer", move || {
        let _com = ComGuard::init();
        if !open_folder_and_select(&paths) {
            show_in_explorer(&first, true);
        }
    });
}

/// SHOpenFolderAndSelectItems with the items' folder and their ids.
fn open_folder_and_select(paths: &[String]) -> bool {
    let Some(dir) = parent(&paths[0]) else { return false };
    let parse = |p: &str| -> Option<*mut ITEMIDLIST> {
        let w = wide(p);
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        // SAFETY: `w` outlives the call; `pidl` is a valid out-pointer.
        unsafe { SHParseDisplayName(PCWSTR(w.as_ptr()), None, &mut pidl, 0, None) }.ok()?;
        (!pidl.is_null()).then_some(pidl)
    };
    let Some(folder) = parse(&dir) else { return false };
    let items: Vec<*mut ITEMIDLIST> = paths.iter().take(1000).filter_map(|p| parse(p)).collect();
    let consts: Vec<*const ITEMIDLIST> = items.iter().map(|&p| p as *const _).collect();
    // SAFETY: valid absolute id lists (Windows accepts those for the items too); freed below.
    let ok = unsafe { SHOpenFolderAndSelectItems(folder, Some(&consts), 0) }.is_ok();
    // SAFETY: each id list came from SHParseDisplayName and is freed once.
    unsafe {
        for p in items {
            CoTaskMemFree(Some(p as *const _));
        }
        CoTaskMemFree(Some(folder as *const _));
    }
    ok
}

/// Opens a terminal in `dir`, on its own thread (looking for the terminal can touch the network
/// folders on PATH): the one the "Terminal" setting names, or with "Automatic" Windows Terminal if
/// it's installed, otherwise Windows PowerShell. A chosen terminal that isn't installed falls back
/// to the automatic one, with a note. `owner` gets a note if it all fails.
pub fn open_terminal(owner: Owner, dir: &str, choice: Terminal) {
    let dir = dir.to_string();
    let started = background("open terminal", move || {
        // A folder that's gone (or a drive that isn't there) would give a terminal somewhere else.
        if !exists(&dir) {
            tell(owner.hwnd(), format!("Couldn't open a terminal: \u{201c}{}\u{201d} can't be reached.", file_name(&dir)));
            return;
        }
        let found = match terminal_command(&dir, choice) {
            Some(c) => Some(c),
            None if choice != Terminal::Auto => {
                let fallback = terminal_command(&dir, Terminal::Auto);
                if fallback.is_some() {
                    tell(owner.hwnd(), format!("{} wasn't found, so the usual terminal opened instead.", choice.name()));
                }
                fallback
            }
            None => None,
        };
        let Some((exe, args, name)) = found else {
            tell(owner.hwnd(), "Couldn't find Windows Terminal or PowerShell.".to_string());
            return;
        };
        if let Err(e) = start_process(&exe, &args, &dir) {
            log::line(&format!("open terminal: {exe} {args} in {dir:?}: {e}"));
            tell(owner.hwnd(), format!("Couldn't open {name}: {}", e.message()));
        }
    });
    if !started {
        tell(owner.hwnd(), "Windows couldn't open a terminal right now.".to_string());
    }
}

/// Whether this terminal is installed (the Settings window says "(not found)" after the others).
/// Looks at the disk and PATH, which can be slow: call it on a background thread.
pub fn terminal_installed(choice: Terminal) -> bool {
    terminal_command("C:\\", choice).is_some()
}

/// The program, its arguments and its name for `choice` in `dir`. None when it isn't installed.
/// Every terminal also gets `dir` as its working folder (see `start_process`).
fn terminal_command(dir: &str, choice: Terminal) -> Option<(String, String, &'static str)> {
    let system = || known_folder(&FOLDERID_System);
    let local = || known_folder(&FOLDERID_LocalAppData);
    let first = |paths: Vec<Option<String>>| paths.into_iter().flatten().find(|p| exists(p));
    match choice {
        Terminal::Auto => terminal_command(dir, Terminal::Wt).or_else(|| terminal_command(dir, Terminal::PowerShell)),
        // wt reads ';' as "next command", so it's escaped.
        Terminal::Wt => find_windows_terminal().map(|wt| (wt, format!("-d {}", quote_arg(&dir.replace(';', "\\;"))), "Windows Terminal")),
        Terminal::PowerShell => first(vec![system().map(|s| format!("{s}\\WindowsPowerShell\\v1.0\\powershell.exe"))])
            .or_else(|| search_path("powershell.exe"))
            .map(|ps| (ps, String::new(), "PowerShell")),
        Terminal::Pwsh => search_path("pwsh.exe")
            .or_else(|| {
                first(vec![
                    known_folder(&FOLDERID_ProgramFiles).map(|p| format!("{p}\\PowerShell\\7\\pwsh.exe")),
                    local().map(|l| format!("{l}\\Microsoft\\WindowsApps\\pwsh.exe")),
                ])
            })
            .map(|p| (p, String::new(), "PowerShell 7")),
        Terminal::Cmd => first(vec![system().map(|s| format!("{s}\\cmd.exe"))]).or_else(|| search_path("cmd.exe")).map(|c| (c, String::new(), "Command Prompt")),
        Terminal::GitBash => first(vec![
            git_install_path().map(|g| format!("{}\\git-bash.exe", g.trim_end_matches('\\'))),
            known_folder(&FOLDERID_ProgramFiles).map(|p| format!("{p}\\Git\\git-bash.exe")),
            local().map(|l| format!("{l}\\Programs\\Git\\git-bash.exe")),
        ])
        .map(|g| (g, quote_arg(&format!("--cd={dir}")), "Git Bash")),
    }
}

/// Where Git for Windows is installed, as its installer writes it in the registry.
fn git_install_path() -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: the key and value names are NUL-terminated constants; `buf` and `size` are a valid
    // out-buffer and its true size in bytes.
    let read = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            windows::core::w!("SOFTWARE\\GitForWindows"),
            windows::core::w!("InstallPath"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    if read.is_err() {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len])).filter(|p| !p.is_empty())
}

/// Windows Terminal's start command (an "app execution alias" in the user's WindowsApps folder),
/// or wt.exe found on PATH.
fn find_windows_terminal() -> Option<String> {
    known_folder(&FOLDERID_LocalAppData)
        .map(|l| format!("{l}\\Microsoft\\WindowsApps\\wt.exe"))
        .filter(|p| exists(p))
        .or_else(|| search_path("wt.exe"))
}

fn exists(path: &str) -> bool {
    let w = wide(path);
    // SAFETY: `w` is NUL-terminated.
    unsafe { GetFileAttributesW(PCWSTR(w.as_ptr())) != INVALID_FILE_ATTRIBUTES }
}

/// A program on PATH (or in Windows' own folders), with its full path.
fn search_path(exe: &str) -> Option<String> {
    let name = wide(exe);
    let mut buf = vec![0u16; 1024];
    // SAFETY: `name` is NUL-terminated; Windows writes at most buf.len() characters.
    let n = unsafe { SearchPathW(PCWSTR::null(), PCWSTR(name.as_ptr()), PCWSTR::null(), Some(&mut buf), None) } as usize;
    (n > 0 && n < buf.len()).then(|| String::from_utf16_lossy(&buf[..n]))
}

/// Starts `exe` with `args`, working in folder `dir`. Through ShellExecute, which also knows
/// Windows Terminal's "app execution alias" and gives a console program a window of its own.
fn start_process(exe: &str, args: &str, dir: &str) -> windows::core::Result<()> {
    let (exe_w, args_w, dir_w, verb) = (wide(exe), wide(args), wide(dir), wide("open"));
    // SAFETY: all strings are NUL-terminated and outlive the call.
    let r = unsafe {
        ShellExecuteW(None, PCWSTR(verb.as_ptr()), PCWSTR(exe_w.as_ptr()), PCWSTR(args_w.as_ptr()), PCWSTR(dir_w.as_ptr()), SW_SHOWNORMAL)
    };
    // Above 32 means it started; otherwise the reason is in GetLastError.
    if r.0 as isize > 32 {
        Ok(())
    } else {
        Err(windows::core::Error::from_thread())
    }
}

/// One command-line argument, quoted the way Windows programs read them back
/// (CommandLineToArgvW): in quotes when needed, with backslashes before a quote doubled.
pub fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    // Backslashes before the closing quote are doubled, so "C:\" stays C:\ and not C:".
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::quote_arg;

    #[test]
    fn arguments_are_quoted_like_windows_reads_them() {
        assert_eq!(quote_arg("C:\\Users"), "C:\\Users");
        assert_eq!(quote_arg("C:\\My Files"), "\"C:\\My Files\"");
        // A trailing backslash would otherwise escape the closing quote.
        assert_eq!(quote_arg("C:\\My Files\\"), "\"C:\\My Files\\\\\"");
        assert_eq!(quote_arg("a \"b\""), "\"a \\\"b\\\"\"");
        assert_eq!(quote_arg("x\\\"y"), "\"x\\\\\\\"y\"");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("\\\\server\\share x"), "\"\\\\server\\share x\"");
    }
}

/// One of the user's own folders.
#[derive(Clone, Debug)]
pub struct KnownFolder {
    /// "Home", "Desktop", "Documents", "Downloads", "Pictures", "Music", "Videos".
    pub name: &'static str,
    pub path: String,
}

/// The user's folders for Quick access, in the usual order. Folders Windows doesn't know are left out.
pub fn known_folders() -> Vec<KnownFolder> {
    const LIST: [(&str, GUID); 7] = [
        ("Home", FOLDERID_Profile),
        ("Desktop", FOLDERID_Desktop),
        ("Documents", FOLDERID_Documents),
        ("Downloads", FOLDERID_Downloads),
        ("Pictures", FOLDERID_Pictures),
        ("Music", FOLDERID_Music),
        ("Videos", FOLDERID_Videos),
    ];
    LIST.iter().filter_map(|(name, id)| known_folder(id).map(|path| KnownFolder { name, path })).collect()
}

/// The path of one known folder (e.g. `FOLDERID_Downloads`). Only the path is looked up: by default
/// Windows would also check that the folder is there, and for a Documents folder moved to a
/// sleeping server that check can take many seconds - on the UI thread, while Glint starts.
pub fn known_folder(id: &GUID) -> Option<String> {
    // SAFETY: the returned string is copied and then freed with CoTaskMemFree, as the API requires.
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DONT_VERIFY, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.filter(|s| !s.is_empty())
    }
}

/// Where a Windows "shell:" place typed in the path bar is on disk, like Explorer's address bar:
/// "shell:startup", "shell:sendto", "shell:Downloads\Setup" (a folder inside one), "shell:Common
/// Startup"... "" for This PC ("shell:MyComputerFolder"). None for places that aren't a folder on
/// disk (Control Panel, the Recycle Bin, Apps) and names Windows doesn't know: Glint hands those to
/// File Explorer (app.rs `navigate`).
///
/// The names are Windows' own "canonical" known-folder names. The common ones are looked up in a
/// table (the same answer on every Windows); the rest are asked of Windows' known-folder manager.
/// Neither checks that the folder exists, so a folder moved to a sleeping server doesn't hold up
/// the window.
pub fn shell_folder(typed: &str) -> Option<String> {
    let rest = typed.get(6..)?.trim();
    let (name, inside) = match rest.find(['\\', '/']) {
        Some(i) => (rest[..i].trim(), rest[i + 1..].trim_matches(['\\', '/'])),
        None => (rest, ""),
    };
    if name.eq_ignore_ascii_case("MyComputerFolder") || name.eq_ignore_ascii_case("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}") {
        return inside.is_empty().then(String::new);
    }
    let base = shell_folder_id(name).and_then(|id| known_folder(&id)).or_else(|| known_folder_by_name(name))?;
    Some(if inside.is_empty() { base } else { crate::folder::join(&base, inside) })
}

/// The known-folder id of a common "shell:" name (Explorer's names, any capitals).
fn shell_folder_id(name: &str) -> Option<GUID> {
    const NAMES: [(&str, GUID); 26] = [
        ("Desktop", FOLDERID_Desktop),
        ("Personal", FOLDERID_Documents),
        ("Documents", FOLDERID_Documents),
        ("Downloads", FOLDERID_Downloads),
        ("My Music", FOLDERID_Music),
        ("My Pictures", FOLDERID_Pictures),
        ("My Video", FOLDERID_Videos),
        ("Profile", FOLDERID_Profile),
        ("UsersFilesFolder", FOLDERID_Profile),
        ("Startup", FOLDERID_Startup),
        ("Common Startup", FOLDERID_CommonStartup),
        ("SendTo", FOLDERID_SendTo),
        ("AppData", FOLDERID_RoamingAppData),
        ("Local AppData", FOLDERID_LocalAppData),
        ("Programs", FOLDERID_Programs),
        ("Common Programs", FOLDERID_CommonPrograms),
        ("Start Menu", FOLDERID_StartMenu),
        ("Common Start Menu", FOLDERID_CommonStartMenu),
        ("Recent", FOLDERID_Recent),
        ("Templates", FOLDERID_Templates),
        ("Favorites", FOLDERID_Favorites),
        ("Windows", FOLDERID_Windows),
        ("System", FOLDERID_System),
        ("ProgramFiles", FOLDERID_ProgramFiles),
        ("Fonts", FOLDERID_Fonts),
        ("Common AppData", FOLDERID_ProgramData),
    ];
    NAMES.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, id)| *id)
}

/// Asks Windows' known-folder manager for the folder with this canonical name ("Quick Launch",
/// "OneDrive", "Common Desktop"...). Only the path is looked up (see `known_folder`).
fn known_folder_by_name(name: &str) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    let name_w = wide(name);
    // SAFETY: plain COM calls on this thread (COM is started on the UI thread in main.rs, and by
    // ComGuard elsewhere); the name outlives the call; the returned string is copied and then freed
    // with CoTaskMemFree, as the API requires.
    unsafe {
        let manager: IKnownFolderManager = CoCreateInstance(&KnownFolderManager, None, CLSCTX_INPROC_SERVER).ok()?;
        let folder = manager.GetFolderByName(PCWSTR(name_w.as_ptr())).ok()?;
        let p = folder.GetPath(KF_FLAG_DONT_VERIFY.0 as u32).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.filter(|s| !s.is_empty())
    }
}

/// The user's home folder (C:\Users\name).
pub fn home() -> String {
    known_folder(&FOLDERID_Profile).unwrap_or_else(|| "C:\\".to_string())
}

/// Whether Windows uses dark mode for apps (Settings > Personalisation > Colours > "Choose your
/// app mode"). None when Windows doesn't say (before Windows 10 1809, or under Wine).
pub fn windows_apps_dark() -> Option<bool> {
    let (mut value, mut size) = (0u32, std::mem::size_of::<u32>() as u32);
    // SAFETY: the key and value names are NUL-terminated constants; `value` and `size` are valid
    // out-pointers and `size` is the buffer's true size. A quick read of the user's own registry hive.
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            windows::core::w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            windows::core::w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    read.is_ok().then_some(value == 0)
}
