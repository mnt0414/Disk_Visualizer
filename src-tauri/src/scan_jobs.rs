use crate::scanner::{self, ScanSummary};
use serde::Serialize;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanJobSnapshot {
    pub id: u64,
    pub path: String,
    pub status: String,
    pub current_path: String,
    pub total_size_bytes: u64,
    pub file_count: u64,
    pub directory_count: u64,
    pub skipped_count: u64,
    pub result: Option<ScanSummary>,
    pub error: Option<String>,
}

struct JobState {
    status: String,
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
            status: state.status.clone(),
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
    fn progress(&self, path: &Path, files: u64, dirs: u64, skipped: u64, size: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.current_path = path.to_string_lossy().into_owned();
        state.file_count = state.file_count.saturating_add(files);
        state.directory_count = state.directory_count.saturating_add(dirs);
        state.skipped_count = state.skipped_count.saturating_add(skipped);
        state.total_size_bytes = state.total_size_bytes.saturating_add(size);
    }
}

#[derive(Default)]
pub struct ScanManager {
    active: Mutex<Option<Arc<ScanJob>>>,
}
impl ScanManager {
    pub fn start(&self, path: String) -> Result<ScanJobSnapshot, String> {
        if !Path::new(&path).is_absolute() {
            return Err("スキャン対象には絶対パスを指定してください".to_owned());
        }
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(job) = active.as_ref() {
            let status = job.snapshot().status;
            if !matches!(status.as_str(), "completed" | "cancelled" | "failed") {
                return Err("別のスキャンが実行中です".to_owned());
            }
        }
        let job = Arc::new(ScanJob {
            id: NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed),
            path: path.clone(),
            state: Mutex::new(JobState {
                status: "running".to_owned(),
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
            let result = scanner::scan_folder_path_controlled(
                Path::new(&path),
                move || control_job.can_continue(),
                move |p, f, d, s, b| progress_job.progress(p, f, d, s, b),
            );
            let cancelled = job
                .control
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .cancelled;
            let mut state = job.state.lock().unwrap_or_else(|e| e.into_inner());
            match result {
                Ok(summary) if !cancelled => {
                    state.status = "completed".to_owned();
                    state.current_path.clear();
                    state.result = Some(summary);
                }
                _ if cancelled => {
                    state.status = "cancelled".to_owned();
                    state.current_path.clear();
                }
                Err(error) => {
                    state.status = "failed".to_owned();
                    state.error = Some(error);
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
            let mut control = job.control.lock().unwrap_or_else(|e| e.into_inner());
            control.paused = true;
        }
        {
            job.state.lock().unwrap_or_else(|e| e.into_inner()).status = "paused".to_owned();
        }
        Ok(job.snapshot())
    }
    pub fn resume(&self, id: u64) -> Result<ScanJobSnapshot, String> {
        let job = self.job(id)?;
        {
            let mut control = job.control.lock().unwrap_or_else(|e| e.into_inner());
            control.paused = false;
        }
        {
            job.state.lock().unwrap_or_else(|e| e.into_inner()).status = "running".to_owned();
        }
        job.wake.notify_all();
        Ok(job.snapshot())
    }
    pub fn cancel(&self, id: u64) -> Result<ScanJobSnapshot, String> {
        let job = self.job(id)?;
        {
            let mut control = job.control.lock().unwrap_or_else(|e| e.into_inner());
            control.cancelled = true;
            control.paused = false;
        }
        job.wake.notify_all();
        Ok(job.snapshot())
    }
}
