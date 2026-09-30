use crate::incremental_rescan::{
    plan_incremental_rescan, FullRescanReason, IncrementalRescanPlan, IncrementalRescanTarget,
};
use crate::incremental_storage::{
    apply_incremental_snapshot, latest_complete_scan_id, IncrementalApplyResult,
};
use crate::incremental_trust::MacosIndexTrustAssessment;
use crate::index_checkpoint::IndexCheckpoint;
use crate::index_trust::{IndexTrustState, ScanRecommendation};
use crate::scanner::{scan_targets_controlled, ScanProgress, TargetScanOutcome, CANCELLED_MESSAGE};
use serde::Serialize;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// 一度の差分更新で扱う再走査target数の上限。超える場合はフルスキャンの方が安全で速い。
pub(crate) const MAX_INCREMENTAL_TARGETS: usize = 256;
/// 差分更新でメモリ上に保持するreplacement entry数の上限。超える場合はフルスキャンへ戻す。
pub(crate) const MAX_REPLACEMENT_ENTRIES: usize = 500_000;

/// 差分更新を適用できず、フルスキャンが必要な理由。
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FullScanRequiredReason {
    /// インデックスの信頼状態が`trusted`ではない。
    TrustNotTrusted { state: IndexTrustState },
    /// 信頼済みなのに次の履歴tokenがない(checkpointを進められない)。
    HistoryTokenMissing,
    /// 変更pathが不正でtargetへ変換できない。
    InvalidChangePath,
    /// 再走査targetが上限を超えた。
    TooManyTargets,
    /// targetまでの途中要素がsymlink・非directory・別volume・volume不明・読取不能。
    UnsafeTargetPath,
    /// directory entryを列挙できず、フルスキャンと同じ記録を再現できない。
    UnreadableDirectoryEntry,
    /// 差分のentry数がメモリ上限を超えた。
    TooManyEntries,
    /// hard link groupが再走査targetの内外に跨がる。
    HardLinkCrossesTarget,
    /// 対象rootの完了済み基準スキャンがない。
    BaselineMissing,
}

impl FullScanRequiredReason {
    pub fn message(&self) -> String {
        let detail = match self {
            Self::TrustNotTrusted { state } => match state {
                IndexTrustState::Trusted => "信頼状態を確認できません",
                IndexTrustState::InitialScanRequired => "基準スキャンがありません",
                IndexTrustState::HistoryUnavailable => "変更履歴を取得できません",
                IndexTrustState::HistoryDiscontinuous => "変更履歴が連続していません",
                IndexTrustState::VolumeChanged => "ボリュームが基準スキャン時と異なります",
                IndexTrustState::RootChanged => "対象フォルダが基準スキャン時と異なります",
                IndexTrustState::Unsupported => "このプラットフォームは変更履歴に対応していません",
            },
            Self::HistoryTokenMissing => "次の履歴位置を取得できません",
            Self::InvalidChangePath => "変更pathが不正です",
            Self::TooManyTargets => "変更箇所が多すぎます",
            Self::UnsafeTargetPath => "変更箇所までの経路を安全に辿れません",
            Self::UnreadableDirectoryEntry => "フォルダ内の項目を読み取れません",
            Self::TooManyEntries => "再走査する項目が多すぎます",
            Self::HardLinkCrossesTarget => "ハードリンクが再走査範囲の内外に跨がっています",
            Self::BaselineMissing => "完了済みの基準スキャンがありません",
        };
        format!("差分更新できないためフルスキャンが必要です: {detail}")
    }
}

impl From<FullRescanReason> for FullScanRequiredReason {
    fn from(reason: FullRescanReason) -> Self {
        match reason {
            FullRescanReason::InvalidChangePath => Self::InvalidChangePath,
            FullRescanReason::TooManyTargets => Self::TooManyTargets,
        }
    }
}

/// 差分更新の結果。`FullScanRequired`ではsessionもcheckpointも書き込まれない。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum IncrementalUpdateOutcome {
    Applied {
        scan_id: i64,
        total_size_bytes: u64,
        file_count: u64,
        directory_count: u64,
        skipped_count: u64,
    },
    FullScanRequired {
        reason: FullScanRequiredReason,
    },
}

fn full_scan_required(reason: FullScanRequiredReason) -> IncrementalUpdateOutcome {
    IncrementalUpdateOutcome::FullScanRequired { reason }
}

/// 信頼状態が差分更新を許可しない場合の理由を返す。
pub(crate) fn trust_gate(assessment: &MacosIndexTrustAssessment) -> Option<FullScanRequiredReason> {
    if assessment.decision.state != IndexTrustState::Trusted
        || assessment.decision.recommendation != ScanRecommendation::Incremental
    {
        return Some(FullScanRequiredReason::TrustNotTrusted {
            state: assessment.decision.state,
        });
    }
    if assessment.next_history_token.is_none() {
        return Some(FullScanRequiredReason::HistoryTokenMissing);
    }
    None
}

fn unix_time() -> Result<i64, String> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_secs(),
    )
    .map_err(|_| "現在時刻が保存可能な範囲を超えています".to_owned())
}

/// 評価済みの信頼状態から、部分再走査とSQLite差分適用までを行う。
///
/// 1. 信頼状態・基準スキャンを確認する
/// 2. 変更pathを再走査targetへ計画する(全targetを再帰走査へ強制する)
/// 3. capability handleでtargetだけを走査する
/// 4. 新しい完全なscan sessionと次のcheckpointを一つのtransactionで確定する
///
/// フルスキャンが必要な場合は`FullScanRequired`を返し、DBへは何も書かない。
/// キャンセルとI/O・SQLiteエラーは`Err`で、この場合もDBは変更されない。
pub(crate) fn run_incremental_update<C, P>(
    database_path: &Path,
    root: &Path,
    baseline_checkpoint: &IndexCheckpoint,
    assessment: &MacosIndexTrustAssessment,
    mut control: C,
    progress: P,
) -> Result<IncrementalUpdateOutcome, String>
where
    C: FnMut() -> bool,
    P: FnMut(&ScanProgress),
{
    if let Some(reason) = trust_gate(assessment) {
        return Ok(full_scan_required(reason));
    }
    let Some(next_history_token) = assessment.next_history_token.clone() else {
        return Ok(full_scan_required(
            FullScanRequiredReason::HistoryTokenMissing,
        ));
    };
    if !root.is_absolute() {
        return Err("差分更新対象には絶対pathが必要です".to_owned());
    }
    let root = root
        .canonicalize()
        .map_err(|error| format!("差分更新対象を解決できません: {error}"))?;
    if !root.is_dir() {
        return Err("差分更新対象はフォルダである必要があります".to_owned());
    }
    if baseline_checkpoint.root_path != root.to_string_lossy() {
        return Err("差分更新checkpointのrootが対象と一致しません".to_owned());
    }
    let Some(baseline_scan_id) = latest_complete_scan_id(database_path, &root)? else {
        return Ok(full_scan_required(FullScanRequiredReason::BaselineMissing));
    };
    let targets = match plan_incremental_rescan(
        &assessment.changes,
        assessment.rescan_subtrees,
        MAX_INCREMENTAL_TARGETS,
    ) {
        IncrementalRescanPlan::Full { reason } => return Ok(full_scan_required(reason.into())),
        // 通常eventのexact targetは、移動されたdirectoryの内容や、directoryからfileへの
        // 置換で残る子孫を取りこぼし得る。planner側の`recursive`に関わらず常に再帰走査する。
        IncrementalRescanPlan::Partial { targets } => targets
            .into_iter()
            .map(|target| IncrementalRescanTarget {
                relative_path: target.relative_path,
                recursive: true,
            })
            .collect::<Vec<_>>(),
    };
    if !control() {
        return Err(CANCELLED_MESSAGE.to_owned());
    }
    let replacements = match scan_targets_controlled(
        &root,
        &targets,
        MAX_REPLACEMENT_ENTRIES,
        &mut control,
        progress,
    )? {
        TargetScanOutcome::Scanned(entries) => entries,
        TargetScanOutcome::UnsafeTarget => {
            return Ok(full_scan_required(FullScanRequiredReason::UnsafeTargetPath))
        }
        TargetScanOutcome::UnreadableDirectoryEntry => {
            return Ok(full_scan_required(
                FullScanRequiredReason::UnreadableDirectoryEntry,
            ))
        }
        TargetScanOutcome::TooManyEntries => {
            return Ok(full_scan_required(FullScanRequiredReason::TooManyEntries))
        }
    };
    if !control() {
        return Err(CANCELLED_MESSAGE.to_owned());
    }
    let checkpoint = IndexCheckpoint {
        history_token: next_history_token,
        updated_at: unix_time()?,
        ..baseline_checkpoint.clone()
    };
    match apply_incremental_snapshot(
        database_path,
        baseline_scan_id,
        &root,
        &targets,
        &replacements,
        &checkpoint,
    )? {
        IncrementalApplyResult::Applied(applied) => Ok(IncrementalUpdateOutcome::Applied {
            scan_id: applied.scan_id,
            total_size_bytes: applied.total_size_bytes,
            file_count: applied.file_count,
            directory_count: applied.directory_count,
            skipped_count: applied.skipped_count,
        }),
        IncrementalApplyResult::HardLinkCrossesTarget => Ok(full_scan_required(
            FullScanRequiredReason::HardLinkCrossesTarget,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsevents_callback::CollectedFseventsChange;
    use crate::index_checkpoint::IndexCheckpointRepository;
    use crate::index_trust::IndexTrustDecision;
    use crate::macos_fsevents::FseventsEvent;
    use crate::scanner;
    use crate::storage::ScanRepository;
    use rusqlite::{params, Connection};
    use std::cell::Cell;
    use std::fs;
    use std::path::PathBuf;

    struct Fixture {
        root: PathBuf,
        data: PathBuf,
        database: PathBuf,
        checkpoint: IndexCheckpoint,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
            let _ = fs::remove_dir_all(&self.data);
        }
    }

    fn temporary_directory(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "disk-visualizer-update-{name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path.canonicalize().unwrap()
    }

    fn path(relative: &str) -> PathBuf {
        relative.split('/').collect()
    }

    fn write(root: &Path, relative: &str, size: usize) {
        let target = root.join(path(relative));
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, vec![7_u8; size]).unwrap();
    }

    fn baseline_tree(root: &Path) {
        write(root, "a.txt", 4);
        write(root, "keep.txt", 3);
        write(root, "dir/b.bin", 8);
        write(root, "dir/sub/c.bin", 5);
    }

    fn fixture(name: &str, populate: impl FnOnce(&Path)) -> Fixture {
        let root = temporary_directory(&format!("{name}-tree"));
        let data = temporary_directory(&format!("{name}-data"));
        populate(&root);
        let database = data.join("scan-index.sqlite3");
        let repository = ScanRepository::new(database.clone());
        repository.initialize().unwrap();
        IndexCheckpointRepository::new(database.clone())
            .initialize()
            .unwrap();
        let checkpoint = IndexCheckpoint {
            root_path: root.to_string_lossy().into_owned(),
            platform: "macos".to_owned(),
            volume_identity: "volume-1".to_owned(),
            root_identity: "root-1".to_owned(),
            history_source: "fsevents".to_owned(),
            history_token: "fsevents:v1:10".to_owned(),
            updated_at: 1,
        };
        save_full_scan(&repository, &root, Some(&checkpoint));
        Fixture {
            root,
            data,
            database,
            checkpoint,
        }
    }

    fn save_full_scan(
        repository: &ScanRepository,
        root: &Path,
        checkpoint: Option<&IndexCheckpoint>,
    ) -> i64 {
        let stream = repository.begin_stream(&root.to_string_lossy()).unwrap();
        let recorder = stream.clone();
        let summary =
            scanner::scan_folder_path_controlled(root, || true, |event| recorder.record(event))
                .unwrap();
        stream
            .complete_with_checkpoint(&summary, checkpoint)
            .unwrap();
        stream.scan_id()
    }

    fn change(relative: &str) -> CollectedFseventsChange {
        CollectedFseventsChange {
            relative_path: path(relative),
            event: FseventsEvent {
                event_id: 11,
                flags: 0,
            },
        }
    }

    fn trusted(changes: &[&str]) -> MacosIndexTrustAssessment {
        MacosIndexTrustAssessment {
            decision: IndexTrustDecision {
                state: IndexTrustState::Trusted,
                recommendation: ScanRecommendation::Incremental,
            },
            changes: changes.iter().map(|value| change(value)).collect(),
            rescan_subtrees: false,
            next_history_token: Some("fsevents:v1:20".to_owned()),
        }
    }

    fn update(
        fixture: &Fixture,
        assessment: &MacosIndexTrustAssessment,
    ) -> IncrementalUpdateOutcome {
        run_incremental_update(
            &fixture.database,
            &fixture.root,
            &fixture.checkpoint,
            assessment,
            || true,
            |_| {},
        )
        .unwrap()
    }

    fn applied_scan_id(outcome: IncrementalUpdateOutcome) -> i64 {
        match outcome {
            IncrementalUpdateOutcome::Applied { scan_id, .. } => scan_id,
            other => panic!("差分更新が適用されていません: {other:?}"),
        }
    }

    type Totals = (i64, i64, i64, i64);

    fn totals(database: &Path, scan_id: i64) -> Totals {
        Connection::open(database)
            .unwrap()
            .query_row(
                "SELECT total_size_bytes,file_count,directory_count,skipped_count FROM scan_sessions WHERE id=?1 AND status='complete'",
                [scan_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap()
    }

    type Row = (String, String, i64, Option<String>);

    fn rows(database: &Path, scan_id: i64) -> Vec<Row> {
        let connection = Connection::open(database).unwrap();
        let mut statement = connection
            .prepare("SELECT relative_path,entry_type,size_bytes,skip_reason FROM scan_entries WHERE scan_id=?1 ORDER BY relative_path,entry_type,skip_reason")
            .unwrap();
        let result = statement
            .query_map([scan_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        result
    }

    fn relative_paths(database: &Path, scan_id: i64) -> Vec<String> {
        rows(database, scan_id)
            .into_iter()
            .map(|row| row.0)
            .collect()
    }

    /// 増分sessionの集計値が、同じtreeの新しいフルスキャンと一致することを確認する。
    fn assert_matches_fresh_full_scan(fixture: &Fixture, scan_id: i64, compare_rows: bool) {
        let fresh = save_full_scan(
            &ScanRepository::new(fixture.database.clone()),
            &fixture.root,
            None,
        );
        assert_eq!(
            totals(&fixture.database, scan_id),
            totals(&fixture.database, fresh)
        );
        if compare_rows {
            assert_eq!(
                rows(&fixture.database, scan_id),
                rows(&fixture.database, fresh)
            );
        }
    }

    fn session_count(database: &Path) -> i64 {
        Connection::open(database)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM scan_sessions", [], |row| row.get(0))
            .unwrap()
    }

    fn stored_checkpoint(fixture: &Fixture) -> IndexCheckpoint {
        IndexCheckpointRepository::new(fixture.database.clone())
            .load(&fixture.root.to_string_lossy())
            .unwrap()
            .unwrap()
    }

    fn assert_no_write(fixture: &Fixture, sessions_before: i64) {
        assert_eq!(session_count(&fixture.database), sessions_before);
        assert_eq!(stored_checkpoint(fixture), fixture.checkpoint);
    }

    #[test]
    fn applies_modified_file_and_advances_checkpoint() {
        let fixture = fixture("modified", baseline_tree);
        write(&fixture.root, "a.txt", 10);
        let outcome = update(&fixture, &trusted(&["a.txt"]));
        let scan_id = applied_scan_id(outcome);
        assert_matches_fresh_full_scan(&fixture, scan_id, true);
        let stored = stored_checkpoint(&fixture);
        assert_eq!(stored.history_token, "fsevents:v1:20");
        assert!(stored.updated_at >= fixture.checkpoint.updated_at);
        assert_eq!(stored.root_path, fixture.checkpoint.root_path);
        assert_eq!(stored.volume_identity, fixture.checkpoint.volume_identity);
        assert_eq!(stored.root_identity, fixture.checkpoint.root_identity);
        assert_eq!(stored.platform, fixture.checkpoint.platform);
        assert_eq!(stored.history_source, fixture.checkpoint.history_source);
    }

    #[test]
    fn preserves_baseline_session_and_reports_totals() {
        let fixture = fixture("baseline", baseline_tree);
        let baseline = latest_complete_scan_id(&fixture.database, &fixture.root)
            .unwrap()
            .unwrap();
        let baseline_rows = rows(&fixture.database, baseline);
        write(&fixture.root, "a.txt", 10);
        let outcome = update(&fixture, &trusted(&["a.txt"]));
        let IncrementalUpdateOutcome::Applied {
            scan_id,
            total_size_bytes,
            file_count,
            directory_count,
            skipped_count,
        } = outcome
        else {
            panic!("差分更新が適用されていません");
        };
        assert_ne!(scan_id, baseline);
        assert_eq!(rows(&fixture.database, baseline), baseline_rows);
        assert_eq!(
            totals(&fixture.database, scan_id),
            (
                total_size_bytes as i64,
                file_count as i64,
                directory_count as i64,
                skipped_count as i64
            )
        );
        assert_eq!(
            latest_complete_scan_id(&fixture.database, &fixture.root).unwrap(),
            Some(scan_id)
        );
    }

    #[test]
    fn applies_added_file() {
        let fixture = fixture("added", baseline_tree);
        write(&fixture.root, "dir/new.bin", 6);
        let scan_id = applied_scan_id(update(&fixture, &trusted(&["dir/new.bin"])));
        assert!(relative_paths(&fixture.database, scan_id)
            .contains(&"dir/new.bin".replace('/', std::path::MAIN_SEPARATOR_STR)));
        assert_matches_fresh_full_scan(&fixture, scan_id, true);
    }

    #[test]
    fn applies_deleted_file() {
        let fixture = fixture("deleted-file", baseline_tree);
        fs::remove_file(fixture.root.join("a.txt")).unwrap();
        let scan_id = applied_scan_id(update(&fixture, &trusted(&["a.txt"])));
        assert!(!relative_paths(&fixture.database, scan_id).contains(&"a.txt".to_owned()));
        assert_matches_fresh_full_scan(&fixture, scan_id, true);
    }

    #[test]
    fn applies_deleted_directory_subtree() {
        let fixture = fixture("deleted-directory", baseline_tree);
        fs::remove_dir_all(fixture.root.join("dir")).unwrap();
        let scan_id = applied_scan_id(update(&fixture, &trusted(&["dir"])));
        assert_eq!(
            relative_paths(&fixture.database, scan_id),
            vec!["a.txt".to_owned(), "keep.txt".to_owned()]
        );
        assert_matches_fresh_full_scan(&fixture, scan_id, true);
    }

    #[test]
    fn treats_missing_intermediate_component_as_deletion() {
        let fixture = fixture("missing-intermediate", baseline_tree);
        fs::remove_dir_all(fixture.root.join("dir")).unwrap();
        let scan_id = applied_scan_id(update(&fixture, &trusted(&["dir/sub/c.bin"])));
        let paths = relative_paths(&fixture.database, scan_id);
        assert!(!paths.iter().any(|value| value.ends_with("c.bin")));
        assert!(paths.iter().any(|value| value.ends_with("b.bin")));
    }

    #[test]
    fn applies_directory_replaced_by_file() {
        let fixture = fixture("dir-to-file", baseline_tree);
        fs::remove_dir_all(fixture.root.join("dir")).unwrap();
        write(&fixture.root, "dir", 9);
        let scan_id = applied_scan_id(update(&fixture, &trusted(&["dir"])));
        let paths = relative_paths(&fixture.database, scan_id);
        assert!(paths.contains(&"dir".to_owned()));
        assert!(!paths.iter().any(|value| value.ends_with("b.bin")));
        assert_matches_fresh_full_scan(&fixture, scan_id, true);
    }

    #[test]
    fn rescans_new_directory_recursively_under_exact_target() {
        let fixture = fixture("new-directory", baseline_tree);
        write(&fixture.root, "fresh/inner/deep.bin", 12);
        write(&fixture.root, "fresh/top.bin", 2);
        let assessment = trusted(&["fresh"]);
        assert!(!assessment.rescan_subtrees);
        let scan_id = applied_scan_id(update(&fixture, &assessment));
        let paths = relative_paths(&fixture.database, scan_id);
        assert!(paths.contains(&"fresh/inner/deep.bin".replace('/', std::path::MAIN_SEPARATOR_STR)));
        assert_matches_fresh_full_scan(&fixture, scan_id, true);
    }

    #[test]
    fn dot_target_rescans_whole_root() {
        let fixture = fixture("dot", baseline_tree);
        write(&fixture.root, "a.txt", 20);
        fs::remove_file(fixture.root.join("keep.txt")).unwrap();
        write(&fixture.root, "dir/sub/d.bin", 1);
        let scan_id = applied_scan_id(update(&fixture, &trusted(&["."])));
        assert_matches_fresh_full_scan(&fixture, scan_id, true);
    }

    #[test]
    fn empty_change_list_still_advances_checkpoint() {
        let fixture = fixture("empty", baseline_tree);
        let scan_id = applied_scan_id(update(&fixture, &trusted(&[])));
        assert_matches_fresh_full_scan(&fixture, scan_id, true);
        assert_eq!(stored_checkpoint(&fixture).history_token, "fsevents:v1:20");
    }

    #[cfg(unix)]
    #[test]
    fn records_symlink_target_as_skipped_without_following() {
        use std::os::unix::fs::symlink;
        let outside = temporary_directory("symlink-outside");
        write(&outside, "secret.bin", 100);
        let fixture = fixture("symlink-target", baseline_tree);
        symlink(&outside, fixture.root.join("link")).unwrap();
        let scan_id = applied_scan_id(update(&fixture, &trusted(&["link"])));
        let recorded = rows(&fixture.database, scan_id);
        assert!(recorded.iter().any(|row| row.0 == "link"
            && row.1 == "other"
            && row.3.as_deref() == Some("link_not_followed")));
        assert!(!recorded.iter().any(|row| row.0.contains("secret")));
        assert_matches_fresh_full_scan(&fixture, scan_id, true);
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn requires_full_scan_for_intermediate_symlink() {
        use std::os::unix::fs::symlink;
        let outside = temporary_directory("intermediate-outside");
        write(&outside, "x.bin", 100);
        let fixture = fixture("intermediate-symlink", baseline_tree);
        fs::remove_dir_all(fixture.root.join("dir")).unwrap();
        symlink(&outside, fixture.root.join("dir")).unwrap();
        let sessions = session_count(&fixture.database);
        let outcome = update(&fixture, &trusted(&["dir/x.bin"]));
        assert_eq!(
            outcome,
            full_scan_required(FullScanRequiredReason::UnsafeTargetPath)
        );
        assert_no_write(&fixture, sessions);
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn requires_full_scan_when_intermediate_component_is_a_file() {
        let fixture = fixture("intermediate-file", baseline_tree);
        let sessions = session_count(&fixture.database);
        let outcome = update(&fixture, &trusted(&["a.txt/child"]));
        assert_eq!(
            outcome,
            full_scan_required(FullScanRequiredReason::UnsafeTargetPath)
        );
        assert_no_write(&fixture, sessions);
    }

    #[cfg(unix)]
    #[test]
    fn requires_full_scan_when_hard_link_crosses_target_boundary() {
        let fixture = fixture("hard-link-crossing", |root| {
            write(root, "a/one.bin", 16);
            write(root, "b/other.bin", 2);
            fs::hard_link(root.join("a/one.bin"), root.join("b/two.bin")).unwrap();
        });
        let sessions = session_count(&fixture.database);
        let outcome = update(&fixture, &trusted(&["a"]));
        assert_eq!(
            outcome,
            full_scan_required(FullScanRequiredReason::HardLinkCrossesTarget)
        );
        assert_no_write(&fixture, sessions);
    }

    #[cfg(unix)]
    #[test]
    fn requires_full_scan_when_new_hard_link_points_outside_target() {
        let fixture = fixture("hard-link-new", |root| {
            write(root, "a/one.bin", 16);
            write(root, "b/other.bin", 2);
        });
        fs::hard_link(
            fixture.root.join("a/one.bin"),
            fixture.root.join("b/two.bin"),
        )
        .unwrap();
        let sessions = session_count(&fixture.database);
        let outcome = update(&fixture, &trusted(&["b/two.bin"]));
        assert_eq!(
            outcome,
            full_scan_required(FullScanRequiredReason::HardLinkCrossesTarget)
        );
        assert_no_write(&fixture, sessions);
    }

    #[cfg(unix)]
    #[test]
    fn counts_hard_links_inside_one_target_once() {
        let fixture = fixture("hard-link-inside", |root| {
            write(root, "d/one.bin", 16);
            write(root, "other.bin", 2);
        });
        fs::hard_link(
            fixture.root.join("d/one.bin"),
            fixture.root.join("d/two.bin"),
        )
        .unwrap();
        let outcome = update(&fixture, &trusted(&["d"]));
        let IncrementalUpdateOutcome::Applied {
            scan_id,
            total_size_bytes,
            file_count,
            ..
        } = outcome
        else {
            panic!("差分更新が適用されていません");
        };
        assert_eq!(total_size_bytes, 18);
        assert_eq!(file_count, 3);
        assert_matches_fresh_full_scan(&fixture, scan_id, false);
    }

    #[cfg(unix)]
    #[test]
    fn counts_hard_links_across_targets_once_when_all_are_replaced() {
        let fixture = fixture("hard-link-two-targets", |root| {
            write(root, "a/one.bin", 16);
            write(root, "other.bin", 2);
        });
        fs::hard_link(fixture.root.join("a/one.bin"), fixture.root.join("b.bin")).unwrap();
        let outcome = update(&fixture, &trusted(&["a", "b.bin"]));
        let IncrementalUpdateOutcome::Applied {
            scan_id,
            total_size_bytes,
            ..
        } = outcome
        else {
            panic!("差分更新が適用されていません");
        };
        assert_eq!(total_size_bytes, 18);
        assert_matches_fresh_full_scan(&fixture, scan_id, false);
    }

    #[test]
    fn requires_full_scan_when_not_trusted() {
        let fixture = fixture("untrusted", baseline_tree);
        let sessions = session_count(&fixture.database);
        for state in [
            IndexTrustState::InitialScanRequired,
            IndexTrustState::HistoryUnavailable,
            IndexTrustState::HistoryDiscontinuous,
            IndexTrustState::VolumeChanged,
            IndexTrustState::RootChanged,
            IndexTrustState::Unsupported,
        ] {
            let mut assessment = trusted(&["a.txt"]);
            assessment.decision = IndexTrustDecision {
                state,
                recommendation: ScanRecommendation::Full,
            };
            assert_eq!(
                update(&fixture, &assessment),
                full_scan_required(FullScanRequiredReason::TrustNotTrusted { state })
            );
        }
        assert_no_write(&fixture, sessions);
    }

    #[test]
    fn requires_full_scan_when_trusted_assessment_lacks_next_token() {
        let fixture = fixture("no-token", baseline_tree);
        let sessions = session_count(&fixture.database);
        let mut assessment = trusted(&["a.txt"]);
        assessment.next_history_token = None;
        assert_eq!(
            update(&fixture, &assessment),
            full_scan_required(FullScanRequiredReason::HistoryTokenMissing)
        );
        assert_no_write(&fixture, sessions);
    }

    #[test]
    fn requires_full_scan_when_baseline_is_missing() {
        let fixture = fixture("no-baseline", baseline_tree);
        let baseline = latest_complete_scan_id(&fixture.database, &fixture.root)
            .unwrap()
            .unwrap();
        // ScanRepository::deleteはcheckpointも消すため、基準だけが欠けた状態をSQLで作る。
        Connection::open(&fixture.database)
            .unwrap()
            .execute("DELETE FROM scan_sessions WHERE id=?1", [baseline])
            .unwrap();
        assert_eq!(
            update(&fixture, &trusted(&["a.txt"])),
            full_scan_required(FullScanRequiredReason::BaselineMissing)
        );
        assert_no_write(&fixture, 0);
    }

    #[test]
    fn ignores_baselines_of_other_roots_and_incomplete_sessions() {
        let fixture = fixture("other-baseline", baseline_tree);
        let connection = Connection::open(&fixture.database).unwrap();
        connection
            .execute(
                "UPDATE scan_sessions SET status='interrupted' WHERE root_path=?1",
                params![fixture.root.to_string_lossy().as_ref()],
            )
            .unwrap();
        drop(connection);
        assert_eq!(
            update(&fixture, &trusted(&["a.txt"])),
            full_scan_required(FullScanRequiredReason::BaselineMissing)
        );
    }

    #[test]
    fn maps_planner_fallbacks_to_full_scan() {
        let fixture = fixture("planner", baseline_tree);
        let sessions = session_count(&fixture.database);
        let mut invalid = trusted(&[]);
        invalid.changes = vec![CollectedFseventsChange {
            relative_path: PathBuf::from("..").join("outside"),
            event: FseventsEvent {
                event_id: 11,
                flags: 0,
            },
        }];
        assert_eq!(
            update(&fixture, &invalid),
            full_scan_required(FullScanRequiredReason::InvalidChangePath)
        );
        let names = (0..=MAX_INCREMENTAL_TARGETS)
            .map(|index| format!("file-{index}"))
            .collect::<Vec<_>>();
        let names = names.iter().map(String::as_str).collect::<Vec<_>>();
        assert_eq!(
            update(&fixture, &trusted(&names)),
            full_scan_required(FullScanRequiredReason::TooManyTargets)
        );
        assert_no_write(&fixture, sessions);
    }

    #[test]
    fn cancellation_writes_neither_session_nor_checkpoint() {
        let fixture = fixture("cancel", baseline_tree);
        write(&fixture.root, "a.txt", 10);
        let sessions = session_count(&fixture.database);
        let calls = Cell::new(0_u32);
        let result = run_incremental_update(
            &fixture.database,
            &fixture.root,
            &fixture.checkpoint,
            &trusted(&["a.txt", "dir"]),
            || {
                calls.set(calls.get() + 1);
                calls.get() < 3
            },
            |_| {},
        );
        assert_eq!(result.unwrap_err(), CANCELLED_MESSAGE);
        assert_no_write(&fixture, sessions);
    }

    #[test]
    fn cancellation_before_start_and_before_apply_writes_nothing() {
        let fixture = fixture("cancel-edges", baseline_tree);
        let sessions = session_count(&fixture.database);
        for allowed_calls in [0_u32, 3] {
            let calls = Cell::new(0_u32);
            let result = run_incremental_update(
                &fixture.database,
                &fixture.root,
                &fixture.checkpoint,
                &trusted(&["a.txt"]),
                || {
                    calls.set(calls.get() + 1);
                    calls.get() <= allowed_calls
                },
                |_| {},
            );
            assert_eq!(result.unwrap_err(), CANCELLED_MESSAGE);
            assert_no_write(&fixture, sessions);
        }
    }

    #[test]
    fn rejects_checkpoint_for_a_different_root() {
        let fixture = fixture("checkpoint-root", baseline_tree);
        let mut checkpoint = fixture.checkpoint.clone();
        checkpoint.root_path = "elsewhere".to_owned();
        let result = run_incremental_update(
            &fixture.database,
            &fixture.root,
            &checkpoint,
            &trusted(&["a.txt"]),
            || true,
            |_| {},
        );
        assert!(result.is_err());
        assert_no_write(&fixture, 1);
    }

    #[test]
    fn serializes_reasons_as_tagged_snake_case() {
        assert_eq!(
            serde_json::to_string(&FullScanRequiredReason::HardLinkCrossesTarget).unwrap(),
            r#"{"kind":"hard_link_crosses_target"}"#
        );
        assert_eq!(
            serde_json::to_string(&FullScanRequiredReason::TrustNotTrusted {
                state: IndexTrustState::HistoryDiscontinuous
            })
            .unwrap(),
            r#"{"kind":"trust_not_trusted","state":"history_discontinuous"}"#
        );
    }
}
