use crate::scanner::ScanSummary;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub struct ScanRepository {
    database_path: PathBuf,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SavedScan {
    pub id: i64,
    pub root_path: String,
    pub total_size_bytes: u64,
    pub file_count: u64,
    pub directory_count: u64,
    pub skipped_count: u64,
    pub completed_at: i64,
}

impl ScanRepository {
    pub fn new(database_path: PathBuf) -> Self {
        Self { database_path }
    }

    fn connection(&self) -> Result<Connection, String> {
        Connection::open(&self.database_path).map_err(|error| format!("スキャン履歴を開けません: {error}"))
    }

    pub fn initialize(&self) -> Result<(), String> {
        if let Some(parent) = self.database_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| format!("保存先を作成できません: {error}"))?;
        }
        let connection = self.connection()?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS scan_sessions (
               id INTEGER PRIMARY KEY,
               root_path TEXT NOT NULL,
               status TEXT NOT NULL CHECK(status IN ('complete')),
               total_size_bytes INTEGER NOT NULL,
               file_count INTEGER NOT NULL,
               directory_count INTEGER NOT NULL,
               skipped_count INTEGER NOT NULL,
               elapsed_milliseconds INTEGER NOT NULL,
               completed_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS scan_entries (
               id INTEGER PRIMARY KEY,
               scan_id INTEGER NOT NULL REFERENCES scan_sessions(id) ON DELETE CASCADE,
               name TEXT NOT NULL,
               path TEXT NOT NULL,
               size_bytes INTEGER NOT NULL,
               file_count INTEGER NOT NULL,
               directory_count INTEGER NOT NULL,
               skipped_count INTEGER NOT NULL,
               is_directory INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS scan_entries_scan_size ON scan_entries(scan_id, size_bytes DESC);
             PRAGMA user_version=1;",
        ).map_err(|error| format!("スキャン履歴を初期化できません: {error}"))
    }

    pub fn save_summary(&self, summary: &ScanSummary) -> Result<i64, String> {
        let mut connection = self.connection()?;
        connection.execute_batch("PRAGMA foreign_keys=ON;").map_err(|error| error.to_string())?;
        let transaction = connection.transaction().map_err(|error| format!("保存を開始できません: {error}"))?;
        let completed_at = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|error| error.to_string())?.as_secs() as i64;
        transaction.execute(
            "INSERT INTO scan_sessions (root_path,status,total_size_bytes,file_count,directory_count,skipped_count,elapsed_milliseconds,completed_at) VALUES (?1,'complete',?2,?3,?4,?5,?6,?7)",
            params![summary.root_path, summary.total_size_bytes as i64, summary.file_count as i64, summary.directory_count as i64, summary.skipped_count as i64, summary.elapsed_milliseconds as i64, completed_at],
        ).map_err(|error| format!("スキャン履歴を保存できません: {error}"))?;
        let scan_id = transaction.last_insert_rowid();
        {
            let mut statement = transaction.prepare_cached(
                "INSERT INTO scan_entries (scan_id,name,path,size_bytes,file_count,directory_count,skipped_count,is_directory) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            ).map_err(|error| error.to_string())?;
            for entry in &summary.entries {
                statement.execute(params![scan_id, entry.name, entry.path, entry.size_bytes as i64, entry.file_count as i64, entry.directory_count as i64, entry.skipped_count as i64, i64::from(entry.is_directory)]).map_err(|error| format!("項目を保存できません: {error}"))?;
            }
        }
        transaction.commit().map_err(|error| format!("スキャン履歴を確定できません: {error}"))?;
        Ok(scan_id)
    }

    pub fn list(&self) -> Result<Vec<SavedScan>, String> {
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT id,root_path,total_size_bytes,file_count,directory_count,skipped_count,completed_at FROM scan_sessions WHERE status='complete' ORDER BY completed_at DESC,id DESC").map_err(|error| error.to_string())?;
        let rows = statement.query_map([], |row| Ok(SavedScan { id: row.get(0)?, root_path: row.get(1)?, total_size_bytes: row.get::<_,i64>(2)? as u64, file_count: row.get::<_,i64>(3)? as u64, directory_count: row.get::<_,i64>(4)? as u64, skipped_count: row.get::<_,i64>(5)? as u64, completed_at: row.get(6)? })).map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>,_>>().map_err(|error| error.to_string())
    }

    pub fn delete(&self, id: i64) -> Result<(), String> {
        let connection = self.connection()?;
        connection.execute_batch("PRAGMA foreign_keys=ON;").map_err(|error| error.to_string())?;
        connection.execute("DELETE FROM scan_sessions WHERE id=?1", [id]).map_err(|error| format!("スキャン履歴を削除できません: {error}"))?;
        Ok(())
    }

    pub fn integrity_check(&self) -> Result<bool, String> {
        let connection = self.connection()?;
        let result: String = connection.query_row("PRAGMA quick_check", [], |row| row.get(0)).map_err(|error| error.to_string())?;
        Ok(result == "ok")
    }

    #[cfg(test)]
    fn path(&self) -> &Path { &self.database_path }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::{ScanEntry, ScanSummary};

    #[test]
    fn saves_lists_and_deletes_complete_scans() {
        let path = std::env::temp_dir().join(format!("disk-visualizer-index-{}.sqlite3", std::process::id()));
        let repository = ScanRepository::new(path);
        let _ = std::fs::remove_file(repository.path());
        repository.initialize().unwrap();
        let summary = ScanSummary { root_path: "/tmp/sample".to_owned(), total_size_bytes: 12, file_count: 1, directory_count: 0, skipped_count: 0, elapsed_milliseconds: 2, entries: vec![ScanEntry { name: "file.bin".to_owned(), path: "/tmp/sample/file.bin".to_owned(), size_bytes: 12, file_count: 1, directory_count: 0, skipped_count: 0, is_directory: false }] };
        let id = repository.save_summary(&summary).unwrap();
        assert_eq!(repository.list().unwrap()[0].id, id);
        assert!(repository.integrity_check().unwrap());
        repository.delete(id).unwrap();
        assert!(repository.list().unwrap().is_empty());
        let _ = std::fs::remove_file(repository.path());
    }
}
