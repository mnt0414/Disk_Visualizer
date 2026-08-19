pub mod cache_activity;
mod cache_catalog;
mod cache_query;
mod file_metrics;
pub mod index_checkpoint;
pub mod index_trust;
mod scan_jobs;
mod scanner;
mod storage;

use cache_catalog::{CacheCatalog, CacheDefinition, CachePathRoot, Platform};
use cache_query::{CacheEntryDetail, CacheQueryRepository};
use index_checkpoint::IndexCheckpointRepository;
use scan_jobs::{ScanJobSnapshot, ScanManager};
use scanner::ScanSummary;
use serde::Serialize;
use storage::{SavedScan, ScanRepository};
use tauri::{Manager, State};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    name: &'static str,
    version: &'static str,
    platform: &'static str,
    architecture: &'static str,
}
#[tauri::command]
fn get_app_info() -> AppInfo {
    AppInfo {
        name: "Disk Visualizer",
        version: env!("CARGO_PKG_VERSION"),
        platform: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
    }
}
#[tauri::command]
fn get_cache_catalog() -> Result<CacheCatalog, String> {
    Ok(cache_catalog::bundled_catalog()?.clone())
}
#[tauri::command]
fn classify_cache_path(
    platform: Platform,
    root: CachePathRoot,
    relative_path: String,
) -> Result<Vec<CacheDefinition>, String> {
    Ok(cache_catalog::bundled_catalog()?
        .classify(platform, root, &relative_path)
        .into_iter()
        .cloned()
        .collect())
}
#[tauri::command]
fn scan_folder(path: String) -> Result<ScanSummary, String> {
    scanner::scan_folder_path(std::path::Path::new(&path))
}
#[tauri::command]
fn start_scan(path: String, manager: State<'_, ScanManager>) -> Result<ScanJobSnapshot, String> {
    manager.start(path)
}
#[tauri::command]
fn get_scan_status(id: u64, manager: State<'_, ScanManager>) -> Result<ScanJobSnapshot, String> {
    manager.status(id)
}
#[tauri::command]
fn pause_scan(id: u64, manager: State<'_, ScanManager>) -> Result<ScanJobSnapshot, String> {
    manager.pause(id)
}
#[tauri::command]
fn resume_scan(id: u64, manager: State<'_, ScanManager>) -> Result<ScanJobSnapshot, String> {
    manager.resume(id)
}
#[tauri::command]
fn cancel_scan(id: u64, manager: State<'_, ScanManager>) -> Result<ScanJobSnapshot, String> {
    manager.cancel(id)
}
#[tauri::command]
fn list_saved_scans(repository: State<'_, ScanRepository>) -> Result<Vec<SavedScan>, String> {
    repository.list()
}
#[tauri::command]
fn list_cache_entries(
    scan_id: i64,
    limit: Option<u32>,
    offset: Option<u32>,
    repository: State<'_, CacheQueryRepository>,
) -> Result<Vec<CacheEntryDetail>, String> {
    repository.list(scan_id, limit, offset)
}
#[tauri::command]
fn delete_saved_scan(id: i64, repository: State<'_, ScanRepository>) -> Result<(), String> {
    repository.delete(id)
}
#[tauri::command]
fn check_scan_index(repository: State<'_, ScanRepository>) -> Result<bool, String> {
    repository.integrity_check()
}
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let database_path = app.path().app_data_dir()?.join("scan-index.sqlite3");
            let cache_query_repository = CacheQueryRepository::new(database_path.clone());
            let checkpoint_repository = IndexCheckpointRepository::new(database_path.clone());
            let repository = ScanRepository::new(database_path);
            repository.initialize().map_err(std::io::Error::other)?;
            checkpoint_repository
                .initialize()
                .map_err(std::io::Error::other)?;
            app.manage(ScanManager::new(repository.clone()));
            app.manage(checkpoint_repository);
            app.manage(cache_query_repository);
            app.manage(repository);
            Ok(())
        })
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            get_app_info,
            get_cache_catalog,
            classify_cache_path,
            scan_folder,
            start_scan,
            get_scan_status,
            pause_scan,
            resume_scan,
            cancel_scan,
            list_saved_scans,
            list_cache_entries,
            delete_saved_scan,
            check_scan_index
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Disk Visualizer");
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn app_info_matches_package_version() {
        let info = get_app_info();
        assert_eq!(info.name, "Disk Visualizer");
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
    }
    #[test]
    fn exposes_bundled_cache_catalog() {
        let catalog = get_cache_catalog().unwrap();
        assert!(!catalog.catalog_version.is_empty());
        assert!(!catalog.definitions.is_empty());
    }
    #[test]
    fn exposes_cache_path_classification() {
        let matches = classify_cache_path(
            Platform::Windows,
            CachePathRoot::LocalAppData,
            "Google/Chrome/User Data/Default/Cache/data_1".to_owned(),
        )
        .unwrap();
        assert_eq!(matches.len(), 1);
    }
}
