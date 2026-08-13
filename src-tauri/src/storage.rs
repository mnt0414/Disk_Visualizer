use crate::scanner::{ScanProgress, ScanSummary};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const WRITE_BATCH_SIZE: usize = 500;
const INCOMPLETE_RETENTION_SECONDS: i64 = 7 * 24 * 60 * 60;

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
#[derive(Clone)]
struct IndexedEntry {
    name: String,
    path: String,
    parent_path: Option<String>,
    relative_path: String,
    entry_type: &'static str,
    counted_size: u64,
    logical_size: u64,
    allocated_size: Option<u64>,
    file_count: u64,
    directory_count: u64,
    skipped_count: u64,
    skip_reason: Option<&'static str>,
    is_directory: bool,
    file_identity: Option<String>,
    volume_identity: Option<String>,
    modified_at: Option<i64>,
}
#[derive(Clone)]
pub struct StreamingScanWriter {
    repository: ScanRepository,
    scan_id: i64,
    root_path: PathBuf,
    pending: Arc<Mutex<Vec<IndexedEntry>>>,
    error: Arc<Mutex<Option<String>>>,
}

fn unix_time() -> Result<i64, String> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs(),
    )
    .map_err(|_| "現在時刻が保存可能な範囲を超えています".to_owned())
}
fn to_i64(value: u64, label: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("{label}が保存可能な範囲を超えています"))
}
fn optional_to_i64(value: Option<u64>, label: &str) -> Result<Option<i64>, String> {
    value.map(|value| to_i64(value, label)).transpose()
}
fn elapsed_to_i64(value: u128) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| "経過時間が保存可能な範囲を超えています".to_owned())
}
fn to_u64(value: i64, label: &str) -> Result<u64, String> {
    u64::try_from(value).map_err(|_| format!("{label}に不正な負数が保存されています"))
}

impl ScanRepository {
    pub fn new(database_path: PathBuf) -> Self {
        Self { database_path }
    }
    fn connection(&self) -> Result<Connection, String> {
        let connection = Connection::open(&self.database_path)
            .map_err(|e| format!("スキャン履歴を開けません: {e}"))?;
        connection
            .execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")
            .map_err(|e| e.to_string())?;
        Ok(connection)
    }
    pub fn initialize(&self) -> Result<(), String> {
        if let Some(parent) = self.database_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("保存先を作成できません: {e}"))?;
        }
        let connection = self.connection()?;
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        match version {
            0 => Self::create_v4(&connection)?,
            1 => {
                self.consistent_backup(&connection, "sqlite3.v1-backup")?;
                Self::migrate_v1_to_v2(&connection)?;
                Self::migrate_v2_to_v3(&connection)?;
                Self::migrate_v3_to_v4(&connection)?
            }
            2 => {
                self.consistent_backup(&connection, "sqlite3.v2-backup")?;
                Self::migrate_v2_to_v3(&connection)?;
                Self::migrate_v3_to_v4(&connection)?
            }
            3 => {
                self.consistent_backup(&connection, "sqlite3.v3-backup")?;
                Self::migrate_v3_to_v4(&connection)?
            }
            4 => {}
            other => return Err(format!("未対応のスキャン履歴バージョンです: {other}")),
        }
        let check: String = connection
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        if check != "ok" {
            return Err(format!("スキャン履歴の整合性を確認できません: {check}"));
        }
        drop(connection);
        self.recover_incomplete_sessions()?;
        self.purge_stale_incomplete_sessions()?;
        Ok(())
    }
    fn consistent_backup(&self, connection: &Connection, extension: &str) -> Result<(), String> {
        let backup = self.database_path.with_extension(extension);
        if backup.exists() {
            return Ok(());
        }
        connection
            .execute("VACUUM INTO ?1", [backup.to_string_lossy().as_ref()])
            .map_err(|e| format!("移行前バックアップを作成できません: {e}"))?;
        let backup_connection = Connection::open(&backup)
            .map_err(|e| format!("移行前バックアップを検証できません: {e}"))?;
        let check: String = backup_connection
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        if check != "ok" {
            let _ = std::fs::remove_file(&backup);
            return Err(format!("移行前バックアップが破損しています: {check}"));
        }
        Ok(())
    }
    fn create_v4(connection: &Connection) -> Result<(), String> {
        connection.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE scan_sessions (id INTEGER PRIMARY KEY,root_path TEXT NOT NULL,status TEXT NOT NULL CHECK(status IN ('in_progress','complete','interrupted','failed')),total_size_bytes INTEGER NOT NULL DEFAULT 0,file_count INTEGER NOT NULL DEFAULT 0,directory_count INTEGER NOT NULL DEFAULT 0,skipped_count INTEGER NOT NULL DEFAULT 0,elapsed_milliseconds INTEGER NOT NULL DEFAULT 0,started_at INTEGER NOT NULL,completed_at INTEGER); CREATE TABLE scan_entries (id INTEGER PRIMARY KEY,scan_id INTEGER NOT NULL REFERENCES scan_sessions(id) ON DELETE CASCADE,name TEXT NOT NULL,path TEXT NOT NULL,parent_path TEXT,relative_path TEXT NOT NULL,entry_type TEXT NOT NULL CHECK(entry_type IN ('file','directory','other')),size_bytes INTEGER NOT NULL,logical_size INTEGER NOT NULL,allocated_size INTEGER,file_count INTEGER NOT NULL,directory_count INTEGER NOT NULL,skipped_count INTEGER NOT NULL DEFAULT 0,is_directory INTEGER NOT NULL,file_identity TEXT,volume_identity TEXT,modified_at INTEGER,skip_reason TEXT); CREATE INDEX scan_entries_scan_size ON scan_entries(scan_id,size_bytes DESC); CREATE INDEX scan_entries_scan_parent ON scan_entries(scan_id,parent_path); PRAGMA user_version=4;").map_err(|e|format!("スキャン履歴を初期化できません: {e}"))
    }
    fn migrate_v1_to_v2(connection: &Connection) -> Result<(), String> {
        connection.execute_batch("PRAGMA foreign_keys=OFF; BEGIN IMMEDIATE; ALTER TABLE scan_entries RENAME TO scan_entries_v1; ALTER TABLE scan_sessions RENAME TO scan_sessions_v1; CREATE TABLE scan_sessions (id INTEGER PRIMARY KEY,root_path TEXT NOT NULL,status TEXT NOT NULL CHECK(status IN ('in_progress','complete','interrupted','failed')),total_size_bytes INTEGER NOT NULL DEFAULT 0,file_count INTEGER NOT NULL DEFAULT 0,directory_count INTEGER NOT NULL DEFAULT 0,skipped_count INTEGER NOT NULL DEFAULT 0,elapsed_milliseconds INTEGER NOT NULL DEFAULT 0,started_at INTEGER NOT NULL,completed_at INTEGER); CREATE TABLE scan_entries (id INTEGER PRIMARY KEY,scan_id INTEGER NOT NULL REFERENCES scan_sessions(id) ON DELETE CASCADE,name TEXT NOT NULL,path TEXT NOT NULL,size_bytes INTEGER NOT NULL,file_count INTEGER NOT NULL,directory_count INTEGER NOT NULL,skipped_count INTEGER NOT NULL DEFAULT 0,is_directory INTEGER NOT NULL); INSERT INTO scan_sessions (id,root_path,status,total_size_bytes,file_count,directory_count,skipped_count,elapsed_milliseconds,started_at,completed_at) SELECT id,root_path,'complete',total_size_bytes,file_count,directory_count,skipped_count,elapsed_milliseconds,completed_at,completed_at FROM scan_sessions_v1; INSERT INTO scan_entries SELECT * FROM scan_entries_v1; DROP TABLE scan_entries_v1; DROP TABLE scan_sessions_v1; CREATE INDEX scan_entries_scan_size ON scan_entries(scan_id,size_bytes DESC); PRAGMA user_version=2; COMMIT; PRAGMA foreign_keys=ON;").map_err(|e|format!("スキャン履歴をv2へ移行できません: {e}"))
    }
    fn migrate_v2_to_v3(connection: &Connection) -> Result<(), String> {
        connection.execute_batch("BEGIN IMMEDIATE; ALTER TABLE scan_entries ADD COLUMN parent_path TEXT; ALTER TABLE scan_entries ADD COLUMN relative_path TEXT NOT NULL DEFAULT ''; ALTER TABLE scan_entries ADD COLUMN entry_type TEXT NOT NULL DEFAULT 'other' CHECK(entry_type IN ('file','directory','other')); ALTER TABLE scan_entries ADD COLUMN logical_size INTEGER NOT NULL DEFAULT 0; ALTER TABLE scan_entries ADD COLUMN allocated_size INTEGER; ALTER TABLE scan_entries ADD COLUMN file_identity TEXT; ALTER TABLE scan_entries ADD COLUMN volume_identity TEXT; ALTER TABLE scan_entries ADD COLUMN modified_at INTEGER;").map_err(|e|format!("スキャン履歴のv3移行を開始できません: {e}"))?;
        let migration = (|| -> Result<(), String> {
            let entries = {
                let mut statement = connection
                .prepare("SELECT e.id,e.path,s.root_path FROM scan_entries e JOIN scan_sessions s ON s.id=e.scan_id")
                .map_err(|e| e.to_string())?;
                let rows = statement
                    .query_map([], |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    })
                    .map_err(|e| e.to_string())?;
                rows.collect::<Result<Vec<_>, _>>()
                    .map_err(|e| e.to_string())?
            };
            let mut update = connection
                .prepare_cached(
                    "UPDATE scan_entries SET parent_path=?2,relative_path=?3 WHERE id=?1",
                )
                .map_err(|e| e.to_string())?;
            for (id, stored_path, root_path) in entries {
                let stored_path = PathBuf::from(stored_path);
                let root_path = PathBuf::from(root_path);
                let parent_path = stored_path
                    .parent()
                    .map(|value| value.to_string_lossy().into_owned());
                let relative_path = stored_path
                    .strip_prefix(&root_path)
                    .unwrap_or(stored_path.as_path())
                    .to_string_lossy()
                    .into_owned();
                update
                    .execute(params![id, parent_path, relative_path])
                    .map_err(|e| e.to_string())?;
            }
            drop(update);
            connection.execute_batch("UPDATE scan_entries SET entry_type=CASE WHEN is_directory=1 THEN 'directory' ELSE 'file' END,logical_size=size_bytes; CREATE INDEX scan_entries_scan_parent ON scan_entries(scan_id,parent_path); PRAGMA user_version=3; COMMIT;").map_err(|e| e.to_string())?;
            Ok(())
        })();
        if let Err(error) = migration {
            let _ = connection.execute_batch("ROLLBACK;");
            return Err(format!("スキャン履歴をv3へ移行できません: {error}"));
        }
        Ok(())
    }
    fn migrate_v3_to_v4(connection: &Connection) -> Result<(), String> {
        connection.execute_batch("BEGIN IMMEDIATE; ALTER TABLE scan_entries ADD COLUMN skip_reason TEXT; PRAGMA user_version=4; COMMIT;").map_err(|e|format!("スキャン履歴をv4へ移行できません: {e}"))
    }
    pub fn recover_incomplete_sessions(&self) -> Result<usize, String> {
        let connection = self.connection()?;
        connection.execute("UPDATE scan_sessions SET status='interrupted',completed_at=?1 WHERE status='in_progress'",[unix_time()?]).map_err(|e|format!("未完了のスキャン履歴を復旧できません: {e}"))
    }
    fn purge_stale_incomplete_sessions(&self) -> Result<usize, String> {
        let cutoff = unix_time()?.saturating_sub(INCOMPLETE_RETENTION_SECONDS);
        self.connection()?.execute("DELETE FROM scan_sessions WHERE status IN ('interrupted','failed') AND completed_at IS NOT NULL AND completed_at < ?1",[cutoff]).map_err(|e|format!("古い未完了スキャンを削除できません: {e}"))
    }
    pub fn begin_stream(&self, root_path: &str) -> Result<StreamingScanWriter, String> {
        let connection = self.connection()?;
        connection.execute("INSERT INTO scan_sessions (root_path,status,started_at) VALUES (?1,'in_progress',?2)",params![root_path,unix_time()?]).map_err(|e|format!("スキャン履歴を開始できません: {e}"))?;
        Ok(StreamingScanWriter {
            repository: self.clone(),
            scan_id: connection.last_insert_rowid(),
            root_path: PathBuf::from(root_path),
            pending: Arc::new(Mutex::new(Vec::with_capacity(WRITE_BATCH_SIZE))),
            error: Arc::new(Mutex::new(None)),
        })
    }
    fn append_batch(&self, scan_id: i64, entries: &[IndexedEntry]) -> Result<(), String> {
        if entries.is_empty() {
            return Ok(());
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction().map_err(|e| e.to_string())?;
        {
            let mut statement=transaction.prepare_cached("INSERT INTO scan_entries (scan_id,name,path,parent_path,relative_path,entry_type,size_bytes,logical_size,allocated_size,file_count,directory_count,skipped_count,skip_reason,is_directory,file_identity,volume_identity,modified_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)").map_err(|e|e.to_string())?;
            for entry in entries {
                statement
                    .execute(params![
                        scan_id,
                        entry.name,
                        entry.path,
                        entry.parent_path,
                        entry.relative_path,
                        entry.entry_type,
                        to_i64(entry.counted_size, "集計サイズ")?,
                        to_i64(entry.logical_size, "論理サイズ")?,
                        optional_to_i64(entry.allocated_size, "割り当て済みサイズ")?,
                        to_i64(entry.file_count, "ファイル数")?,
                        to_i64(entry.directory_count, "フォルダ数")?,
                        to_i64(entry.skipped_count, "読み飛ばし数")?,
                        entry.skip_reason,
                        if entry.is_directory { 1_i64 } else { 0_i64 },
                        entry.file_identity,
                        entry.volume_identity,
                        entry.modified_at,
                    ])
                    .map_err(|e| format!("スキャン項目を保存できません: {e}"))?;
            }
        }
        transaction.commit().map_err(|e| e.to_string())
    }
    fn finish_stream(&self, scan_id: i64, summary: &ScanSummary) -> Result<(), String> {
        self.connection()?.execute("UPDATE scan_sessions SET status='complete',total_size_bytes=?2,file_count=?3,directory_count=?4,skipped_count=?5,elapsed_milliseconds=?6,completed_at=?7 WHERE id=?1 AND status='in_progress'",params![scan_id,to_i64(summary.total_size_bytes,"合計サイズ")?,to_i64(summary.file_count,"ファイル数")?,to_i64(summary.directory_count,"フォルダ数")?,to_i64(summary.skipped_count,"読み飛ばし数")?,elapsed_to_i64(summary.elapsed_milliseconds)?,unix_time()?]).map_err(|e|format!("スキャン履歴を確定できません: {e}"))?;
        Ok(())
    }
    fn stop_stream(&self, scan_id: i64, status: &str) -> Result<(), String> {
        self.connection()?.execute("UPDATE scan_sessions SET status=?2,completed_at=?3 WHERE id=?1 AND status='in_progress'",params![scan_id,status,unix_time()?]).map_err(|e|e.to_string())?;
        Ok(())
    }
    pub fn list(&self) -> Result<Vec<SavedScan>, String> {
        let connection = self.connection()?;
        let mut statement=connection.prepare("SELECT id,root_path,total_size_bytes,file_count,directory_count,skipped_count,completed_at FROM scan_sessions WHERE status='complete' ORDER BY completed_at DESC,id DESC").map_err(|e|e.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        rows.map(|row| {
            let (id, root_path, size, files, dirs, skipped, completed_at) =
                row.map_err(|e| e.to_string())?;
            Ok(SavedScan {
                id,
                root_path,
                total_size_bytes: to_u64(size, "合計サイズ")?,
                file_count: to_u64(files, "ファイル数")?,
                directory_count: to_u64(dirs, "フォルダ数")?,
                skipped_count: to_u64(skipped, "読み飛ばし数")?,
                completed_at,
            })
        })
        .collect()
    }
    pub fn delete(&self, id: i64) -> Result<(), String> {
        self.connection()?
            .execute("DELETE FROM scan_sessions WHERE id=?1", [id])
            .map_err(|e| format!("スキャン履歴を削除できません: {e}"))?;
        Ok(())
    }
    pub fn integrity_check(&self) -> Result<bool, String> {
        let result: String = self
            .connection()?
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        Ok(result == "ok")
    }
    #[cfg(test)]
    fn path(&self) -> &std::path::Path {
        &self.database_path
    }
    #[cfg(test)]
    fn count_entries(&self) -> i64 {
        self.connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM scan_entries", [], |row| row.get(0))
            .unwrap()
    }
}
impl StreamingScanWriter {
    pub(crate) fn record(&self, progress: &ScanProgress) {
        if progress.file_count == 0 && progress.directory_count == 0 && progress.skipped_count == 0
        {
            return;
        }
        let path = &progress.path;
        let entry = IndexedEntry {
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            path: path.to_string_lossy().into_owned(),
            parent_path: path
                .parent()
                .map(|value| value.to_string_lossy().into_owned()),
            relative_path: path
                .strip_prefix(&self.root_path)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned(),
            entry_type: if progress.directory_count > 0 {
                "directory"
            } else if progress.file_count > 0 {
                "file"
            } else {
                "other"
            },
            counted_size: progress.counted_size_bytes,
            logical_size: progress.logical_size_bytes,
            allocated_size: progress.allocated_size_bytes,
            file_count: progress.file_count,
            directory_count: progress.directory_count,
            skipped_count: progress.skipped_count,
            skip_reason: progress.skip_reason,
            is_directory: progress.directory_count > 0,
            file_identity: progress.file_identity.clone(),
            volume_identity: progress.volume_identity.clone(),
            modified_at: progress.modified_at,
        };
        let batch = {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            pending.push(entry);
            if pending.len() < WRITE_BATCH_SIZE {
                return;
            }
            std::mem::take(&mut *pending)
        };
        self.write(batch)
    }
    fn write(&self, batch: Vec<IndexedEntry>) {
        if self
            .error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return;
        }
        if let Err(error) = self.repository.append_batch(self.scan_id, &batch) {
            *self.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
        }
    }
    fn flush(&self) {
        let batch = {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *pending)
        };
        self.write(batch)
    }
    pub fn complete(&self, summary: &ScanSummary) -> Result<(), String> {
        self.flush();
        if let Some(error) = self.error.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            self.repository.stop_stream(self.scan_id, "failed")?;
            return Err(error);
        }
        self.repository.finish_stream(self.scan_id, summary)
    }
    pub fn interrupt(&self, failed: bool) -> Result<(), String> {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.repository
            .stop_stream(self.scan_id, if failed { "failed" } else { "interrupted" })
    }
    #[cfg(test)]
    fn pending_len(&self) -> usize {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn repository(name: &str) -> ScanRepository {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "disk-visualizer-{name}-{}-{unique}.sqlite3",
            std::process::id()
        ));
        let repository = ScanRepository::new(path);
        repository.initialize().unwrap();
        repository
    }
    fn summary(files: u64) -> ScanSummary {
        ScanSummary {
            root_path: "/tmp/sample".to_owned(),
            total_size_bytes: files * 1024,
            allocated_size_bytes: files * 4096,
            file_count: files,
            hard_link_duplicate_count: 0,
            sparse_file_count: 0,
            compressed_file_count: 0,
            directory_count: 0,
            skipped_count: 0,
            elapsed_milliseconds: 2,
            entries: vec![],
            entries_truncated: false,
        }
    }
    fn progress(path: PathBuf) -> ScanProgress {
        ScanProgress {
            path,
            file_count: 1,
            directory_count: 0,
            skipped_count: 0,
            skip_reason: None,
            counted_size_bytes: 1024,
            logical_size_bytes: 1024,
            allocated_size_bytes: Some(4096),
            file_identity: Some("42".to_owned()),
            volume_identity: Some("7".to_owned()),
            modified_at: Some(1234),
        }
    }
    #[test]
    fn streams_lists_and_deletes_complete_scans() {
        let repository = repository("stream");
        let writer = repository.begin_stream("/tmp/sample").unwrap();
        writer.record(&progress(PathBuf::from("/tmp/sample/file.bin")));
        writer.complete(&summary(1)).unwrap();
        assert_eq!(repository.list().unwrap().len(), 1);
        assert!(repository.integrity_check().unwrap());
        let _ = std::fs::remove_file(repository.path());
    }
    #[test]
    fn persists_filesystem_metadata() {
        let repository = repository("metadata");
        let writer = repository.begin_stream("/tmp/sample").unwrap();
        writer.record(&progress(PathBuf::from("/tmp/sample/file.bin")));
        writer.complete(&summary(1)).unwrap();
        let stored: (i64, i64, String, String, i64) = repository
            .connection()
            .unwrap()
            .query_row(
                "SELECT logical_size,allocated_size,file_identity,volume_identity,modified_at FROM scan_entries LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .unwrap();
        assert_eq!(stored, (1024, 4096, "42".to_owned(), "7".to_owned(), 1234));
        let _ = std::fs::remove_file(repository.path());
    }
    #[test]
    fn persists_skipped_entries_with_reason() {
        let repository = repository("skipped-entry");
        let writer = repository.begin_stream("/tmp/sample").unwrap();
        let mut skipped = progress(PathBuf::from("/tmp/sample/linked"));
        skipped.file_count = 0;
        skipped.skipped_count = 1;
        skipped.skip_reason = Some("link_not_followed");
        skipped.counted_size_bytes = 0;
        skipped.logical_size_bytes = 0;
        skipped.allocated_size_bytes = None;
        skipped.file_identity = None;
        writer.record(&skipped);
        let mut completed = summary(0);
        completed.skipped_count = 1;
        writer.complete(&completed).unwrap();
        let stored: (i64, String, String) = repository
            .connection()
            .unwrap()
            .query_row(
                "SELECT skipped_count,skip_reason,entry_type FROM scan_entries LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            stored,
            (1, "link_not_followed".to_owned(), "other".to_owned())
        );
        let _ = std::fs::remove_file(repository.path());
    }
    #[test]
    fn counted_entry_sizes_match_completed_session_total() {
        let repository = repository("hard-link-counted-size");
        let writer = repository.begin_stream("/tmp/sample").unwrap();
        writer.record(&progress(PathBuf::from("/tmp/sample/original.bin")));
        let mut duplicate = progress(PathBuf::from("/tmp/sample/linked.bin"));
        duplicate.counted_size_bytes = 0;
        writer.record(&duplicate);
        let mut completed = summary(2);
        completed.total_size_bytes = 1024;
        writer.complete(&completed).unwrap();
        let stored: (i64, i64, i64, i64) = repository
            .connection()
            .unwrap()
            .query_row(
                "SELECT (SELECT total_size_bytes FROM scan_sessions LIMIT 1),SUM(size_bytes),SUM(logical_size),COUNT(*) FROM scan_entries",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(stored, (1024, 1024, 2048, 2));
        let _ = std::fs::remove_file(repository.path());
    }
    #[test]
    fn buffer_never_retains_a_full_batch() {
        let repository = repository("bounded");
        let writer = repository.begin_stream("/tmp/sample").unwrap();
        for index in 0..(WRITE_BATCH_SIZE * 3 + 7) {
            writer.record(&progress(PathBuf::from(format!("/tmp/sample/{index}.bin"))));
            assert!(writer.pending_len() < WRITE_BATCH_SIZE);
        }
        writer
            .complete(&summary((WRITE_BATCH_SIZE * 3 + 7) as u64))
            .unwrap();
        assert_eq!(
            repository.count_entries(),
            (WRITE_BATCH_SIZE * 3 + 7) as i64
        );
        let _ = std::fs::remove_file(repository.path());
    }
    #[test]
    fn migrates_v2_with_consistent_backup() {
        let repository = repository("migration");
        let path = repository.path().to_path_buf();
        {
            let connection = repository.connection().unwrap();
            connection.execute_batch("PRAGMA user_version=2; DROP INDEX scan_entries_scan_parent; CREATE TABLE replacement AS SELECT id,scan_id,name,path,size_bytes,file_count,directory_count,skipped_count,is_directory FROM scan_entries; DROP TABLE scan_entries; ALTER TABLE replacement RENAME TO scan_entries; CREATE INDEX scan_entries_scan_size ON scan_entries(scan_id,size_bytes DESC); INSERT INTO scan_sessions (id,root_path,status,started_at) VALUES (1,'/tmp/sample','complete',1); INSERT INTO scan_entries (id,scan_id,name,path,size_bytes,file_count,directory_count,skipped_count,is_directory) VALUES (1,1,'file.bin','/tmp/sample/nested/file.bin',1024,1,0,0,0);").unwrap();
        }
        repository.initialize().unwrap();
        let version: i64 = repository
            .connection()
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 4);
        let migrated_paths: (Option<String>, String) = repository
            .connection()
            .unwrap()
            .query_row(
                "SELECT parent_path,relative_path FROM scan_entries WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            migrated_paths,
            (
                Some("/tmp/sample/nested".to_owned()),
                "nested/file.bin".to_owned()
            )
        );
        assert!(path.with_extension("sqlite3.v2-backup").exists());
        let _ = std::fs::remove_file(path);
    }
    #[test]
    #[ignore = "manual million-entry benchmark"]
    fn benchmark_million_entries() {
        let repository = repository("benchmark");
        let writer = repository.begin_stream("/benchmark").unwrap();
        let count = 1_000_000_u64;
        let started = std::time::Instant::now();
        for index in 0..count {
            writer.record(&progress(PathBuf::from(format!("/benchmark/{index}.bin"))));
        }
        writer.complete(&summary(count)).unwrap();
        assert_eq!(repository.count_entries(), count as i64);
        println!(
            "entries={count} elapsed_ms={} db_bytes={}",
            started.elapsed().as_millis(),
            std::fs::metadata(repository.path()).unwrap().len()
        );
        let _ = std::fs::remove_file(repository.path());
    }
}
