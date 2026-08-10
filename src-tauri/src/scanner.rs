use serde::Serialize;
use std::cmp::Reverse;
use std::fs;
use std::path::Path;
use std::time::Instant;

#[derive(Debug, Serialize, PartialEq, Eq)]
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

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScanSummary {
    pub root_path: String,
    pub total_size_bytes: u64,
    pub file_count: u64,
    pub directory_count: u64,
    pub skipped_count: u64,
    pub elapsed_milliseconds: u128,
    pub entries: Vec<ScanEntry>,
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

fn scan_entry(path: &Path) -> Totals {
    let mut totals = Totals::default();
    let mut pending = vec![path.to_path_buf()];

    while let Some(current) = pending.pop() {
        let metadata = match fs::symlink_metadata(current.as_path()) {
            Ok(metadata) => metadata,
            Err(_) => {
                totals.skipped = totals.skipped.saturating_add(1);
                continue;
            }
        };

        if metadata.file_type().is_symlink() {
            totals.skipped = totals.skipped.saturating_add(1);
        } else if metadata.is_file() {
            totals.size = totals.size.saturating_add(metadata.len());
            totals.files = totals.files.saturating_add(1);
        } else if metadata.is_dir() {
            totals.directories = totals.directories.saturating_add(1);
            match fs::read_dir(current.as_path()) {
                Ok(children) => {
                    for child in children {
                        match child {
                            Ok(child) => pending.push(child.path()),
                            Err(_) => totals.skipped = totals.skipped.saturating_add(1),
                        }
                    }
                }
                Err(_) => totals.skipped = totals.skipped.saturating_add(1),
            }
        } else {
            totals.skipped = totals.skipped.saturating_add(1);
        }
    }
    totals
}

pub fn scan_folder_path(path: &Path) -> Result<ScanSummary, String> {
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
    let children = fs::read_dir(root.as_path())
        .map_err(|error| format!("スキャン対象を読み取れません: {error}"))?;
    let mut entries = Vec::new();
    let mut totals = Totals::default();

    for child in children {
        let child = match child {
            Ok(child) => child,
            Err(_) => {
                totals.skipped = totals.skipped.saturating_add(1);
                continue;
            }
        };
        let child_path = child.path();
        let item = scan_entry(child_path.as_path());
        let is_directory =
            fs::symlink_metadata(child_path.as_path()).is_ok_and(|metadata| metadata.is_dir());
        entries.push(ScanEntry {
            name: child.file_name().to_string_lossy().into_owned(),
            path: child_path.to_string_lossy().into_owned(),
            size_bytes: item.size,
            file_count: item.files,
            directory_count: item.directories,
            skipped_count: item.skipped,
            is_directory,
        });
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_directory(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock must be valid")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("disk-visualizer-{name}-{unique}"));
        fs::create_dir_all(path.as_path()).expect("temporary directory should be created");
        path
    }

    #[test]
    fn scans_files_and_nested_directories() {
        let root = temporary_directory("scan");
        let nested = root.join("projects");
        fs::create_dir(nested.as_path()).expect("nested directory should be created");
        fs::write(root.join("note.txt"), b"1234").expect("root file should be written");
        fs::write(nested.join("video.bin"), [0_u8; 8]).expect("nested file should be written");

        let summary = scan_folder_path(root.as_path()).expect("folder scan should succeed");
        assert_eq!(summary.total_size_bytes, 12);
        assert_eq!(summary.file_count, 2);
        assert_eq!(summary.directory_count, 1);
        assert_eq!(summary.entries[0].name, "projects");
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }

    #[test]
    fn rejects_relative_paths() {
        let error = scan_folder_path(Path::new("relative/path"))
            .expect_err("relative paths must not be accepted");
        assert!(error.contains("絶対パス"));
    }

    #[test]
    fn rejects_file_roots() {
        let root = temporary_directory("file-root");
        let file = root.join("file.txt");
        fs::write(file.as_path(), b"data").expect("file should be written");
        let error = scan_folder_path(file.as_path()).expect_err("files must be rejected");
        assert!(error.contains("フォルダ"));
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }
}
