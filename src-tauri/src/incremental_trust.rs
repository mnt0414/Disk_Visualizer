use crate::fsevents_callback::CollectedFseventsChange;
use crate::fsevents_history::FseventsHistoryRead;
use crate::index_checkpoint::{IndexCheckpoint, IndexCheckpointRepository};
use crate::index_trust::{
    evaluate, IndexTrustDecision, IndexTrustEvidence, IndexTrustState, ScanRecommendation,
};
use crate::macos_fsevents::{FseventsBatchDecision, FseventsFallbackReason};
use crate::{change_history::HistoryToken, fsevents_history};
use std::path::Path;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MacosIndexTrustAssessment {
    pub decision: IndexTrustDecision,
    pub changes: Vec<CollectedFseventsChange>,
    pub rescan_subtrees: bool,
    pub next_history_token: Option<String>,
}

fn full_assessment(state: IndexTrustState) -> MacosIndexTrustAssessment {
    MacosIndexTrustAssessment {
        decision: IndexTrustDecision {
            state,
            recommendation: ScanRecommendation::Full,
        },
        changes: Vec::new(),
        rescan_subtrees: false,
        next_history_token: None,
    }
}

fn evidence_decision(
    has_baseline: bool,
    history_available: bool,
    history_continuous: bool,
    volume_matches: bool,
    root_matches: bool,
) -> IndexTrustDecision {
    evaluate(IndexTrustEvidence {
        platform_history_supported: true,
        has_baseline,
        history_available,
        history_continuous,
        volume_matches,
        root_matches,
    })
}

fn checkpoint_event_id(checkpoint: &IndexCheckpoint) -> Result<u64, IndexTrustDecision> {
    if checkpoint.platform != "macos" || checkpoint.history_source != "fsevents" {
        return Err(evidence_decision(true, true, false, true, true));
    }
    match HistoryToken::parse(&checkpoint.history_token) {
        Ok(HistoryToken::Fsevents { event_id }) => Ok(event_id),
        _ => Err(evidence_decision(true, true, false, true, true)),
    }
}

fn evaluate_history(
    checkpoint: &IndexCheckpoint,
    current_volume_identity: &str,
    current_root_identity: &str,
    history: Result<FseventsHistoryRead, String>,
) -> MacosIndexTrustAssessment {
    if checkpoint.volume_identity != current_volume_identity {
        return full_assessment(IndexTrustState::VolumeChanged);
    }
    if checkpoint.root_identity != current_root_identity {
        return full_assessment(IndexTrustState::RootChanged);
    }
    let Ok(read) = history else {
        return full_assessment(IndexTrustState::HistoryUnavailable);
    };
    match read.decision {
        FseventsBatchDecision::Incremental { next_event_id } => MacosIndexTrustAssessment {
            decision: evidence_decision(true, true, true, true, true),
            changes: read.changes,
            rescan_subtrees: false,
            next_history_token: Some(HistoryToken::Fsevents { event_id: next_event_id }.encode()),
        },
        FseventsBatchDecision::RescanSubtrees { next_event_id } => MacosIndexTrustAssessment {
            decision: evidence_decision(true, true, true, true, true),
            changes: read.changes,
            rescan_subtrees: true,
            next_history_token: Some(HistoryToken::Fsevents { event_id: next_event_id }.encode()),
        },
        FseventsBatchDecision::FullScan {
            reason: FseventsFallbackReason::RootChanged,
        } => full_assessment(IndexTrustState::RootChanged),
        FseventsBatchDecision::FullScan { .. } => {
            full_assessment(IndexTrustState::HistoryDiscontinuous)
        }
    }
}

#[cfg(target_os = "macos")]
fn current_identity(root: &Path) -> Result<(String, String), String> {
    use cap_std::{ambient_authority, fs::Dir};
    use std::os::unix::fs::MetadataExt;

    let directory = Dir::open_ambient_dir(root, ambient_authority())
        .map_err(|error| format!("差分更新対象を安全に開けません: {error}"))?;
    let metadata = directory
        .into_std_file()
        .metadata()
        .map_err(|error| format!("差分更新対象のidentityを取得できません: {error}"))?;
    Ok((metadata.dev().to_string(), metadata.ino().to_string()))
}

pub fn assess_macos_index_trust(
    root: &Path,
    repository: &IndexCheckpointRepository,
    max_changes: Option<usize>,
    timeout: Duration,
) -> Result<MacosIndexTrustAssessment, String> {
    #[cfg(target_os = "macos")]
    {
        if !root.is_absolute() {
            return Err("差分更新対象には絶対pathが必要です".to_owned());
        }
        let canonical_root = root
            .canonicalize()
            .map_err(|error| format!("差分更新対象を解決できません: {error}"))?;
        if !canonical_root.is_dir() {
            return Err("差分更新対象はdirectoryである必要があります".to_owned());
        }
        let root_path = canonical_root.to_string_lossy();
        let Some(checkpoint) = repository.load(&root_path)? else {
            return Ok(full_assessment(IndexTrustState::InitialScanRequired));
        };
        let checkpoint_event_id = match checkpoint_event_id(&checkpoint) {
            Ok(event_id) => event_id,
            Err(decision) => {
                return Ok(MacosIndexTrustAssessment {
                    decision,
                    changes: Vec::new(),
                    rescan_subtrees: false,
                    next_history_token: None,
                })
            }
        };
        let (current_volume_identity, current_root_identity) = current_identity(&canonical_root)?;
        if checkpoint.volume_identity != current_volume_identity {
            return Ok(full_assessment(IndexTrustState::VolumeChanged));
        }
        if checkpoint.root_identity != current_root_identity {
            return Ok(full_assessment(IndexTrustState::RootChanged));
        }
        let history = fsevents_history::read_history(
            &canonical_root,
            checkpoint_event_id,
            max_changes,
            timeout,
        );
        Ok(evaluate_history(
            &checkpoint,
            &current_volume_identity,
            &current_root_identity,
            history,
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (root, repository, max_changes, timeout);
        Ok(full_assessment(IndexTrustState::Unsupported))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macos_fsevents::{FseventsEvent, FseventsFallbackReason};

    fn checkpoint() -> IndexCheckpoint {
        IndexCheckpoint {
            root_path: "/Volumes/Data".to_owned(),
            platform: "macos".to_owned(),
            volume_identity: "volume-1".to_owned(),
            root_identity: "root-1".to_owned(),
            history_source: "fsevents".to_owned(),
            history_token: HistoryToken::Fsevents { event_id: 10 }.encode(),
            updated_at: 1,
        }
    }

    fn history(decision: FseventsBatchDecision) -> Result<FseventsHistoryRead, String> {
        Ok(FseventsHistoryRead {
            changes: vec![CollectedFseventsChange {
                relative_path: "changed.txt".into(),
                event: FseventsEvent {
                    event_id: 11,
                    flags: 0,
                },
            }],
            decision,
        })
    }

    #[test]
    fn accepts_matching_continuous_history_and_advances_token() {
        let assessment = evaluate_history(
            &checkpoint(),
            "volume-1",
            "root-1",
            history(FseventsBatchDecision::Incremental { next_event_id: 11 }),
        );
        assert_eq!(assessment.decision.state, IndexTrustState::Trusted);
        assert_eq!(
            assessment.next_history_token.as_deref(),
            Some("fsevents:v1:11")
        );
        assert_eq!(assessment.changes.len(), 1);
        assert!(!assessment.rescan_subtrees);
    }

    #[test]
    fn preserves_subtree_rescan_requirement() {
        let assessment = evaluate_history(
            &checkpoint(),
            "volume-1",
            "root-1",
            history(FseventsBatchDecision::RescanSubtrees { next_event_id: 11 }),
        );
        assert_eq!(assessment.decision.state, IndexTrustState::Trusted);
        assert!(assessment.rescan_subtrees);
    }

    #[test]
    fn rejects_identity_changes_before_accepting_history() {
        let volume = evaluate_history(
            &checkpoint(),
            "volume-2",
            "root-1",
            history(FseventsBatchDecision::Incremental { next_event_id: 11 }),
        );
        assert_eq!(volume.decision.state, IndexTrustState::VolumeChanged);
        assert!(volume.changes.is_empty());

        let root = evaluate_history(
            &checkpoint(),
            "volume-1",
            "root-2",
            history(FseventsBatchDecision::Incremental { next_event_id: 11 }),
        );
        assert_eq!(root.decision.state, IndexTrustState::RootChanged);
    }

    #[test]
    fn maps_history_failures_to_full_scan_states() {
        let unavailable = evaluate_history(
            &checkpoint(),
            "volume-1",
            "root-1",
            Err("timeout".to_owned()),
        );
        assert_eq!(
            unavailable.decision.state,
            IndexTrustState::HistoryUnavailable
        );

        let dropped = evaluate_history(
            &checkpoint(),
            "volume-1",
            "root-1",
            history(FseventsBatchDecision::FullScan {
                reason: FseventsFallbackReason::KernelDropped,
            }),
        );
        assert_eq!(
            dropped.decision.state,
            IndexTrustState::HistoryDiscontinuous
        );
        assert_eq!(dropped.decision.recommendation, ScanRecommendation::Full);
    }

    #[test]
    fn validates_checkpoint_platform_source_and_token() {
        let mut invalid = checkpoint();
        invalid.platform = "windows".to_owned();
        assert_eq!(
            checkpoint_event_id(&invalid).unwrap_err().state,
            IndexTrustState::HistoryDiscontinuous
        );
        invalid = checkpoint();
        invalid.history_token = "broken".to_owned();
        assert_eq!(
            checkpoint_event_id(&invalid).unwrap_err().state,
            IndexTrustState::HistoryDiscontinuous
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn reports_unsupported_platform_without_reading_history() {
        let repository = IndexCheckpointRepository::new("unused.sqlite3".into());
        let assessment = assess_macos_index_trust(
            Path::new("/"),
            &repository,
            Some(4),
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(assessment.decision.state, IndexTrustState::Unsupported);
    }
}
