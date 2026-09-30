use crate::file_metrics;
use crate::incremental_rescan::IncrementalRescanTarget;
use crate::incremental_storage::IncrementalEntry;
use cap_std::ambient_authority;
use cap_std::fs::{Dir, DirEntry, FileType, ReadDir};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::cell::Cell;
use std::cmp::Reverse;
use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const MAX_SUMMARY_ENTRIES: usize = 200;
pub(crate) const CANCELLED_MESSAGE: &str = "スキャンはキャンセルされました";

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
        self.hard_link_duplicates = self
            .hard_link_duplicates
            .saturating_add(other.hard_link_duplicates);
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
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "disk-visualizer-seen-{}-{unique}.sqlite3",
            std::process::id()
        ));
        let connection = Connection::open(&path)
            .map_err(|error| format!("重複判定用DBを開けません: {error}"))?;
        connection.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA temp_store=FILE; PRAGMA cache_size=-2048; CREATE TABLE seen_files (identity TEXT PRIMARY KEY) WITHOUT ROWID; BEGIN IMMEDIATE;").map_err(|error| format!("重複判定用DBを初期化できません: {error}"))?;
        Ok(Self { connection, path })
    }

    fn is_duplicate(&self, identity: Option<String>) -> Result<bool, String> {
        let Some(identity) = identity else {
            return Ok(false);
        };
        self.connection
            .execute(
                "INSERT OR IGNORE INTO seen_files (identity) VALUES (?1)",
                params![identity],
            )
            .map(|changed| changed == 0)
            .map_err(|error| format!("ハードリンクを判定できません: {error}"))
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

/// 走査対象のentryを表す。フルスキャンでは親directoryの列挙結果、
/// 部分再走査では親Dir handleと名前の組を使い、以降の判定処理を共有する。
enum EntryHandle<'a> {
    Listed(DirEntry),
    Named { parent: &'a Dir, name: OsString },
}

impl EntryHandle<'_> {
    fn file_name(&self) -> OsString {
        match self {
            Self::Listed(entry) => entry.file_name(),
            Self::Named { name, .. } => name.clone(),
        }
    }

    fn file_type(&self) -> io::Result<FileType> {
        match self {
            Self::Listed(entry) => entry.file_type(),
            Self::Named { parent, name } => parent
                .symlink_metadata(name)
                .map(|metadata| metadata.file_type()),
        }
    }

    fn open(&self) -> io::Result<cap_std::fs::File> {
        match self {
            Self::Listed(entry) => entry.open(),
            Self::Named { parent, name } => parent.open(name),
        }
    }

    fn open_dir(&self) -> io::Result<Dir> {
        match self {
            Self::Listed(entry) => entry.open_dir(),
            Self::Named { parent, name } => parent.open_dir(name),
        }
    }
}

fn next_entry(stack: &mut Vec<DirectoryFrame>) -> Option<(PathBuf, Result<DirEntry, ()>)> {
    loop {
        let frame = stack.last_mut()?;
        match frame.entries.next() {
            Some(Ok(entry)) => return Some((frame.relative_path.clone(), Ok(entry))),
            Some(Err(_)) => return Some((frame.relative_path.clone(), Err(()))),
            None => {
                stack.pop();
            }
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

fn skipped<P: FnMut(&ScanProgress)>(
    totals: &mut Totals,
    path: PathBuf,
    reason: &'static str,
    progress: &mut P,
) {
    totals.skipped = totals.skipped.saturating_add(1);
    progress(&ScanProgress {
        path,
        file_count: 0,
        directory_count: 0,
        skipped_count: 1,
        skip_reason: Some(reason),
        counted_size_bytes: 0,
        logical_size_bytes: 0,
        allocated_size_bytes: None,
        file_identity: None,
        volume_identity: None,
        modified_at: None,
    });
}

fn scan_entry<C, P>(
    entry: EntryHandle<'_>,
    relative_path: PathBuf,
    root: &Path,
    root_volume_identity: Option<&str>,
    control: &mut C,
    progress: &mut P,
    seen_files: &SeenFileStore,
) -> Result<Totals, String>
where
    C: FnMut() -> bool,
    P: FnMut(&ScanProgress),
{
    let mut totals = Totals::default();
    let mut current = Some((relative_path, entry));
    let mut stack = Vec::new();
    loop {
        let (parent, entry) = match current.take() {
            Some(value) => value,
            None => match next_entry(&mut stack) {
                Some((parent, Ok(entry))) => (parent, EntryHandle::Listed(entry)),
                Some((parent, Err(()))) => {
                    skipped(
                        &mut totals,
                        root.join(parent),
                        "directory_entry_unreadable",
                        progress,
                    );
                    continue;
                }
                None => break,
            },
        };
        if !control() {
            return Err(CANCELLED_MESSAGE.to_owned());
        }
        let name = entry.file_name();
        let relative = parent.join(&name);
        let path = root.join(&relative);
        let file_type = match entry.file_type() {
            Ok(value) => value,
            Err(_) => {
                skipped(&mut totals, path, "metadata_unavailable", progress);
                continue;
            }
        };
        if file_type.is_symlink() {
            skipped(&mut totals, path, "link_not_followed", progress);
        } else if file_type.is_file() {
            let file = match entry.open() {
                Ok(value) => value.into_std(),
                Err(_) => {
                    skipped(&mut totals, path, "file_snapshot_unavailable", progress);
                    continue;
                }
            };
            let metrics = match file_metrics::collect_open_file(&file) {
                Some(value) => value,
                None => {
                    skipped(&mut totals, path, "file_snapshot_unavailable", progress);
                    continue;
                }
            };
            let key = metrics
                .volume_identity
                .as_ref()
                .zip(metrics.file_identity.as_ref())
                .map(|(volume, file)| format!("{volume}:{file}"));
            let duplicate = seen_files.is_duplicate(key)?;
            totals.files = totals.files.saturating_add(1);
            if duplicate {
                totals.hard_link_duplicates = totals.hard_link_duplicates.saturating_add(1);
            } else {
                totals.size = totals.size.saturating_add(metrics.logical_size);
                if let Some(value) = metrics.allocated_size {
                    totals.allocated = totals.allocated.saturating_add(value);
                }
                totals.sparse_files = totals
                    .sparse_files
                    .saturating_add(u64::from(metrics.is_sparse));
                totals.compressed_files = totals
                    .compressed_files
                    .saturating_add(u64::from(metrics.is_compressed));
            }
            progress(&ScanProgress {
                path,
                file_count: 1,
                directory_count: 0,
                skipped_count: 0,
                skip_reason: None,
                counted_size_bytes: if duplicate { 0 } else { metrics.logical_size },
                logical_size_bytes: metrics.logical_size,
                allocated_size_bytes: metrics.allocated_size,
                file_identity: metrics.file_identity,
                volume_identity: metrics.volume_identity,
                modified_at: metrics.modified_at,
            });
        } else if file_type.is_dir() {
            let directory = match entry.open_dir() {
                Ok(value) => value,
                Err(_) => {
                    skipped(
                        &mut totals,
                        path,
                        "directory_replaced_or_unreadable",
                        progress,
                    );
                    continue;
                }
            };
            let std_directory = directory.into_std_file();
            let volume_identity = file_metrics::volume_identity_from_open_file(&std_directory);
            if crosses_volume(root_volume_identity, volume_identity.as_deref()) {
                skipped(
                    &mut totals,
                    path,
                    if volume_identity.is_some() {
                        "different_volume"
                    } else {
                        "volume_identity_unavailable"
                    },
                    progress,
                );
                continue;
            }
            let directory = Dir::from_std_file(std_directory);
            let entries = match directory.entries() {
                Ok(value) => value,
                Err(_) => {
                    skipped(&mut totals, path, "directory_unreadable", progress);
                    continue;
                }
            };
            totals.directories = totals.directories.saturating_add(1);
            progress(&ScanProgress {
                path,
                file_count: 0,
                directory_count: 1,
                skipped_count: 0,
                skip_reason: None,
                counted_size_bytes: 0,
                logical_size_bytes: 0,
                allocated_size_bytes: None,
                file_identity: None,
                volume_identity,
                modified_at: None,
            });
            stack.push(DirectoryFrame {
                relative_path: relative,
                entries,
            });
        } else {
            skipped(&mut totals, path, "unsupported_entry_type", progress);
        }
    }
    Ok(totals)
}

fn retain_largest(entries: &mut Vec<ScanEntry>, entry: ScanEntry) -> bool {
    entries.push(entry);
    if entries.len() <= MAX_SUMMARY_ENTRIES {
        return false;
    }
    let smallest = entries
        .iter()
        .enumerate()
        .min_by_key(|(_, entry)| entry.size_bytes)
        .map(|(index, _)| index)
        .unwrap_or(0);
    entries.swap_remove(smallest);
    true
}

pub fn scan_folder_path_controlled<C, P>(
    path: &Path,
    mut control: C,
    mut progress: P,
) -> Result<ScanSummary, String>
where
    C: FnMut() -> bool,
    P: FnMut(&ScanProgress),
{
    if !path.is_absolute() {
        return Err("スキャン対象には絶対パスを指定してください".to_owned());
    }
    let root = path
        .canonicalize()
        .map_err(|error| format!("スキャン対象を開けません: {error}"))?;
    if !root.is_dir() {
        return Err("スキャン対象はフォルダである必要があります".to_owned());
    }
    let root_directory = Dir::open_ambient_dir(&root, ambient_authority())
        .map_err(|error| format!("スキャン対象を開けません: {error}"))?;
    let std_root = root_directory.into_std_file();
    let root_volume_identity = file_metrics::volume_identity_from_open_file(&std_root);
    let root_directory = Dir::from_std_file(std_root);
    let children = root_directory
        .entries()
        .map_err(|error| format!("スキャン対象を読み取れません: {error}"))?;
    let started = Instant::now();
    let mut entries = Vec::with_capacity(MAX_SUMMARY_ENTRIES);
    let mut totals = Totals::default();
    let mut entries_truncated = false;
    let seen_files = SeenFileStore::new()?;
    for child in children {
        if !control() {
            return Err(CANCELLED_MESSAGE.to_owned());
        }
        let child = match child {
            Ok(value) => value,
            Err(_) => {
                skipped(
                    &mut totals,
                    root.clone(),
                    "directory_entry_unreadable",
                    &mut progress,
                );
                continue;
            }
        };
        let name = child.file_name();
        let child_path = root.join(&name);
        let is_directory = child.file_type().is_ok_and(|value| value.is_dir());
        let item = scan_entry(
            EntryHandle::Listed(child),
            PathBuf::new(),
            &root,
            root_volume_identity.as_deref(),
            &mut control,
            &mut progress,
            &seen_files,
        )?;
        entries_truncated |= retain_largest(
            &mut entries,
            ScanEntry {
                name: name.to_string_lossy().into_owned(),
                path: child_path.to_string_lossy().into_owned(),
                size_bytes: item.size,
                allocated_size_bytes: item.allocated,
                file_count: item.files,
                directory_count: item.directories,
                skipped_count: item.skipped,
                hard_link_duplicate_count: item.hard_link_duplicates,
                sparse_file_count: item.sparse_files,
                compressed_file_count: item.compressed_files,
                is_directory,
            },
        );
        totals.include(&item);
    }
    entries.sort_by_key(|entry| Reverse(entry.size_bytes));
    Ok(ScanSummary {
        root_path: root.to_string_lossy().into_owned(),
        total_size_bytes: totals.size,
        allocated_size_bytes: totals.allocated,
        file_count: totals.files,
        directory_count: totals.directories,
        skipped_count: totals.skipped,
        hard_link_duplicate_count: totals.hard_link_duplicates,
        sparse_file_count: totals.sparse_files,
        compressed_file_count: totals.compressed_files,
        elapsed_milliseconds: started.elapsed().as_millis(),
        entries,
        entries_truncated,
    })
}

pub fn scan_folder_path(path: &Path) -> Result<ScanSummary, String> {
    scan_folder_path_controlled(path, || true, |_| {})
}

/// 部分再走査の結果。`Scanned`以外は差分更新を安全に続行できないことを表し、
/// 呼び出し側はフルスキャンへ戻す。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TargetScanOutcome {
    Scanned(Vec<IncrementalEntry>),
    UnsafeTarget,
    UnreadableDirectoryEntry,
    TooManyEntries,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TargetScanAbort {
    UnreadableDirectoryEntry,
    TooManyEntries,
}

enum TargetParent {
    Absent,
    Unsafe,
    Found(Dir),
}

fn incremental_entry(progress: &ScanProgress) -> Option<IncrementalEntry> {
    if progress.file_count == 0 && progress.directory_count == 0 && progress.skipped_count == 0 {
        return None;
    }
    Some(IncrementalEntry {
        path: progress.path.clone(),
        file_count: progress.file_count,
        directory_count: progress.directory_count,
        skipped_count: progress.skipped_count,
        skip_reason: progress.skip_reason,
        counted_size_bytes: progress.counted_size_bytes,
        logical_size_bytes: progress.logical_size_bytes,
        allocated_size_bytes: progress.allocated_size_bytes,
        file_identity: progress.file_identity.clone(),
        volume_identity: progress.volume_identity.clone(),
        modified_at: progress.modified_at,
    })
}

fn target_components(relative_path: &Path) -> Option<Vec<OsString>> {
    if relative_path.as_os_str().is_empty() {
        return None;
    }
    let mut components = Vec::new();
    for component in relative_path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => components.push(value.to_owned()),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(components)
}

/// rootから対象の親directoryまで、symlinkを辿らず同一volume上の実directoryだけを降りる。
/// 途中が存在しなければ対象は削除済み(`Absent`)、symlink・非directory・別volume・
/// volume不明・読取不能ならfail closed(`Unsafe`)とする。
fn descend_to_parent(
    root_directory: &Dir,
    root_volume_identity: Option<&str>,
    parents: &[OsString],
) -> Result<TargetParent, String> {
    let mut current = root_directory
        .try_clone()
        .map_err(|error| format!("スキャン対象を開けません: {error}"))?;
    for component in parents {
        let metadata = match current.symlink_metadata(component) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(TargetParent::Absent)
            }
            Err(_) => return Ok(TargetParent::Unsafe),
        };
        if !metadata.file_type().is_dir() {
            return Ok(TargetParent::Unsafe);
        }
        let next = match current.open_dir(component) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(TargetParent::Absent)
            }
            Err(_) => return Ok(TargetParent::Unsafe),
        };
        let std_next = next.into_std_file();
        let volume_identity = file_metrics::volume_identity_from_open_file(&std_next);
        let same_volume = matches!(
            (root_volume_identity, volume_identity.as_deref()),
            (Some(root), Some(candidate)) if root == candidate
        );
        if !same_volume {
            return Ok(TargetParent::Unsafe);
        }
        current = Dir::from_std_file(std_next);
    }
    Ok(TargetParent::Found(current))
}

/// 指定targetだけをcapability handle経由で再帰走査し、フルスキャンと同じ規則の
/// `IncrementalEntry`を返す。走査は読み取り専用で、symlinkを辿らず、volumeを跨がない。
/// 存在しないtargetは何も出力しない(=削除)。
pub(crate) fn scan_targets_controlled<C, P>(
    root: &Path,
    targets: &[IncrementalRescanTarget],
    max_entries: usize,
    mut control: C,
    mut progress: P,
) -> Result<TargetScanOutcome, String>
where
    C: FnMut() -> bool,
    P: FnMut(&ScanProgress),
{
    if !root.is_absolute() {
        return Err("スキャン対象には絶対パスを指定してください".to_owned());
    }
    let root = root
        .canonicalize()
        .map_err(|error| format!("スキャン対象を開けません: {error}"))?;
    if !root.is_dir() {
        return Err("スキャン対象はフォルダである必要があります".to_owned());
    }
    let root_directory = Dir::open_ambient_dir(&root, ambient_authority())
        .map_err(|error| format!("スキャン対象を開けません: {error}"))?;
    let std_root = root_directory.into_std_file();
    let root_volume_identity = file_metrics::volume_identity_from_open_file(&std_root);
    let root_directory = Dir::from_std_file(std_root);
    let seen_files = SeenFileStore::new()?;
    let abort: Cell<Option<TargetScanAbort>> = Cell::new(None);
    let mut replacements: Vec<IncrementalEntry> = Vec::new();
    let mut guarded_control = || abort.get().is_none() && control();
    let mut collecting_progress = |event: &ScanProgress| {
        if event.skip_reason == Some("directory_entry_unreadable") {
            abort.set(Some(TargetScanAbort::UnreadableDirectoryEntry));
        }
        if let Some(entry) = incremental_entry(event) {
            replacements.push(entry);
            if replacements.len() > max_entries && abort.get().is_none() {
                abort.set(Some(TargetScanAbort::TooManyEntries));
            }
        }
        progress(event);
    };
    let mut result = Ok(());
    for target in targets {
        if !guarded_control() {
            result = Err(CANCELLED_MESSAGE.to_owned());
            break;
        }
        let Some(components) = target_components(&target.relative_path) else {
            return Ok(TargetScanOutcome::UnsafeTarget);
        };
        let scanned = if let Some((name, parents)) = components.split_last() {
            match descend_to_parent(&root_directory, root_volume_identity.as_deref(), parents)? {
                TargetParent::Unsafe => return Ok(TargetScanOutcome::UnsafeTarget),
                TargetParent::Absent => Ok(()),
                TargetParent::Found(parent) => {
                    if let Err(error) = parent.symlink_metadata(name) {
                        if error.kind() == io::ErrorKind::NotFound {
                            continue;
                        }
                    }
                    scan_entry(
                        EntryHandle::Named {
                            parent: &parent,
                            name: name.clone(),
                        },
                        parents.iter().collect(),
                        &root,
                        root_volume_identity.as_deref(),
                        &mut guarded_control,
                        &mut collecting_progress,
                        &seen_files,
                    )
                    .map(|_| ())
                }
            }
        } else {
            scan_root_children(
                &root_directory,
                &root,
                root_volume_identity.as_deref(),
                &mut guarded_control,
                &mut collecting_progress,
                &seen_files,
                &abort,
            )
        };
        if let Err(error) = scanned {
            result = Err(error);
            break;
        }
    }
    match (abort.get(), result) {
        (Some(TargetScanAbort::UnreadableDirectoryEntry), _) => {
            Ok(TargetScanOutcome::UnreadableDirectoryEntry)
        }
        (Some(TargetScanAbort::TooManyEntries), _) => Ok(TargetScanOutcome::TooManyEntries),
        (None, Err(error)) => Err(error),
        (None, Ok(())) => Ok(TargetScanOutcome::Scanned(replacements)),
    }
}

fn scan_root_children<C, P>(
    root_directory: &Dir,
    root: &Path,
    root_volume_identity: Option<&str>,
    control: &mut C,
    progress: &mut P,
    seen_files: &SeenFileStore,
    abort: &Cell<Option<TargetScanAbort>>,
) -> Result<(), String>
where
    C: FnMut() -> bool,
    P: FnMut(&ScanProgress),
{
    let children = root_directory
        .entries()
        .map_err(|error| format!("スキャン対象を読み取れません: {error}"))?;
    for child in children {
        if !control() {
            return Err(CANCELLED_MESSAGE.to_owned());
        }
        let Ok(child) = child else {
            abort.set(Some(TargetScanAbort::UnreadableDirectoryEntry));
            return Ok(());
        };
        scan_entry(
            EntryHandle::Listed(child),
            PathBuf::new(),
            root,
            root_volume_identity,
            control,
            progress,
            seen_files,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    fn temporary_directory(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("disk-visualizer-{name}-{unique}"));
        fs::create_dir_all(&path).unwrap();
        path
    }
    #[test]
    fn scans_files_and_nested_directories() {
        let root = temporary_directory("scan");
        let nested = root.join("projects");
        fs::create_dir(&nested).unwrap();
        fs::write(root.join("note.txt"), b"1234").unwrap();
        fs::write(nested.join("video.bin"), [0_u8; 8]).unwrap();
        let summary = scan_folder_path(&root).unwrap();
        assert_eq!(summary.total_size_bytes, 12);
        assert_eq!(summary.file_count, 2);
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn deduplicates_hard_links() {
        let root = temporary_directory("hard-links");
        let original = root.join("original.bin");
        let linked = root.join("linked.bin");
        fs::write(&original, [0_u8; 16]).unwrap();
        fs::hard_link(&original, &linked).unwrap();
        let summary = scan_folder_path(&root).unwrap();
        assert_eq!(summary.total_size_bytes, 16);
        assert_eq!(summary.hard_link_duplicate_count, 1);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn fails_closed_when_volume_identity_is_missing() {
        assert!(crosses_volume(Some("root"), None));
        assert!(crosses_volume(Some("root"), Some("other")));
        assert!(!crosses_volume(Some("root"), Some("root")));
    }
    #[cfg(unix)]
    #[test]
    fn does_not_follow_symbolic_links() {
        use std::os::unix::fs::symlink;
        let root = temporary_directory("symbolic-link");
        let outside = temporary_directory("target");
        fs::write(outside.join("outside.bin"), [0_u8; 16]).unwrap();
        symlink(&outside, root.join("linked-directory")).unwrap();
        let summary = scan_folder_path(&root).unwrap();
        assert_eq!(summary.total_size_bytes, 0);
        assert_eq!(summary.skipped_count, 1);
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }
    #[test]
    fn limits_summary_entries() {
        let root = temporary_directory("bounded");
        for index in 0..(MAX_SUMMARY_ENTRIES + 25) {
            fs::write(root.join(format!("{index}.bin")), [0_u8; 1]).unwrap();
        }
        let summary = scan_folder_path(&root).unwrap();
        assert_eq!(summary.entries.len(), MAX_SUMMARY_ENTRIES);
        assert!(summary.entries_truncated);
        fs::remove_dir_all(root).unwrap();
    }
    fn target(path: &str) -> IncrementalRescanTarget {
        IncrementalRescanTarget {
            relative_path: path.split('/').collect(),
            recursive: true,
        }
    }
    fn scanned(root: &Path, targets: &[&str]) -> TargetScanOutcome {
        let targets = targets
            .iter()
            .map(|value| target(value))
            .collect::<Vec<_>>();
        scan_targets_controlled(root, &targets, usize::MAX, || true, |_| {}).unwrap()
    }
    fn scanned_paths(outcome: TargetScanOutcome, root: &Path) -> Vec<String> {
        let TargetScanOutcome::Scanned(entries) = outcome else {
            panic!("部分再走査が完了していません: {outcome:?}");
        };
        let mut paths = entries
            .iter()
            .map(|entry| {
                entry
                    .path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/")
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }
    #[test]
    fn scans_only_requested_targets_recursively() {
        let root = temporary_directory("partial").canonicalize().unwrap();
        fs::create_dir_all(root.join("dir").join("sub")).unwrap();
        fs::write(root.join("skip.bin"), [0_u8; 3]).unwrap();
        fs::write(root.join("dir").join("a.bin"), [0_u8; 2]).unwrap();
        fs::write(root.join("dir").join("sub").join("b.bin"), [0_u8; 4]).unwrap();
        fs::write(root.join("other.bin"), [0_u8; 5]).unwrap();
        let paths = scanned_paths(scanned(&root, &["dir", "other.bin"]), &root);
        assert_eq!(
            paths,
            ["dir", "dir/a.bin", "dir/sub", "dir/sub/b.bin", "other.bin"]
        );
        let nested = scanned_paths(scanned(&root, &["dir/sub/b.bin"]), &root);
        assert_eq!(nested, ["dir/sub/b.bin"]);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn dot_target_scans_every_child_of_root() {
        let root = temporary_directory("partial-root").canonicalize().unwrap();
        fs::create_dir(root.join("dir")).unwrap();
        fs::write(root.join("dir").join("a.bin"), [0_u8; 2]).unwrap();
        fs::write(root.join("b.bin"), [0_u8; 4]).unwrap();
        let paths = scanned_paths(scanned(&root, &["."]), &root);
        assert_eq!(paths, ["b.bin", "dir", "dir/a.bin"]);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn absent_targets_produce_no_entries() {
        let root = temporary_directory("partial-absent")
            .canonicalize()
            .unwrap();
        fs::write(root.join("kept.bin"), [0_u8; 2]).unwrap();
        let outcome = scanned(&root, &["missing", "missing/child", "kept.bin/../x"]);
        // ".."を含むtargetは経路として不正なのでfail closedになる。
        assert_eq!(outcome, TargetScanOutcome::UnsafeTarget);
        let outcome = scanned(&root, &["missing", "missing/child/deep"]);
        assert_eq!(outcome, TargetScanOutcome::Scanned(Vec::new()));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn rejects_non_directory_intermediate_component() {
        let root = temporary_directory("partial-file").canonicalize().unwrap();
        fs::write(root.join("file.bin"), [0_u8; 2]).unwrap();
        assert_eq!(
            scanned(&root, &["file.bin/child"]),
            TargetScanOutcome::UnsafeTarget
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn stops_when_replacement_entries_exceed_limit() {
        let root = temporary_directory("partial-limit").canonicalize().unwrap();
        for index in 0..4 {
            fs::write(root.join(format!("{index}.bin")), [0_u8; 1]).unwrap();
        }
        let outcome = scan_targets_controlled(&root, &[target(".")], 3, || true, |_| {}).unwrap();
        assert_eq!(outcome, TargetScanOutcome::TooManyEntries);
        let outcome = scan_targets_controlled(&root, &[target(".")], 4, || true, |_| {}).unwrap();
        assert!(matches!(outcome, TargetScanOutcome::Scanned(_)));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn cancels_partial_scan_and_reports_progress() {
        let root = temporary_directory("partial-cancel")
            .canonicalize()
            .unwrap();
        for index in 0..4 {
            fs::write(root.join(format!("{index}.bin")), [0_u8; 1]).unwrap();
        }
        let mut seen = 0_u64;
        let mut allowed = 5_u32;
        let result = scan_targets_controlled(
            &root,
            &[target(".")],
            usize::MAX,
            || {
                allowed = allowed.saturating_sub(1);
                allowed > 0
            },
            |event| seen += event.file_count,
        );
        assert_eq!(result.unwrap_err(), CANCELLED_MESSAGE);
        assert!(seen >= 1);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn partial_scan_requires_absolute_existing_root() {
        assert!(
            scan_targets_controlled(Path::new("relative"), &[], usize::MAX, || true, |_| {})
                .is_err()
        );
    }
    #[cfg(unix)]
    #[test]
    fn partial_scan_does_not_follow_symbolic_links() {
        use std::os::unix::fs::symlink;
        let root = temporary_directory("partial-symlink")
            .canonicalize()
            .unwrap();
        let outside = temporary_directory("partial-outside");
        fs::write(outside.join("outside.bin"), [0_u8; 16]).unwrap();
        fs::create_dir(root.join("real")).unwrap();
        fs::write(root.join("real").join("in.bin"), [0_u8; 1]).unwrap();
        symlink(&outside, root.join("link")).unwrap();
        symlink(&outside, root.join("real").join("inner-link")).unwrap();
        let TargetScanOutcome::Scanned(entries) = scanned(&root, &["link", "real"]) else {
            panic!("部分再走査が完了していません");
        };
        let skipped = entries
            .iter()
            .filter(|entry| entry.skip_reason == Some("link_not_followed"))
            .count();
        assert_eq!(skipped, 2);
        assert!(!entries
            .iter()
            .any(|entry| entry.path.to_string_lossy().contains("outside.bin")));
        assert_eq!(
            scanned(&root, &["link/outside.bin"]),
            TargetScanOutcome::UnsafeTarget
        );
        assert_eq!(
            scanned(&root, &["real/inner-link/outside.bin"]),
            TargetScanOutcome::UnsafeTarget
        );
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }
}
