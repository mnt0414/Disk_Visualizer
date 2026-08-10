mod scanner;

use scanner::ScanSummary;
use serde::Serialize;

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
fn scan_folder(path: String) -> Result<ScanSummary, String> {
    scanner::scan_folder_path(std::path::Path::new(&path))
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![get_app_info, scan_folder])
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
}
