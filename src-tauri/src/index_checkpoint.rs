use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::PathBuf;
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

    pub fn initialize(&self) -> Result<(), String> {
        let connection = self.connection()?;
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|error| error.to_string())?;
        match version {
            6 => {
                let backup = self.database_path.with_extension("sqlite3.v6-backup");
                if !backup.exists() {
                    connection
                        .execute("VACUUM INTO ?1", [backup.to_string_lossy().as_ref()])
                        .map_err(|error| {
                            format!("v7移行前バックアップを作成できません: {error}")
                        })?;
                    let backup_connection = Connection::open(&backup)
                        .map_err(|error| format!("v7移行前バックアップを開けません: {error}"))?;
                    let check: String = backup_connection
                        .query_row("PRAGMA quick_check", [], |row| row.get(0))
                        .map_err(|error| error.to_string())?;
                    if check != "ok" {
                        let _ = std::fs::remove_file(&backup);
                        return Err(format!("v7移行前バックアップが破損しています: {check}"));
                    }
                }
                connection.execute_batch("BEGIN IMMEDIATE; CREATE TABLE index_checkpoints (root_path TEXT PRIMARY KEY,platform TEXT NOT NULL CHECK(platform IN ('macos','windows')),volume_identity TEXT NOT NULL,root_identity TEXT NOT NULL,history_source TEXT NOT NULL CHECK(history_source IN ('fsevents','usn')),history_token TEXT NOT NULL,updated_at INTEGER NOT NULL); PRAGMA user_version=7; COMMIT;").map_err(|error|format!("スキャン履歴をv7へ移行できません: {error}"))?;
            }
            7 => {}
            other => {
                return Err(format!(
                    "checkpoint移行元として未対応のDBバージョンです: {other}"
                ))
            }
        }
        Ok(())
    }

    pub fn save(&self, checkpoint: &IndexCheckpoint) -> Result<(), String> {
        if checkpoint.root_path.is_empty()
            || checkpoint.volume_identity.is_empty()
            || checkpoint.root_identity.is_empty()
            || checkpoint.history_token.is_empty()
        {
            return Err("差分更新checkpointのidentityまたはtokenが不足しています".to_owned());
        }
        self.connection()?.execute("INSERT INTO index_checkpoints (root_path,platform,volume_identity,root_identity,history_source,history_token,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(root_path) DO UPDATE SET platform=excluded.platform,volume_identity=excluded.volume_identity,root_identity=excluded.root_identity,history_source=excluded.history_source,history_token=excluded.history_token,updated_at=excluded.updated_at",params![checkpoint.root_path,checkpoint.platform,checkpoint.volume_identity,checkpoint.root_identity,checkpoint.history_source,checkpoint.history_token,checkpoint.updated_at]).map_err(|error|format!("差分更新checkpointを保存できません: {error}"))?;
        Ok(())
    }

    pub fn save_current(
        &self,
        root_path: String,
        platform: String,
        volume_identity: String,
        root_identity: String,
        history_source: String,
        history_token: String,
    ) -> Result<(), String> {
        self.save(&IndexCheckpoint {
            root_path,
            platform,
            volume_identity,
            root_identity,
            history_source,
            history_token,
            updated_at: unix_time()?,
        })
    }

    pub fn load(&self, root_path: &str) -> Result<Option<IndexCheckpoint>, String> {
        self.connection()?
            .query_row("SELECT root_path,platform,volume_identity,root_identity,history_source,history_token,updated_at FROM index_checkpoints WHERE root_path=?1",[root_path],|row|Ok(IndexCheckpoint { root_path: row.get(0)?, platform: row.get(1)?, volume_identity: row.get(2)?, root_identity: row.get(3)?, history_source: row.get(4)?, history_token: row.get(5)?, updated_at: row.get(6)? }))
            .optional()
            .map_err(|error| format!("差分更新checkpointを取得できません: {error}"))
    }
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

    fn checkpoint(token: &str) -> IndexCheckpoint {
        IndexCheckpoint {
            root_path: "/Volumes/Data".to_owned(),
            platform: "macos".to_owned(),
            volume_identity: "volume-1".to_owned(),
            root_identity: "root-1".to_owned(),
            history_source: "fsevents".to_owned(),
            history_token: token.to_owned(),
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
        assert_eq!(version, 7);
        assert!(repository
            .database_path
            .with_extension("sqlite3.v6-backup")
            .exists());
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
