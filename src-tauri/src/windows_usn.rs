use crate::change_history::AvailableHistory;
use std::path::Path;

#[cfg(windows)]
#[repr(C)]
struct UsnJournalDataV0 {
    journal_id: u64,
    first_usn: i64,
    next_usn: i64,
    lowest_valid_usn: i64,
    max_usn: i64,
    maximum_size: u64,
    allocation_delta: u64,
}

#[cfg(windows)]
fn volume_device_path(root: &Path) -> Result<Vec<u16>, String> {
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Component, Prefix};

    let mut components = root.components();
    let drive = match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => letter,
            _ => return Err("USN Journalはローカルdrive rootだけに対応しています".to_owned()),
        },
        _ => return Err("USN Journalには絶対drive pathが必要です".to_owned()),
    };
    if !matches!(components.next(), Some(Component::RootDir)) || components.next().is_some() {
        return Err("USN Journalの照会先はdrive rootである必要があります".to_owned());
    }
    let value = format!(r"\\.\{}:", char::from(drive).to_ascii_uppercase());
    Ok(std::ffi::OsStr::new(&value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect())
}

#[cfg(windows)]
pub fn query_available_history(root: &Path) -> Result<AvailableHistory, String> {
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_QUERY_USN_JOURNAL;

    const GENERIC_READ_ACCESS: u32 = 0x8000_0000;
    let device_path = volume_device_path(root)?;
    let handle = unsafe {
        CreateFileW(
            device_path.as_ptr(),
            GENERIC_READ_ACCESS,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(format!(
            "volumeを開けないためUSN Journalを確認できません: {}",
            std::io::Error::last_os_error()
        ));
    }

    let mut data = unsafe { zeroed::<UsnJournalDataV0>() };
    let mut returned = 0_u32;
    let succeeded = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_QUERY_USN_JOURNAL,
            std::ptr::null(),
            0,
            &mut data as *mut _ as *mut c_void,
            size_of::<UsnJournalDataV0>() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    unsafe { CloseHandle(handle) };
    if succeeded == 0 || returned < size_of::<UsnJournalDataV0>() as u32 {
        return Err(format!(
            "USN Journal metadataを取得できません: {}",
            std::io::Error::last_os_error()
        ));
    }
    if data.journal_id == 0 || data.lowest_valid_usn < 0 || data.lowest_valid_usn > data.next_usn {
        return Err("USN Journal metadataの範囲が不正です".to_owned());
    }
    Ok(AvailableHistory::Usn {
        journal_id: data.journal_id,
        lowest_valid_usn: data.lowest_valid_usn,
        next_usn: data.next_usn,
    })
}

#[cfg(not(windows))]
pub fn query_available_history(_root: &Path) -> Result<AvailableHistory, String> {
    Err("USN JournalはWindowsでのみ利用できます".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn accepts_only_local_drive_roots() {
        assert!(volume_device_path(Path::new(r"C:\")).is_ok());
        assert!(volume_device_path(Path::new(r"C:\Users")).is_err());
        assert!(volume_device_path(Path::new(r"\\server\share")).is_err());
        assert!(volume_device_path(Path::new("relative")).is_err());
    }

    #[cfg(not(windows))]
    #[test]
    fn reports_unsupported_platform() {
        assert!(query_available_history(Path::new("/")).is_err());
    }
}
