use crate::change_history::HistoryToken;
use serde::Serialize;
use std::path::Path;

const FLAG_MUST_SCAN_SUBDIRS: u32 = 0x0000_0001;
const FLAG_USER_DROPPED: u32 = 0x0000_0002;
const FLAG_KERNEL_DROPPED: u32 = 0x0000_0004;
const FLAG_EVENT_IDS_WRAPPED: u32 = 0x0000_0008;
const FLAG_ROOT_CHANGED: u32 = 0x0000_0020;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FseventsEvent {
    pub event_id: u64,
    pub flags: u32,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FseventsFallbackReason {
    UserDropped,
    KernelDropped,
    EventIdsWrapped,
    RootChanged,
    InvalidEventId,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum FseventsBatchDecision {
    Incremental { next_event_id: u64 },
    RescanSubtrees { next_event_id: u64 },
    FullScan { reason: FseventsFallbackReason },
}

pub fn evaluate_batch(
    checkpoint_event_id: u64,
    events: &[FseventsEvent],
) -> FseventsBatchDecision {
    if checkpoint_event_id == 0 || checkpoint_event_id == u64::MAX {
        return FseventsBatchDecision::FullScan {
            reason: FseventsFallbackReason::InvalidEventId,
        };
    }

    for event in events {
        let reason = if event.flags & FLAG_ROOT_CHANGED != 0 {
            Some(FseventsFallbackReason::RootChanged)
        } else if event.flags & FLAG_EVENT_IDS_WRAPPED != 0 {
            Some(FseventsFallbackReason::EventIdsWrapped)
        } else if event.flags & FLAG_KERNEL_DROPPED != 0 {
            Some(FseventsFallbackReason::KernelDropped)
        } else if event.flags & FLAG_USER_DROPPED != 0 {
            Some(FseventsFallbackReason::UserDropped)
        } else {
            None
        };
        if let Some(reason) = reason {
            return FseventsBatchDecision::FullScan { reason };
        }
    }

    let mut next_event_id = checkpoint_event_id;
    let mut rescan_subtrees = false;
    for event in events {
        if event.event_id == 0
            || event.event_id == u64::MAX
            || event.event_id < next_event_id
        {
            return FseventsBatchDecision::FullScan {
                reason: FseventsFallbackReason::InvalidEventId,
            };
        }
        next_event_id = event.event_id;
        rescan_subtrees |= event.flags & FLAG_MUST_SCAN_SUBDIRS != 0;
    }

    if rescan_subtrees {
        FseventsBatchDecision::RescanSubtrees { next_event_id }
    } else {
        FseventsBatchDecision::Incremental { next_event_id }
    }
}

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

    #[test]
    fn advances_safe_batches_and_preserves_empty_checkpoint() {
        assert_eq!(
            evaluate_batch(10, &[]),
            FseventsBatchDecision::Incremental { next_event_id: 10 }
        );
        assert_eq!(
            evaluate_batch(
                10,
                &[
                    FseventsEvent {
                        event_id: 11,
                        flags: 0,
                    },
                    FseventsEvent {
                        event_id: 12,
                        flags: 0,
                    },
                ],
            ),
            FseventsBatchDecision::Incremental { next_event_id: 12 }
        );
    }

    #[test]
    fn requests_subtree_rescan_for_coalesced_changes() {
        assert_eq!(
            evaluate_batch(
                10,
                &[FseventsEvent {
                    event_id: 11,
                    flags: FLAG_MUST_SCAN_SUBDIRS,
                }],
            ),
            FseventsBatchDecision::RescanSubtrees { next_event_id: 11 }
        );
    }

    #[test]
    fn falls_back_for_dropped_wrapped_and_root_changed_events() {
        for (flags, reason) in [
            (FLAG_USER_DROPPED, FseventsFallbackReason::UserDropped),
            (FLAG_KERNEL_DROPPED, FseventsFallbackReason::KernelDropped),
            (
                FLAG_EVENT_IDS_WRAPPED,
                FseventsFallbackReason::EventIdsWrapped,
            ),
            (FLAG_ROOT_CHANGED, FseventsFallbackReason::RootChanged),
        ] {
            assert_eq!(
                evaluate_batch(
                    10,
                    &[FseventsEvent {
                        event_id: if flags == FLAG_ROOT_CHANGED { 0 } else { 11 },
                        flags,
                    }],
                ),
                FseventsBatchDecision::FullScan { reason }
            );
        }
    }

    #[test]
    fn rejects_invalid_or_regressing_event_ids() {
        for events in [
            vec![FseventsEvent {
                event_id: 0,
                flags: 0,
            }],
            vec![FseventsEvent {
                event_id: u64::MAX,
                flags: 0,
            }],
            vec![
                FseventsEvent {
                    event_id: 12,
                    flags: 0,
                },
                FseventsEvent {
                    event_id: 11,
                    flags: 0,
                },
            ],
        ] {
            assert_eq!(
                evaluate_batch(10, &events),
                FseventsBatchDecision::FullScan {
                    reason: FseventsFallbackReason::InvalidEventId,
                }
            );
        }
        assert_eq!(
            evaluate_batch(0, &[]),
            FseventsBatchDecision::FullScan {
                reason: FseventsFallbackReason::InvalidEventId,
            }
        );
    }

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
