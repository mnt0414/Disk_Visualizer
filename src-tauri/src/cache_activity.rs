use crate::file_metrics;
use serde::Serialize;
use std::fs;
use std::path::Path;

/// Runtime state inferred from two metadata observations.
///
/// This deliberately reports only what Disk Visualizer can observe. It does
/// not claim that another process has a file open because that signal is not
/// portable between macOS and Windows.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CacheRuntimeState {
    Stable,
    Changing,
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CacheObservation {
    pub logical_size: Option<u64>,
    pub modified_at: Option<i64>,
    pub file_identity: Option<String>,
}

impl CacheObservation {
    fn has_signal(&self) -> bool {
        self.logical_size.is_some() || self.modified_at.is_some() || self.file_identity.is_some()
    }
}

/// Captures a read-only observation of a regular file.
///
/// Links, directories, missing paths, and unavailable handle snapshots return
/// `None` so callers can preserve `unknown` rather than infer a false state.
pub fn observe_path(path: &Path) -> Option<CacheObservation> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || file_metrics::is_non_followed_link(&metadata) {
        return None;
    }
    let metrics = file_metrics::collect(path, &metadata)?;
    let file_identity = match (metrics.volume_identity, metrics.file_identity) {
        (Some(volume), Some(file)) => Some(format!("{volume}:{file}")),
        (None, Some(file)) => Some(file),
        _ => None,
    };
    Some(CacheObservation {
        logical_size: Some(metrics.logical_size),
        modified_at: metrics.modified_at,
        file_identity,
    })
}

/// Compares observations captured around a scan operation.
///
/// Missing observations, or observations with no usable metadata, remain
/// unknown. A changed identity, size, or modification time is reported as
/// changing. Equal observable fields are reported as stable.
pub fn evaluate_runtime_state(
    before: Option<&CacheObservation>,
    after: Option<&CacheObservation>,
) -> CacheRuntimeState {
    let (Some(before), Some(after)) = (before, after) else {
        return CacheRuntimeState::Unknown;
    };
    if !before.has_signal() || !after.has_signal() {
        return CacheRuntimeState::Unknown;
    }
    if before.file_identity != after.file_identity
        || before.logical_size != after.logical_size
        || before.modified_at != after.modified_at
    {
        CacheRuntimeState::Changing
    } else {
        CacheRuntimeState::Stable
    }
}

/// Re-observes a path after an earlier observation and evaluates its state.
pub fn evaluate_path_after(
    path: &Path,
    before: Option<&CacheObservation>,
) -> CacheRuntimeState {
    let after = observe_path(path);
    evaluate_runtime_state(before, after.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn observation() -> CacheObservation {
        CacheObservation {
            logical_size: Some(1024),
            modified_at: Some(1_723_700_000),
            file_identity: Some("volume-7:file-42".to_owned()),
        }
    }

    fn temporary_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "disk-visualizer-cache-activity-{name}-{}-{unique}",
            std::process::id()
        ));
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn reports_stable_for_equal_observations() {
        let before = observation();
        let after = before.clone();
        assert_eq!(
            evaluate_runtime_state(Some(&before), Some(&after)),
            CacheRuntimeState::Stable
        );
    }

    #[test]
    fn reports_changing_when_size_changes() {
        let before = observation();
        let mut after = before.clone();
        after.logical_size = Some(2048);
        assert_eq!(
            evaluate_runtime_state(Some(&before), Some(&after)),
            CacheRuntimeState::Changing
        );
    }

    #[test]
    fn reports_changing_when_identity_changes() {
        let before = observation();
        let mut after = before.clone();
        after.file_identity = Some("volume-7:file-43".to_owned());
        assert_eq!(
            evaluate_runtime_state(Some(&before), Some(&after)),
            CacheRuntimeState::Changing
        );
    }

    #[test]
    fn reports_unknown_when_an_observation_is_missing() {
        let before = observation();
        assert_eq!(
            evaluate_runtime_state(Some(&before), None),
            CacheRuntimeState::Unknown
        );
    }

    #[test]
    fn reports_unknown_without_usable_metadata() {
        let empty = CacheObservation::default();
        assert_eq!(
            evaluate_runtime_state(Some(&empty), Some(&empty)),
            CacheRuntimeState::Unknown
        );
    }

    #[test]
    fn observes_regular_files_without_reading_contents() {
        let path = temporary_file("observe", b"1234");
        let observed = observe_path(&path).expect("regular file observation");
        assert_eq!(observed.logical_size, Some(4));
        assert!(observed.modified_at.is_some());
        #[cfg(any(unix, windows))]
        assert!(observed.file_identity.is_some());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn re_observation_detects_size_changes() {
        let path = temporary_file("changing", b"1234");
        let before = observe_path(&path);
        fs::write(&path, b"12345678").unwrap();
        assert_eq!(
            evaluate_path_after(&path, before.as_ref()),
            CacheRuntimeState::Changing
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn missing_re_observation_remains_unknown() {
        let path = temporary_file("missing", b"1234");
        let before = observe_path(&path);
        fs::remove_file(&path).unwrap();
        assert_eq!(
            evaluate_path_after(&path, before.as_ref()),
            CacheRuntimeState::Unknown
        );
    }
}
