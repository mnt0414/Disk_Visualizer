use std::fs::{File, Metadata};
use std::path::Path;
use std::time::UNIX_EPOCH;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileMetrics {
    pub logical_size: u64,
    pub allocated_size: Option<u64>,
    pub file_identity: Option<String>,
    pub volume_identity: Option<String>,
    pub modified_at: Option<i64>,
    pub is_sparse: bool,
    pub is_compressed: bool,
}

pub fn modified_at(metadata: &Metadata) -> Option<i64> {
    let seconds = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    i64::try_from(seconds).ok()
}

pub fn is_non_followed_link(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        return metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(unix)]
pub fn volume_identity(_path: &Path, metadata: &Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.dev().to_string())
}

#[cfg(windows)]
pub fn volume_identity(path: &Path, metadata: &Metadata) -> Option<String> {
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;
    let snapshot = windows_snapshot(path)?;
    let opened_is_directory = snapshot.information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    (opened_is_directory == metadata.is_dir())
        .then(|| snapshot.information.dwVolumeSerialNumber.to_string())
}

#[cfg(not(any(unix, windows)))]
pub fn volume_identity(_path: &Path, _metadata: &Metadata) -> Option<String> {
    None
}

#[cfg(unix)]
pub fn volume_identity_from_open_file(file: &File) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    Some(file.metadata().ok()?.dev().to_string())
}

#[cfg(windows)]
pub fn volume_identity_from_open_file(file: &File) -> Option<String> {
    Some(
        windows_snapshot_from_file(file)?
            .information
            .dwVolumeSerialNumber
            .to_string(),
    )
}

#[cfg(not(any(unix, windows)))]
pub fn volume_identity_from_open_file(_file: &File) -> Option<String> {
    None
}

#[cfg(unix)]
fn metrics_from_metadata(metadata: &Metadata) -> FileMetrics {
    use std::os::unix::fs::MetadataExt;
    let allocated_size = metadata.blocks().saturating_mul(512);
    let logical_size = metadata.len();
    FileMetrics {
        logical_size,
        allocated_size: Some(allocated_size),
        file_identity: Some(metadata.ino().to_string()),
        volume_identity: Some(metadata.dev().to_string()),
        modified_at: modified_at(metadata),
        is_sparse: allocated_size < logical_size,
        is_compressed: false,
    }
}

#[cfg(unix)]
pub fn collect(_path: &Path, metadata: &Metadata) -> Option<FileMetrics> {
    Some(metrics_from_metadata(metadata))
}

#[cfg(unix)]
pub fn collect_open_file(file: &File) -> Option<FileMetrics> {
    Some(metrics_from_metadata(&file.metadata().ok()?))
}

#[cfg(windows)]
struct WindowsSnapshot {
    information: windows_sys::Win32::Storage::FileSystem::BY_HANDLE_FILE_INFORMATION,
    allocated_size: Option<u64>,
    modified_at: Option<i64>,
}

#[cfg(windows)]
fn windows_snapshot_from_file(file: &File) -> Option<WindowsSnapshot> {
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileStandardInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT, FILE_STANDARD_INFO,
    };
    let handle = file.as_raw_handle() as _;
    // SAFETY: these are correctly sized Windows output buffers initialized before FFI writes.
    let mut information = unsafe { zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    // SAFETY: `file` owns a valid handle and `information` is writable for the full structure.
    if unsafe { GetFileInformationByHandle(handle, &mut information) } == 0 {
        return None;
    }
    if information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return None;
    }
    // SAFETY: this is a correctly sized output buffer for FileStandardInfo.
    let mut standard = unsafe { zeroed::<FILE_STANDARD_INFO>() };
    // SAFETY: the info class, output pointer, and byte count match FILE_STANDARD_INFO.
    let succeeded = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileStandardInfo,
            &mut standard as *mut _ as *mut c_void,
            size_of::<FILE_STANDARD_INFO>() as u32,
        )
    };
    let allocated_size = (succeeded != 0)
        .then(|| u64::try_from(standard.AllocationSize).ok())
        .flatten();
    Some(WindowsSnapshot {
        information,
        allocated_size,
        modified_at: file.metadata().ok().and_then(|value| modified_at(&value)),
    })
}

#[cfg(windows)]
fn windows_snapshot(path: &Path) -> Option<WindowsSnapshot> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let file = OpenOptions::new()
        .read(true)
        .access_mode(0)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .ok()?;
    windows_snapshot_from_file(&file)
}

#[cfg(windows)]
fn metrics_from_snapshot(snapshot: WindowsSnapshot) -> Option<FileMetrics> {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_COMPRESSED, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_SPARSE_FILE,
    };
    let attributes = snapshot.information.dwFileAttributes;
    if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        return None;
    }
    let logical_size = ((snapshot.information.nFileSizeHigh as u64) << 32)
        | snapshot.information.nFileSizeLow as u64;
    let file_index = ((snapshot.information.nFileIndexHigh as u64) << 32)
        | snapshot.information.nFileIndexLow as u64;
    Some(FileMetrics {
        logical_size,
        allocated_size: snapshot.allocated_size,
        file_identity: Some(file_index.to_string()),
        volume_identity: Some(snapshot.information.dwVolumeSerialNumber.to_string()),
        modified_at: snapshot.modified_at,
        is_sparse: attributes & FILE_ATTRIBUTE_SPARSE_FILE != 0,
        is_compressed: attributes & FILE_ATTRIBUTE_COMPRESSED != 0,
    })
}

#[cfg(windows)]
pub fn collect(path: &Path, _metadata: &Metadata) -> Option<FileMetrics> {
    metrics_from_snapshot(windows_snapshot(path)?)
}

#[cfg(windows)]
pub fn collect_open_file(file: &File) -> Option<FileMetrics> {
    metrics_from_snapshot(windows_snapshot_from_file(file)?)
}

#[cfg(not(any(unix, windows)))]
pub fn collect(_path: &Path, metadata: &Metadata) -> Option<FileMetrics> {
    Some(FileMetrics {
        logical_size: metadata.len(),
        allocated_size: Some(metadata.len()),
        modified_at: modified_at(metadata),
        ..FileMetrics::default()
    })
}

#[cfg(not(any(unix, windows)))]
pub fn collect_open_file(file: &File) -> Option<FileMetrics> {
    let metadata = file.metadata().ok()?;
    collect(Path::new(""), &metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    #[test]
    fn rejects_unavailable_handle_snapshots() {
        let missing = std::env::temp_dir().join(format!(
            "disk-visualizer-missing-snapshot-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&missing);
        assert!(windows_snapshot(&missing).is_none());
    }
    #[cfg(any(unix, windows))]
    #[test]
    fn collects_from_verified_open_handle() {
        let file_path = std::env::temp_dir().join(format!(
            "disk-visualizer-open-handle-{}",
            std::process::id()
        ));
        std::fs::write(&file_path, [0_u8; 16]).unwrap();
        let file = File::open(&file_path).unwrap();
        let metrics = collect_open_file(&file).unwrap();
        assert_eq!(metrics.logical_size, 16);
        assert!(metrics.file_identity.is_some());
        assert!(metrics.volume_identity.is_some());
        std::fs::remove_file(file_path).unwrap();
    }
    #[cfg(any(unix, windows))]
    #[test]
    fn resolves_volume_identity_for_directories() {
        let root = std::env::temp_dir().join(format!(
            "disk-visualizer-volume-identity-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let file = File::open(&root).unwrap();
        assert!(volume_identity_from_open_file(&file).is_some());
        std::fs::remove_dir_all(root).unwrap();
    }
}
