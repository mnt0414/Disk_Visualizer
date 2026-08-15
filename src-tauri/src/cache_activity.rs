use serde::Serialize;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn observation() -> CacheObservation {
        CacheObservation {
            logical_size: Some(1024),
            modified_at: Some(1_723_700_000),
            file_identity: Some("volume-7:file-42".to_owned()),
        }
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
}
