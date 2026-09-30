use crate::incremental_trust::{assess_macos_index_trust, MacosIndexTrustAssessment};
use crate::incremental_update::{
    run_incremental_update, trust_gate, FullScanRequiredReason, IncrementalUpdateOutcome,
};
use crate::index_checkpoint::{capture_full_scan_checkpoint, IndexCheckpointRepository};
use crate::index_trust::IndexTrustState;
use crate::scanner::{self, ScanProgress, ScanSummary};
use crate::storage::ScanRepository;
use serde::Serialize;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(1);
/// FSEvents履歴から一度に読み取る変更数の上限。超える場合は履歴不連続としてフルスキャンへ戻る。
const MAX_HISTORY_CHANGES: usize = 4_096;
const HISTORY_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// scanジョブの種類。既存の`start_scan`は`full`、差分更新は`incremental`。
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ScanJobMode {
    Full,
    Incremental,
}
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
    /// 差分更新の完了時は`None`。ScanSummary(上位項目)の構築にはSQLite全体の再集計が必要なため、
    /// 保存済みsessionは`saved_scan_id`から取得する。
    pub result: Option<ScanSummary>,
    pub error: Option<String>,
    pub mode: ScanJobMode,
    /// 完了時に保存されたscan sessionのID。キャンセル・失敗・フルスキャン要求では`None`。
    pub saved_scan_id: Option<i64>,
    /// 差分更新できずフルスキャンが必要な場合の理由。このときstatusは`failed`になり、
    /// sessionもcheckpointも書き込まれていない。
    pub full_scan_required: Option<FullScanRequiredReason>,
}
struct JobState {
    status: ScanJobStatus,
    current_path: String,
    total_size_bytes: u64,
    file_count: u64,
    directory_count: u64,
    skipped_count: u64,
    result: Option<ScanSummary>,
    error: Option<String>,
    saved_scan_id: Option<i64>,
    full_scan_required: Option<FullScanRequiredReason>,
}
struct Control {
    paused: bool,
    cancelled: bool,
}
struct ScanJob {
    id: u64,
    path: String,
    mode: ScanJobMode,
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
            error: state.error.clone(),
            mode: self.mode,
            saved_scan_id: state.saved_scan_id,
            full_scan_required: state.full_scan_required,
        }
    }
    fn new(path: &str, mode: ScanJobMode) -> Arc<Self> {
        Arc::new(Self {
            id: NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed),
            path: path.to_owned(),
            mode,
            state: Mutex::new(JobState {
                status: ScanJobStatus::Running,
                current_path: path.to_owned(),
                total_size_bytes: 0,
                file_count: 0,
                directory_count: 0,
                skipped_count: 0,
                result: None,
                error: None,
                saved_scan_id: None,
                full_scan_required: None,
            }),
            control: Mutex::new(Control {
                paused: false,
                cancelled: false,
            }),
            wake: Condvar::new(),
        })
    }
    fn can_continue(&self) -> bool {
        let mut control = self.control.lock().unwrap_or_else(|e| e.into_inner());
        while control.paused && !control.cancelled {
            control = self.wake.wait(control).unwrap_or_else(|e| e.into_inner());
        }
        !control.cancelled
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
impl ScanJob {
    /// 差分更新の結果をjob状態へ反映する。適用済みのsessionがあればキャンセル要求より優先して
    /// 完了として報告する。`FullScanRequired`とエラーではsessionもcheckpointも書き込まれていない。
    fn finish_incremental(&self, result: Result<IncrementalUpdateOutcome, String>) {
        let cancelled = self
            .control
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cancelled;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.current_path.clear();
        match result {
            Ok(IncrementalUpdateOutcome::Applied {
                scan_id,
                total_size_bytes,
                file_count,
                directory_count,
                skipped_count,
            }) => {
                state.status = ScanJobStatus::Completed;
                state.saved_scan_id = Some(scan_id);
                state.total_size_bytes = total_size_bytes;
                state.file_count = file_count;
                state.directory_count = directory_count;
                state.skipped_count = skipped_count;
            }
            Ok(IncrementalUpdateOutcome::FullScanRequired { .. }) | Err(_) if cancelled => {
                state.status = ScanJobStatus::Cancelled;
            }
            Ok(IncrementalUpdateOutcome::FullScanRequired { reason }) => {
                state.status = ScanJobStatus::Failed;
                state.error = Some(reason.message());
                state.full_scan_required = Some(reason);
            }
            Err(error) => {
                state.status = ScanJobStatus::Failed;
                state.error = Some(error);
            }
        }
    }
}
fn run_incremental_job<A>(
    job: &Arc<ScanJob>,
    database_path: &Path,
    path: &str,
    assess: A,
) -> Result<IncrementalUpdateOutcome, String>
where
    A: FnOnce(&Path, &IndexCheckpointRepository) -> Result<MacosIndexTrustAssessment, String>,
{
    let root = Path::new(path)
        .canonicalize()
        .map_err(|error| format!("差分更新対象を解決できません: {error}"))?;
    let repository = IndexCheckpointRepository::new(database_path.to_path_buf());
    let assessment = assess(&root, &repository)?;
    if let Some(reason) = trust_gate(&assessment) {
        return Ok(IncrementalUpdateOutcome::FullScanRequired { reason });
    }
    let Some(checkpoint) = repository.load(&root.to_string_lossy())? else {
        return Ok(IncrementalUpdateOutcome::FullScanRequired {
            reason: FullScanRequiredReason::TrustNotTrusted {
                state: IndexTrustState::InitialScanRequired,
            },
        });
    };
    let control_job = Arc::clone(job);
    let progress_job = Arc::clone(job);
    run_incremental_update(
        database_path,
        &root,
        &checkpoint,
        &assessment,
        move || control_job.can_continue(),
        move |progress| progress_job.progress(progress),
    )
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
    pub fn start(&self, path: String) -> Result<ScanJobSnapshot, String> {
        if !Path::new(&path).is_absolute() {
            return Err("スキャン対象には絶対パスを指定してください".to_owned());
        }
        // sessionのroot_pathとcheckpointのroot_pathを常に一致させるため、canonical pathで
        // 保存・走査する。snapshotのpathはUI互換のため利用者が指定した文字列のまま返す。
        let root = Path::new(&path)
            .canonicalize()
            .map_err(|error| format!("スキャン対象を開けません: {error}"))?;
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        Self::ensure_idle(&active)?;
        let checkpoint = capture_full_scan_checkpoint(&root).ok().flatten();
        let stream = self.repository.begin_stream(&root.to_string_lossy())?;
        let job = ScanJob::new(&path, ScanJobMode::Full);
        *active = Some(Arc::clone(&job));
        let snapshot = job.snapshot();
        std::thread::spawn(move || {
            let control_job = Arc::clone(&job);
            let progress_job = Arc::clone(&job);
            let progress_stream = stream.clone();
            let result = scanner::scan_folder_path_controlled(
                &root,
                move || control_job.can_continue(),
                move |progress| {
                    progress_job.progress(progress);
                    progress_stream.record(progress);
                },
            );
            let cancelled = job
                .control
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .cancelled;
            match result {
                Ok(summary) if !cancelled => {
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
    /// 保存済みcheckpointとOSの変更履歴から差分更新jobを開始する。
    /// フルスキャンと同じ単一のactive slotを使い、pause/resume/cancelも共通。
    pub fn start_incremental(&self, path: String) -> Result<ScanJobSnapshot, String> {
        self.start_incremental_with(path, |root, repository| {
            assess_macos_index_trust(
                root,
                repository,
                Some(MAX_HISTORY_CHANGES),
                HISTORY_READ_TIMEOUT,
            )
        })
    }
    fn start_incremental_with<A>(&self, path: String, assess: A) -> Result<ScanJobSnapshot, String>
    where
        A: FnOnce(&Path, &IndexCheckpointRepository) -> Result<MacosIndexTrustAssessment, String>
            + Send
            + 'static,
    {
        if !Path::new(&path).is_absolute() {
            return Err("差分更新対象には絶対パスを指定してください".to_owned());
        }
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        Self::ensure_idle(&active)?;
        let job = ScanJob::new(&path, ScanJobMode::Incremental);
        *active = Some(Arc::clone(&job));
        let snapshot = job.snapshot();
        let database_path = self.repository.database_path().to_path_buf();
        std::thread::spawn(move || {
            let result = run_incremental_job(&job, &database_path, &path, assess);
            job.finish_incremental(result);
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
            job.control.lock().unwrap_or_else(|e| e.into_inner()).paused = true;
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
        }
        {
            let mut control = job.control.lock().unwrap_or_else(|e| e.into_inner());
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
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::Duration;

    fn job(status: ScanJobStatus, paused: bool) -> Arc<ScanJob> {
        Arc::new(ScanJob {
            id: 9001,
            path: std::env::temp_dir().to_string_lossy().into_owned(),
            mode: ScanJobMode::Full,
            state: Mutex::new(JobState {
                status,
                current_path: String::new(),
                total_size_bytes: 0,
                file_count: 0,
                directory_count: 0,
                skipped_count: 0,
                result: None,
                error: None,
                saved_scan_id: None,
                full_scan_required: None,
            }),
            control: Mutex::new(Control {
                paused,
                cancelled: false,
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
    struct Workspace {
        tree: PathBuf,
        data: PathBuf,
        database: PathBuf,
        manager: ScanManager,
    }
    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.tree);
            let _ = std::fs::remove_dir_all(&self.data);
        }
    }
    fn temporary_directory(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "disk-visualizer-jobs-{name}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path.canonicalize().unwrap()
    }
    fn wait_until_finished(manager: &ScanManager, id: u64) -> ScanJobSnapshot {
        for _ in 0..500 {
            let snapshot = manager.status(id).unwrap();
            if !matches!(
                snapshot.status,
                ScanJobStatus::Running | ScanJobStatus::Paused
            ) {
                return snapshot;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("スキャンジョブが終了しません");
    }
    fn checkpoint(root: &Path, token: &str) -> crate::index_checkpoint::IndexCheckpoint {
        crate::index_checkpoint::IndexCheckpoint {
            root_path: root.to_string_lossy().into_owned(),
            platform: "macos".to_owned(),
            volume_identity: "volume-1".to_owned(),
            root_identity: "root-1".to_owned(),
            history_source: "fsevents".to_owned(),
            history_token: token.to_owned(),
            updated_at: 1,
        }
    }
    fn trusted(changes: &[&str]) -> MacosIndexTrustAssessment {
        use crate::fsevents_callback::CollectedFseventsChange;
        use crate::index_trust::{IndexTrustDecision, ScanRecommendation};
        use crate::macos_fsevents::FseventsEvent;
        MacosIndexTrustAssessment {
            decision: IndexTrustDecision {
                state: IndexTrustState::Trusted,
                recommendation: ScanRecommendation::Incremental,
            },
            changes: changes
                .iter()
                .map(|value| CollectedFseventsChange {
                    relative_path: PathBuf::from(value),
                    event: FseventsEvent {
                        event_id: 11,
                        flags: 0,
                    },
                })
                .collect(),
            rescan_subtrees: false,
            next_history_token: Some("fsevents:v1:20".to_owned()),
        }
    }
    /// 初期化済みDBと、フルスキャン済みで保存済みcheckpointを持つ作業領域を用意する。
    fn workspace(name: &str) -> Workspace {
        let tree = temporary_directory(&format!("{name}-tree"));
        let data = temporary_directory(&format!("{name}-data"));
        std::fs::write(tree.join("a.txt"), [1_u8; 4]).unwrap();
        std::fs::create_dir(tree.join("dir")).unwrap();
        std::fs::write(tree.join("dir").join("b.bin"), [2_u8; 8]).unwrap();
        let database = data.join("scan-index.sqlite3");
        let repository = ScanRepository::new(database.clone());
        repository.initialize().unwrap();
        let checkpoints = IndexCheckpointRepository::new(database.clone());
        checkpoints.initialize().unwrap();
        let manager = ScanManager::new(repository);
        let started = manager.start(tree.to_string_lossy().into_owned()).unwrap();
        let finished = wait_until_finished(&manager, started.id);
        assert_eq!(finished.status, ScanJobStatus::Completed);
        assert_eq!(finished.mode, ScanJobMode::Full);
        assert!(finished.saved_scan_id.is_some());
        checkpoints
            .save(&checkpoint(&tree, "fsevents:v1:10"))
            .unwrap();
        Workspace {
            tree,
            data,
            database,
            manager,
        }
    }
    fn session_count(database: &Path) -> i64 {
        rusqlite::Connection::open(database)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM scan_sessions", [], |row| row.get(0))
            .unwrap()
    }
    fn stored_token(workspace: &Workspace) -> String {
        IndexCheckpointRepository::new(workspace.database.clone())
            .load(&workspace.tree.to_string_lossy())
            .unwrap()
            .unwrap()
            .history_token
    }
    #[test]
    fn rejects_incremental_start_while_active() {
        let manager = manager(job(ScanJobStatus::Running, false));
        assert_eq!(
            manager
                .start_incremental(std::env::temp_dir().to_string_lossy().into_owned())
                .err()
                .unwrap(),
            "別のスキャンが実行中です"
        );
    }
    fn stored_root_paths(database: &Path) -> Vec<String> {
        let connection = rusqlite::Connection::open(database).unwrap();
        let mut statement = connection
            .prepare("SELECT root_path FROM scan_sessions ORDER BY id")
            .unwrap();
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    }
    #[test]
    fn full_scan_through_non_canonical_path_stores_canonical_root() {
        let workspace = workspace("canonical-dot");
        let user_path = workspace.tree.join(".").to_string_lossy().into_owned();
        let started = workspace.manager.start(user_path.clone()).unwrap();
        assert_eq!(started.path, user_path);
        let finished = wait_until_finished(&workspace.manager, started.id);
        assert_eq!(finished.status, ScanJobStatus::Completed);
        assert_eq!(finished.path, user_path);
        let canonical = workspace.tree.to_string_lossy().into_owned();
        assert!(stored_root_paths(&workspace.database)
            .iter()
            .all(|root| root == &canonical));
    }
    #[cfg(unix)]
    #[test]
    fn full_scan_through_symlinked_path_stores_canonical_root() {
        let workspace = workspace("canonical-link");
        let links = temporary_directory("canonical-links");
        let link = links.join("via-link");
        std::os::unix::fs::symlink(&workspace.tree, &link).unwrap();
        let started = workspace
            .manager
            .start(link.to_string_lossy().into_owned())
            .unwrap();
        let finished = wait_until_finished(&workspace.manager, started.id);
        assert_eq!(finished.status, ScanJobStatus::Completed);
        let canonical = workspace.tree.to_string_lossy().into_owned();
        assert!(stored_root_paths(&workspace.database)
            .iter()
            .all(|root| root == &canonical));
        let entries: i64 = rusqlite::Connection::open(&workspace.database)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM scan_entries WHERE scan_id=?1 AND relative_path=?2",
                rusqlite::params![finished.saved_scan_id.unwrap(), "a.txt"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(entries, 1);
        std::fs::remove_dir_all(links).unwrap();
    }
    #[test]
    fn rejects_full_scan_of_missing_path() {
        let manager = manager(job(ScanJobStatus::Completed, false));
        let missing = std::env::temp_dir().join("disk-visualizer-missing-scan-root");
        assert!(manager
            .start(missing.to_string_lossy().into_owned())
            .is_err());
    }
    #[test]
    fn deleting_latest_scan_forces_full_scan_for_incremental_job() {
        let workspace = workspace("delete-latest");
        let saved = ScanRepository::new(workspace.database.clone())
            .list()
            .unwrap();
        ScanRepository::new(workspace.database.clone())
            .delete(saved[0].id)
            .unwrap();
        assert_eq!(
            IndexCheckpointRepository::new(workspace.database.clone())
                .load(&workspace.tree.to_string_lossy())
                .unwrap(),
            None
        );
        let sessions = session_count(&workspace.database);
        let started = workspace
            .manager
            .start_incremental_with(workspace.tree.to_string_lossy().into_owned(), |_, _| {
                Ok(trusted(&["a.txt"]))
            })
            .unwrap();
        let finished = wait_until_finished(&workspace.manager, started.id);
        assert_eq!(finished.status, ScanJobStatus::Failed);
        assert_eq!(
            finished.full_scan_required,
            Some(FullScanRequiredReason::TrustNotTrusted {
                state: IndexTrustState::InitialScanRequired
            })
        );
        assert_eq!(session_count(&workspace.database), sessions);
    }
    #[test]
    fn rejects_relative_incremental_path() {
        let manager = manager(job(ScanJobStatus::Completed, false));
        assert!(manager.start_incremental("relative".to_owned()).is_err());
    }
    #[test]
    fn completes_incremental_job_with_saved_session() {
        let workspace = workspace("incremental");
        std::fs::write(workspace.tree.join("a.txt"), [1_u8; 10]).unwrap();
        let sessions = session_count(&workspace.database);
        let started = workspace
            .manager
            .start_incremental_with(workspace.tree.to_string_lossy().into_owned(), |_, _| {
                Ok(trusted(&["a.txt"]))
            })
            .unwrap();
        assert_eq!(started.mode, ScanJobMode::Incremental);
        let finished = wait_until_finished(&workspace.manager, started.id);
        assert_eq!(finished.status, ScanJobStatus::Completed);
        assert!(finished.error.is_none());
        assert!(finished.full_scan_required.is_none());
        assert!(finished.result.is_none());
        assert_eq!(session_count(&workspace.database), sessions + 1);
        let fresh = scanner::scan_folder_path(&workspace.tree).unwrap();
        assert_eq!(finished.total_size_bytes, fresh.total_size_bytes);
        assert_eq!(finished.file_count, fresh.file_count);
        assert_eq!(finished.directory_count, fresh.directory_count);
        assert_eq!(finished.skipped_count, fresh.skipped_count);
        let saved = ScanRepository::new(workspace.database.clone())
            .list()
            .unwrap();
        assert_eq!(saved[0].id, finished.saved_scan_id.unwrap());
        assert_eq!(saved[0].total_size_bytes, fresh.total_size_bytes);
        assert_eq!(stored_token(&workspace), "fsevents:v1:20");
        // 完了後は同じslotで次のjobを開始できる。
        let next = workspace
            .manager
            .start_incremental_with(workspace.tree.to_string_lossy().into_owned(), |_, _| {
                Ok(trusted(&[]))
            })
            .unwrap();
        assert_eq!(
            wait_until_finished(&workspace.manager, next.id).status,
            ScanJobStatus::Completed
        );
    }
    #[test]
    fn reports_full_scan_required_without_writing() {
        let workspace = workspace("full-required");
        let sessions = session_count(&workspace.database);
        let started =
            workspace
                .manager
                .start_incremental_with(workspace.tree.to_string_lossy().into_owned(), |_, _| {
                    Ok(trusted(&["a.txt", "..", "x"])
                        .with_state(IndexTrustState::HistoryDiscontinuous))
                })
                .unwrap();
        let finished = wait_until_finished(&workspace.manager, started.id);
        assert_eq!(finished.status, ScanJobStatus::Failed);
        assert_eq!(
            finished.full_scan_required,
            Some(FullScanRequiredReason::TrustNotTrusted {
                state: IndexTrustState::HistoryDiscontinuous
            })
        );
        assert!(finished.error.is_some());
        assert!(finished.saved_scan_id.is_none());
        assert_eq!(session_count(&workspace.database), sessions);
        assert_eq!(stored_token(&workspace), "fsevents:v1:10");
    }
    #[test]
    fn cancelling_incremental_job_writes_nothing() {
        let workspace = workspace("incremental-cancel");
        std::fs::write(workspace.tree.join("a.txt"), [1_u8; 10]).unwrap();
        let sessions = session_count(&workspace.database);
        let (release, gate) = mpsc::channel::<()>();
        let started = workspace
            .manager
            .start_incremental_with(
                workspace.tree.to_string_lossy().into_owned(),
                move |_, _| {
                    gate.recv().unwrap();
                    Ok(trusted(&["a.txt"]))
                },
            )
            .unwrap();
        assert_eq!(
            workspace
                .manager
                .start_incremental(workspace.tree.to_string_lossy().into_owned())
                .err()
                .unwrap(),
            "別のスキャンが実行中です"
        );
        workspace.manager.cancel(started.id).unwrap();
        release.send(()).unwrap();
        let finished = wait_until_finished(&workspace.manager, started.id);
        assert_eq!(finished.status, ScanJobStatus::Cancelled);
        assert!(finished.saved_scan_id.is_none());
        assert_eq!(session_count(&workspace.database), sessions);
        assert_eq!(stored_token(&workspace), "fsevents:v1:10");
    }
    #[test]
    fn incremental_failure_reports_error_without_writing() {
        let workspace = workspace("incremental-error");
        let sessions = session_count(&workspace.database);
        let started = workspace
            .manager
            .start_incremental_with(workspace.tree.to_string_lossy().into_owned(), |_, _| {
                Err("履歴を取得できません".to_owned())
            })
            .unwrap();
        let finished = wait_until_finished(&workspace.manager, started.id);
        assert_eq!(finished.status, ScanJobStatus::Failed);
        assert_eq!(finished.error.as_deref(), Some("履歴を取得できません"));
        assert!(finished.full_scan_required.is_none());
        assert_eq!(session_count(&workspace.database), sessions);
    }
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn unsupported_platform_requires_full_scan() {
        let root = temporary_directory("unsupported");
        let manager = ScanManager::new(ScanRepository::new(root.join("unused.sqlite3")));
        let started = manager
            .start_incremental(root.to_string_lossy().into_owned())
            .unwrap();
        let finished = wait_until_finished(&manager, started.id);
        assert_eq!(finished.status, ScanJobStatus::Failed);
        assert_eq!(
            finished.full_scan_required,
            Some(FullScanRequiredReason::TrustNotTrusted {
                state: IndexTrustState::Unsupported
            })
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn applied_session_is_reported_even_if_cancel_arrived_late() {
        let active_job = job(ScanJobStatus::Running, false);
        active_job.control.lock().unwrap().cancelled = true;
        active_job.finish_incremental(Ok(IncrementalUpdateOutcome::Applied {
            scan_id: 7,
            total_size_bytes: 3,
            file_count: 2,
            directory_count: 1,
            skipped_count: 0,
        }));
        let snapshot = active_job.snapshot();
        assert_eq!(snapshot.status, ScanJobStatus::Completed);
        assert_eq!(snapshot.saved_scan_id, Some(7));
        assert_eq!(snapshot.total_size_bytes, 3);
    }
    #[test]
    fn serializes_snapshot_with_backward_compatible_names() {
        let value = serde_json::to_value(job(ScanJobStatus::Running, false).snapshot()).unwrap();
        for name in [
            "id",
            "path",
            "status",
            "currentPath",
            "totalSizeBytes",
            "fileCount",
            "directoryCount",
            "skippedCount",
            "result",
            "error",
        ] {
            assert!(value.get(name).is_some(), "{name}");
        }
        assert_eq!(value["mode"], "full");
        assert!(value["savedScanId"].is_null());
        assert!(value["fullScanRequired"].is_null());
        let mut required = job(ScanJobStatus::Running, false);
        Arc::get_mut(&mut required).unwrap().mode = ScanJobMode::Incremental;
        required.finish_incremental(Ok(IncrementalUpdateOutcome::FullScanRequired {
            reason: FullScanRequiredReason::UnsafeTargetPath,
        }));
        let value = serde_json::to_value(required.snapshot()).unwrap();
        assert_eq!(value["mode"], "incremental");
        assert_eq!(value["fullScanRequired"]["kind"], "unsafe_target_path");
        assert_eq!(value["status"], "failed");
    }
    impl MacosIndexTrustAssessment {
        fn with_state(mut self, state: IndexTrustState) -> Self {
            self.decision.state = state;
            self.decision.recommendation = crate::index_trust::ScanRecommendation::Full;
            self
        }
    }
}
