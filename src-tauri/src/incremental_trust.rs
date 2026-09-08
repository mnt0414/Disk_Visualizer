use crate::fsevents_callback::CollectedFseventsChange;
use crate::index_checkpoint::{IndexCheckpoint, IndexCheckpointRepository};
use crate::index_trust::{IndexTrustDecision, IndexTrustState, ScanRecommendation};

#[cfg(any(target_os = "macos", test))]
use crate::incremental_paths::ChangeScope;
use std::path::Path;
use std::time::Duration;

#[cfg(any(target_os = "macos", test))]
use crate::change_history::HistoryToken;
#[cfg(any(target_os = "macos", test))]
use crate::fsevents_history::FseventsHistoryRead;
#[cfg(any(target_os = "macos", test))]
use crate::index_trust::{evaluate, IndexTrustEvidence};
#[cfg(any(target_os = "macos", test))]
use crate::macos_fsevents::{FseventsBatchDecision, FseventsFallbackReason};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MacosIndexTrustAssessment {
    pub decision: IndexTrustDecision,
    pub changes: Vec<CollectedFseventsChange>,
    pub rescan_subtrees: bool,
    pub next_history_token: Option<String>,
    /// 差分更新の基準となる完了済みscan session。checkpointが明示的に指したものだけを載せる。
    pub baseline_scan_id: Option<i64>,
    /// 検証に使ったcheckpointそのもの。差分適用時の前提として、確定transaction内で
    /// 保存済みcheckpointと突き合わせる。
    pub checkpoint: Option<IndexCheckpoint>,
    /// scan rootの祖先に対して部分木全体の再走査要求が届いた。root配下の変更範囲を
    /// 保証できないため、差分更新ではなくフルスキャンへ戻す必要がある。
    pub ancestor_rescan_required: bool,
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
        baseline_scan_id: None,
        checkpoint: None,
        ancestor_rescan_required: false,
    }
}

#[cfg(any(target_os = "macos", test))]
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

#[cfg(any(target_os = "macos", test))]
fn checkpoint_event_id(checkpoint: &IndexCheckpoint) -> Result<u64, IndexTrustDecision> {
    if checkpoint.platform != "macos" || checkpoint.history_source != "fsevents" {
        return Err(evidence_decision(true, true, false, true, true));
    }
    match HistoryToken::parse(&checkpoint.history_token) {
        Ok(HistoryToken::Fsevents { event_id }) => Ok(event_id),
        _ => Err(evidence_decision(true, true, false, true, true)),
    }
}

/// device-relative pathの変更を、走査root基準の相対pathへ読み替える。
///
/// root外の変更は走査結果に現れないため落とす。祖先への変更は落とすが、落としたことを
/// 呼び出し側へ伝える。相対pathとして解釈できない変更は範囲を推測せず、fail closedで
/// フルスキャンへ戻せるようエラーにする。
#[cfg(any(target_os = "macos", test))]
fn to_scan_root_changes(
    device_relative_root: &Path,
    changes: Vec<CollectedFseventsChange>,
) -> Result<(Vec<CollectedFseventsChange>, bool), String> {
    let mut converted = Vec::with_capacity(changes.len());
    let mut ancestor_changed = false;
    for change in changes {
        match crate::incremental_paths::to_scan_root_relative(
            device_relative_root,
            &change.relative_path,
        ) {
            ChangeScope::Inside(relative_path) => converted.push(CollectedFseventsChange {
                relative_path,
                ..change
            }),
            ChangeScope::Ancestor => ancestor_changed = true,
            ChangeScope::Outside => {}
            ChangeScope::Invalid => return Err("変更pathを走査root基準で解釈できません".to_owned()),
        }
    }
    Ok((converted, ancestor_changed))
}

#[cfg(any(target_os = "macos", test))]
fn evaluate_history(
    checkpoint: &IndexCheckpoint,
    identities: (&str, &str),
    device_relative_root: &Path,
    history: Result<FseventsHistoryRead, String>,
) -> Result<MacosIndexTrustAssessment, String> {
    let (current_volume_identity, current_root_identity) = identities;
    if checkpoint.volume_identity != current_volume_identity {
        return Ok(full_assessment(IndexTrustState::VolumeChanged));
    }
    if checkpoint.root_identity != current_root_identity {
        return Ok(full_assessment(IndexTrustState::RootChanged));
    }
    // checkpointが基準scanを明示していない間は、差分の当て先が確定しない。
    let Some(baseline_scan_id) = checkpoint.baseline_scan_id else {
        return Ok(full_assessment(IndexTrustState::InitialScanRequired));
    };
    let Ok(read) = history else {
        return Ok(full_assessment(IndexTrustState::HistoryUnavailable));
    };
    let (rescan_subtrees, next_event_id) = match read.decision {
        FseventsBatchDecision::Incremental { next_event_id } => (false, next_event_id),
        FseventsBatchDecision::RescanSubtrees { next_event_id } => (true, next_event_id),
        FseventsBatchDecision::FullScan {
            reason: FseventsFallbackReason::RootChanged,
        } => return Ok(full_assessment(IndexTrustState::RootChanged)),
        FseventsBatchDecision::FullScan { .. } => {
            return Ok(full_assessment(IndexTrustState::HistoryDiscontinuous))
        }
    };
    let (changes, ancestor_changed) = to_scan_root_changes(device_relative_root, read.changes)?;
    Ok(MacosIndexTrustAssessment {
        decision: evidence_decision(true, true, true, true, true),
        changes,
        rescan_subtrees,
        next_history_token: Some(
            HistoryToken::Fsevents {
                event_id: next_event_id,
            }
            .encode(),
        ),
        baseline_scan_id: Some(baseline_scan_id),
        checkpoint: Some(checkpoint.clone()),
        // 祖先への部分木再走査要求は、root配下の変更を列挙し切れないことを意味する。
        ancestor_rescan_required: ancestor_changed && rescan_subtrees,
    })
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
                    baseline_scan_id: None,
                    checkpoint: None,
                    ancestor_rescan_required: false,
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
        let history = crate::fsevents_history::read_history(
            &canonical_root,
            checkpoint_event_id,
            max_changes,
            timeout,
        );
        evaluate_history(
            &checkpoint,
            (&current_volume_identity, &current_root_identity),
            &crate::incremental_paths::device_relative_root(&canonical_root)?,
            history,
        )
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
    use std::path::PathBuf;

    fn checkpoint() -> IndexCheckpoint {
        IndexCheckpoint {
            root_path: "/Volumes/Data".to_owned(),
            platform: "macos".to_owned(),
            volume_identity: "volume-1".to_owned(),
            root_identity: "root-1".to_owned(),
            history_source: "fsevents".to_owned(),
            history_token: HistoryToken::Fsevents { event_id: 10 }.encode(),
            baseline_scan_id: Some(1),
            updated_at: 1,
        }
    }

    fn unwrapped(result: Result<MacosIndexTrustAssessment, String>) -> MacosIndexTrustAssessment {
        result.expect("解釈できる変更履歴は評価できる必要があります")
    }

    /// FSEventsはdevice-relative pathを返す。走査rootはvolume rootより下にある。
    const DEVICE_RELATIVE_ROOT: &str = "Volumes/Data";

    fn history(decision: FseventsBatchDecision) -> Result<FseventsHistoryRead, String> {
        changed_history(decision, "Volumes/Data/changed.txt")
    }

    fn changed_history(
        decision: FseventsBatchDecision,
        change: &str,
    ) -> Result<FseventsHistoryRead, String> {
        Ok(FseventsHistoryRead {
            changes: vec![CollectedFseventsChange {
                relative_path: change.into(),
                event: FseventsEvent {
                    event_id: 11,
                    flags: 0,
                },
            }],
            decision,
        })
    }

    #[test]
    fn represents_missing_baseline_as_initial_full_scan() {
        let assessment = full_assessment(IndexTrustState::InitialScanRequired);
        assert_eq!(
            assessment.decision.state,
            IndexTrustState::InitialScanRequired
        );
        assert_eq!(assessment.decision.recommendation, ScanRecommendation::Full);
        assert!(assessment.changes.is_empty());
        assert!(assessment.next_history_token.is_none());
    }

    #[test]
    fn accepts_matching_continuous_history_and_advances_token() {
        let assessment = unwrapped(evaluate_history(
            &checkpoint(),
            ("volume-1", "root-1"),
            Path::new(DEVICE_RELATIVE_ROOT),
            history(FseventsBatchDecision::Incremental { next_event_id: 11 }),
        ));
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
        let assessment = unwrapped(evaluate_history(
            &checkpoint(),
            ("volume-1", "root-1"),
            Path::new(DEVICE_RELATIVE_ROOT),
            history(FseventsBatchDecision::RescanSubtrees { next_event_id: 11 }),
        ));
        assert_eq!(assessment.decision.state, IndexTrustState::Trusted);
        assert!(assessment.rescan_subtrees);
    }

    /// 祖先へのMustScanSubDirsは「その配下すべてを走査し直せ」という指示で、
    /// root配下の個別変更は列挙されない。落として差分更新を続けると取りこぼす。
    #[test]
    fn requires_a_full_scan_when_an_ancestor_demands_a_subtree_rescan() {
        for ancestor in ["Volumes", "."] {
            let assessment = unwrapped(evaluate_history(
                &checkpoint(),
                ("volume-1", "root-1"),
                Path::new(DEVICE_RELATIVE_ROOT),
                changed_history(
                    FseventsBatchDecision::RescanSubtrees { next_event_id: 11 },
                    ancestor,
                ),
            ));
            assert!(assessment.ancestor_rescan_required, "{ancestor}");
            assert!(assessment.changes.is_empty(), "{ancestor}");
        }
    }

    /// 部分木再走査要求を伴わない祖先の変更は、root配下の内容を変えない。
    /// これは無視してよく、無視したことでフルスキャンへ倒す必要もない。
    #[test]
    fn ignores_ancestor_changes_without_a_subtree_rescan() {
        let assessment = unwrapped(evaluate_history(
            &checkpoint(),
            ("volume-1", "root-1"),
            Path::new(DEVICE_RELATIVE_ROOT),
            changed_history(
                FseventsBatchDecision::Incremental { next_event_id: 11 },
                "Volumes",
            ),
        ));
        assert_eq!(assessment.decision.state, IndexTrustState::Trusted);
        assert!(!assessment.ancestor_rescan_required);
        assert!(assessment.changes.is_empty());
    }

    #[test]
    fn rejects_identity_changes_before_accepting_history() {
        let volume = unwrapped(evaluate_history(
            &checkpoint(),
            ("volume-2", "root-1"),
            Path::new(DEVICE_RELATIVE_ROOT),
            history(FseventsBatchDecision::Incremental { next_event_id: 11 }),
        ));
        assert_eq!(volume.decision.state, IndexTrustState::VolumeChanged);
        assert!(volume.changes.is_empty());

        let root = unwrapped(evaluate_history(
            &checkpoint(),
            ("volume-1", "root-2"),
            Path::new(DEVICE_RELATIVE_ROOT),
            history(FseventsBatchDecision::Incremental { next_event_id: 11 }),
        ));
        assert_eq!(root.decision.state, IndexTrustState::RootChanged);
    }

    #[test]
    fn maps_history_failures_to_full_scan_states() {
        let unavailable = unwrapped(evaluate_history(
            &checkpoint(),
            ("volume-1", "root-1"),
            Path::new(DEVICE_RELATIVE_ROOT),
            Err("timeout".to_owned()),
        ));
        assert_eq!(
            unavailable.decision.state,
            IndexTrustState::HistoryUnavailable
        );

        let dropped = unwrapped(evaluate_history(
            &checkpoint(),
            ("volume-1", "root-1"),
            Path::new(DEVICE_RELATIVE_ROOT),
            history(FseventsBatchDecision::FullScan {
                reason: FseventsFallbackReason::KernelDropped,
            }),
        ));
        assert_eq!(
            dropped.decision.state,
            IndexTrustState::HistoryDiscontinuous
        );
        assert_eq!(dropped.decision.recommendation, ScanRecommendation::Full);
    }

    #[test]
    fn converts_change_paths_to_scan_root_relative_paths() {
        let assessment = unwrapped(evaluate_history(
            &checkpoint(),
            ("volume-1", "root-1"),
            Path::new(DEVICE_RELATIVE_ROOT),
            changed_history(
                FseventsBatchDecision::Incremental { next_event_id: 11 },
                "Volumes/Data/nested/changed.txt",
            ),
        ));
        assert_eq!(
            assessment.changes[0].relative_path,
            PathBuf::from("nested/changed.txt")
        );

        let root_itself = unwrapped(evaluate_history(
            &checkpoint(),
            ("volume-1", "root-1"),
            Path::new(DEVICE_RELATIVE_ROOT),
            changed_history(
                FseventsBatchDecision::Incremental { next_event_id: 11 },
                "Volumes/Data",
            ),
        ));
        assert_eq!(root_itself.changes[0].relative_path, PathBuf::from("."));
    }

    #[test]
    fn drops_changes_outside_the_scan_root() {
        for outside in ["Volumes", "Volumes/Other/file", "Volumes/DataSet/file"] {
            let assessment = unwrapped(evaluate_history(
                &checkpoint(),
                ("volume-1", "root-1"),
                Path::new(DEVICE_RELATIVE_ROOT),
                changed_history(
                    FseventsBatchDecision::Incremental { next_event_id: 11 },
                    outside,
                ),
            ));
            assert!(assessment.changes.is_empty(), "{outside}");
            assert_eq!(assessment.decision.state, IndexTrustState::Trusted);
        }
    }

    #[test]
    fn fails_closed_when_a_change_path_cannot_be_interpreted() {
        for invalid in ["/Volumes/Data/file", "../file", ""] {
            assert!(
                evaluate_history(
                    &checkpoint(),
                    ("volume-1", "root-1"),
                    Path::new(DEVICE_RELATIVE_ROOT),
                    changed_history(
                        FseventsBatchDecision::Incremental { next_event_id: 11 },
                        invalid,
                    ),
                )
                .is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn requires_a_full_scan_until_the_checkpoint_names_a_baseline() {
        let mut without_baseline = checkpoint();
        without_baseline.baseline_scan_id = None;
        let assessment = unwrapped(evaluate_history(
            &without_baseline,
            ("volume-1", "root-1"),
            Path::new(DEVICE_RELATIVE_ROOT),
            history(FseventsBatchDecision::Incremental { next_event_id: 11 }),
        ));
        assert_eq!(
            assessment.decision.state,
            IndexTrustState::InitialScanRequired
        );
        assert!(assessment.baseline_scan_id.is_none());
    }

    #[test]
    fn reports_the_baseline_scan_the_checkpoint_points_at() {
        let assessment = unwrapped(evaluate_history(
            &checkpoint(),
            ("volume-1", "root-1"),
            Path::new(DEVICE_RELATIVE_ROOT),
            history(FseventsBatchDecision::Incremental { next_event_id: 11 }),
        ));
        assert_eq!(assessment.baseline_scan_id, Some(1));
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
        let assessment =
            assess_macos_index_trust(Path::new("/"), &repository, Some(4), Duration::from_secs(1))
                .unwrap();
        assert_eq!(assessment.decision.state, IndexTrustState::Unsupported);
    }
}
