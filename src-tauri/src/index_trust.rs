use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IndexTrustState {
    InitialScanRequired,
    Trusted,
    HistoryUnavailable,
    HistoryDiscontinuous,
    VolumeChanged,
    RootChanged,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScanRecommendation {
    Incremental,
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexTrustEvidence {
    pub platform_history_supported: bool,
    pub has_baseline: bool,
    pub history_available: bool,
    pub history_continuous: bool,
    pub volume_matches: bool,
    pub root_matches: bool,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IndexTrustDecision {
    pub state: IndexTrustState,
    pub recommendation: ScanRecommendation,
}

pub fn evaluate(evidence: IndexTrustEvidence) -> IndexTrustDecision {
    let state = if !evidence.platform_history_supported {
        IndexTrustState::Unsupported
    } else if !evidence.has_baseline {
        IndexTrustState::InitialScanRequired
    } else if !evidence.volume_matches {
        IndexTrustState::VolumeChanged
    } else if !evidence.root_matches {
        IndexTrustState::RootChanged
    } else if !evidence.history_available {
        IndexTrustState::HistoryUnavailable
    } else if !evidence.history_continuous {
        IndexTrustState::HistoryDiscontinuous
    } else {
        IndexTrustState::Trusted
    };
    IndexTrustDecision {
        state,
        recommendation: if state == IndexTrustState::Trusted {
            ScanRecommendation::Incremental
        } else {
            ScanRecommendation::Full
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trusted_evidence() -> IndexTrustEvidence {
        IndexTrustEvidence {
            platform_history_supported: true,
            has_baseline: true,
            history_available: true,
            history_continuous: true,
            volume_matches: true,
            root_matches: true,
        }
    }

    #[test]
    fn allows_incremental_only_for_continuous_matching_history() {
        assert_eq!(
            evaluate(trusted_evidence()),
            IndexTrustDecision {
                state: IndexTrustState::Trusted,
                recommendation: ScanRecommendation::Incremental,
            }
        );
    }

    #[test]
    fn requires_full_scan_without_baseline() {
        let mut evidence = trusted_evidence();
        evidence.has_baseline = false;
        assert_eq!(evaluate(evidence).state, IndexTrustState::InitialScanRequired);
        assert_eq!(evaluate(evidence).recommendation, ScanRecommendation::Full);
    }

    #[test]
    fn requires_full_scan_when_history_is_missing_or_discontinuous() {
        let mut missing = trusted_evidence();
        missing.history_available = false;
        assert_eq!(evaluate(missing).state, IndexTrustState::HistoryUnavailable);

        let mut discontinuous = trusted_evidence();
        discontinuous.history_continuous = false;
        assert_eq!(
            evaluate(discontinuous).state,
            IndexTrustState::HistoryDiscontinuous
        );
        assert_eq!(evaluate(discontinuous).recommendation, ScanRecommendation::Full);
    }

    #[test]
    fn requires_full_scan_when_target_identity_changes() {
        let mut volume_changed = trusted_evidence();
        volume_changed.volume_matches = false;
        assert_eq!(evaluate(volume_changed).state, IndexTrustState::VolumeChanged);

        let mut root_changed = trusted_evidence();
        root_changed.root_matches = false;
        assert_eq!(evaluate(root_changed).state, IndexTrustState::RootChanged);
    }

    #[test]
    fn unsupported_history_never_enables_incremental_scan() {
        let mut evidence = trusted_evidence();
        evidence.platform_history_supported = false;
        assert_eq!(evaluate(evidence).state, IndexTrustState::Unsupported);
        assert_eq!(evaluate(evidence).recommendation, ScanRecommendation::Full);
    }
}
