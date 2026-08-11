use crate::scanner::{self, ScanProgress, ScanSummary};
use crate::storage::ScanRepository;
use serde::Serialize;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(1);
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
    pub error: Option<String>,
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
}
struct Control {
    paused: bool,
    cancelled: bool,
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
        if let Some(job) = active.as_ref() {
            if matches!(
                job.snapshot().status,
                ScanJobStatus::Running | ScanJobStatus::Paused
            ) {
                return Err("別のスキャンが実行中です".to_owned());
            }
        }
        let stream = self.repository.begin_stream(&path)?;
        let job = Arc::new(ScanJob {
            id: NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed),
            path: path.clone(),
            state: Mutex::new(JobState {
                status: ScanJobStatus::Running,
                current_path: path.clone(),
                total_size_bytes: 0,
                file_count: 0,
                directory_count: 0,
                skipped_count: 0,
                result: None,
                error: None,
            }),
            control: Mutex::new(Control {
                paused: false,
                cancelled: false,
            }),
            wake: Condvar::new(),
        });
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
            let cancelled = job
                .control
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .cancelled;
            match result {
                Ok(summary) if !cancelled => {
                    let persisted = stream.complete(&summary);
                    let mut state = job.state.lock().unwrap_or_else(|e| e.into_inner());
                    match persisted {
                        Ok(()) => {
                            state.status = ScanJobStatus::Completed;
                            state.current_path.clear();
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
