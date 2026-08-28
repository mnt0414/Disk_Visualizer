use crate::cache_activity::{self, CacheObservation, CacheRuntimeState};
use crate::cache_catalog;
use crate::incremental_rescan::IncrementalRescanTarget;
use crate::index_checkpoint::{upsert_checkpoint, IndexCheckpoint};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IncrementalEntry {
    pub path: PathBuf,
    pub file_count: u64,
    pub directory_count: u64,
    pub skipped_count: u64,
    pub skip_reason: Option<&'static str>,
    pub counted_size_bytes: u64,
    pub logical_size_bytes: u64,
    pub allocated_size_bytes: Option<u64>,
    pub file_identity: Option<String>,
    pub volume_identity: Option<String>,
    pub modified_at: Option<i64>,
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

fn to_i64(value: u64, label: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("{label}が保存可能な範囲を超えています"))
}

fn optional_to_i64(value: Option<u64>, label: &str) -> Result<Option<i64>, String> {
    value.map(|value| to_i64(value, label)).transpose()
}

fn normalize_relative(path: &Path) -> Option<PathBuf> {
    if path.as_os_str().is_empty() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => normalized.push(value),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if normalized.as_os_str().is_empty() {
        Some(PathBuf::from("."))
    } else {
        Some(normalized)
    }
}

fn target_covers(target: &IncrementalRescanTarget, relative: &Path) -> bool {
    if target.relative_path == Path::new(".") {
        return target.recursive;
    }
    relative == target.relative_path
        || (target.recursive && relative.starts_with(&target.relative_path))
}

fn observation(entry: &IncrementalEntry) -> Option<CacheObservation> {
    if entry.file_count != 1 || entry.skipped_count != 0 {
        return None;
    }
    let file_identity = match (
        entry.volume_identity.as_ref(),
        entry.file_identity.as_ref(),
    ) {
        (Some(volume), Some(file)) => Some(format!("{volume}:{file}")),
        (None, Some(file)) => Some(file.clone()),
        _ => None,
    };
    Some(CacheObservation {
        logical_size: Some(entry.logical_size_bytes),
        modified_at: entry.modified_at,
        file_identity,
    })
}

fn insert_entry(
    connection: &Connection,
    scan_id: i64,
    root: &Path,
    entry: &IncrementalEntry,
) -> Result<(), String> {
    let relative = entry
        .path
        .strip_prefix(root)
        .map_err(|_| "部分再走査結果が走査root外を指しています".to_owned())?;
    let classification = cache_catalog::classify_absolute_path(&entry.path);
    let before = classification.as_ref().and_then(|_| observation(entry));
    let runtime_state = classification
        .as_ref()
        .map(|_| cache_activity::evaluate_path_after(&entry.path, before.as_ref()));
    let name = entry
        .path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let path = entry.path.to_string_lossy().into_owned();
    let parent_path = entry
        .path
        .parent()
        .map(|value| value.to_string_lossy().into_owned());
    let relative_path = relative.to_string_lossy().into_owned();
    let entry_type = if entry.directory_count > 0 {
        "directory"
    } else if entry.file_count > 0 {
        "file"
    } else {
        "other"
    };
    connection.execute("INSERT INTO scan_entries (scan_id,name,path,parent_path,relative_path,entry_type,size_bytes,logical_size,allocated_size,file_count,directory_count,skipped_count,skip_reason,is_directory,file_identity,volume_identity,modified_at,cache_catalog_version,cache_definition_id,cache_definition_version,cache_runtime_state) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",params![scan_id,name,path,parent_path,relative_path,entry_type,to_i64(entry.counted_size_bytes,"集計サイズ")?,to_i64(entry.logical_size_bytes,"論理サイズ")?,optional_to_i64(entry.allocated_size_bytes,"割り当て済みサイズ")?,to_i64(entry.file_count,"ファイル数")?,to_i64(entry.directory_count,"フォルダ数")?,to_i64(entry.skipped_count,"読み飛ばし数")?,entry.skip_reason,if entry.directory_count > 0 { 1_i64 } else { 0_i64 },entry.file_identity,entry.volume_identity,entry.modified_at,classification.as_ref().map(|value|value.catalog_version.as_str()),classification.as_ref().map(|value|value.definition_id.as_str()),classification.as_ref().map(|value|i64::from(value.definition_version)),runtime_state.as_ref().map(CacheRuntimeState::as_str)]).map_err(|error|format!("部分再走査結果を保存できません: {error}"))?;
    Ok(())
}

pub fn apply_incremental_snapshot(
    database_path: &Path,
    baseline_scan_id: i64,
    root_path: &Path,
    targets: &[IncrementalRescanTarget],
    replacements: &[IncrementalEntry],
    checkpoint: &IndexCheckpoint,
) -> Result<i64, String> {
    if !root_path.is_absolute() {
        return Err("部分更新対象には絶対pathが必要です".to_owned());
    }
    if checkpoint.root_path != root_path.to_string_lossy() {
        return Err("部分更新対象とcheckpointのrootが一致しません".to_owned());
    }
    let normalized_targets = targets
        .iter()
        .map(|target| {
            normalize_relative(&target.relative_path)
                .map(|relative_path| IncrementalRescanTarget {
                    relative_path,
                    recursive: target.recursive,
                })
                .ok_or_else(|| "部分再走査targetが不正です".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut seen = BTreeSet::new();
    for entry in replacements {
        let relative = entry
            .path
            .strip_prefix(root_path)
            .map_err(|_| "部分再走査結果が走査root外を指しています".to_owned())?;
        let relative = normalize_relative(relative)
            .ok_or_else(|| "部分再走査結果の相対pathが不正です".to_owned())?;
        if !normalized_targets
            .iter()
            .any(|target| target_covers(target, &relative))
        {
            return Err("部分再走査結果が指定target外を指しています".to_owned());
        }
        if !seen.insert(relative) {
            return Err("部分再走査結果に重複pathがあります".to_owned());
        }
    }

    let mut connection = Connection::open(database_path)
        .map_err(|error| format!("スキャン履歴を開けません: {error}"))?;
    connection
        .execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")
        .map_err(|error| error.to_string())?;
    let transaction = connection.transaction().map_err(|error| error.to_string())?;
    let baseline_root: Option<String> = transaction
        .query_row(
            "SELECT root_path FROM scan_sessions WHERE id=?1 AND status='complete'",
            [baseline_scan_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    if baseline_root.as_deref() != Some(root_path.to_string_lossy().as_ref()) {
        return Err("一致する完了済み基準スキャンがありません".to_owned());
    }
    transaction.execute("INSERT INTO scan_sessions (root_path,status,started_at) VALUES (?1,'in_progress',?2)",params![root_path.to_string_lossy().as_ref(),unix_time()?]).map_err(|error|format!("部分更新sessionを開始できません: {error}"))?;
    let scan_id = transaction.last_insert_rowid();
    transaction.execute("INSERT INTO scan_entries (scan_id,name,path,parent_path,relative_path,entry_type,size_bytes,logical_size,allocated_size,file_count,directory_count,skipped_count,skip_reason,is_directory,file_identity,volume_identity,modified_at,cache_catalog_version,cache_definition_id,cache_definition_version,cache_runtime_state) SELECT ?1,name,path,parent_path,relative_path,entry_type,size_bytes,logical_size,allocated_size,file_count,directory_count,skipped_count,skip_reason,is_directory,file_identity,volume_identity,modified_at,cache_catalog_version,cache_definition_id,cache_definition_version,cache_runtime_state FROM scan_entries WHERE scan_id=?2",params![scan_id,baseline_scan_id]).map_err(|error|format!("基準スキャンを複製できません: {error}"))?;
    for target in &normalized_targets {
        if target.relative_path == Path::new(".") {
            transaction
                .execute("DELETE FROM scan_entries WHERE scan_id=?1", [scan_id])
                .map_err(|error| error.to_string())?;
        } else if target.recursive {
            let relative = target.relative_path.to_string_lossy();
            let prefix = format!("{}{sep}", relative, sep = std::path::MAIN_SEPARATOR);
            transaction.execute("DELETE FROM scan_entries WHERE scan_id=?1 AND (relative_path=?2 OR instr(relative_path,?3)=1)",params![scan_id,relative.as_ref(),prefix]).map_err(|error|format!("部分再走査範囲を置換できません: {error}"))?;
        } else {
            transaction.execute("DELETE FROM scan_entries WHERE scan_id=?1 AND relative_path=?2",params![scan_id,target.relative_path.to_string_lossy().as_ref()]).map_err(|error|format!("部分再走査項目を置換できません: {error}"))?;
        }
    }
    for entry in replacements {
        insert_entry(&transaction, scan_id, root_path, entry)?;
    }
    let (size, files, directories, skipped): (i64, i64, i64, i64) = transaction.query_row("SELECT COALESCE(SUM(size_bytes),0),COALESCE(SUM(file_count),0),COALESCE(SUM(directory_count),0),COALESCE(SUM(skipped_count),0) FROM scan_entries WHERE scan_id=?1",[scan_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).map_err(|error|format!("部分更新の集計値を計算できません: {error}"))?;
    let changed = transaction.execute("UPDATE scan_sessions SET status='complete',total_size_bytes=?2,file_count=?3,directory_count=?4,skipped_count=?5,elapsed_milliseconds=0,completed_at=?6 WHERE id=?1 AND status='in_progress'",params![scan_id,size,files,directories,skipped,unix_time()?]).map_err(|error|format!("部分更新sessionを確定できません: {error}"))?;
    if changed != 1 {
        return Err("部分更新sessionを確定できません".to_owned());
    }
    upsert_checkpoint(&transaction, checkpoint)?;
    transaction
        .commit()
        .map_err(|error| format!("部分更新結果とcheckpointを確定できません: {error}"))?;
    Ok(scan_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "disk-visualizer-incremental-{name}-{}-{unique}.sqlite3",
            std::process::id()
        ));
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE scan_sessions (id INTEGER PRIMARY KEY,root_path TEXT NOT NULL,status TEXT NOT NULL,total_size_bytes INTEGER NOT NULL DEFAULT 0,file_count INTEGER NOT NULL DEFAULT 0,directory_count INTEGER NOT NULL DEFAULT 0,skipped_count INTEGER NOT NULL DEFAULT 0,elapsed_milliseconds INTEGER NOT NULL DEFAULT 0,started_at INTEGER NOT NULL,completed_at INTEGER); CREATE TABLE scan_entries (id INTEGER PRIMARY KEY,scan_id INTEGER NOT NULL REFERENCES scan_sessions(id) ON DELETE CASCADE,name TEXT NOT NULL,path TEXT NOT NULL,parent_path TEXT,relative_path TEXT NOT NULL,entry_type TEXT NOT NULL,size_bytes INTEGER NOT NULL,logical_size INTEGER NOT NULL,allocated_size INTEGER,file_count INTEGER NOT NULL,directory_count INTEGER NOT NULL,skipped_count INTEGER NOT NULL DEFAULT 0,is_directory INTEGER NOT NULL,file_identity TEXT,volume_identity TEXT,modified_at INTEGER,skip_reason TEXT,cache_catalog_version TEXT,cache_definition_id TEXT,cache_definition_version INTEGER,cache_runtime_state TEXT); CREATE TABLE index_checkpoints (root_path TEXT PRIMARY KEY,platform TEXT NOT NULL,volume_identity TEXT NOT NULL,root_identity TEXT NOT NULL,history_source TEXT NOT NULL,history_token TEXT NOT NULL,updated_at INTEGER NOT NULL); INSERT INTO scan_sessions (id,root_path,status,total_size_bytes,file_count,directory_count,skipped_count,started_at,completed_at) VALUES (1,'/tmp/sample','complete',6,3,1,0,1,1); INSERT INTO scan_entries (scan_id,name,path,parent_path,relative_path,entry_type,size_bytes,logical_size,file_count,directory_count,skipped_count,is_directory) VALUES (1,'keep','/tmp/sample/keep','/tmp/sample','keep','file',1,1,1,0,0,0),(1,'old','/tmp/sample/old','/tmp/sample','old','file',2,2,1,0,0,0),(1,'dir','/tmp/sample/dir','/tmp/sample','dir','directory',0,0,0,1,0,1),(1,'nested','/tmp/sample/dir/nested','/tmp/sample/dir','dir/nested','file',3,3,1,0,0,0);").unwrap();
        path
    }

    fn target(path: &str, recursive: bool) -> IncrementalRescanTarget {
        IncrementalRescanTarget {
            relative_path: path.into(),
            recursive,
        }
    }

    fn entry(path: &str, size: u64) -> IncrementalEntry {
        IncrementalEntry {
            path: path.into(),
            file_count: 1,
            directory_count: 0,
            skipped_count: 0,
            skip_reason: None,
            counted_size_bytes: size,
            logical_size_bytes: size,
            allocated_size_bytes: Some(size),
            file_identity: Some(format!("file-{size}")),
            volume_identity: Some("volume-1".to_owned()),
            modified_at: Some(2),
        }
    }

    fn checkpoint(token: &str) -> IndexCheckpoint {
        IndexCheckpoint {
            root_path: "/tmp/sample".to_owned(),
            platform: "macos".to_owned(),
            volume_identity: "volume-1".to_owned(),
            root_identity: "root-1".to_owned(),
            history_source: "fsevents".to_owned(),
            history_token: token.to_owned(),
            updated_at: 2,
        }
    }

    #[test]
    fn creates_new_snapshot_and_preserves_baseline() {
        let path = database("success");
        let scan_id = apply_incremental_snapshot(
            &path,
            1,
            Path::new("/tmp/sample"),
            &[target("old", false), target("dir", true)],
            &[entry("/tmp/sample/old", 5)],
            &checkpoint("fsevents:v1:20"),
        )
        .unwrap();
        let connection = Connection::open(&path).unwrap();
        let baseline_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM scan_entries WHERE scan_id=1", [], |row| row.get(0))
            .unwrap();
        let updated: (String, i64, i64) = connection
            .query_row("SELECT status,total_size_bytes,file_count FROM scan_sessions WHERE id=?1",[scan_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
        let updated_paths: String = connection.query_row("SELECT group_concat(relative_path,',') FROM (SELECT relative_path FROM scan_entries WHERE scan_id=?1 ORDER BY relative_path)",[scan_id],|row|row.get(0)).unwrap();
        let token: String = connection.query_row("SELECT history_token FROM index_checkpoints WHERE root_path='/tmp/sample'",[],|row|row.get(0)).unwrap();
        assert_eq!(baseline_count, 4);
        assert_eq!(updated, ("complete".to_owned(), 6, 2));
        assert_eq!(updated_paths, "keep,old");
        assert_eq!(token, "fsevents:v1:20");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rolls_back_when_replacement_is_outside_target() {
        let path = database("outside");
        assert!(apply_incremental_snapshot(&path,1,Path::new("/tmp/sample"),&[target("old",false)],&[entry("/tmp/sample/other",5)],&checkpoint("fsevents:v1:20")).is_err());
        let connection = Connection::open(&path).unwrap();
        let sessions: i64 = connection.query_row("SELECT COUNT(*) FROM scan_sessions",[],|row|row.get(0)).unwrap();
        assert_eq!(sessions, 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rolls_back_snapshot_when_checkpoint_is_invalid() {
        let path = database("checkpoint");
        assert!(apply_incremental_snapshot(&path,1,Path::new("/tmp/sample"),&[target("old",false)],&[],&checkpoint("")).is_err());
        let connection = Connection::open(&path).unwrap();
        let sessions: i64 = connection.query_row("SELECT COUNT(*) FROM scan_sessions",[],|row|row.get(0)).unwrap();
        assert_eq!(sessions, 1);
        let _ = std::fs::remove_file(path);
    }
}
