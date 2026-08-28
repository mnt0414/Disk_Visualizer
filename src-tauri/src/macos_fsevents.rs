use crate::change_history::HistoryToken;
use std::path::Path;

#[cfg(target_os = "macos")]
#[link(name = "CoreServices", kind = "framework")]
extern "C" {
    fn FSEventsGetLastEventIdForDeviceBeforeTime(device: i32, before_time: f64) -> u64;
}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFAbsoluteTimeGetCurrent() -> f64;
}

#[cfg(target_os = "macos")]
fn query_with(
    root: &Path,
    query_latest: impl FnOnce(i32, f64) -> u64,
) -> Result<HistoryToken, String> {
    use std::os::unix::fs::MetadataExt;

    if !root.is_absolute() {
        return Err("FSEventsには絶対pathが必要です".to_owned());
    }
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("FSEventsの走査rootを解決できません: {error}"))?;
    let metadata = std::fs::metadata(&canonical_root)
        .map_err(|error| format!("FSEventsの走査rootを確認できません: {error}"))?;
    if !metadata.is_dir() {
        return Err("FSEventsの走査rootはdirectoryである必要があります".to_owned());
    }
    let device = i32::try_from(metadata.dev())
        .map_err(|_| "FSEventsのdevice IDが対応範囲外です".to_owned())?;
    let now = unsafe { CFAbsoluteTimeGetCurrent() };
    if !now.is_finite() {
        return Err("FSEventsの現在時刻を取得できません".to_owned());
    }
    let event_id = query_latest(device, now);
    if event_id == 0 || event_id == u64::MAX {
        return Err("FSEventsの有効なevent IDを取得できません".to_owned());
    }
    Ok(HistoryToken::Fsevents { event_id })
}

#[cfg(target_os = "macos")]
pub fn query_checkpoint(root: &Path) -> Result<HistoryToken, String> {
    query_with(root, |device, before_time| unsafe {
        FSEventsGetLastEventIdForDeviceBeforeTime(device, before_time)
    })
}

#[cfg(not(target_os = "macos"))]
pub fn query_checkpoint(_root: &Path) -> Result<HistoryToken, String> {
    Err("FSEventsはmacOSでのみ利用できます".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn converts_device_event_id_to_checkpoint() {
        let checkpoint = query_with(Path::new("/"), |_device, _time| 42).unwrap();
        assert_eq!(checkpoint, HistoryToken::Fsevents { event_id: 42 });
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn rejects_relative_paths_and_invalid_event_ids() {
        assert!(query_with(Path::new("relative"), |_device, _time| 42).is_err());
        assert!(query_with(Path::new("/"), |_device, _time| 0).is_err());
        assert!(query_with(Path::new("/"), |_device, _time| u64::MAX).is_err());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn reports_unsupported_platform() {
        assert!(query_checkpoint(Path::new("/")).is_err());
    }
}
