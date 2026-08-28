use crate::fsevents_callback::CollectedFseventsChange;
use crate::macos_fsevents::FseventsBatchDecision;
use std::path::Path;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FseventsHistoryRead {
    pub changes: Vec<CollectedFseventsChange>,
    pub decision: FseventsBatchDecision,
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use crate::fsevents_callback::{FseventsCallbackCollector, FseventsCallbackFailure};
    use crate::macos_fsevents::{evaluate_batch, FseventsEvent};
    use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
    use objc2_core_foundation::{CFArray, CFRetained, CFString};
    use objc2_core_services::{
        kFSEventStreamCreateFlagFileEvents, kFSEventStreamCreateFlagNoDefer,
        kFSEventStreamCreateFlagWatchRoot, ConstFSEventStreamRef, FSEventStreamContext,
        FSEventStreamCreateRelativeToDevice, FSEventStreamEventFlags, FSEventStreamEventId,
        FSEventStreamInvalidate, FSEventStreamRef, FSEventStreamRelease,
        FSEventStreamSetDispatchQueue, FSEventStreamStart, FSEventStreamStop,
    };
    use std::ffi::{c_void, CStr, CString, OsStr};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    use std::path::PathBuf;
    use std::ptr::NonNull;
    use std::time::Instant;

    struct NativeHistoryStream {
        raw: FSEventStreamRef,
        started: bool,
        queue: DispatchRetained<DispatchQueue>,
        _collector: Box<FseventsCallbackCollector>,
        _paths: CFRetained<CFArray<CFString>>,
    }

    impl Drop for NativeHistoryStream {
        fn drop(&mut self) {
            unsafe {
                if self.started {
                    FSEventStreamStop(self.raw);
                    self.started = false;
                }
                FSEventStreamInvalidate(self.raw);
            }
            self.queue.exec_sync(|| {});
            unsafe { FSEventStreamRelease(self.raw) };
        }
    }

    unsafe extern "C-unwind" fn callback(
        _stream: ConstFSEventStreamRef,
        info: *mut c_void,
        event_count: usize,
        event_paths: NonNull<c_void>,
        event_flags: NonNull<FSEventStreamEventFlags>,
        event_ids: NonNull<FSEventStreamEventId>,
    ) {
        let Some(collector) = (unsafe { (info as *const FseventsCallbackCollector).as_ref() })
        else {
            return;
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let paths = unsafe {
                std::slice::from_raw_parts(
                    event_paths.as_ptr().cast::<*const std::ffi::c_char>(),
                    event_count,
                )
            };
            let flags = unsafe { std::slice::from_raw_parts(event_flags.as_ptr(), event_count) };
            let ids = unsafe { std::slice::from_raw_parts(event_ids.as_ptr(), event_count) };
            let mut path_bytes = Vec::with_capacity(event_count);
            let mut events = Vec::with_capacity(event_count);
            for index in 0..event_count {
                let path = paths[index];
                if path.is_null() {
                    return Err(FseventsCallbackFailure::MalformedBatch);
                }
                path_bytes.push(unsafe { CStr::from_ptr(path) }.to_bytes());
                events.push(FseventsEvent {
                    event_id: ids[index],
                    flags: flags[index],
                });
            }
            collector.ingest(&path_bytes, &events)
        }));
        if result.is_err() {
            let _ = collector.ingest(&[b"panic"], &[]);
        }
    }

    pub(super) fn read_history(
        root: &Path,
        checkpoint_event_id: u64,
        max_changes: Option<usize>,
        timeout: Duration,
    ) -> Result<FseventsHistoryRead, String> {
        if checkpoint_event_id == 0 || checkpoint_event_id == u64::MAX {
            return Err("FSEvents checkpoint event IDが不正です".to_owned());
        }
        if timeout.is_zero() {
            return Err("FSEvents履歴待機時間は0より大きい必要があります".to_owned());
        }
        let canonical_root = root
            .canonicalize()
            .map_err(|error| format!("FSEventsの走査rootを解決できません: {error}"))?;
        if !canonical_root.is_dir() {
            return Err("FSEventsの走査rootはdirectoryである必要があります".to_owned());
        }
        let metadata = std::fs::metadata(&canonical_root)
            .map_err(|error| format!("FSEventsの走査rootを確認できません: {error}"))?;
        let device: libc::dev_t = metadata
            .dev()
            .try_into()
            .map_err(|_| "FSEventsのdevice IDが対応範囲外です".to_owned())?;
        let mount_point = mount_point(&canonical_root)?;
        let relative_root = device_relative_root(&canonical_root, &mount_point)?;
        let native_root = if relative_root.is_empty() {
            "."
        } else {
            &relative_root
        };
        let path = CFString::from_str(native_root);
        let paths = CFArray::from_retained_objects(&[path]);
        let collector = Box::new(FseventsCallbackCollector::new(max_changes)?);
        let mut context = FSEventStreamContext {
            version: 0,
            info: (&*collector as *const FseventsCallbackCollector)
                .cast_mut()
                .cast(),
            retain: None,
            release: None,
            copyDescription: None,
        };
        let raw = unsafe {
            FSEventStreamCreateRelativeToDevice(
                None,
                Some(callback),
                &mut context,
                device,
                paths.as_opaque(),
                checkpoint_event_id,
                0.05,
                kFSEventStreamCreateFlagFileEvents
                    | kFSEventStreamCreateFlagNoDefer
                    | kFSEventStreamCreateFlagWatchRoot,
            )
        };
        if raw.is_null() {
            return Err("FSEvents履歴streamを作成できません".to_owned());
        }
        let queue = DispatchQueue::new(
            "com.disk-visualizer.fsevents-history",
            DispatchQueueAttr::SERIAL,
        );
        unsafe { FSEventStreamSetDispatchQueue(raw, Some(&queue)) };
        let mut stream = NativeHistoryStream {
            raw,
            started: false,
            queue,
            _collector: collector,
            _paths: paths,
        };
        if !unsafe { FSEventStreamStart(raw) } {
            return Err("FSEvents履歴streamを開始できません".to_owned());
        }
        stream.started = true;

        let deadline = Instant::now() + timeout;
        loop {
            let snapshot = stream
                ._collector
                .snapshot()
                .map_err(callback_failure_message)?;
            if snapshot.history_done {
                let events = snapshot
                    .changes
                    .iter()
                    .map(|change| change.event)
                    .collect::<Vec<_>>();
                return Ok(FseventsHistoryRead {
                    changes: snapshot.changes,
                    decision: evaluate_batch(checkpoint_event_id, &events),
                });
            }
            if Instant::now() >= deadline {
                return Err("FSEvents履歴がHistoryDoneまでに完了しませんでした".to_owned());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn callback_failure_message(failure: FseventsCallbackFailure) -> String {
        format!("FSEvents callback batchを安全に収集できません: {failure:?}")
    }

    fn mount_point(root: &Path) -> Result<PathBuf, String> {
        let root_c = CString::new(root.as_os_str().as_bytes())
            .map_err(|_| "FSEventsの走査rootにNULが含まれています".to_owned())?;
        let mut info = std::mem::MaybeUninit::<libc::statfs>::uninit();
        if unsafe { libc::statfs(root_c.as_ptr(), info.as_mut_ptr()) } != 0 {
            return Err(format!(
                "FSEventsのmount pointを取得できません: {}",
                std::io::Error::last_os_error()
            ));
        }
        let info = unsafe { info.assume_init() };
        let bytes = unsafe { CStr::from_ptr(info.f_mntonname.as_ptr()) }.to_bytes();
        let mount = PathBuf::from(OsStr::from_bytes(bytes));
        if root.starts_with(&mount) {
            Ok(mount)
        } else {
            Ok(PathBuf::from("/"))
        }
    }

    fn device_relative_root(root: &Path, mount_point: &Path) -> Result<String, String> {
        let relative = root.strip_prefix(mount_point).map_err(|_| {
            format!(
                "FSEventsの走査root {} をmount point {} から相対化できません",
                root.display(),
                mount_point.display()
            )
        })?;
        relative
            .to_str()
            .map(|path| path.trim_matches('/').to_owned())
            .ok_or_else(|| "FSEventsの走査rootはUTF-8で表現できる必要があります".to_owned())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn creates_device_relative_paths_without_parent_escape() {
            assert_eq!(
                device_relative_root(Path::new("/Volumes/Data/work"), Path::new("/Volumes/Data"))
                    .unwrap(),
                "work"
            );
            assert_eq!(
                device_relative_root(Path::new("/"), Path::new("/")).unwrap(),
                ""
            );
            assert!(device_relative_root(Path::new("/other"), Path::new("/Volumes/Data")).is_err());
        }
    }
}

pub fn read_history(
    root: &Path,
    checkpoint_event_id: u64,
    max_changes: Option<usize>,
    timeout: Duration,
) -> Result<FseventsHistoryRead, String> {
    #[cfg(target_os = "macos")]
    {
        platform::read_history(root, checkpoint_event_id, max_changes, timeout)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (root, checkpoint_event_id, max_changes, timeout);
        Err("FSEvents履歴streamはmacOSでのみ利用できます".to_owned())
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn reports_unsupported_platform() {
        assert!(read_history(Path::new("/"), 1, Some(4), Duration::from_secs(1)).is_err());
    }
}
