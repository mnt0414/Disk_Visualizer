mod scan_jobs;
mod scanner;

use scan_jobs::{ScanJobSnapshot, ScanManager};
use scanner::ScanSummary;
use serde::Serialize;
use tauri::State;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo { name: &'static str, version: &'static str, platform: &'static str, architecture: &'static str }
#[tauri::command] fn get_app_info()->AppInfo { AppInfo { name:"Disk Visualizer", version:env!("CARGO_PKG_VERSION"), platform:std::env::consts::OS, architecture:std::env::consts::ARCH } }
#[tauri::command] fn scan_folder(path:String)->Result<ScanSummary,String>{ scanner::scan_folder_path(std::path::Path::new(&path)) }
#[tauri::command] fn start_scan(path:String,manager:State<'_,ScanManager>)->Result<ScanJobSnapshot,String>{ manager.start(path) }
#[tauri::command] fn get_scan_status(id:u64,manager:State<'_,ScanManager>)->Result<ScanJobSnapshot,String>{ manager.status(id) }
#[tauri::command] fn pause_scan(id:u64,manager:State<'_,ScanManager>)->Result<ScanJobSnapshot,String>{ manager.pause(id) }
#[tauri::command] fn resume_scan(id:u64,manager:State<'_,ScanManager>)->Result<ScanJobSnapshot,String>{ manager.resume(id) }
#[tauri::command] fn cancel_scan(id:u64,manager:State<'_,ScanManager>)->Result<ScanJobSnapshot,String>{ manager.cancel(id) }

pub fn run(){ tauri::Builder::default().manage(ScanManager::default()).plugin(tauri_plugin_dialog::init()).invoke_handler(tauri::generate_handler![get_app_info,scan_folder,start_scan,get_scan_status,pause_scan,resume_scan,cancel_scan]).run(tauri::generate_context!()).expect("failed to run Disk Visualizer"); }

#[cfg(test)] mod tests { use super::*; #[test] fn app_info_matches_package_version(){ let info=get_app_info(); assert_eq!(info.name,"Disk Visualizer"); assert_eq!(info.version,env!("CARGO_PKG_VERSION")); } }
