use std::fs::Metadata;
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileMetrics {
    pub allocated_size: u64,
    pub identity: Option<String>,
    pub is_sparse: bool,
    pub is_compressed: bool,
}

#[cfg(unix)]
pub fn collect(_path: &Path, metadata: &Metadata) -> FileMetrics {
    use std::os::unix::fs::MetadataExt;

    let allocated_size = metadata.blocks().saturating_mul(512);
    let logical_size = metadata.len();
    FileMetrics {
        allocated_size,
        identity: Some(format!("{}:{}", metadata.dev(), metadata.ino())),
        is_sparse: allocated_size < logical_size,
        is_compressed: false,
    }
}

#[cfg(windows)]
pub fn collect(path: &Path, metadata: &Metadata) -> FileMetrics {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::{
        GetCompressedFileSizeW, FILE_ATTRIBUTE_COMPRESSED, FILE_ATTRIBUTE_SPARSE_FILE,
    };

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut high = 0_u32;
    let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
    let allocated_size = ((high as u64) << 32) | low as u64;
    let attributes = metadata.file_attributes();
    let identity = metadata
        .volume_serial_number()
        .zip(metadata.file_index())
        .map(|(volume, index)| format!("{volume}:{index}"));
    FileMetrics {
        allocated_size,
        identity,
        is_sparse: attributes & FILE_ATTRIBUTE_SPARSE_FILE != 0,
        is_compressed: attributes & FILE_ATTRIBUTE_COMPRESSED != 0,
    }
}

#[cfg(not(any(unix, windows)))]
pub fn collect(_path: &Path, metadata: &Metadata) -> FileMetrics {
    FileMetrics {
        allocated_size: metadata.len(),
        ..FileMetrics::default()
    }
}
