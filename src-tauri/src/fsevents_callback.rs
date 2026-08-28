use crate::macos_fsevents::FseventsEvent;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

const FLAG_HISTORY_DONE: u32 = 0x0000_0010;
const DEFAULT_MAX_CHANGES: usize = 4_096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectedFseventsChange {
    pub relative_path: PathBuf,
    pub event: FseventsEvent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FseventsCallbackFailure {
    MalformedBatch,
    InvalidPath,
    CapacityExceeded,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectedFseventsBatch {
    pub changes: Vec<CollectedFseventsChange>,
    pub history_done: bool,
}

#[derive(Debug)]
struct CallbackState {
    changes: Vec<CollectedFseventsChange>,
    history_done: bool,
    failure: Option<FseventsCallbackFailure>,
}

#[derive(Debug)]
pub struct FseventsCallbackCollector {
    max_changes: usize,
    state: Mutex<CallbackState>,
}

impl FseventsCallbackCollector {
    pub fn new(max_changes: Option<usize>) -> Result<Self, String> {
        let max_changes = max_changes.unwrap_or(DEFAULT_MAX_CHANGES);
        if max_changes == 0 {
            return Err("FSEvents callbackの保持上限は1以上である必要があります".to_owned());
        }
        Ok(Self {
            max_changes,
            state: Mutex::new(CallbackState {
                changes: Vec::new(),
                history_done: false,
                failure: None,
            }),
        })
    }

    pub fn ingest(
        &self,
        paths: &[&[u8]],
        events: &[FseventsEvent],
    ) -> Result<(), FseventsCallbackFailure> {
        if paths.len() != events.len() {
            return self.fail(FseventsCallbackFailure::MalformedBatch);
        }
        let mut state = self.state.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(failure) = state.failure {
            return Err(failure);
        }
        for (path, event) in paths.iter().zip(events) {
            if event.flags & FLAG_HISTORY_DONE != 0 {
                state.history_done = true;
                continue;
            }
            let relative_path = match validated_relative_path(path) {
                Ok(path) => path,
                Err(failure) => {
                    state.changes.clear();
                    state.failure = Some(failure);
                    return Err(failure);
                }
            };
            if state.changes.len() >= self.max_changes {
                state.changes.clear();
                state.failure = Some(FseventsCallbackFailure::CapacityExceeded);
                return Err(FseventsCallbackFailure::CapacityExceeded);
            }
            state.changes.push(CollectedFseventsChange {
                relative_path,
                event: *event,
            });
        }
        Ok(())
    }

    pub fn snapshot(&self) -> Result<CollectedFseventsBatch, FseventsCallbackFailure> {
        let state = self.state.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(failure) = state.failure {
            return Err(failure);
        }
        Ok(CollectedFseventsBatch {
            changes: state.changes.clone(),
            history_done: state.history_done,
        })
    }

    fn fail(&self, failure: FseventsCallbackFailure) -> Result<(), FseventsCallbackFailure> {
        let mut state = self.state.lock().unwrap_or_else(|poison| poison.into_inner());
        state.changes.clear();
        state.failure = Some(failure);
        Err(failure)
    }
}

fn validated_relative_path(bytes: &[u8]) -> Result<PathBuf, FseventsCallbackFailure> {
    if bytes.is_empty() || bytes.contains(&0) {
        return Err(FseventsCallbackFailure::InvalidPath);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| FseventsCallbackFailure::InvalidPath)?;
    let path = Path::new(text);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(FseventsCallbackFailure::InvalidPath);
    }
    Ok(path.to_path_buf())
}

#[cfg(target_os = "macos")]
pub mod native {
    use super::*;
    use std::ffi::{c_void, CStr};

    pub type FseventStreamRef = *mut c_void;

    /// Receives one CoreServices FSEvents callback batch.
    ///
    /// # Safety
    ///
    /// `info` must point to a live `FseventsCallbackCollector`. For nonzero
    /// `count`, all three callback arrays must be valid for `count` elements
    /// and every path pointer must reference a NUL-terminated C string.
    pub unsafe extern "C" fn callback(
        _stream: FseventStreamRef,
        info: *mut c_void,
        count: usize,
        event_paths: *mut c_void,
        event_flags: *const u32,
        event_ids: *const u64,
    ) {
        if info.is_null() {
            return;
        }
        let collector = unsafe { &*(info as *const FseventsCallbackCollector) };
        if count == 0 {
            return;
        }
        if event_paths.is_null() || event_flags.is_null() || event_ids.is_null() {
            let _ = collector.fail(FseventsCallbackFailure::MalformedBatch);
            return;
        }
        let paths = unsafe {
            std::slice::from_raw_parts(event_paths as *const *const i8, count)
        };
        let flags = unsafe { std::slice::from_raw_parts(event_flags, count) };
        let ids = unsafe { std::slice::from_raw_parts(event_ids, count) };
        let mut path_bytes = Vec::with_capacity(count);
        let mut events = Vec::with_capacity(count);
        for index in 0..count {
            let Some(path) = paths.get(index).copied().filter(|path| !path.is_null()) else {
                let _ = collector.fail(FseventsCallbackFailure::MalformedBatch);
                return;
            };
            path_bytes.push(unsafe { CStr::from_ptr(path) }.to_bytes());
            events.push(FseventsEvent {
                event_id: ids[index],
                flags: flags[index],
            });
        }
        let _ = collector.ingest(&path_bytes, &events);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(event_id: u64, flags: u32) -> FseventsEvent {
        FseventsEvent { event_id, flags }
    }

    #[test]
    fn collects_relative_changes_and_history_marker() {
        let collector = FseventsCallbackCollector::new(Some(4)).unwrap();
        collector
            .ingest(
                &[b"Users/example/file.txt", b"."],
                &[event(11, 0), event(12, FLAG_HISTORY_DONE)],
            )
            .unwrap();
        assert_eq!(
            collector.snapshot().unwrap(),
            CollectedFseventsBatch {
                changes: vec![CollectedFseventsChange {
                    relative_path: PathBuf::from("Users/example/file.txt"),
                    event: event(11, 0),
                }],
                history_done: true,
            }
        );
    }

    #[test]
    fn rejects_malformed_and_ambiguous_paths() {
        let malformed = FseventsCallbackCollector::new(Some(4)).unwrap();
        assert_eq!(
            malformed.ingest(&[b"one"], &[]),
            Err(FseventsCallbackFailure::MalformedBatch)
        );
        for path in [
            b"/absolute".as_slice(),
            b"../escape".as_slice(),
            b"with\0nul".as_slice(),
            &[0xff],
        ] {
            let collector = FseventsCallbackCollector::new(Some(4)).unwrap();
            assert_eq!(
                collector.ingest(&[path], &[event(11, 0)]),
                Err(FseventsCallbackFailure::InvalidPath)
            );
            assert_eq!(
                collector.snapshot(),
                Err(FseventsCallbackFailure::InvalidPath)
            );
        }
    }

    #[test]
    fn invalidates_and_clears_on_capacity_overflow() {
        let collector = FseventsCallbackCollector::new(Some(1)).unwrap();
        collector.ingest(&[b"one"], &[event(11, 0)]).unwrap();
        assert_eq!(
            collector.ingest(&[b"two"], &[event(12, 0)]),
            Err(FseventsCallbackFailure::CapacityExceeded)
        );
        assert_eq!(
            collector.snapshot(),
            Err(FseventsCallbackFailure::CapacityExceeded)
        );
    }

    #[test]
    fn rejects_zero_capacity() {
        assert!(FseventsCallbackCollector::new(Some(0)).is_err());
    }
}
