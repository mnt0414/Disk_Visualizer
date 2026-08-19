use std::fs;
use std::path::Path;

const ICON_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 32, 0, 0, 0, 32, 8, 6,
    0, 0, 0, 115, 122, 122, 244, 0, 0, 0, 48, 73, 68, 65, 84, 120, 218, 237, 206, 33, 1, 0, 0, 8,
    3, 48, 2, 145, 129, 12, 244, 47, 3, 49, 110, 38, 230, 87, 61, 123, 73, 37, 32, 32, 32, 32, 32,
    32, 32, 32, 32, 32, 32, 32, 32, 32, 144, 14, 60, 123, 84, 228, 91, 110, 74, 184, 224, 0, 0, 0,
    0, 73, 69, 78, 68, 174, 66, 96, 130,
];

fn main() {
    let icon = Path::new("icons/icon.png");
    if !icon.exists() {
        fs::create_dir_all("icons").expect("failed to create icon directory");
        fs::write(icon, ICON_PNG).expect("failed to create build icon");
    }
    tauri_build::build()
}
