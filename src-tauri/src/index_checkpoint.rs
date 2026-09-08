use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IndexCheckpoint {
    pub root_path: String,
    pub platform: String,
    pub volume_identity: String,
    pub root_identity: String,
    pub history_source: String,
    pub history_token: String,
    /// このcheckpointが指す完了済みscan session。差分更新の基準として明示的に検証する。
    pub baseline_scan_id: Option<i64>,
    pub updated_at: i64,
}

#[derive(Clone)]
pub struct IndexCheckpointRepository {
    database_path: PathBuf,
}

fn unix_time() -> Result<i64, String> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_secs(),
    )
    .map_err(|_| "現在時刻が保存可能な範囲を超えています".to_owned())
}

pub(crate) fn validate_checkpoint(checkpoint: &IndexCheckpoint) -> Result<(), String> {
    if checkpoint.root_path.is_empty()
        || checkpoint.volume_identity.is_empty()
        || checkpoint.root_identity.is_empty()
        || checkpoint.history_token.is_empty()
    {
        return Err("差分更新checkpointのidentityまたはtokenが不足しています".to_owned());
    }
    if checkpoint.baseline_scan_id.is_some_and(|id| id <= 0) {
        return Err("差分更新checkpointの基準scan IDが不正です".to_owned());
    }
    Ok(())
}

pub(crate) fn upsert_checkpoint(
    connection: &Connection,
    checkpoint: &IndexCheckpoint,
) -> Result<(), String> {
    validate_checkpoint(checkpoint)?;
    connection.execute("INSERT INTO index_checkpoints (root_path,platform,volume_identity,root_identity,history_source,history_token,baseline_scan_id,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(root_path) DO UPDATE SET platform=excluded.platform,volume_identity=excluded.volume_identity,root_identity=excluded.root_identity,history_source=excluded.history_source,history_token=excluded.history_token,baseline_scan_id=excluded.baseline_scan_id,updated_at=excluded.updated_at",params![checkpoint.root_path,checkpoint.platform,checkpoint.volume_identity,checkpoint.root_identity,checkpoint.history_source,checkpoint.history_token,checkpoint.baseline_scan_id,checkpoint.updated_at]).map_err(|error|format!("差分更新checkpointを保存できません: {error}"))?;
    Ok(())
}

/// 走査rootが今もcheckpointと同じ実体かを確認する。
///
/// 走査開始時の検証だけでは、走査中のunmount・rename・差し替えを見逃す。確定直前に
/// 見直すことで、別のvolumeやdirectoryを走査した結果を基準scanへ当てない。identityを
/// 取得できないplatformでは検証できないため、fail closedで失敗させる。
pub(crate) fn verify_root_identity(
    root: &Path,
    checkpoint: &IndexCheckpoint,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        use cap_std::{ambient_authority, fs::Dir};
        use std::os::unix::fs::MetadataExt;

        let metadata = Dir::open_ambient_dir(root, ambient_authority())
            .map_err(|error| format!("走査rootを安全に開けません: {error}"))?
            .into_std_file()
            .metadata()
            .map_err(|error| format!("走査rootのidentityを取得できません: {error}"))?;
        if metadata.dev().to_string() != checkpoint.volume_identity {
            return Err("走査中に走査rootのvolumeが入れ替わりました".to_owned());
        }
        if metadata.ino().to_string() != checkpoint.root_identity {
            return Err("走査中に走査rootが入れ替わりました".to_owned());
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (root, checkpoint);
        Err("走査rootの同一性検証はこのplatformでは未対応です".to_owned())
    }
}

pub fn capture_full_scan_checkpoint(root: &Path) -> Result<Option<IndexCheckpoint>, String> {
    #[cfg(target_os = "macos")]
    {
        use cap_std::{ambient_authority, fs::Dir};
        use std::os::unix::fs::MetadataExt;

        if !root.is_absolute() {
            return Err("checkpoint取得には絶対pathが必要です".to_owned());
        }
        let canonical_root = root
            .canonicalize()
            .map_err(|error| format!("checkpoint対象を解決できません: {error}"))?;
        if !canonical_root.is_dir() {
            return Err("checkpoint対象はdirectoryである必要があります".to_owned());
        }
        let directory = Dir::open_ambient_dir(&canonical_root, ambient_authority())
            .map_err(|error| format!("checkpoint対象を安全に開けません: {error}"))?;
        let metadata = directory
            .into_std_file()
            .metadata()
            .map_err(|error| format!("checkpoint対象のidentityを取得できません: {error}"))?;
        let history_token = crate::macos_fsevents::query_checkpoint(&canonical_root)?.encode();
        Ok(Some(IndexCheckpoint {
            root_path: canonical_root.to_string_lossy().into_owned(),
            platform: "macos".to_owned(),
            volume_identity: metadata.dev().to_string(),
            root_identity: metadata.ino().to_string(),
            history_source: "fsevents".to_owned(),
            history_token,
            baseline_scan_id: None,
            updated_at: unix_time()?,
        }))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = root;
        Ok(None)
    }
}

impl IndexCheckpointRepository {
    pub fn new(database_path: PathBuf) -> Self {
        Self { database_path }
    }

    fn connection(&self) -> Result<Connection, String> {
        let connection = Connection::open(&self.database_path)
            .map_err(|error| format!("差分更新checkpointを開けません: {error}"))?;
        connection
            .execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")
            .map_err(|error| error.to_string())?;
        Ok(connection)
    }

    /// 移行前に、VACUUM INTOで一貫したバックアップを作りquick_checkで健全性を確認する。
    fn consistent_backup(&self, connection: &Connection, extension: &str) -> Result<(), String> {
        let backup = self.database_path.with_extension(extension);
        if backup.exists() {
            return Ok(());
        }
        connection
            .execute("VACUUM INTO ?1", [backup.to_string_lossy().as_ref()])
            .map_err(|error| format!("移行前バックアップを作成できません: {error}"))?;
        let backup_connection = Connection::open(&backup)
            .map_err(|error| format!("移行前バックアップを開けません: {error}"))?;
        let check: String = backup_connection
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .map_err(|error| error.to_string())?;
        if check != "ok" {
            let _ = std::fs::remove_file(&backup);
            return Err(format!("移行前バックアップが破損しています: {check}"));
        }
        Ok(())
    }

    pub fn initialize(&self) -> Result<(), String> {
        let connection = self.connection()?;
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|error| error.to_string())?;
        match version {
            6 => {
                self.consistent_backup(&connection, "sqlite3.v6-backup")?;
                connection.execute_batch("BEGIN IMMEDIATE; CREATE TABLE index_checkpoints (root_path TEXT PRIMARY KEY,platform TEXT NOT NULL CHECK(platform IN ('macos','windows')),volume_identity TEXT NOT NULL,root_identity TEXT NOT NULL,history_source TEXT NOT NULL CHECK(history_source IN ('fsevents','usn')),history_token TEXT NOT NULL,baseline_scan_id INTEGER,updated_at INTEGER NOT NULL); PRAGMA user_version=8; COMMIT;").map_err(|error|format!("スキャン履歴をv8へ移行できません: {error}"))?;
            }
            7 => {
                self.consistent_backup(&connection, "sqlite3.v7-backup")?;
                connection.execute_batch("BEGIN IMMEDIATE; ALTER TABLE index_checkpoints ADD COLUMN baseline_scan_id INTEGER; PRAGMA user_version=8; COMMIT;").map_err(|error|format!("スキャン履歴をv8へ移行できません: {error}"))?;
            }
            8 => {}
            other => {
                return Err(format!(
                    "checkpoint移行元として未対応のDBバージョンです: {other}"
                ))
            }
        }
        Ok(())
    }

    pub fn save(&self, checkpoint: &IndexCheckpoint) -> Result<(), String> {
        upsert_checkpoint(&self.connection()?, checkpoint)
    }

    /// 更新時刻を現在時刻に打ち直してcheckpointを保存する。
    pub fn save_current(&self, checkpoint: &IndexCheckpoint) -> Result<(), String> {
        self.save(&IndexCheckpoint {
            updated_at: unix_time()?,
            ..checkpoint.clone()
        })
    }

    pub fn load(&self, root_path: &str) -> Result<Option<IndexCheckpoint>, String> {
        load_checkpoint(&self.connection()?, root_path)
    }
}

/// 与えられた接続でcheckpointを読む。確定transaction内から前提を読み直すために使う。
pub(crate) fn load_checkpoint(
    connection: &Connection,
    root_path: &str,
) -> Result<Option<IndexCheckpoint>, String> {
    connection
        .query_row("SELECT root_path,platform,volume_identity,root_identity,history_source,history_token,baseline_scan_id,updated_at FROM index_checkpoints WHERE root_path=?1",[root_path],|row|Ok(IndexCheckpoint { root_path: row.get(0)?, platform: row.get(1)?, volume_identity: row.get(2)?, root_identity: row.get(3)?, history_source: row.get(4)?, history_token: row.get(5)?, baseline_scan_id: row.get(6)?, updated_at: row.get(7)? }))
        .optional()
        .map_err(|error| format!("差分更新checkpointを取得できません: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository(name: &str) -> IndexCheckpointRepository {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "disk-visualizer-checkpoint-{name}-{}-{unique}.sqlite3",
            std::process::id()
        ));
        Connection::open(&path)
            .unwrap()
            .execute_batch("PRAGMA user_version=6;")
            .unwrap();
        let repository = IndexCheckpointRepository::new(path);
        repository.initialize().unwrap();
        repository
    }

    /// v7で実際に作られていたindex_checkpointsの定義。CHECK制約も当時のまま。
    const V7_SCHEMA: &str = "CREATE TABLE index_checkpoints (root_path TEXT PRIMARY KEY,platform TEXT NOT NULL CHECK(platform IN ('macos','windows')),volume_identity TEXT NOT NULL,root_identity TEXT NOT NULL,history_source TEXT NOT NULL CHECK(history_source IN ('fsevents','usn')),history_token TEXT NOT NULL,updated_at INTEGER NOT NULL);";

    fn temporary_database(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "disk-visualizer-checkpoint-{name}-{}-{unique}.sqlite3",
            std::process::id()
        ))
    }

    /// 実際のv7形式で、baseline_scan_idを持たない履歴を作る。
    fn v7_database(name: &str, rows: &str) -> PathBuf {
        let path = temporary_database(name);
        Connection::open(&path)
            .unwrap()
            .execute_batch(&format!("{V7_SCHEMA}{rows} PRAGMA user_version=7;"))
            .unwrap();
        path
    }

    /// 列順の違いを無視して比較するため、名前順に並べた列定義を取る。
    fn columns(path: &Path) -> Vec<(String, String, i64, Option<String>, i64)> {
        let connection = Connection::open(path).unwrap();
        let mut statement = connection
            .prepare("SELECT name,type,\"notnull\",dflt_value,pk FROM pragma_table_info('index_checkpoints') ORDER BY name")
            .unwrap();
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    }

    fn checkpoint(token: &str) -> IndexCheckpoint {
        IndexCheckpoint {
            root_path: "/Volumes/Data".to_owned(),
            platform: "macos".to_owned(),
            volume_identity: "volume-1".to_owned(),
            root_identity: "root-1".to_owned(),
            history_source: "fsevents".to_owned(),
            history_token: token.to_owned(),
            baseline_scan_id: Some(7),
            updated_at: 1234,
        }
    }

    #[test]
    fn migrates_v6_with_backup() {
        let repository = repository("migration");
        let version: i64 = repository
            .connection()
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 8);
        assert!(repository
            .database_path
            .with_extension("sqlite3.v6-backup")
            .exists());
    }

    #[test]
    fn migrates_v7_by_adding_baseline_link_with_backup() {
        let path = v7_database(
            "v7",
            " INSERT INTO index_checkpoints VALUES ('/Volumes/Data','macos','volume-1','root-1','fsevents','fsevents:v1:100',1234),('/Volumes/Other','macos','volume-2','root-2','fsevents','fsevents:v1:200',1235);",
        );
        let repository = IndexCheckpointRepository::new(path.clone());
        repository.initialize().unwrap();
        let version: i64 = repository
            .connection()
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 8);
        assert!(path.with_extension("sqlite3.v7-backup").exists());
        // 過去の正常な履歴はすべて残し、対応する基準scanは推測しない。
        for (root, token) in [
            ("/Volumes/Data", "fsevents:v1:100"),
            ("/Volumes/Other", "fsevents:v1:200"),
        ] {
            let migrated = repository.load(root).unwrap().unwrap();
            assert_eq!(migrated.baseline_scan_id, None);
            assert_eq!(migrated.history_token, token);
        }
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("sqlite3.v7-backup"));
    }

    #[test]
    fn new_and_migrated_databases_agree_on_the_v8_schema() {
        let fresh = repository("schema-new");
        let migrated_path = v7_database(
            "schema-migrated",
            " INSERT INTO index_checkpoints VALUES ('/Volumes/Data','macos','volume-1','root-1','fsevents','fsevents:v1:100',1234);",
        );
        let migrated = IndexCheckpointRepository::new(migrated_path.clone());
        migrated.initialize().unwrap();

        // ALTER TABLEは列を末尾に足すため物理的な列順は揃わない。列名で参照する限り
        // 差はないので、名前順に並べた定義で比較する。
        assert_eq!(columns(&fresh.database_path), columns(&migrated_path));
        // CHECK制約もv7から引き継がれ、新規DBと同じ入力を拒否する。
        for repository in [&fresh, &migrated] {
            let mut invalid = checkpoint("fsevents:v1:300");
            invalid.platform = "linux".to_owned();
            assert!(repository.save(&invalid).is_err());
            let mut valid = checkpoint("fsevents:v1:300");
            valid.baseline_scan_id = None;
            assert!(repository.save(&valid).is_ok());
            assert_eq!(
                repository
                    .load("/Volumes/Data")
                    .unwrap()
                    .unwrap()
                    .baseline_scan_id,
                None
            );
        }
        let _ = std::fs::remove_file(&migrated_path);
        let _ = std::fs::remove_file(migrated_path.with_extension("sqlite3.v7-backup"));
    }

    #[test]
    fn keeps_the_v7_database_and_its_backup_when_migration_fails() {
        // 既にbaseline_scan_id列がある壊れたv7で、ALTER TABLEを失敗させる。
        let path = v7_database(
            "migration-failure",
            " ALTER TABLE index_checkpoints ADD COLUMN baseline_scan_id INTEGER; INSERT INTO index_checkpoints (root_path,platform,volume_identity,root_identity,history_source,history_token,updated_at) VALUES ('/Volumes/Data','macos','volume-1','root-1','fsevents','fsevents:v1:100',1234);",
        );
        let repository = IndexCheckpointRepository::new(path.clone());

        assert!(repository.initialize().is_err());

        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        // 移行できないままversionを進めない。次回起動でも同じ移行を試せる。
        assert_eq!(version, 7);
        let token: String = connection
            .query_row(
                "SELECT history_token FROM index_checkpoints WHERE root_path='/Volumes/Data'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(token, "fsevents:v1:100");
        // 移行前バックアップは健全な状態で残る。
        let backup = path.with_extension("sqlite3.v7-backup");
        let backup_connection = Connection::open(&backup).unwrap();
        let check: String = backup_connection
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(check, "ok");
        let backed_up: i64 = backup_connection
            .query_row("SELECT COUNT(*) FROM index_checkpoints", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(backed_up, 1);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&backup);
    }

    #[test]
    fn rejects_invalid_baseline_scan_id() {
        let repository = repository("baseline");
        let mut value = checkpoint("100");
        value.baseline_scan_id = Some(0);
        assert!(repository.save(&value).is_err());
        value.baseline_scan_id = None;
        assert!(repository.save(&value).is_ok());
    }

    #[test]
    fn saves_loads_and_updates_checkpoint_by_root() {
        let repository = repository("upsert");
        repository.save(&checkpoint("100")).unwrap();
        assert_eq!(
            repository.load("/Volumes/Data").unwrap(),
            Some(checkpoint("100"))
        );
        repository.save(&checkpoint("200")).unwrap();
        assert_eq!(
            repository.load("/Volumes/Data").unwrap(),
            Some(checkpoint("200"))
        );
    }

    #[test]
    fn rejects_incomplete_checkpoint() {
        let repository = repository("incomplete");
        let mut value = checkpoint("100");
        value.volume_identity.clear();
        assert!(repository.save(&value).is_err());
    }
}
