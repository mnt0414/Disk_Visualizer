use std::fs::Metadata;
use std::path::Path;
use std::time::UNIX_EPOCH;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileMetrics {
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
pub fn volume_identity(path: &Path, _metadata: &Metadata) -> Option<String> {
    file_identity(path).map(|(_, volume)| volume)
}

#[cfg(not(any(unix, windows)))]
pub fn volume_identity(_path: &Path, _metadata: &Metadata) -> Option<String> {
    None
}

#[cfg(unix)]
pub fn collect(_path: &Path, metadata: &Metadata) -> FileMetrics {
    use std::os::unix::fs::MetadataExt;

    let allocated_size = metadata.blocks().saturating_mul(512);
    let logical_size = metadata.len();
    FileMetrics {
        allocated_size: Some(allocated_size),
        file_identity: Some(metadata.ino().to_string()),
        volume_identity: Some(metadata.dev().to_string()),
        modified_at: modified_at(metadata),
        is_sparse: allocated_size < logical_size,
        is_compressed: false,
    }
}

#[cfg(windows)]
fn file_identity(path: &Path) -> Option<(String, String)> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let file = OpenOptions::new()
        .read(true)
        .access_mode(0)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .ok()?;
    let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    // SAFETY: `file` owns a valid Windows handle for the duration of the call, and
    // `information` points to writable, correctly sized storage.
    let succeeded =
        unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut information) };
    if succeeded == 0 {
        return None;
    }
    let file_index = ((information.nFileIndexHigh as u64) << 32) | information.nFileIndexLow as u64;
    Some((
        file_index.to_string(),
        information.dwVolumeSerialNumber.to_string(),
    ))
}

#[cfg(windows)]
fn compressed_file_size(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{GetLastError, SetLastError, ERROR_SUCCESS};
    use windows_sys::Win32::Storage::FileSystem::GetCompressedFileSizeW;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut high = 0_u32;
    // SAFETY: clearing the calling thread's last-error value is required to
    // distinguish a valid low-order `u32::MAX` result from API failure.
    unsafe { SetLastError(ERROR_SUCCESS) };
    // SAFETY: `wide` is NUL-terminated and remains alive for the call; `high` is a
    // valid writable pointer for the high-order result.
    let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
    // SAFETY: `GetLastError` reads thread-local state immediately after the API call.
    if low == u32::MAX && unsafe { GetLastError() } != ERROR_SUCCESS {
        return None;
    }
    Some(((high as u64) << 32) | low as u64)
}

#[cfg(windows)]
pub fn collect(path: &Path, metadata: &Metadata) -> FileMetrics {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_COMPRESSED, FILE_ATTRIBUTE_SPARSE_FILE,
    };

    let allocated_size = compressed_file_size(path);
    let attributes = metadata.file_attributes();
    let (file_identity, volume_identity) = file_identity(path)
        .map(|(file, volume)| (Some(file), Some(volume)))
        .unwrap_or_default();
    FileMetrics {
        allocated_size,
        file_identity,
        volume_identity,
        modified_at: modified_at(metadata),
        is_sparse: attributes & FILE_ATTRIBUTE_SPARSE_FILE != 0,
        is_compressed: attributes & FILE_ATTRIBUTE_COMPRESSED != 0,
    }
}

#[cfg(not(any(unix, windows)))]
pub fn collect(_path: &Path, metadata: &Metadata) -> FileMetrics {
    FileMetrics {
        allocated_size: Some(metadata.len()),
        modified_at: modified_at(metadata),
        ..FileMetrics::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn reports_missing_allocated_size_for_unavailable_path() {
        let missing = std::env::temp_dir().join(format!(
            "disk-visualizer-missing-allocated-size-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&missing);
        assert_eq!(compressed_file_size(&missing), None);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn resolves_volume_identity_for_directories() {
        let root = std::env::temp_dir().join(format!(
            "disk-visualizer-volume-identity-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let metadata = std::fs::symlink_metadata(&root).unwrap();

        assert!(volume_identity(&root, &metadata).is_some());

        std::fs::remove_dir_all(root).unwrap();
    }
}
