use crate::file_metrics;
use cap_std::fs::{Dir, DirEntry, ReadDir};
use cap_std::ambient_authority;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::cmp::Reverse;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const MAX_SUMMARY_ENTRIES: usize = 200;

#[derive(Clone, Debug)]
pub(crate) struct ScanProgress {
    pub path: PathBuf,
    pub file_count: u64,
    pub directory_count: u64,
    pub skipped_count: u64,
    pub skip_reason: Option<&'static str>,
    pub counted_size_bytes: u64,
    pub logical_size_bytes: u64,
    pub allocated_size_bytes: Option<u64>,
    pub file_identity: Option<String>,
    pub volume_identity: Option<String>,
    pub modified_at: Option<i64>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScanEntry {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub allocated_size_bytes: u64,
    pub file_count: u64,
    pub directory_count: u64,
    pub skipped_count: u64,
    pub hard_link_duplicate_count: u64,
    pub sparse_file_count: u64,
    pub compressed_file_count: u64,
    pub is_directory: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScanSummary {
    pub root_path: String,
    pub total_size_bytes: u64,
    pub allocated_size_bytes: u64,
    pub file_count: u64,
    pub directory_count: u64,
    pub skipped_count: u64,
    pub hard_link_duplicate_count: u64,
    pub sparse_file_count: u64,
    pub compressed_file_count: u64,
    pub elapsed_milliseconds: u128,
    pub entries: Vec<ScanEntry>,
    pub entries_truncated: bool,
}

#[derive(Default)]
struct Totals {
    size: u64,
    allocated: u64,
    files: u64,
    directories: u64,
    skipped: u64,
    hard_link_duplicates: u64,
    sparse_files: u64,
    compressed_files: u64,
}

impl Totals {
    fn include(&mut self, other: &Self) {
        self.size = self.size.saturating_add(other.size);
        self.allocated = self.allocated.saturating_add(other.allocated);
        self.files = self.files.saturating_add(other.files);
        self.directories = self.directories.saturating_add(other.directories);
        self.skipped = self.skipped.saturating_add(other.skipped);
        self.hard_link_duplicates = self.hard_link_duplicates.saturating_add(other.hard_link_duplicates);
        self.sparse_files = self.sparse_files.saturating_add(other.sparse_files);
        self.compressed_files = self.compressed_files.saturating_add(other.compressed_files);
    }
}

struct SeenFileStore {
    connection: Connection,
    path: PathBuf,
}

impl SeenFileStore {
    fn new() -> Result<Self, String> {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|error| error.to_string())?.as_nanos();
        let path = std::env::temp_dir().join(format!("disk-visualizer-seen-{}-{unique}.sqlite3", std::process::id()));
        let connection = Connection::open(&path).map_err(|error| format!("重複判定用DBを開けません: {error}"))?;
        connection.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA temp_store=FILE; PRAGMA cache_size=-2048; CREATE TABLE seen_files (identity TEXT PRIMARY KEY) WITHOUT ROWID; BEGIN IMMEDIATE;").map_err(|error| format!("重複判定用DBを初期化できません: {error}"))?;
        Ok(Self { connection, path })
    }

    fn is_duplicate(&self, identity: Option<String>) -> Result<bool, String> {
        let Some(identity) = identity else { return Ok(false) };
        self.connection.execute("INSERT OR IGNORE INTO seen_files (identity) VALUES (?1)", params![identity]).map(|changed| changed == 0).map_err(|error| format!("ハードリンクを判定できません: {error}"))
    }
}

impl Drop for SeenFileStore {
    fn drop(&mut self) {
        let _ = self.connection.execute_batch("ROLLBACK;");
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(self.path.with_extension("sqlite3-journal"));
    }
}

struct DirectoryFrame {
    relative_path: PathBuf,
    entries: ReadDir,
}

fn next_entry(stack: &mut Vec<DirectoryFrame>) -> Option<(PathBuf, Result<DirEntry, ()>)> {
    loop {
        let frame = stack.last_mut()?;
        match frame.entries.next() {
            Some(Ok(entry)) => return Some((frame.relative_path.clone(), Ok(entry))),
            Some(Err(_)) => return Some((frame.relative_path.clone(), Err(()))),
            None => { stack.pop(); }
        }
    }
}

fn crosses_volume(root: Option<&str>, current: Option<&str>) -> bool {
    match (root, current) {
        (Some(root), Some(current)) => root != current,
        (Some(_), None) => true,
        _ => false,
    }
}

fn skipped<P: FnMut(&ScanProgress)>(totals: &mut Totals, path: PathBuf, reason: &'static str, progress: &mut P) {
    totals.skipped = totals.skipped.saturating_add(1);
    progress(&ScanProgress { path, file_count: 0, directory_count: 0, skipped_count: 1, skip_reason: Some(reason), counted_size_bytes: 0, logical_size_bytes: 0, allocated_size_bytes: None, file_identity: None, volume_identity: None, modified_at: None });
}

fn scan_entry<C, P>(entry: DirEntry, relative_path: PathBuf, root: &Path, root_volume_identity: Option<&str>, control: &mut C, progress: &mut P, seen_files: &SeenFileStore) -> Result<Totals, String>
where C: FnMut() -> bool, P: FnMut(&ScanProgress) {
    let mut totals = Totals::default();
    let mut current = Some((relative_path, entry));
    let mut stack = Vec::new();
    loop {
        let (parent, entry) = match current.take() {
            Some(value) => value,
            None => match next_entry(&mut stack) {
                Some((parent, Ok(entry))) => (parent, entry),
                Some((parent, Err(()))) => { skipped(&mut totals, root.join(parent), "directory_entry_unreadable", progress); continue; }
                None => break,
            },
        };
        if !control() { return Err("スキャンはキャンセルされました".to_owned()) }
        let name = entry.file_name();
        let relative = parent.join(&name);
        let path = root.join(&relative);
        let file_type = match entry.file_type() {
            Ok(value) => value,
            Err(_) => { skipped(&mut totals, path, "metadata_unavailable", progress); continue; }
        };
        if file_type.is_symlink() {
            skipped(&mut totals, path, "link_not_followed", progress);
        } else if file_type.is_file() {
            let file = match entry.open() {
                Ok(value) => value.into_std(),
                Err(_) => { skipped(&mut totals, path, "file_snapshot_unavailable", progress); continue; }
            };
            let metrics = match file_metrics::collect_open_file(&file) {
                Some(value) => value,
                None => { skipped(&mut totals, path, "file_snapshot_unavailable", progress); continue; }
            };
            let key = metrics.volume_identity.as_ref().zip(metrics.file_identity.as_ref()).map(|(volume, file)| format!("{volume}:{file}"));
            let duplicate = seen_files.is_duplicate(key)?;
            totals.files = totals.files.saturating_add(1);
            if duplicate { totals.hard_link_duplicates = totals.hard_link_duplicates.saturating_add(1); } else {
                totals.size = totals.size.saturating_add(metrics.logical_size);
                if let Some(value) = metrics.allocated_size { totals.allocated = totals.allocated.saturating_add(value); }
                totals.sparse_files = totals.sparse_files.saturating_add(u64::from(metrics.is_sparse));
                totals.compressed_files = totals.compressed_files.saturating_add(u64::from(metrics.is_compressed));
            }
            progress(&ScanProgress { path, file_count: 1, directory_count: 0, skipped_count: 0, skip_reason: None, counted_size_bytes: if duplicate { 0 } else { metrics.logical_size }, logical_size_bytes: metrics.logical_size, allocated_size_bytes: metrics.allocated_size, file_identity: metrics.file_identity, volume_identity: metrics.volume_identity, modified_at: metrics.modified_at });
        } else if file_type.is_dir() {
            let directory = match entry.open_dir() {
                Ok(value) => value,
                Err(_) => { skipped(&mut totals, path, "directory_replaced_or_unreadable", progress); continue; }
            };
            let std_directory = directory.into_std_file();
            let volume_identity = file_metrics::volume_identity_from_open_file(&std_directory);
            if crosses_volume(root_volume_identity, volume_identity.as_deref()) {
                skipped(&mut totals, path, if volume_identity.is_some() { "different_volume" } else { "volume_identity_unavailable" }, progress);
                continue;
            }
            let directory = Dir::from_std_file(std_directory);
            let entries = match directory.entries() {
                Ok(value) => value,
                Err(_) => { skipped(&mut totals, path, "directory_unreadable", progress); continue; }
            };
            totals.directories = totals.directories.saturating_add(1);
            progress(&ScanProgress { path, file_count: 0, directory_count: 1, skipped_count: 0, skip_reason: None, counted_size_bytes: 0, logical_size_bytes: 0, allocated_size_bytes: None, file_identity: None, volume_identity, modified_at: None });
            stack.push(DirectoryFrame { relative_path: relative, entries });
        } else {
            skipped(&mut totals, path, "unsupported_entry_type", progress);
        }
    }
    Ok(totals)
}

fn retain_largest(entries: &mut Vec<ScanEntry>, entry: ScanEntry) -> bool {
    entries.push(entry);
    if entries.len() <= MAX_SUMMARY_ENTRIES { return false }
    let smallest = entries.iter().enumerate().min_by_key(|(_, entry)| entry.size_bytes).map(|(index, _)| index).unwrap_or(0);
    entries.swap_remove(smallest);
    true
}

pub fn scan_folder_path_controlled<C, P>(path: &Path, mut control: C, mut progress: P) -> Result<ScanSummary, String>
where C: FnMut() -> bool, P: FnMut(&ScanProgress) {
    if !path.is_absolute() { return Err("スキャン対象には絶対パスを指定してください".to_owned()) }
    let root = path.canonicalize().map_err(|error| format!("スキャン対象を開けません: {error}"))?;
    if !root.is_dir() { return Err("スキャン対象はフォルダである必要があります".to_owned()) }
    let root_directory = Dir::open_ambient_dir(&root, ambient_authority()).map_err(|error| format!("スキャン対象を開けません: {error}"))?;
    let std_root = root_directory.into_std_file();
    let root_volume_identity = file_metrics::volume_identity_from_open_file(&std_root);
    let root_directory = Dir::from_std_file(std_root);
    let children = root_directory.entries().map_err(|error| format!("スキャン対象を読み取れません: {error}"))?;
    let started = Instant::now();
    let mut entries = Vec::with_capacity(MAX_SUMMARY_ENTRIES);
    let mut totals = Totals::default();
    let mut entries_truncated = false;
    let seen_files = SeenFileStore::new()?;
    for child in children {
        if !control() { return Err("スキャンはキャンセルされました".to_owned()) }
        let child = match child {
            Ok(value) => value,
            Err(_) => { skipped(&mut totals, root.clone(), "directory_entry_unreadable", &mut progress); continue; }
        };
        let name = child.file_name();
        let child_path = root.join(&name);
        let is_directory = child.file_type().is_ok_and(|value| value.is_dir());
        let item = scan_entry(child, PathBuf::new(), &root, root_volume_identity.as_deref(), &mut control, &mut progress, &seen_files)?;
        entries_truncated |= retain_largest(&mut entries, ScanEntry { name: name.to_string_lossy().into_owned(), path: child_path.to_string_lossy().into_owned(), size_bytes: item.size, allocated_size_bytes: item.allocated, file_count: item.files, directory_count: item.directories, skipped_count: item.skipped, hard_link_duplicate_count: item.hard_link_duplicates, sparse_file_count: item.sparse_files, compressed_file_count: item.compressed_files, is_directory });
        totals.include(&item);
    }
    entries.sort_by_key(|entry| Reverse(entry.size_bytes));
    Ok(ScanSummary { root_path: root.to_string_lossy().into_owned(), total_size_bytes: totals.size, allocated_size_bytes: totals.allocated, file_count: totals.files, directory_count: totals.directories, skipped_count: totals.skipped, hard_link_duplicate_count: totals.hard_link_duplicates, sparse_file_count: totals.sparse_files, compressed_file_count: totals.compressed_files, elapsed_milliseconds: started.elapsed().as_millis(), entries, entries_truncated })
}

pub fn scan_folder_path(path: &Path) -> Result<ScanSummary, String> { scan_folder_path_controlled(path, || true, |_| {}) }

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    fn temporary_directory(name: &str) -> PathBuf { let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(); let path = std::env::temp_dir().join(format!("disk-visualizer-{name}-{unique}")); fs::create_dir_all(&path).unwrap(); path }
    #[test] fn scans_files_and_nested_directories(){let root=temporary_directory("scan");let nested=root.join("projects");fs::create_dir(&nested).unwrap();fs::write(root.join("note.txt"),b"1234").unwrap();fs::write(nested.join("video.bin"),[0_u8;8]).unwrap();let summary=scan_folder_path(&root).unwrap();assert_eq!(summary.total_size_bytes,12);assert_eq!(summary.file_count,2);fs::remove_dir_all(root).unwrap();}
    #[cfg(unix)] #[test] fn deduplicates_hard_links(){let root=temporary_directory("hard-links");let original=root.join("original.bin");let linked=root.join("linked.bin");fs::write(&original,[0_u8;16]).unwrap();fs::hard_link(&original,&linked).unwrap();let summary=scan_folder_path(&root).unwrap();assert_eq!(summary.total_size_bytes,16);assert_eq!(summary.hard_link_duplicate_count,1);fs::remove_dir_all(root).unwrap();}
    #[test] fn fails_closed_when_volume_identity_is_missing(){assert!(crosses_volume(Some("root"),None));assert!(crosses_volume(Some("root"),Some("other")));assert!(!crosses_volume(Some("root"),Some("root")));}
    #[cfg(unix)] #[test] fn does_not_follow_symbolic_links(){use std::os::unix::fs::symlink;let root=temporary_directory("symbolic-link");let outside=temporary_directory("target");fs::write(outside.join("outside.bin"),[0_u8;16]).unwrap();symlink(&outside,root.join("linked-directory")).unwrap();let summary=scan_folder_path(&root).unwrap();assert_eq!(summary.total_size_bytes,0);assert_eq!(summary.skipped_count,1);fs::remove_dir_all(root).unwrap();fs::remove_dir_all(outside).unwrap();}
    #[test] fn limits_summary_entries(){let root=temporary_directory("bounded");for index in 0..(MAX_SUMMARY_ENTRIES+25){fs::write(root.join(format!("{index}.bin")),[0_u8;1]).unwrap();}let summary=scan_folder_path(&root).unwrap();assert_eq!(summary.entries.len(),MAX_SUMMARY_ENTRIES);assert!(summary.entries_truncated);fs::remove_dir_all(root).unwrap();}
}
