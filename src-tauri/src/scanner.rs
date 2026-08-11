use serde::Serialize;
use std::cmp::Reverse;
use std::fs::{self, ReadDir};
use std::path::Path;
use std::time::Instant;
const MAX_SUMMARY_ENTRIES: usize = 200;
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScanEntry {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub file_count: u64,
    pub directory_count: u64,
    pub skipped_count: u64,
    pub is_directory: bool,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScanSummary {
    pub root_path: String,
    pub total_size_bytes: u64,
    pub file_count: u64,
    pub directory_count: u64,
    pub skipped_count: u64,
    pub elapsed_milliseconds: u128,
    pub entries: Vec<ScanEntry>,
    pub entries_truncated: bool,
}
#[derive(Default)]
struct Totals {
    size: u64,
    files: u64,
    directories: u64,
    skipped: u64,
}
impl Totals {
    fn include(&mut self, other: &Self) {
        self.size = self.size.saturating_add(other.size);
        self.files = self.files.saturating_add(other.files);
        self.directories = self.directories.saturating_add(other.directories);
        self.skipped = self.skipped.saturating_add(other.skipped);
    }
}
fn next_path(stack: &mut Vec<ReadDir>) -> Option<Result<std::path::PathBuf, ()>> {
    loop {
        let children = stack.last_mut()?;
        match children.next() {
            Some(Ok(child)) => return Some(Ok(child.path())),
            Some(Err(_)) => return Some(Err(())),
            None => {
                stack.pop();
            }
        }
    }
}
fn scan_entry<C, P>(path: &Path, control: &mut C, progress: &mut P) -> Result<Totals, String>
where
    C: FnMut() -> bool,
    P: FnMut(&Path, u64, u64, u64, u64),
{
    let mut totals = Totals::default();
    let mut current = Some(path.to_path_buf());
    let mut stack: Vec<ReadDir> = Vec::new();
    loop {
        let path = match current.take() {
            Some(path) => path,
            None => match next_path(&mut stack) {
                Some(Ok(path)) => path,
                Some(Err(())) => {
                    totals.skipped = totals.skipped.saturating_add(1);
                    continue;
                }
                None => break,
            },
        };
        if !control() {
            return Err("スキャンはキャンセルされました".to_owned());
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => {
                totals.skipped = totals.skipped.saturating_add(1);
                progress(&path, 0, 0, 1, 0);
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            totals.skipped = totals.skipped.saturating_add(1);
            progress(&path, 0, 0, 1, 0);
        } else if metadata.is_file() {
            totals.size = totals.size.saturating_add(metadata.len());
            totals.files = totals.files.saturating_add(1);
            progress(&path, 1, 0, 0, metadata.len());
        } else if metadata.is_dir() {
            totals.directories = totals.directories.saturating_add(1);
            progress(&path, 0, 1, 0, 0);
            match fs::read_dir(&path) {
                Ok(children) => stack.push(children),
                Err(_) => {
                    totals.skipped = totals.skipped.saturating_add(1);
                    progress(&path, 0, 0, 1, 0);
                }
            }
        } else {
            totals.skipped = totals.skipped.saturating_add(1);
            progress(&path, 0, 0, 1, 0);
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
    P: FnMut(&Path, u64, u64, u64, u64),
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
    let started = Instant::now();
    let children =
        fs::read_dir(&root).map_err(|error| format!("スキャン対象を読み取れません: {error}"))?;
    let mut entries = Vec::with_capacity(MAX_SUMMARY_ENTRIES);
    let mut totals = Totals::default();
    let mut entries_truncated = false;
    for child in children {
        if !control() {
            return Err("スキャンはキャンセルされました".to_owned());
        }
        let child = match child {
            Ok(child) => child,
            Err(_) => {
                totals.skipped = totals.skipped.saturating_add(1);
                progress(&root, 0, 0, 1, 0);
                continue;
            }
        };
        let child_path = child.path();
        let item = scan_entry(&child_path, &mut control, &mut progress)?;
        let is_directory =
            fs::symlink_metadata(&child_path).is_ok_and(|metadata| metadata.is_dir());
        entries_truncated |= retain_largest(
            &mut entries,
            ScanEntry {
                name: child.file_name().to_string_lossy().into_owned(),
                path: child_path.to_string_lossy().into_owned(),
                size_bytes: item.size,
                file_count: item.files,
                directory_count: item.directories,
                skipped_count: item.skipped,
                is_directory,
            },
        );
        totals.include(&item);
    }
    entries.sort_by_key(|entry| Reverse(entry.size_bytes));
    Ok(ScanSummary {
        root_path: root.to_string_lossy().into_owned(),
        total_size_bytes: totals.size,
        file_count: totals.files,
        directory_count: totals.directories,
        skipped_count: totals.skipped,
        elapsed_milliseconds: started.elapsed().as_millis(),
        entries,
        entries_truncated,
    })
}
pub fn scan_folder_path(path: &Path) -> Result<ScanSummary, String> {
    scan_folder_path_controlled(path, || true, |_, _, _, _, _| {})
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};
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
        assert_eq!(summary.entries[0].name, "projects");
        assert!(!summary.entries_truncated);
        fs::remove_dir_all(root).unwrap();
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
    #[test]
    fn rejects_relative_paths() {
        assert!(scan_folder_path(Path::new("relative/path"))
            .unwrap_err()
            .contains("絶対パス"));
    }
}
