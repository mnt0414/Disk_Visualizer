use crate::incremental_rescan::{
    plan_incremental_rescan, FullRescanReason, IncrementalRescanPlan, IncrementalRescanTarget,
};
use crate::incremental_scan::rescan_targets;
use crate::incremental_storage::{apply_staged_snapshot, CheckpointTransition};
use crate::incremental_trust::assess_macos_index_trust;
use crate::index_checkpoint::{
    capture_full_scan_checkpoint, verify_root_identity, IndexCheckpointRepository,
};
use crate::index_trust::{IndexTrustDecision, IndexTrustState, ScanRecommendation};
use crate::scanner::{self, ScanProgress, ScanSummary};
use crate::storage::ScanRepository;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(1);
/// FSEvents履歴の待機上限。応答しない履歴でUIを止めないための境界。
const HISTORY_TIMEOUT: Duration = Duration::from_secs(5);
/// 1回の差分更新で保持する変更件数の上限。超えた履歴は破棄され、フルスキャンへ戻る。
const MAX_HISTORY_CHANGES: usize = 65_536;
/// 部分再走査targetの上限。超えたらフルスキャンの方が安いので切り替える。
const MAX_RESCAN_TARGETS: usize = 4_096;
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ScanJobStatus {
    Running,
    Paused,
    Completed,
    Cancelled,
    Failed,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanJobSnapshot {
    pub id: u64,
    pub path: String,
    pub status: ScanJobStatus,
    pub current_path: String,
    pub total_size_bytes: u64,
    pub file_count: u64,
    pub directory_count: u64,
    pub skipped_count: u64,
    pub result: Option<ScanSummary>,
    /// 確定したscan sessionのID。差分更新は結果をメモリへ載せず、ここから参照させる。
    pub saved_scan_id: Option<i64>,
    pub error: Option<String>,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum FullScanReason {
    /// indexの信頼状態が差分更新を許さない。
    IndexUntrusted(IndexTrustState),
    /// 履歴は使えるが、部分再走査の計画が成立しない。
    PlanRejected(FullRescanReason),
    /// scan rootの祖先に部分木全体の再走査要求が届き、root配下の変更範囲を保証できない。
    AncestorSubtreeChanged,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IncrementalScanStart {
    pub trust: IndexTrustDecision,
    /// フルスキャンへ切り替えた理由。差分更新を開始したときはNone。
    pub full_scan_reason: Option<FullScanReason>,
    pub job: ScanJobSnapshot,
}
struct JobState {
    status: ScanJobStatus,
    current_path: String,
    total_size_bytes: u64,
    file_count: u64,
    directory_count: u64,
    skipped_count: u64,
    result: Option<ScanSummary>,
    saved_scan_id: Option<i64>,
    error: Option<String>,
}
struct Control {
    paused: bool,
    cancelled: bool,
    /// 確定フェーズへ入ったら二度と戻らない。中断要求の受け付けはここで閉じる。
    finalizing: bool,
}
struct ScanJob {
    id: u64,
    path: String,
    state: Mutex<JobState>,
    control: Mutex<Control>,
    wake: Condvar,
}
impl ScanJob {
    fn snapshot(&self) -> ScanJobSnapshot {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        ScanJobSnapshot {
            id: self.id,
            path: self.path.clone(),
            status: state.status,
            current_path: state.current_path.clone(),
            total_size_bytes: state.total_size_bytes,
            file_count: state.file_count,
            directory_count: state.directory_count,
            skipped_count: state.skipped_count,
            result: state.result.clone(),
            saved_scan_id: state.saved_scan_id,
            error: state.error.clone(),
        }
    }
    fn can_continue(&self) -> bool {
        let mut control = self.control.lock().unwrap_or_else(|e| e.into_inner());
        while control.paused && !control.cancelled {
            control = self.wake.wait(control).unwrap_or_else(|e| e.into_inner());
        }
        !control.cancelled
    }
    /// 確定フェーズへ入る。入れたらtrueを返し、以降の中断要求を受け付けない。
    ///
    /// 一時停止中は`can_continue`と同じく待つ。キャンセル済みなら入らない。
    /// 判定とフラグ立ては同じcontrol lock区間で行うので、`pause`・`cancel`が
    /// この境界をまたいで成功することはない。
    fn begin_finalizing(&self) -> bool {
        let mut control = self.control.lock().unwrap_or_else(|e| e.into_inner());
        while control.paused && !control.cancelled {
            control = self.wake.wait(control).unwrap_or_else(|e| e.into_inner());
        }
        if control.cancelled {
            return false;
        }
        control.finalizing = true;
        true
    }
    fn progress(&self, progress: &ScanProgress) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.current_path = progress.path.to_string_lossy().into_owned();
        state.file_count = state.file_count.saturating_add(progress.file_count);
        state.directory_count = state
            .directory_count
            .saturating_add(progress.directory_count);
        state.skipped_count = state.skipped_count.saturating_add(progress.skipped_count);
        state.total_size_bytes = state
            .total_size_bytes
            .saturating_add(progress.counted_size_bytes);
    }
}
fn new_job(path: &str) -> Arc<ScanJob> {
    Arc::new(ScanJob {
        id: NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed),
        path: path.to_owned(),
        state: Mutex::new(JobState {
            status: ScanJobStatus::Running,
            current_path: path.to_owned(),
            total_size_bytes: 0,
            file_count: 0,
            directory_count: 0,
            skipped_count: 0,
            result: None,
            saved_scan_id: None,
            error: None,
        }),
        control: Mutex::new(Control {
            paused: false,
            cancelled: false,
            finalizing: false,
        }),
        wake: Condvar::new(),
    })
}
fn ensure_idle(active: &Option<Arc<ScanJob>>) -> Result<(), String> {
    if let Some(job) = active.as_ref() {
        if matches!(
            job.snapshot().status,
            ScanJobStatus::Running | ScanJobStatus::Paused
        ) {
            return Err("別のスキャンが実行中です".to_owned());
        }
    }
    Ok(())
}
/// 部分再走査から確定までを1本の失敗経路で実行する。
///
/// 確定に成功したときだけcheckpointが進む。キャンセル・走査失敗・適用失敗では
/// transactionごと巻き戻るので、baselineも過去の履歴もそのまま残る。
fn run_incremental(
    job: &ScanJob,
    database: &Path,
    root: &Path,
    targets: &[IncrementalRescanTarget],
    baseline_scan_id: i64,
    transition: &CheckpointTransition,
) {
    run_incremental_with_hooks(
        job,
        database,
        root,
        targets,
        baseline_scan_id,
        transition,
        (|| {}, || {}),
    )
}

/// 確定フェーズの前後にテスト用のフックを差し込めるようにした実装。
///
/// `before_finalize`は確定フェーズへ入る直前、`after_finalize`は入った直後に呼ぶ。
/// この2点の間が「キャンセルを受け付ける最後の地点」で、これ以降のtransactionは
/// 取り消せない。テストは両側で実スレッドを止めて、pause・cancel・checkpointの
/// 追い越しを決定論的に再現する。
fn run_incremental_with_hooks<B: Fn(), A: Fn()>(
    job: &ScanJob,
    database: &Path,
    root: &Path,
    targets: &[IncrementalRescanTarget],
    baseline_scan_id: i64,
    transition: &CheckpointTransition,
    (before_finalize, after_finalize): (B, A),
) {
    let outcome = rescan_targets(
        root,
        targets,
        || job.can_continue(),
        |progress| job.progress(progress),
    )
    .and_then(|mut staging| {
        before_finalize();
        // 確定は取り消せない。中断要求の確認と確定フェーズ入りを同じ境界で行い、
        // 「確認は通ったがそのあとcancelが成功する」隙間を残さない。
        if !job.begin_finalizing() {
            return Err("スキャンはキャンセルされました".to_owned());
        }
        after_finalize();
        // 走査中にrootが差し替わっていれば、この結果は基準scanの続きではない。
        verify_root_identity(root, &transition.expected)?;
        apply_staged_snapshot(database, &mut staging, baseline_scan_id, transition)
    });
    let cancelled = job
        .control
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .cancelled;
    let mut state = job.state.lock().unwrap_or_else(|e| e.into_inner());
    state.current_path.clear();
    match outcome {
        Ok(scan_id) => {
            state.status = ScanJobStatus::Completed;
            state.saved_scan_id = Some(scan_id);
        }
        // 確定前に終わっているので、baselineもcheckpointも動いていない。
        Err(_) if cancelled => state.status = ScanJobStatus::Cancelled,
        Err(error) => {
            state.status = ScanJobStatus::Failed;
            state.error = Some(error);
        }
    }
}
pub struct ScanManager {
    active: Mutex<Option<Arc<ScanJob>>>,
    repository: ScanRepository,
}
impl ScanManager {
    pub fn new(repository: ScanRepository) -> Self {
        Self {
            active: Mutex::new(None),
            repository,
        }
    }
    pub fn start(&self, path: String) -> Result<ScanJobSnapshot, String> {
        if !Path::new(&path).is_absolute() {
            return Err("スキャン対象には絶対パスを指定してください".to_owned());
        }
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        ensure_idle(&active)?;
        let checkpoint = capture_full_scan_checkpoint(Path::new(&path))
            .ok()
            .flatten();
        let stream = self.repository.begin_stream(&path)?;
        let job = new_job(&path);
        *active = Some(Arc::clone(&job));
        let snapshot = job.snapshot();
        std::thread::spawn(move || {
            let control_job = Arc::clone(&job);
            let progress_job = Arc::clone(&job);
            let progress_stream = stream.clone();
            let result = scanner::scan_folder_path_controlled(
                Path::new(&path),
                move || control_job.can_continue(),
                move |progress| {
                    progress_job.progress(progress);
                    progress_stream.record(progress);
                },
            );
            // 保存の開始とキャンセル受付の終了を、差分更新と同じ境界で決める。
            // ここを過ぎたcancelは成功を返さないので、保存した結果を後から
            // Cancelledへ倒すことはない。
            let committing = result.is_ok() && job.begin_finalizing();
            let cancelled = !committing
                && job
                    .control
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .cancelled;
            match result {
                Ok(summary) if committing => {
                    let persisted = stream.complete_with_checkpoint(&summary, checkpoint.as_ref());
                    let mut state = job.state.lock().unwrap_or_else(|e| e.into_inner());
                    match persisted {
                        Ok(()) => {
                            state.status = ScanJobStatus::Completed;
                            state.current_path.clear();
                            state.saved_scan_id = Some(stream.scan_id());
                            state.result = Some(summary);
                        }
                        Err(error) => {
                            state.status = ScanJobStatus::Failed;
                            state.error = Some(error);
                        }
                    }
                }
                _ if cancelled => {
                    let _ = stream.interrupt(false);
                    let mut state = job.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.status = ScanJobStatus::Cancelled;
                    state.current_path.clear();
                }
                Err(error) => {
                    let persistence_error = stream.interrupt(true).err();
                    let mut state = job.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.status = ScanJobStatus::Failed;
                    state.error = Some(persistence_error.unwrap_or(error));
                }
                _ => {}
            }
        });
        Ok(snapshot)
    }
    /// 変更履歴を評価し、差分更新かフルスキャンのどちらかを開始する。
    ///
    /// 信頼できない履歴も、計画が成立しない変更も、そのままフルスキャンへ倒す。
    /// 呼び出し側には信頼状態と切り替えた理由を返し、判断の根拠を隠さない。
    pub fn start_incremental(
        &self,
        path: String,
        checkpoints: &IndexCheckpointRepository,
    ) -> Result<IncrementalScanStart, String> {
        if !Path::new(&path).is_absolute() {
            return Err("スキャン対象には絶対パスを指定してください".to_owned());
        }
        // 履歴読み取りはactive lockの外で行い、進捗照会や中断要求を止めない。
        let assessment = assess_macos_index_trust(
            Path::new(&path),
            checkpoints,
            Some(MAX_HISTORY_CHANGES),
            HISTORY_TIMEOUT,
        )?;
        let trust = assessment.decision;
        if trust.recommendation == ScanRecommendation::Full {
            return Ok(IncrementalScanStart {
                trust,
                full_scan_reason: Some(FullScanReason::IndexUntrusted(trust.state)),
                job: self.start(path)?,
            });
        }
        // 祖先への部分木再走査要求は、root配下の個別変更を列挙しない。範囲を保証できない
        // まま差分を当てると取りこぼすので、理由を明示してフルスキャンへ戻す。
        if assessment.ancestor_rescan_required {
            return Ok(IncrementalScanStart {
                trust,
                full_scan_reason: Some(FullScanReason::AncestorSubtreeChanged),
                job: self.start(path)?,
            });
        }
        let plan = plan_incremental_rescan(
            &assessment.changes,
            assessment.rescan_subtrees,
            MAX_RESCAN_TARGETS,
        );
        let targets = match plan {
            IncrementalRescanPlan::Partial { targets } => targets,
            IncrementalRescanPlan::Full { reason } => {
                return Ok(IncrementalScanStart {
                    trust,
                    full_scan_reason: Some(FullScanReason::PlanRejected(reason)),
                    job: self.start(path)?,
                })
            }
        };
        // 差分を当てる先はcheckpointが名指しした基準scanだけ。揃わなければ進めない。
        let (Some(checkpoint), Some(baseline_scan_id), Some(history_token)) = (
            assessment.checkpoint,
            assessment.baseline_scan_id,
            assessment.next_history_token,
        ) else {
            return Err("差分更新の基準情報が揃っていません".to_owned());
        };
        let job = self.spawn_incremental(
            targets,
            baseline_scan_id,
            CheckpointTransition {
                expected: checkpoint,
                next_history_token: history_token,
            },
        )?;
        Ok(IncrementalScanStart {
            trust,
            full_scan_reason: None,
            job,
        })
    }
    fn spawn_incremental(
        &self,
        targets: Vec<IncrementalRescanTarget>,
        baseline_scan_id: i64,
        transition: CheckpointTransition,
    ) -> Result<ScanJobSnapshot, String> {
        let root = PathBuf::from(&transition.expected.root_path);
        if !root.is_absolute() {
            return Err("checkpointの走査rootが絶対パスではありません".to_owned());
        }
        let database = self.repository.path().to_path_buf();
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        ensure_idle(&active)?;
        let job = new_job(&transition.expected.root_path);
        *active = Some(Arc::clone(&job));
        let snapshot = job.snapshot();
        std::thread::spawn(move || {
            run_incremental(
                &job,
                &database,
                &root,
                &targets,
                baseline_scan_id,
                &transition,
            )
        });
        Ok(snapshot)
    }
    fn job(&self, id: u64) -> Result<Arc<ScanJob>, String> {
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|job| job.id == id)
            .cloned()
            .ok_or_else(|| "スキャンジョブが見つかりません".to_owned())
    }
    pub fn status(&self, id: u64) -> Result<ScanJobSnapshot, String> {
        Ok(self.job(id)?.snapshot())
    }
    pub fn pause(&self, id: u64) -> Result<ScanJobSnapshot, String> {
        let job = self.job(id)?;
        {
            let mut state = job.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.status != ScanJobStatus::Running {
                return Err("実行中のスキャンだけを一時停止できます".to_owned());
            }
            let mut control = job.control.lock().unwrap_or_else(|e| e.into_inner());
            // 確定フェーズへ入ったあとは止められない。成功を返すと、確定した結果を
            // 止めたと誤解させる。
            if control.finalizing {
                return Err("スキャン結果の確定中は一時停止できません".to_owned());
            }
            control.paused = true;
            state.status = ScanJobStatus::Paused;
        }
        Ok(job.snapshot())
    }
    pub fn resume(&self, id: u64) -> Result<ScanJobSnapshot, String> {
        let job = self.job(id)?;
        {
            let mut state = job.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.status != ScanJobStatus::Paused {
                return Err("一時停止中のスキャンだけを再開できます".to_owned());
            }
            job.control.lock().unwrap_or_else(|e| e.into_inner()).paused = false;
            state.status = ScanJobStatus::Running;
        }
        job.wake.notify_all();
        Ok(job.snapshot())
    }
    pub fn cancel(&self, id: u64) -> Result<ScanJobSnapshot, String> {
        let job = self.job(id)?;
        {
            let state = job.state.lock().unwrap_or_else(|e| e.into_inner());
            if !matches!(state.status, ScanJobStatus::Running | ScanJobStatus::Paused) {
                return Err("完了済みのスキャンはキャンセルできません".to_owned());
            }
            // stateを持ったままcontrolへ入る。lockの順序はpauseと同じで、
            // 確定フェーズの開始判定とこの受け付けはcontrol lockで排他になる。
            let mut control = job.control.lock().unwrap_or_else(|e| e.into_inner());
            if control.finalizing {
                return Err("スキャン結果の確定中はキャンセルできません".to_owned());
            }
            control.cancelled = true;
            control.paused = false;
        }
        job.wake.notify_all();
        Ok(job.snapshot())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn job(status: ScanJobStatus, paused: bool) -> Arc<ScanJob> {
        Arc::new(ScanJob {
            id: 9001,
            path: std::env::temp_dir().to_string_lossy().into_owned(),
            state: Mutex::new(JobState {
                status,
                current_path: String::new(),
                total_size_bytes: 0,
                file_count: 0,
                directory_count: 0,
                skipped_count: 0,
                result: None,
                saved_scan_id: None,
                error: None,
            }),
            control: Mutex::new(Control {
                paused,
                cancelled: false,
                finalizing: false,
            }),
            wake: Condvar::new(),
        })
    }
    fn manager(active_job: Arc<ScanJob>) -> ScanManager {
        ScanManager {
            active: Mutex::new(Some(active_job)),
            repository: ScanRepository::new(PathBuf::from("unused.sqlite3")),
        }
    }
    #[test]
    fn transitions_pause_resume_and_cancel() {
        let active_job = job(ScanJobStatus::Running, false);
        let manager = manager(Arc::clone(&active_job));
        assert_eq!(
            manager.pause(active_job.id).unwrap().status,
            ScanJobStatus::Paused
        );
        assert_eq!(
            manager.resume(active_job.id).unwrap().status,
            ScanJobStatus::Running
        );
        manager.cancel(active_job.id).unwrap();
        let control = active_job.control.lock().unwrap();
        assert!(control.cancelled);
        assert!(!control.paused);
    }
    #[test]
    fn cancelling_paused_job_wakes_waiter() {
        let active_job = job(ScanJobStatus::Paused, true);
        let waiting = Arc::clone(&active_job);
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || sender.send(waiting.can_continue()).unwrap());
        std::thread::sleep(Duration::from_millis(20));
        manager(Arc::clone(&active_job))
            .cancel(active_job.id)
            .unwrap();
        assert!(!receiver.recv_timeout(Duration::from_secs(1)).unwrap());
    }
    /// 確定フェーズはフルスキャンと差分更新で共通の境界を使う。
    ///
    /// 入ったあとのpause・cancelは、どちらの経路でも成功を返さない。
    #[test]
    fn refuses_pause_and_cancel_once_the_finalizing_phase_started() {
        let active_job = job(ScanJobStatus::Running, false);
        let manager = manager(Arc::clone(&active_job));
        assert!(active_job.begin_finalizing());
        assert_eq!(
            manager.pause(active_job.id).err().as_deref(),
            Some("スキャン結果の確定中は一時停止できません")
        );
        assert_eq!(
            manager.cancel(active_job.id).err().as_deref(),
            Some("スキャン結果の確定中はキャンセルできません")
        );
        // 確定中でも状態照会は通る。
        assert_eq!(
            manager.status(active_job.id).unwrap().status,
            ScanJobStatus::Running
        );
    }
    #[test]
    fn cancelling_a_paused_job_keeps_it_out_of_the_finalizing_phase() {
        let active_job = job(ScanJobStatus::Paused, true);
        let waiting = Arc::clone(&active_job);
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || sender.send(waiting.begin_finalizing()).unwrap());
        std::thread::sleep(Duration::from_millis(20));
        manager(Arc::clone(&active_job))
            .cancel(active_job.id)
            .unwrap();
        assert!(!receiver.recv_timeout(Duration::from_secs(1)).unwrap());
        assert!(!active_job.control.lock().unwrap().finalizing);
    }
    #[test]
    fn rejects_second_start_while_active() {
        let manager = manager(job(ScanJobStatus::Running, false));
        assert_eq!(
            manager
                .start(std::env::temp_dir().to_string_lossy().into_owned())
                .err()
                .unwrap(),
            "別のスキャンが実行中です"
        );
    }
}

#[cfg(test)]
mod incremental_tests {
    use super::*;
    use crate::index_checkpoint::IndexCheckpoint;
    use rusqlite::Connection;
    use std::fs;
    use std::sync::Barrier;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    fn temporary(name: &str, suffix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "disk-visualizer-jobs-{name}-{}-{unique}{suffix}",
            std::process::id()
        ))
    }

    fn remove_database(database: &Path) {
        let _ = fs::remove_file(database);
        for suffix in ["-wal", "-shm"] {
            let _ = fs::remove_file(PathBuf::from(format!("{}{suffix}", database.display())));
        }
    }

    fn session_ids(database: &Path) -> Vec<i64> {
        let connection = Connection::open(database).unwrap();
        let mut statement = connection
            .prepare("SELECT id FROM scan_sessions ORDER BY id")
            .unwrap();
        let ids = statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        ids
    }

    #[test]
    fn refuses_concurrent_starts_while_a_scan_is_active() {
        let root = temporary("concurrent", "");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("only.bin"), [0_u8; 4]).unwrap();
        let root = root.canonicalize().unwrap();
        let database = temporary("concurrent", ".sqlite3");
        let repository = ScanRepository::new(database.clone());
        repository.initialize().unwrap();
        let manager = ScanManager::new(repository);
        let path = root.to_string_lossy().into_owned();
        let gate = Barrier::new(3);

        // active lockを握ったまま実行中jobを差し込み、解放と同時に2本のstartを競わせる。
        // 差し込んだjobは終わらないので、勝者が先に完了して枠が空く余地はない。
        let mut active = manager.active.lock().unwrap();
        *active = Some(new_job(&path));
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let path = path.clone();
                    let manager = &manager;
                    let gate = &gate;
                    scope.spawn(move || {
                        gate.wait();
                        manager.start(path)
                    })
                })
                .collect();
            gate.wait();
            drop(active);
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });

        for result in &results {
            assert_eq!(
                result.as_ref().err().map(String::as_str),
                Some("別のスキャンが実行中です")
            );
        }
        // 弾かれたstartはscan sessionも作らない。
        assert!(session_ids(&database).is_empty());

        fs::remove_dir_all(root).unwrap();
        remove_database(&database);
    }

    fn wait_for_terminal(manager: &ScanManager, id: u64) -> ScanJobSnapshot {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let snapshot = manager.status(id).unwrap();
            if !matches!(
                snapshot.status,
                ScanJobStatus::Running | ScanJobStatus::Paused
            ) {
                return snapshot;
            }
            assert!(Instant::now() < deadline, "スキャンが終了しません");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// フルスキャンも差分更新と同じ確定境界を通る。
    ///
    /// 保存を始める前に中断要求の受け付けが閉じるので、そのあとのcancelは断る。
    #[test]
    fn a_completed_full_scan_closes_the_cancel_window_before_saving() {
        let root = temporary("full-finalize", "");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("only.bin"), [0_u8; 4]).unwrap();
        let root = root.canonicalize().unwrap();
        let database = temporary("full-finalize", ".sqlite3");
        let repository = ScanRepository::new(database.clone());
        repository.initialize().unwrap();
        let manager = ScanManager::new(repository);

        let started = manager.start(root.to_string_lossy().into_owned()).unwrap();
        let snapshot = wait_for_terminal(&manager, started.id);

        assert_eq!(snapshot.status, ScanJobStatus::Completed);
        assert!(snapshot.saved_scan_id.is_some());
        let job = manager
            .active
            .lock()
            .unwrap()
            .as_ref()
            .map(Arc::clone)
            .unwrap();
        assert!(job.control.lock().unwrap().finalizing);
        assert_eq!(
            manager.cancel(started.id).err().as_deref(),
            Some("完了済みのスキャンはキャンセルできません")
        );
        assert_eq!(session_ids(&database).len(), 1);

        fs::remove_dir_all(root).unwrap();
        remove_database(&database);
    }

    #[test]
    fn falls_back_to_a_full_scan_when_the_index_is_not_trusted() {
        let root = temporary("fallback", "");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("only.bin"), [0_u8; 4]).unwrap();
        let root = root.canonicalize().unwrap();
        let database = temporary("fallback", ".sqlite3");
        let repository = ScanRepository::new(database.clone());
        repository.initialize().unwrap();
        let checkpoints = IndexCheckpointRepository::new(database.clone());
        checkpoints.initialize().unwrap();
        let manager = ScanManager::new(repository);

        let start = manager
            .start_incremental(root.to_string_lossy().into_owned(), &checkpoints)
            .unwrap();

        assert_eq!(start.trust.recommendation, ScanRecommendation::Full);
        assert_eq!(
            start.full_scan_reason,
            Some(FullScanReason::IndexUntrusted(start.trust.state))
        );
        let snapshot = wait_for_terminal(&manager, start.job.id);
        assert_eq!(snapshot.status, ScanJobStatus::Completed);
        assert!(snapshot.saved_scan_id.is_some());
        assert_eq!(session_ids(&database).len(), 1);

        fs::remove_dir_all(root).unwrap();
        remove_database(&database);
    }

    /// 基準scanとcheckpointを進める経路の検証。
    ///
    /// 確定前に走査rootのidentityを検証するため、identityを取得できないplatformでは
    /// fail closedになる。unix以外での挙動は差分更新自体が未対応なので検証しない。
    #[cfg(unix)]
    mod baseline_updates {
        use super::*;
        use crate::index_checkpoint::upsert_checkpoint;
        use std::sync::mpsc::{self, Receiver, SyncSender};
        use std::thread::JoinHandle;

        struct Fixture {
            root: PathBuf,
            database: PathBuf,
            checkpoints: IndexCheckpointRepository,
            baseline_scan_id: i64,
            checkpoint: IndexCheckpoint,
        }

        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.root);
                remove_database(&self.database);
            }
        }

        fn full_scan(
            repository: &ScanRepository,
            root: &Path,
            checkpoint: &IndexCheckpoint,
        ) -> i64 {
            let stream = repository
                .begin_stream(root.to_string_lossy().as_ref())
                .unwrap();
            let recorder = stream.clone();
            let summary = scanner::scan_folder_path_controlled(
                root,
                || true,
                |progress| recorder.record(progress),
            )
            .unwrap();
            stream
                .complete_with_checkpoint(&summary, Some(checkpoint))
                .unwrap();
            stream.scan_id()
        }

        /// 実際の走査rootのidentityを読む。確定直前の検証と同じ観測に揃える。
        fn root_identity(root: &Path) -> (String, String) {
            use std::os::unix::fs::MetadataExt;
            let metadata = fs::metadata(root).unwrap();
            (metadata.dev().to_string(), metadata.ino().to_string())
        }

        /// 基準scanとcheckpointが対応した状態のfixtureを作る。
        fn fixture(name: &str) -> Fixture {
            let root = temporary(name, "");
            fs::create_dir_all(root.join("dir")).unwrap();
            fs::write(root.join("dir/kept.bin"), [0_u8; 8]).unwrap();
            fs::write(root.join("changed.bin"), [0_u8; 4]).unwrap();
            let root = root.canonicalize().unwrap();
            let database = temporary(name, ".sqlite3");
            let repository = ScanRepository::new(database.clone());
            repository.initialize().unwrap();
            let checkpoints = IndexCheckpointRepository::new(database.clone());
            checkpoints.initialize().unwrap();
            let root_path = root.to_string_lossy().into_owned();
            let (volume_identity, root_identity) = root_identity(&root);
            full_scan(
                &repository,
                &root,
                &IndexCheckpoint {
                    root_path: root_path.clone(),
                    platform: "macos".to_owned(),
                    volume_identity,
                    root_identity,
                    history_source: "fsevents".to_owned(),
                    history_token: "fsevents:v1:10".to_owned(),
                    baseline_scan_id: None,
                    updated_at: 1,
                },
            );
            let checkpoint = checkpoints.load(&root_path).unwrap().unwrap();
            Fixture {
                baseline_scan_id: checkpoint.baseline_scan_id.unwrap(),
                root,
                database,
                checkpoints,
                checkpoint,
            }
        }

        impl Fixture {
            /// 保存済みcheckpointを前提に、確定できたら進めるtokenを組み合わせる。
            fn advanced(&self, next_history_token: &str) -> CheckpointTransition {
                CheckpointTransition {
                    expected: self.checkpoint.clone(),
                    next_history_token: next_history_token.to_owned(),
                }
            }
            fn stored_checkpoint(&self) -> IndexCheckpoint {
                self.checkpoints
                    .load(&self.checkpoint.root_path)
                    .unwrap()
                    .unwrap()
            }
            fn apply(
                &self,
                job: &ScanJob,
                names: &[&str],
                baseline: i64,
                transition: &CheckpointTransition,
            ) {
                run_incremental(
                    job,
                    &self.database,
                    &self.root,
                    &targets(names),
                    baseline,
                    transition,
                )
            }
        }

        fn targets(paths: &[&str]) -> Vec<IncrementalRescanTarget> {
            paths
                .iter()
                .map(|path| IncrementalRescanTarget {
                    relative_path: PathBuf::from(*path),
                    recursive: false,
                })
                .collect()
        }

        fn counted_size(database: &Path, scan_id: i64) -> i64 {
            Connection::open(database)
                .unwrap()
                .query_row(
                    "SELECT COALESCE(SUM(size_bytes),0) FROM scan_entries WHERE scan_id=?1",
                    [scan_id],
                    |row| row.get(0),
                )
                .unwrap()
        }

        /// 確定境界で止められるworkerと、それを操作する実ScanManager。
        ///
        /// pause・resume・cancelはすべて`manager`のAPIから送る。controlを直接
        /// 書き換えると、受け付け可否そのものを検証できない。
        struct Boundary {
            manager: ScanManager,
            job: Arc<ScanJob>,
            handle: Option<JoinHandle<()>>,
        }

        impl Boundary {
            fn id(&self) -> u64 {
                self.job.id
            }
            fn join(&mut self) -> ScanJobSnapshot {
                self.handle.take().unwrap().join().unwrap();
                self.job.snapshot()
            }
        }

        /// 確定フェーズの前後で止まるworkerを、実ScanManagerの管理下で起動する。
        fn spawn_incremental_worker<B, A>(
            fixture: &Fixture,
            names: &[&str],
            transition: CheckpointTransition,
            before_finalize: B,
            after_finalize: A,
        ) -> Boundary
        where
            B: Fn() + Send + 'static,
            A: Fn() + Send + 'static,
        {
            let job = new_job(&fixture.checkpoint.root_path);
            let manager = ScanManager {
                active: Mutex::new(Some(Arc::clone(&job))),
                repository: ScanRepository::new(fixture.database.clone()),
            };
            let worker = Arc::clone(&job);
            let database = fixture.database.clone();
            let root = fixture.root.clone();
            let targets = targets(names);
            let baseline = fixture.baseline_scan_id;
            let handle = std::thread::spawn(move || {
                run_incremental_with_hooks(
                    &worker,
                    &database,
                    &root,
                    &targets,
                    baseline,
                    &transition,
                    (before_finalize, after_finalize),
                )
            });
            Boundary {
                manager,
                job,
                handle: Some(handle),
            }
        }

        /// workerを止めるフックと、その操作端を作る。
        ///
        /// 呼び出し側は「境界に着いた」を受け取ってから状態を変え、「再開」を送る。
        /// 実スレッドの順序をchannelで固定するので、待ち時間に依存しない。
        fn gate() -> (impl Fn() + Send + 'static, Receiver<()>, SyncSender<()>) {
            let (reached, arrived) = mpsc::sync_channel::<()>(0);
            let (release, resume) = mpsc::sync_channel::<()>(0);
            let hook = move || {
                reached.send(()).unwrap();
                resume.recv().unwrap();
            };
            (hook, arrived, release)
        }

        /// 確定フェーズへ入る直前のcancelは受け付け、確定させない。
        #[test]
        fn cancels_at_the_last_moment_before_the_commit() {
            let fixture = fixture("cancel-boundary");
            fs::write(fixture.root.join("changed.bin"), [0_u8; 32]).unwrap();
            let before = session_ids(&fixture.database);
            let (before_finalize, at_boundary, release) = gate();
            let mut boundary = spawn_incremental_worker(
                &fixture,
                &["changed.bin"],
                fixture.advanced("fsevents:v1:20"),
                before_finalize,
                || {},
            );

            at_boundary.recv().unwrap();
            boundary.manager.cancel(boundary.id()).unwrap();
            release.send(()).unwrap();
            let snapshot = boundary.join();

            assert_eq!(snapshot.status, ScanJobStatus::Cancelled);
            assert_eq!(snapshot.saved_scan_id, None);
            assert_eq!(session_ids(&fixture.database), before);
            assert_eq!(fixture.stored_checkpoint(), fixture.checkpoint);
        }

        #[test]
        fn defers_the_commit_while_paused_and_finishes_after_resume() {
            let fixture = fixture("pause-boundary");
            fs::write(fixture.root.join("changed.bin"), [0_u8; 32]).unwrap();
            let before = session_ids(&fixture.database);
            let (before_finalize, at_boundary, release) = gate();
            let mut boundary = spawn_incremental_worker(
                &fixture,
                &["changed.bin"],
                fixture.advanced("fsevents:v1:20"),
                before_finalize,
                || {},
            );

            at_boundary.recv().unwrap();
            boundary.manager.pause(boundary.id()).unwrap();
            release.send(()).unwrap();
            // pauseは確定フェーズへ入る前に立つので、workerはここを越えられない。
            assert_eq!(
                boundary.manager.status(boundary.id()).unwrap().status,
                ScanJobStatus::Paused
            );
            assert_eq!(session_ids(&fixture.database), before);

            boundary.manager.resume(boundary.id()).unwrap();
            let snapshot = boundary.join();

            assert_eq!(snapshot.status, ScanJobStatus::Completed);
            let saved = snapshot.saved_scan_id.unwrap();
            assert_eq!(counted_size(&fixture.database, saved), 8 + 32);
            let stored = fixture.stored_checkpoint();
            assert_eq!(stored.history_token, "fsevents:v1:20");
            assert_eq!(stored.baseline_scan_id, Some(saved));
        }

        /// 確定開始とcancelが同時でも、応答と結果が食い違わない。
        ///
        /// どちらが先でもよいが、cancelが成功を返したなら確定していない、
        /// 断られたなら確定している、のどちらかでなければならない。
        #[test]
        fn settles_a_cancel_that_races_the_start_of_the_commit() {
            let fixture = fixture("cancel-race");
            fs::write(fixture.root.join("changed.bin"), [0_u8; 32]).unwrap();
            let before = session_ids(&fixture.database);
            // 確定開始とcancelを同じbarrierから同時に離す。channelの往復を挟むと
            // 片方が必ず先に着いてしまい、競合そのものを再現できない。
            let start = Arc::new(Barrier::new(2));
            let worker_start = Arc::clone(&start);
            let mut boundary = spawn_incremental_worker(
                &fixture,
                &["changed.bin"],
                fixture.advanced("fsevents:v1:20"),
                move || {
                    worker_start.wait();
                },
                || {},
            );

            start.wait();
            let cancelled = boundary.manager.cancel(boundary.id());
            let snapshot = boundary.join();

            match cancelled {
                // 受け付けたなら、baselineもcheckpointも動いていない。
                Ok(_) => {
                    assert_eq!(snapshot.status, ScanJobStatus::Cancelled);
                    assert_eq!(snapshot.saved_scan_id, None);
                    assert_eq!(session_ids(&fixture.database), before);
                    assert_eq!(fixture.stored_checkpoint(), fixture.checkpoint);
                }
                // 断ったなら、確定は最後まで進んでいる。
                Err(error) => {
                    assert_eq!(error, "スキャン結果の確定中はキャンセルできません");
                    assert_eq!(snapshot.status, ScanJobStatus::Completed);
                    let saved = snapshot.saved_scan_id.unwrap();
                    assert_eq!(session_ids(&fixture.database).len(), before.len() + 1);
                    let stored = fixture.stored_checkpoint();
                    assert_eq!(stored.history_token, "fsevents:v1:20");
                    assert_eq!(stored.baseline_scan_id, Some(saved));
                }
            }
        }

        /// 確定フェーズへ入ったあとのpause・cancelは成功を返さない。
        #[test]
        fn refuses_to_pause_or_cancel_once_the_commit_has_started() {
            let fixture = fixture("finalizing");
            fs::write(fixture.root.join("changed.bin"), [0_u8; 32]).unwrap();
            let before = session_ids(&fixture.database);
            let (after_finalize, inside, release_inside) = gate();
            let mut boundary = spawn_incremental_worker(
                &fixture,
                &["changed.bin"],
                fixture.advanced("fsevents:v1:20"),
                || {},
                after_finalize,
            );

            inside.recv().unwrap();

            assert_eq!(
                boundary.manager.pause(boundary.id()).err().as_deref(),
                Some("スキャン結果の確定中は一時停止できません")
            );
            assert_eq!(
                boundary.manager.cancel(boundary.id()).err().as_deref(),
                Some("スキャン結果の確定中はキャンセルできません")
            );
            // 確定処理の間も状態照会は待たされない。
            assert_eq!(
                boundary.manager.status(boundary.id()).unwrap().status,
                ScanJobStatus::Running
            );
            assert_eq!(session_ids(&fixture.database), before);

            release_inside.send(()).unwrap();
            let snapshot = boundary.join();

            assert_eq!(snapshot.status, ScanJobStatus::Completed);
            let saved = snapshot.saved_scan_id.unwrap();
            assert_eq!(session_ids(&fixture.database).len(), before.len() + 1);
            assert_eq!(fixture.stored_checkpoint().baseline_scan_id, Some(saved));
        }

        /// 確定に成功したjobを、あとから来たpause・cancelで覆さない。
        #[test]
        fn refuses_to_pause_or_cancel_after_the_commit_succeeded() {
            let fixture = fixture("after-commit");
            fs::write(fixture.root.join("changed.bin"), [0_u8; 32]).unwrap();
            let mut boundary = spawn_incremental_worker(
                &fixture,
                &["changed.bin"],
                fixture.advanced("fsevents:v1:20"),
                || {},
                || {},
            );

            let snapshot = boundary.join();
            assert_eq!(snapshot.status, ScanJobStatus::Completed);
            let saved = snapshot.saved_scan_id.unwrap();

            assert_eq!(
                boundary.manager.pause(boundary.id()).err().as_deref(),
                Some("実行中のスキャンだけを一時停止できます")
            );
            assert_eq!(
                boundary.manager.cancel(boundary.id()).err().as_deref(),
                Some("完了済みのスキャンはキャンセルできません")
            );
            let after = boundary.manager.status(boundary.id()).unwrap();
            assert_eq!(after.status, ScanJobStatus::Completed);
            assert_eq!(after.saved_scan_id, Some(saved));
            assert_eq!(fixture.stored_checkpoint().baseline_scan_id, Some(saved));
        }

        /// 確定に失敗したjobはFailedのまま。断ったcancelでCancelledへ倒さない。
        #[test]
        fn reports_a_failed_commit_as_failed_even_when_a_cancel_was_rejected() {
            let fixture = fixture("finalize-failure");
            fs::write(fixture.root.join("changed.bin"), [0_u8; 32]).unwrap();
            let before = session_ids(&fixture.database);
            let (after_finalize, inside, release_inside) = gate();
            let mut boundary = spawn_incremental_worker(
                &fixture,
                &["changed.bin"],
                fixture.advanced("fsevents:v1:20"),
                || {},
                after_finalize,
            );

            inside.recv().unwrap();
            assert_eq!(
                boundary.manager.cancel(boundary.id()).err().as_deref(),
                Some("スキャン結果の確定中はキャンセルできません")
            );
            // 確定フェーズの内側で、別のスキャンがcheckpointを進めた状態を作る。
            let newer = IndexCheckpoint {
                history_token: "fsevents:v1:90".to_owned(),
                updated_at: fixture.checkpoint.updated_at + 1,
                ..fixture.checkpoint.clone()
            };
            upsert_checkpoint(&Connection::open(&fixture.database).unwrap(), &newer).unwrap();
            release_inside.send(()).unwrap();
            let snapshot = boundary.join();

            assert_eq!(snapshot.status, ScanJobStatus::Failed);
            assert_eq!(
                snapshot.error.as_deref(),
                Some("差分更新の前提となるcheckpointが更新されています")
            );
            assert_eq!(snapshot.saved_scan_id, None);
            assert_eq!(session_ids(&fixture.database), before);
            assert_eq!(fixture.stored_checkpoint(), newer);
        }

        #[test]
        fn refuses_to_commit_when_another_scan_advanced_the_checkpoint() {
            let fixture = fixture("stale-assessment");
            fs::write(fixture.root.join("changed.bin"), [0_u8; 32]).unwrap();
            let before = session_ids(&fixture.database);
            let (before_finalize, at_boundary, release) = gate();
            let mut boundary = spawn_incremental_worker(
                &fixture,
                &["changed.bin"],
                fixture.advanced("fsevents:v1:20"),
                before_finalize,
                || {},
            );

            at_boundary.recv().unwrap();
            // 評価から確定までの間に、別のスキャンがcheckpointを進めた状態を作る。
            let newer = IndexCheckpoint {
                history_token: "fsevents:v1:90".to_owned(),
                updated_at: fixture.checkpoint.updated_at + 1,
                ..fixture.checkpoint.clone()
            };
            upsert_checkpoint(&Connection::open(&fixture.database).unwrap(), &newer).unwrap();
            release.send(()).unwrap();
            let snapshot = boundary.join();

            assert_eq!(snapshot.status, ScanJobStatus::Failed);
            assert_eq!(
                snapshot.error.as_deref(),
                Some("差分更新の前提となるcheckpointが更新されています")
            );
            assert_eq!(snapshot.saved_scan_id, None);
            assert_eq!(session_ids(&fixture.database), before);
            // 進んだcheckpointを古いtokenで巻き戻さない。
            assert_eq!(fixture.stored_checkpoint(), newer);
        }

        #[test]
        fn refuses_to_commit_when_the_scan_root_was_replaced() {
            let fixture = fixture("root-replaced");
            fs::write(fixture.root.join("changed.bin"), [0_u8; 32]).unwrap();
            let before = session_ids(&fixture.database);
            let (before_finalize, at_boundary, release) = gate();
            let mut boundary = spawn_incremental_worker(
                &fixture,
                &["changed.bin"],
                fixture.advanced("fsevents:v1:20"),
                before_finalize,
                || {},
            );

            at_boundary.recv().unwrap();
            // 走査中に同じpathへ別のdirectoryが入れ替わった状態を作る。
            let replaced = fixture.root.with_extension("replaced");
            fs::rename(&fixture.root, &replaced).unwrap();
            fs::create_dir_all(&fixture.root).unwrap();
            release.send(()).unwrap();
            let snapshot = boundary.join();

            assert_eq!(snapshot.status, ScanJobStatus::Failed);
            assert_eq!(
                snapshot.error.as_deref(),
                Some("走査中に走査rootが入れ替わりました")
            );
            assert_eq!(session_ids(&fixture.database), before);
            assert_eq!(fixture.stored_checkpoint(), fixture.checkpoint);
            let _ = fs::remove_dir_all(replaced);
        }

        #[test]
        fn completes_an_incremental_update_and_points_at_the_new_snapshot() {
            let fixture = fixture("complete");
            fs::write(fixture.root.join("changed.bin"), [0_u8; 32]).unwrap();
            fs::write(fixture.root.join("added.bin"), [0_u8; 2]).unwrap();
            let job = new_job(&fixture.checkpoint.root_path);

            fixture.apply(
                &job,
                &["changed.bin", "added.bin"],
                fixture.baseline_scan_id,
                &fixture.advanced("fsevents:v1:20"),
            );

            let snapshot = job.snapshot();
            assert_eq!(snapshot.status, ScanJobStatus::Completed);
            assert_eq!(snapshot.error, None);
            let saved = snapshot.saved_scan_id.unwrap();
            assert_ne!(saved, fixture.baseline_scan_id);
            assert_eq!(counted_size(&fixture.database, saved), 8 + 32 + 2);
            // 基準scanは破棄せず、過去の履歴として残す。
            assert_eq!(
                counted_size(&fixture.database, fixture.baseline_scan_id),
                12
            );
            let stored = fixture.stored_checkpoint();
            assert_eq!(stored.history_token, "fsevents:v1:20");
            assert_eq!(stored.baseline_scan_id, Some(saved));
        }

        #[test]
        fn keeps_the_baseline_and_checkpoint_when_cancelled() {
            let fixture = fixture("cancelled");
            fs::write(fixture.root.join("changed.bin"), [0_u8; 32]).unwrap();
            let before = session_ids(&fixture.database);
            let job = new_job(&fixture.checkpoint.root_path);
            job.control
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .cancelled = true;

            fixture.apply(
                &job,
                &["changed.bin"],
                fixture.baseline_scan_id,
                &fixture.advanced("fsevents:v1:20"),
            );

            let snapshot = job.snapshot();
            assert_eq!(snapshot.status, ScanJobStatus::Cancelled);
            assert_eq!(snapshot.saved_scan_id, None);
            assert_eq!(session_ids(&fixture.database), before);
            assert_eq!(fixture.stored_checkpoint(), fixture.checkpoint);
        }

        #[test]
        fn rolls_back_the_new_snapshot_when_the_checkpoint_cannot_be_saved() {
            let fixture = fixture("rollback");
            fs::write(fixture.root.join("changed.bin"), [0_u8; 32]).unwrap();
            let before = session_ids(&fixture.database);
            let job = new_job(&fixture.checkpoint.root_path);

            // tokenを保存できないcheckpointで、確定直前の失敗を再現する。
            fixture.apply(
                &job,
                &["changed.bin"],
                fixture.baseline_scan_id,
                &fixture.advanced(""),
            );

            let snapshot = job.snapshot();
            assert_eq!(snapshot.status, ScanJobStatus::Failed);
            assert!(snapshot.error.is_some());
            assert_eq!(snapshot.saved_scan_id, None);
            assert_eq!(session_ids(&fixture.database), before);
            assert_eq!(fixture.stored_checkpoint(), fixture.checkpoint);
        }

        /// 読み取れない対象を含む差分は確定しない。
        ///
        /// 権限を落とすのはfixture配下のdirectoryだけで、実データには触れない。
        #[test]
        fn refuses_to_commit_when_a_target_cannot_be_read() {
            use std::os::unix::fs::PermissionsExt;
            let fixture = fixture("unreadable-target");
            let before = session_ids(&fixture.database);
            let job = new_job(&fixture.checkpoint.root_path);
            let denied = fixture.root.join("dir");
            fs::set_permissions(&denied, fs::Permissions::from_mode(0o000)).unwrap();

            fixture.apply(
                &job,
                &["dir"],
                fixture.baseline_scan_id,
                &fixture.advanced("fsevents:v1:20"),
            );

            fs::set_permissions(&denied, fs::Permissions::from_mode(0o755)).unwrap();
            let snapshot = job.snapshot();
            assert_eq!(snapshot.status, ScanJobStatus::Failed);
            assert_eq!(
                snapshot.error.as_deref(),
                Some("部分再走査targetを完全に読み取れません（directory_replaced_or_unreadable）")
            );
            assert_eq!(snapshot.saved_scan_id, None);
            assert_eq!(session_ids(&fixture.database), before);
            assert_eq!(fixture.stored_checkpoint(), fixture.checkpoint);
        }

        #[test]
        fn refuses_to_apply_when_the_checkpoint_does_not_name_the_update_baseline() {
            let fixture = fixture("mismatch");
            let before = session_ids(&fixture.database);
            let job = new_job(&fixture.checkpoint.root_path);

            fixture.apply(
                &job,
                &["changed.bin"],
                fixture.baseline_scan_id + 1,
                &fixture.advanced("fsevents:v1:20"),
            );

            let snapshot = job.snapshot();
            assert_eq!(snapshot.status, ScanJobStatus::Failed);
            assert_eq!(
                snapshot.error.as_deref(),
                Some("checkpointが指す基準スキャンと更新元が一致しません")
            );
            assert_eq!(session_ids(&fixture.database), before);
            assert_eq!(fixture.stored_checkpoint(), fixture.checkpoint);
        }
    }
}
