//! Changing what's inside a .zip or .7z: removing, renaming and moving items in it, and putting
//! files and folders from the disk into it (as WinRAR and 7-Zip do).
//!
//! An archive can't be changed in place safely, so [`change`] writes a whole new one next to it
//! and only then puts it in its place: when anything fails (or CANCEL is clicked) the archive is as
//! it was. The archive as it was goes to Glint's temporary folder, so Ctrl+Z can put it back
//! ([`swap`]; Ctrl+Y swaps again).
//!
//! A zip's items that stay are copied as they're packed, without unpacking them, so a change to a
//! big zip is about as quick as copying it. A 7z packs its files together in blocks, so it's
//! unpacked to the temporary folder and packed again whole (WinRAR and 7-Zip do the same).

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{
    dos_time, filetime_to_system, fnv, fold, free_name, inner_parts, inside, list, open_error, read_error, resolve, temp_dir_for, unpack, walk, write_error, Item, Kind,
    Meter, Opened, Packed, Packing, CANCELLED, ZIP64_FROM,
};
use crate::folder;

/// What Glint says about archives it can't change.
const LOCKED: &str = "Glint can't change password-protected archives.";
const NESTED: &str = "Glint can only change an archive that isn't inside another one. Copy it out first.";

/// A change to an archive (paths inside it, `\` between parts, "" for its root).
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// These items go (a folder with everything in it).
    Remove(Vec<String>),
    /// `from` gets the name `name` (already checked), in the same folder.
    Rename { from: String, name: String },
    /// These items go into the folder `into` of the archive. A name taken there is numbered
    /// ("notes (2).txt"): nothing is replaced.
    Move { items: Vec<String>, into: String },
    /// Copies of these files and folders on the disk go into the folder `into`, numbered like
    /// Move's when a name is taken.
    Add { sources: Vec<String>, into: String },
    /// A new empty folder ("New folder") or text file ("New Text Document.txt") in `into`.
    New { into: String, folder: bool },
}

/// What a change did.
#[derive(Clone, Debug)]
pub struct Changed {
    /// The archive as it was, kept in Glint's temporary folder: [`swap`] puts it back.
    pub backup: String,
    /// The folder inside the archive that new, moved or renamed items are in, and their names (to
    /// select them).
    pub made: Option<(String, Vec<String>)>,
}

/// Where an item of the new archive comes from.
enum Source {
    /// This item of the archive (its index among the items).
    Kept(usize),
    /// This item of the plan of files from the disk.
    Disk(usize),
    /// Nothing: a folder, or an empty new file.
    Empty,
}

/// One item of the new archive.
struct Out {
    source: Source,
    /// Its path inside, `\` between parts.
    path: String,
    is_dir: bool,
    modified: Option<SystemTime>,
}

/// Makes the change to `archive` (an archive file on the disk, not one inside another), writing a
/// new archive and putting it in the old one's place. Ok(None) when there was nothing to do (items
/// moved to where they are, a name given again). Reports to the copy queue as it goes; `stop` stops
/// looking through folders to add.
pub fn change(archive: &str, change: &Change, stop: &AtomicBool) -> Result<Option<Changed>, String> {
    let kind = super::kind_of(archive).ok_or_else(|| "This isn't a zip or 7z file.".to_string())?;
    for dir in folders_named(change) {
        if !resolve(archive, &dir).is_ok_and(|(real, _)| real == archive) {
            return Err(NESTED.to_string());
        }
    }
    let (mut opened, items) = list(archive)?;
    if items.iter().any(|i| i.locked) {
        return Err(LOCKED.to_string());
    }
    let mut disk = Packing { items: Vec::new(), total: 0 };
    let Some((outs, made)) = plan(&items, change, &mut disk, stop)? else { return Ok(None) };
    let new = new_file_beside(archive)?;
    let written = File::options().write(true).open(&new).map_err(|e| write_error(&new, &e)).and_then(|file| match kind {
        Kind::Zip => match &mut opened {
            Opened::Zip(zip) => write_zip(archive, zip, &items, &outs, &disk, file, &new),
            Opened::SevenZ(..) => Err("This isn't a zip file.".to_string()),
        },
        Kind::SevenZ => write_7z(archive, &mut opened, &items, &outs, &disk, file, &new),
    });
    // The archive is closed before it's moved away.
    drop(opened);
    if let Err(why) = written {
        let _ = std::fs::remove_file(&new);
        return Err(why);
    }
    let backup = put_in_place(Path::new(archive), &new)?;
    Ok(Some(Changed { backup: backup.to_string_lossy().into_owned(), made }))
}

/// The folders of the archive a change touches: each must be in the archive itself, not in an
/// archive inside it.
fn folders_named(change: &Change) -> Vec<String> {
    let parent = |p: &String| parent_of(p).to_string();
    match change {
        Change::Remove(items) => items.iter().map(parent).collect(),
        Change::Rename { from, .. } => vec![parent(from)],
        Change::Move { items, into } => items.iter().map(parent).chain([into.clone()]).collect(),
        Change::Add { into, .. } | Change::New { into, .. } => vec![into.clone()],
    }
}

/// `path`'s folder ("" at the root).
fn parent_of(path: &str) -> &str {
    path.rfind('\\').map_or("", |i| &path[..i])
}

/// `path`'s own name.
fn name_of(path: &str) -> &str {
    path.rfind('\\').map_or(path, |i| &path[i + 1..])
}

fn joined(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}\\{name}")
    }
}

/// What's after `want` in `path` ("" for `want` itself), when `path` is it or under it (names
/// compared ignoring case, as Windows does).
fn under<'a>(path: &'a str, want: &str) -> Option<&'a str> {
    if want.is_empty() {
        return Some(path);
    }
    let (head, rest) = match path.get(..want.len()) {
        Some(head) => (head, &path[want.len()..]),
        None => return None,
    };
    if fold(head) != fold(want) {
        return None;
    }
    match rest.strip_prefix('\\') {
        Some(rest) => Some(rest),
        None if rest.is_empty() => Some(""),
        None => None,
    }
}

/// The new archive's items for this change, and where its new items are (to select them). None
/// when there's nothing to do.
#[allow(clippy::type_complexity)]
fn plan(items: &[Item], change: &Change, disk: &mut Packing, stop: &AtomicBool) -> Result<Option<(Vec<Out>, Option<(String, Vec<String>)>)>, String> {
    // Every folder (lower case -> as named), those only implied by the paths under them too; and
    // every file.
    let mut dirs: HashMap<String, String> = HashMap::new();
    let mut files: HashSet<String> = HashSet::new();
    for item in items {
        let parts: Vec<&str> = item.parts().collect();
        let implied = if item.is_dir { parts.len() } else { parts.len() - 1 };
        for n in 1..=implied {
            let p = parts[..n].join("\\");
            dirs.entry(fold(&p)).or_insert(p);
        }
        if !item.is_dir {
            files.insert(fold(&item.path));
        }
    }
    let is_there = |p: &str| dirs.contains_key(&fold(p)) || files.contains(&fold(p));
    let clean = |p: &str| inner_parts(p).join("\\");
    let folder_there = |into: &str| -> Result<String, String> {
        let into = clean(into);
        if into.is_empty() || dirs.contains_key(&fold(&into)) {
            Ok(dirs.get(&fold(&into)).cloned().unwrap_or(into))
        } else {
            Err(format!("There's no folder called \u{201c}{into}\u{201d} in this archive any more."))
        }
    };
    let gone = |p: &str| format!("\u{201c}{}\u{201d} isn't in this archive any more.", name_of(p));
    // Names taken in a folder (lower case), but for these items that leave it.
    let taken_in = |into: &str, leaving: &[String]| -> HashSet<String> {
        let leaving: HashSet<String> = leaving.iter().map(|p| fold(p)).collect();
        dirs.keys().chain(files.iter()).filter(|k| parent_of(k) == fold(into) && !k.is_empty() && !leaving.contains(*k)).map(|k| name_of(k).to_string()).collect()
    };

    // Where each path goes (None: it goes away), and what's new.
    let mut moves: Vec<(String, String)> = Vec::new();
    let mut removed: Vec<String> = Vec::new();
    let mut added: Vec<Out> = Vec::new();
    let made;
    match change {
        Change::Remove(paths) => {
            for p in paths {
                let p = clean(p);
                if p.is_empty() {
                    return Err("Select what to delete first.".to_string());
                }
                if !is_there(&p) {
                    return Err(gone(&p));
                }
                removed.push(p);
            }
            if removed.is_empty() {
                return Ok(None);
            }
            made = None;
        }
        Change::Rename { from, name } => {
            let from = clean(from);
            if from.is_empty() || !is_there(&from) {
                return Err(gone(&from));
            }
            let dir = parent_of(&from).to_string();
            if name == name_of(&from) {
                return Ok(None);
            }
            let to = joined(&dir, name);
            // (Only capitals changed: the same item.)
            if fold(&to) != fold(&from) && is_there(&to) {
                return Err(format!("There's already an item called \u{201c}{name}\u{201d} here."));
            }
            moves.push((from, to));
            made = Some((dir, vec![name.clone()]));
        }
        Change::Move { items: paths, into } => {
            let into = folder_there(into)?;
            let mut names = Vec::new();
            let paths: Vec<String> = paths.iter().map(|p| clean(p)).collect();
            let mut taken = taken_in(&into, &paths);
            for p in &paths {
                if p.is_empty() || !is_there(p) {
                    return Err(gone(p));
                }
                if under(&into, p).is_some() {
                    return Err(format!("\u{201c}{}\u{201d} can't be moved into itself.", name_of(p)));
                }
                // Already there: it stays as it is.
                if fold(parent_of(p)) == fold(&into) {
                    taken.insert(fold(name_of(p)));
                    continue;
                }
                let is_dir = dirs.contains_key(&fold(p));
                let name = free_name(name_of(p), is_dir, |n| taken.contains(&fold(n)));
                taken.insert(fold(&name));
                moves.push((p.clone(), joined(&into, &name)));
                names.push(name);
            }
            if moves.is_empty() {
                return Ok(None);
            }
            made = Some((into, names));
        }
        Change::Add { sources, into } => {
            let into = folder_there(into)?;
            let mut taken = taken_in(&into, &[]);
            let mut names = Vec::new();
            for source in sources {
                let path = Path::new(source);
                let meta = std::fs::metadata(path).map_err(|e| read_error(path, &e))?;
                let name = free_name(folder::file_name(source), meta.is_dir(), |n| taken.contains(&fold(n)));
                taken.insert(fold(&name));
                disk.add(path, name.clone(), &meta);
                if meta.is_dir() && !walk(path, &name, disk, stop)? {
                    return Err(CANCELLED.to_string());
                }
                names.push(name);
            }
            for (at, p) in disk.items.iter().enumerate() {
                added.push(Out { source: Source::Disk(at), path: joined(&into, &p.name.replace('/', "\\")), is_dir: p.is_dir, modified: p.modified });
            }
            if names.is_empty() {
                return Ok(None);
            }
            made = Some((into, names));
        }
        Change::New { into, folder } => {
            let into = folder_there(into)?;
            let taken = taken_in(&into, &[]);
            let (base, ext) = if *folder { ("New folder", "") } else { ("New Text Document", ".txt") };
            let name = crate::fileops::unique_name(base, ext, |n| taken.contains(&fold(n)));
            added.push(Out { source: Source::Empty, path: joined(&into, &name), is_dir: *folder, modified: Some(SystemTime::now()) });
            made = Some((into, vec![name]));
        }
    }

    // Where a path of the archive goes now.
    let goes = |p: &str| -> Option<String> {
        if removed.iter().any(|r| under(p, r).is_some()) {
            return None;
        }
        for (from, to) in &moves {
            if let Some(rest) = under(p, from) {
                return Some(if rest.is_empty() { to.clone() } else { format!("{to}\\{rest}") });
            }
        }
        Some(p.to_string())
    };
    let mut outs: Vec<Out> = Vec::with_capacity(items.len() + added.len());
    // (Folders once each, lower case: an archive can list one twice.)
    let mut out_dirs: HashSet<String> = HashSet::new();
    for (i, item) in items.iter().enumerate() {
        let Some(path) = goes(&item.path) else { continue };
        if item.is_dir && !out_dirs.insert(fold(&path)) {
            continue;
        }
        outs.push(Out { source: Source::Kept(i), path, is_dir: item.is_dir, modified: filetime_to_system(item.modified) });
    }
    // A folder that was only there through the files in it stays, as an empty folder, when they all
    // went (as in a real folder): it gets an entry of its own.
    let mut implied: HashSet<String> = HashSet::new();
    for o in &outs {
        let parts: Vec<&str> = o.path.split('\\').collect();
        let upto = if o.is_dir { parts.len() } else { parts.len() - 1 };
        for n in 1..=upto {
            implied.insert(fold(&parts[..n].join("\\")));
        }
    }
    let mut kept_dirs: Vec<String> = dirs.values().filter_map(|d| goes(d)).filter(|d| !implied.contains(&fold(d))).collect();
    kept_dirs.sort();
    for d in kept_dirs {
        if out_dirs.insert(fold(&d)) {
            outs.push(Out { source: Source::Empty, path: d, is_dir: true, modified: None });
        }
    }
    outs.extend(added);
    Ok(Some((outs, made)))
}

/// A new, empty file next to `archive` for the new archive, hidden while it's written (it's on the
/// same drive, so it can be put in the archive's place in one step).
fn new_file_beside(archive: &str) -> Result<PathBuf, String> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    let dir = Path::new(archive).parent().ok_or_else(|| "This archive isn't in a folder.".to_string())?;
    let name = folder::file_name(archive);
    for n in 1.. {
        let p = dir.join(format!("{name}.glint-{n}.tmp"));
        match File::options().write(true).create_new(true).attributes(FILE_ATTRIBUTE_HIDDEN).open(&p) {
            Ok(_) => return Ok(p),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && n < 1000 => {}
            Err(e) => return Err(write_error(&p, &e)),
        }
    }
    unreachable!()
}

/// Puts the new archive `new` in `archive`'s place; the old one goes to Glint's temporary folder.
/// Returns where it went. When the archive can't be moved (another program has it open), nothing
/// changes and `new` is removed.
fn put_in_place(archive: &Path, new: &Path) -> Result<PathBuf, String> {
    let attributes = crate::fileops::attributes(&archive.to_string_lossy()).unwrap_or(0x80);
    let backup = backup_path(archive)?;
    if let Err(e) = move_file(archive, &backup) {
        let _ = std::fs::remove_file(new);
        return Err(in_use(archive, &e));
    }
    if let Err(e) = std::fs::rename(new, archive) {
        let _ = move_file(&backup, archive);
        let _ = std::fs::remove_file(new);
        return Err(write_error(archive, &e));
    }
    // (It was written hidden: now it has the archive's own attributes.)
    set_attributes(archive, attributes);
    Ok(backup)
}

/// Swaps `archive` and `other` (an earlier or later version of it, kept by [`change`]): Ctrl+Z and
/// Ctrl+Y. Afterwards `other` holds what `archive` held. When it can't be done, both stay as they
/// were.
pub fn swap(archive: &str, other: &str) -> Result<(), String> {
    let (a, o) = (Path::new(archive), Path::new(other));
    let name = folder::file_name(archive);
    if !o.is_file() {
        return Err(format!("The other version of \u{201c}{name}\u{201d} isn't in Glint's temporary folder any more."));
    }
    if !a.is_file() {
        return Err(format!("\u{201c}{name}\u{201d} isn't there any more."));
    }
    // The other version comes next to the archive first, so it takes its place in one step.
    let side = new_file_beside(archive)?;
    // (Only its name is wanted: a copy from another drive won't write over a hidden file.)
    let _ = std::fs::remove_file(&side);
    if let Err(e) = move_file(o, &side) {
        let _ = std::fs::remove_file(&side);
        return Err(write_error(&side, &e));
    }
    if let Err(e) = move_file(a, o) {
        let _ = move_file(&side, o);
        return Err(in_use(a, &e));
    }
    if let Err(e) = std::fs::rename(&side, a) {
        let _ = move_file(o, a);
        let _ = move_file(&side, o);
        return Err(write_error(a, &e));
    }
    Ok(())
}

/// Where the archive as it was is kept: a new folder of its own in %TEMP%\Glint\archives\before,
/// under its own name.
fn backup_path(archive: &Path) -> Result<PathBuf, String> {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    let n = COUNT.fetch_add(1, Ordering::SeqCst);
    let key = fnv(archive.to_string_lossy().to_lowercase().bytes().chain(now.to_le_bytes()).chain(n.to_le_bytes()).chain(std::process::id().to_le_bytes()));
    let dir = std::env::temp_dir().join("Glint").join("archives").join("before").join(format!("{key:016x}"));
    std::fs::create_dir_all(&dir).map_err(|e| write_error(&dir, &e))?;
    Ok(dir.join(archive.file_name().unwrap_or_default()))
}

/// How long the archive as it was before a change is kept at most (Ctrl+Z forgets it sooner, after 30
/// more changes).
const KEPT_FOR: std::time::Duration = std::time::Duration::from_secs(2 * 24 * 60 * 60);

/// Removes the versions of archives kept for Ctrl+Z ([`backup_path`]) that are older than
/// `KEPT_FOR`: a Glint closed since can't undo them any more (one still open keeps them that long).
pub fn forget_old_versions() {
    let dir = std::env::temp_dir().join("Glint").join("archives").join("before");
    let Ok(list) = std::fs::read_dir(&dir) else { return };
    for e in list.flatten() {
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > KEPT_FOR);
        if old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// Moves a file, to another drive too (copied there, then removed here). Replaces nothing that
/// isn't Glint's own.
fn move_file(from: &Path, to: &Path) -> io::Result<()> {
    const ERROR_NOT_SAME_DEVICE: i32 = 17;
    match std::fs::rename(from, to) {
        Err(e) if e.raw_os_error() == Some(ERROR_NOT_SAME_DEVICE) => {
            std::fs::copy(from, to)?;
            if let Err(e) = std::fs::remove_file(from) {
                let _ = std::fs::remove_file(to);
                return Err(e);
            }
            Ok(())
        }
        other => other,
    }
}

fn set_attributes(path: &Path, attributes: u32) {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{SetFileAttributesW, FILE_FLAGS_AND_ATTRIBUTES};
    let p = folder::wide_path(&path.to_string_lossy());
    // SAFETY: a NUL-terminated path that outlives the call.
    let _ = unsafe { SetFileAttributesW(PCWSTR(p.as_ptr()), FILE_FLAGS_AND_ATTRIBUTES(attributes)) };
}

/// The archive couldn't be moved: open in another program, as a rule.
fn in_use(archive: &Path, e: &io::Error) -> String {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_LOCK_VIOLATION: i32 = 33;
    let name = archive.file_name().unwrap_or_default().to_string_lossy();
    match e.raw_os_error() {
        Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION) => crate::inuse::open_in(&archive.to_string_lossy()).unwrap_or_else(|| format!("\u{201c}{name}\u{201d} is open in another program. Close it there and try again.")),
        _ if e.kind() == io::ErrorKind::NotFound => format!("\u{201c}{name}\u{201d} isn't there any more."),
        _ if e.kind() == io::ErrorKind::PermissionDenied => format!("Glint isn't allowed to change \u{201c}{name}\u{201d}. It may be open in another program."),
        _ => format!("\u{201c}{name}\u{201d} couldn't be changed ({e})."),
    }
}

/// The new zip: items that stay are copied as they're packed (renamed when they moved), new ones
/// are packed with Deflate as Compress to zip packs them.
fn write_zip(archive: &str, zip: &mut zip::ZipArchive<BufReader<File>>, items: &[Item], outs: &[Out], disk: &Packing, file: File, out: &Path) -> Result<(), String> {
    use zip::write::SimpleFileOptions;
    let zip_error = |e: zip::result::ZipError| match e {
        zip::result::ZipError::Io(e) => write_error(out, &e),
        e => format!("\u{201c}{}\u{201d} couldn't be written ({e}).", out.display()),
    };
    // How much each item holds packed: what copying it moves.
    let packed: Vec<u64> = items
        .iter()
        .map(|i| match i.hidden {
            Some(h) => h.compressed,
            None => zip.by_index_data(i.index).map_or(i.size, |e| e.compressed_size()),
        })
        .collect();
    let total: u64 = outs
        .iter()
        .map(|o| match o.source {
            Source::Kept(i) if !o.is_dir => packed[i],
            Source::Disk(at) => disk.items[at].size,
            _ => 0,
        })
        .sum();
    let meter = Meter::new(total);
    let comment = zip.comment().to_vec();
    let mut w = zip::ZipWriter::new(BufWriter::new(file)).set_auto_large_file();
    let _ = w.set_raw_comment(comment.into_boxed_slice());
    // For entries the zip library has no index for: the file, read from each one's own header.
    let mut direct: Option<BufReader<File>> = None;
    let mut buf = vec![0u8; 256 * 1024];
    for o in outs {
        let name = o.path.replace('\\', "/");
        let mut opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).compression_level(Some(6));
        if let Some(t) = o.modified.and_then(dos_time) {
            opts = opts.last_modified_time(t);
        }
        if o.is_dir {
            w.add_directory(name.as_str(), opts).map_err(zip_error)?;
            continue;
        }
        match o.source {
            Source::Kept(i) => {
                let item = &items[i];
                let damaged = |_: zip::result::ZipError| format!("\u{201c}{}\u{201d} is damaged in this zip file.", folder::file_name(&item.path));
                match item.hidden {
                    None => {
                        let entry = zip.by_index_raw(item.index).map_err(damaged)?;
                        w.raw_copy_file_rename(entry, name.as_str()).map_err(zip_error)?;
                    }
                    Some(at) => {
                        if direct.is_none() {
                            direct = Some(BufReader::new(File::open(archive).map_err(|e| open_error(&e))?));
                        }
                        let Some(source) = direct.as_mut() else { continue };
                        source.seek(io::SeekFrom::Start(at.header)).map_err(|e| damaged(zip::result::ZipError::Io(e)))?;
                        let options = zip::ZipReadOptions::new().override_compressed_size(at.compressed).override_uncompressed_size(at.size).override_crc(at.crc);
                        let entry = zip::read::read_zipfile_from_stream_with_options(source, options).map_err(damaged)?.ok_or_else(|| format!("\u{201c}{}\u{201d} is damaged in this zip file.", folder::file_name(&item.path)))?;
                        w.raw_copy_file_rename(entry, name.as_str()).map_err(zip_error)?;
                    }
                }
                if !meter.add(packed[i].max(1)) {
                    return Err(CANCELLED.to_string());
                }
            }
            Source::Empty => {
                w.start_file(name.as_str(), opts).map_err(zip_error)?;
            }
            Source::Disk(at) => {
                let p = &disk.items[at];
                w.start_file(name.as_str(), opts.large_file(p.size >= ZIP64_FROM)).map_err(zip_error)?;
                let mut feed = super::Feed::new(&p.path, &meter);
                loop {
                    let n = match feed.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => return Err(meter.problem.take().unwrap_or_else(|| read_error(&p.path, &io::Error::other("read failed")))),
                    };
                    w.write_all(&buf[..n]).map_err(|e| write_error(out, &e))?;
                }
            }
        }
    }
    let mut f = w.finish().map_err(zip_error)?;
    f.flush().map_err(|e| write_error(out, &e))
}

/// The new 7z: what stays is unpacked to Glint's temporary folder, then everything is packed again
/// as Compress to 7z packs it.
fn write_7z(archive: &str, opened: &mut Opened, items: &[Item], outs: &[Out], disk: &Packing, file: File, out: &Path) -> Result<(), String> {
    let tmp = PathBuf::from(temp_dir_for(archive)?);
    let result = (|| {
        let kept: Vec<(usize, PathBuf)> = outs
            .iter()
            .filter(|o| !o.is_dir)
            .filter_map(|o| match o.source {
                Source::Kept(i) => Some(inside(&tmp.to_string_lossy(), &items[i].parts().collect::<Vec<_>>()).map(|p| (i, p))),
                _ => None,
            })
            .collect::<Result<_, _>>()?;
        let kept_bytes: u64 = kept.iter().map(|(i, _)| items[*i].size).sum();
        // Unpacking and packing both count.
        let meter = Meter::new(kept_bytes * 2 + disk.total);
        unpack(archive, opened, items, &kept, &meter)?;
        let unpacked: HashMap<usize, &PathBuf> = kept.iter().map(|(i, p)| (*i, p)).collect();
        let mut plan = Packing { items: Vec::with_capacity(outs.len()), total: 0 };
        for o in outs {
            let name = o.path.replace('\\', "/");
            let packed = match o.source {
                Source::Kept(i) if !o.is_dir => Packed { path: unpacked[&i].clone(), name, is_dir: false, size: items[i].size, modified: o.modified, created: None, accessed: None },
                Source::Disk(at) => {
                    let p = &disk.items[at];
                    Packed { path: p.path.clone(), name, is_dir: p.is_dir, size: p.size, modified: p.modified, created: p.created, accessed: p.accessed }
                }
                _ => Packed { path: PathBuf::new(), name, is_dir: o.is_dir, size: 0, modified: o.modified, created: None, accessed: None },
            };
            plan.total += packed.size;
            plan.items.push(packed);
        }
        super::pack_7z(&plan, file, out, &meter)
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

#[cfg(test)]
mod tests {
    use super::super::tests::*;
    use super::*;

    fn never() -> AtomicBool {
        AtomicBool::new(false)
    }

    fn edit(archive: &Path, c: Change) -> Changed {
        change(&s(archive), &c, &never()).expect("change").expect("something to do")
    }

    /// Everything in the archive, unpacked, as `tree` lists it.
    fn contents(archive: &Path, t: &Scratch) -> Vec<(String, Option<Vec<u8>>)> {
        static N: AtomicU64 = AtomicU64::new(0);
        let out = t.dir(&format!("out{}", N.fetch_add(1, Ordering::SeqCst)));
        super::super::extract_all(&s(archive), &s(&out), &mut Vec::new()).expect("extract");
        tree(&out)
    }

    fn start(t: &Scratch, ext: &str) -> PathBuf {
        let a = t.join(&format!("box{ext}"));
        archive_with(&a, &[("readme.txt", b"hello"), ("docs/", b""), ("docs/a.txt", b"aaaa"), ("docs/sub/b.txt", b"bb"), ("src/main.rs", b"fn main() {}")]);
        a
    }

    #[test]
    fn items_are_removed_renamed_moved_and_added_in_zips_and_7zs() {
        for ext in [".zip", ".7z"] {
            let t = Scratch::new(&format!("edit{}", ext.replace('.', "")));
            let a = start(&t, ext);
            // Removing the only file of a folder known only through it leaves the folder.
            edit(&a, Change::Remove(vec!["src\\main.rs".into(), "readme.txt".into()]));
            assert_eq!(contents(&a, &t), expected(&[("docs", None), ("docs\\a.txt", Some(b"aaaa")), ("docs\\sub", None), ("docs\\sub\\b.txt", Some(b"bb")), ("src", None)]), "{ext}");
            // A folder renamed takes what's in it along.
            let c = edit(&a, Change::Rename { from: "docs".into(), name: "Papers".into() });
            assert_eq!(c.made, Some((String::new(), vec!["Papers".to_string()])));
            // Moved into another folder, and to the root.
            edit(&a, Change::Move { items: vec!["Papers\\sub".into()], into: "src".into() });
            let c = edit(&a, Change::Move { items: vec!["src\\sub\\b.txt".into()], into: String::new() });
            assert_eq!(c.made, Some((String::new(), vec!["b.txt".to_string()])));
            // Files and a folder from the disk.
            let d = t.dir("disk");
            std::fs::write(d.join("b.txt"), b"another b").expect("write");
            std::fs::create_dir_all(d.join("pics").join("deep")).expect("mkdir");
            std::fs::write(d.join("pics").join("deep").join("p.jpg"), b"jpeg").expect("write");
            let c = edit(&a, Change::Add { sources: vec![s(&d.join("b.txt")), s(&d.join("pics"))], into: String::new() });
            assert_eq!(c.made, Some((String::new(), vec!["b (2).txt".to_string(), "pics".to_string()])), "a name taken is numbered");
            let c = edit(&a, Change::New { into: "Papers".into(), folder: true });
            assert_eq!(c.made, Some(("Papers".to_string(), vec!["New folder".to_string()])));
            edit(&a, Change::New { into: "Papers".into(), folder: false });
            assert_eq!(
                contents(&a, &t),
                expected(&[
                    ("Papers", None),
                    ("Papers\\a.txt", Some(b"aaaa")),
                    ("Papers\\New folder", None),
                    ("Papers\\New Text Document.txt", Some(b"")),
                    ("b (2).txt", Some(b"another b")),
                    ("b.txt", Some(b"bb")),
                    ("pics", None),
                    ("pics\\deep", None),
                    ("pics\\deep\\p.jpg", Some(b"jpeg")),
                    ("src", None),
                    ("src\\sub", None),
                ]),
                "{ext}"
            );
            // Nothing left behind next to it.
            assert!(!names_in(&t.0).iter().any(|n| n.ends_with(".tmp")), "{ext}: {:?}", names_in(&t.0));
        }
    }

    #[test]
    fn undo_puts_the_archive_back_exactly_and_redo_swaps_again() {
        for ext in [".zip", ".7z"] {
            let t = Scratch::new(&format!("undo{}", ext.replace('.', "")));
            let a = start(&t, ext);
            let before = std::fs::read(&a).expect("read");
            let c = edit(&a, Change::Remove(vec!["docs".into()]));
            let after = std::fs::read(&a).expect("read");
            assert_ne!(before, after);
            swap(&s(&a), &c.backup).expect("undo");
            assert_eq!(std::fs::read(&a).expect("read"), before, "{ext}: undone exactly");
            swap(&s(&a), &c.backup).expect("redo");
            assert_eq!(std::fs::read(&a).expect("read"), after, "{ext}: redone exactly");
            // With the other version gone, nothing changes.
            std::fs::remove_file(&c.backup).expect("remove");
            assert!(swap(&s(&a), &c.backup).is_err());
            assert_eq!(std::fs::read(&a).expect("read"), after);
        }
    }

    #[test]
    fn what_cant_be_done_says_why_and_leaves_the_archive_alone() {
        let t = Scratch::new("refuse");
        let a = start(&t, ".zip");
        let before = std::fs::read(&a).expect("read");
        let err = |c: Change| change(&s(&a), &c, &never()).expect_err("refused");
        assert!(err(Change::Move { items: vec!["docs".into()], into: "docs\\sub".into() }).contains("into itself"));
        assert!(err(Change::Rename { from: "readme.txt".into(), name: "DOCS".into() }).contains("already an item"));
        assert!(err(Change::Remove(vec!["nope.txt".into()])).contains("isn't in this archive"));
        assert!(err(Change::Add { sources: vec![s(&t.join("missing.txt"))], into: String::new() }).contains("isn't there"));
        assert!(err(Change::New { into: "nope".into(), folder: true }).contains("no folder"));
        // Nothing to do.
        assert!(change(&s(&a), &Change::Move { items: vec!["docs\\a.txt".into()], into: "docs".into() }, &never()).expect("ok").is_none());
        assert!(change(&s(&a), &Change::Rename { from: "readme.txt".into(), name: "readme.txt".into() }, &never()).expect("ok").is_none());
        // Open in a program that won't share it: nothing changes, nothing is left behind.
        {
            use std::os::windows::fs::OpenOptionsExt;
            // (It can be read, but not moved: as most programs hold a file they have open.)
            let _held = File::options().read(true).share_mode(1).open(&a).expect("hold");
            let why = err(Change::Remove(vec!["readme.txt".into()]));
            // (Named when Windows knows who has it: here the test itself.)
            assert!(why.contains("open in another program") || why.contains(" is open in "), "{why}");
        }
        assert_eq!(std::fs::read(&a).expect("read"), before);
        assert_eq!(names_in(&t.0), ["box.zip"]);
        // Only capitals changed: fine.
        edit(&a, Change::Rename { from: "readme.txt".into(), name: "README.txt".into() });
        assert_eq!(names(&super::super::read(&s(&a), "", "")), ["README.txt", "docs\\", "src\\"]);
    }

    #[test]
    fn a_password_protected_or_nested_archive_isnt_changed() {
        let t = Scratch::new("locked");
        let inner = zip_bytes(&[("x.txt", b"x")]);
        let a = t.join("outer.zip");
        zip_with(&a, &[("in.zip", &inner), ("y.txt", b"y")]);
        let err = change(&s(&a), &Change::Remove(vec!["in.zip\\x.txt".into()]), &never()).expect_err("nested");
        assert_eq!(err, NESTED);
        // The inner archive itself can go, though.
        edit(&a, Change::Remove(vec!["in.zip".into()]));
        assert_eq!(names(&super::super::read(&s(&a), "", "")), ["y.txt"]);
    }

    #[test]
    fn the_same_name_twice_in_a_zip_keeps_both() {
        let t = Scratch::new("twice");
        let a = t.join("twice.zip");
        zip_with(&a, &[("notes.txt", b"first"), ("notez.txt", b"second"), ("other.txt", b"o")]);
        patch(&a, b"notez.txt", b"notes.txt");
        edit(&a, Change::Remove(vec!["other.txt".into()]));
        assert_eq!(contents(&a, &t), expected(&[("notes (2).txt", Some(b"second")), ("notes.txt", Some(b"first"))]));
        // And from a zip written as a stream (its sizes after the data).
        let mut w = zip::ZipWriter::new_stream(Vec::new());
        let opts = zip::write::SimpleFileOptions::default();
        for (n, data) in [("x.txt", &b"first one"[..]), ("y.txt", &b"second, longer one"[..]), ("z.txt", &b"z"[..])] {
            w.start_file(n, opts).expect("start");
            w.write_all(data).expect("write");
        }
        let b = t.join("stream.zip");
        std::fs::write(&b, w.finish().expect("finish").into_inner()).expect("write");
        patch(&b, b"y.txt", b"x.txt");
        edit(&b, Change::Remove(vec!["z.txt".into()]));
        assert_eq!(contents(&b, &t), expected(&[("x (2).txt", Some(b"second, longer one")), ("x.txt", Some(b"first one"))]));
    }
}
