use crate::cache_activity::{self, CacheObservation, CacheRuntimeState};
use crate::cache_catalog;
use crate::incremental_paths::normalize_relative;
use crate::incremental_rescan::IncrementalRescanTarget;
use crate::index_checkpoint::{load_checkpoint, upsert_checkpoint, IndexCheckpoint};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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
    let file_identity = match (entry.volume_identity.as_ref(), entry.file_identity.as_ref()) {
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

/// 一時DBへ書き出す前にメモリへ保持する最大件数。
const STAGING_BATCH_SIZE: usize = 500;
/// 差分を確定するconnectionのメモリ上限。
///
/// page cacheを2MiBに固定し、GROUP BYなどの中間結果はheapではなくディスクへ出す。
/// SQLiteの既定値（cache_size=-2000、compile-timeのTEMP_STORE=1）に頼ると、
/// ビルド構成が変わったときに上限も変わってしまう。
const APPLY_CONNECTION_PRAGMAS: &str = "PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000; PRAGMA cache_size=-2048; PRAGMA temp_store=FILE;";

/// 1回の差分更新で置換できる最大件数。超えた場合はフルスキャンへ戻す。
pub const MAX_REPLACEMENT_ENTRIES: usize = 1_000_000;

const STAGED_COLUMNS: &str = "name,path,parent_path,relative_path,entry_type,size_bytes,logical_size,allocated_size,file_count,directory_count,skipped_count,skip_reason,is_directory,file_identity,volume_identity,modified_at,cache_catalog_version,cache_definition_id,cache_definition_version,cache_runtime_state";

/// 部分再走査の結果を一時SQLiteへ退避する。
///
/// 置換entryをVecへ溜め込まないための境界。メモリに載るのは常に
/// `STAGING_BATCH_SIZE` 件までで、残りは一時DBへ書き出す。適用時も
/// `INSERT ... SELECT` でSQLite内を流れるため、件数に比例したメモリを使わない。
pub struct IncrementalStaging {
    root_path: PathBuf,
    targets: Vec<IncrementalRescanTarget>,
    connection: Connection,
    path: PathBuf,
    pending: Vec<IncrementalEntry>,
    staged: usize,
    max_entries: usize,
}

impl Drop for IncrementalStaging {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(self.path.with_extension("sqlite3-journal"));
    }
}

/// 一時DBの名前を分ける連番。
///
/// 時刻はµs精度までしか持たない環境があり、同時に開いたstagingが同じ名前になりうる。
/// 名前が重なると別々の走査結果が1つの一時DBへ混ざるので、process内で必ず分ける。
static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl IncrementalStaging {
    /// 走査rootと置換対象targetを固定してstagingを開く。
    pub fn new(root_path: &Path, targets: &[IncrementalRescanTarget]) -> Result<Self, String> {
        if !root_path.is_absolute() {
            return Err("部分更新対象には絶対pathが必要です".to_owned());
        }
        let targets = targets
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
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos();
        let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "disk-visualizer-staging-{}-{unique}-{sequence}.sqlite3",
            std::process::id()
        ));
        let connection = Connection::open(&path)
            .map_err(|error| format!("部分更新の一時DBを開けません: {error}"))?;
        connection.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA temp_store=FILE; PRAGMA cache_size=-2048; CREATE TABLE staged_entries (relative_path TEXT PRIMARY KEY,name TEXT NOT NULL,path TEXT NOT NULL,parent_path TEXT,entry_type TEXT NOT NULL,size_bytes INTEGER NOT NULL,logical_size INTEGER NOT NULL,allocated_size INTEGER,file_count INTEGER NOT NULL,directory_count INTEGER NOT NULL,skipped_count INTEGER NOT NULL,skip_reason TEXT,is_directory INTEGER NOT NULL,file_identity TEXT,volume_identity TEXT,modified_at INTEGER,cache_catalog_version TEXT,cache_definition_id TEXT,cache_definition_version INTEGER,cache_runtime_state TEXT);").map_err(|error| format!("部分更新の一時DBを初期化できません: {error}"))?;
        Ok(Self {
            root_path: root_path.to_path_buf(),
            targets,
            connection,
            path,
            pending: Vec::with_capacity(STAGING_BATCH_SIZE),
            staged: 0,
            max_entries: MAX_REPLACEMENT_ENTRIES,
        })
    }

    /// 置換entryを1件受け取る。target外・root外・上限超過はここでfail closedする。
    pub fn record(&mut self, entry: &IncrementalEntry) -> Result<(), String> {
        let relative = entry
            .path
            .strip_prefix(&self.root_path)
            .map_err(|_| "部分再走査結果が走査root外を指しています".to_owned())?;
        let relative = normalize_relative(relative)
            .ok_or_else(|| "部分再走査結果の相対pathが不正です".to_owned())?;
        if !self
            .targets
            .iter()
            .any(|target| target_covers(target, &relative))
        {
            return Err("部分再走査結果が指定target外を指しています".to_owned());
        }
        if self.staged.saturating_add(self.pending.len()) >= self.max_entries {
            return Err("部分再走査の置換件数が上限を超えました".to_owned());
        }
        self.pending.push(entry.clone());
        if self.pending.len() >= STAGING_BATCH_SIZE {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), String> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let transaction = self
            .connection
            .transaction()
            .map_err(|error| error.to_string())?;
        {
            let mut statement = transaction.prepare_cached(&format!("INSERT INTO staged_entries ({STAGED_COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)")).map_err(|error| error.to_string())?;
            for entry in &self.pending {
                let relative = entry
                    .path
                    .strip_prefix(&self.root_path)
                    .map_err(|_| "部分再走査結果が走査root外を指しています".to_owned())?;
                let classification = cache_catalog::classify_absolute_path(&entry.path);
                let before = classification.as_ref().and_then(|_| observation(entry));
                let runtime_state = classification
                    .as_ref()
                    .map(|_| cache_activity::evaluate_path_after(&entry.path, before.as_ref()));
                let entry_type = if entry.directory_count > 0 {
                    "directory"
                } else if entry.file_count > 0 {
                    "file"
                } else {
                    "other"
                };
                statement
                    .execute(params![
                        entry
                            .path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .as_ref(),
                        entry.path.to_string_lossy().as_ref(),
                        entry
                            .path
                            .parent()
                            .map(|value| value.to_string_lossy().into_owned()),
                        relative.to_string_lossy().as_ref(),
                        entry_type,
                        to_i64(entry.counted_size_bytes, "集計サイズ")?,
                        to_i64(entry.logical_size_bytes, "論理サイズ")?,
                        optional_to_i64(entry.allocated_size_bytes, "割り当て済みサイズ")?,
                        to_i64(entry.file_count, "ファイル数")?,
                        to_i64(entry.directory_count, "フォルダ数")?,
                        to_i64(entry.skipped_count, "読み飛ばし数")?,
                        entry.skip_reason,
                        i64::from(entry.directory_count > 0),
                        entry.file_identity,
                        entry.volume_identity,
                        entry.modified_at,
                        classification
                            .as_ref()
                            .map(|value| value.catalog_version.as_str()),
                        classification
                            .as_ref()
                            .map(|value| value.definition_id.as_str()),
                        classification
                            .as_ref()
                            .map(|value| i64::from(value.definition_version)),
                        runtime_state.as_ref().map(CacheRuntimeState::as_str)
                    ])
                    .map_err(|error| match error {
                        rusqlite::Error::SqliteFailure(failure, _)
                            if failure.code == rusqlite::ErrorCode::ConstraintViolation =>
                        {
                            "部分再走査結果に重複pathがあります".to_owned()
                        }
                        other => format!("部分再走査結果を退避できません: {other}"),
                    })?;
            }
        }
        transaction
            .commit()
            .map_err(|error| format!("部分再走査結果を退避できません: {error}"))?;
        self.staged = self.staged.saturating_add(self.pending.len());
        self.pending.clear();
        Ok(())
    }
}

#[cfg(test)]
impl IncrementalStaging {
    /// 上限超過の挙動を、実際に上限件数を作らずに確かめるための縮小。
    pub(crate) fn with_max_entries(mut self, max_entries: usize) -> Self {
        self.max_entries = max_entries;
        self
    }

    /// 退避済みの相対pathとentry種別を昇順で返す。
    pub(crate) fn staged_rows(&mut self) -> Result<Vec<(String, String)>, String> {
        self.flush()?;
        let mut statement = self
            .connection
            .prepare("SELECT relative_path,entry_type FROM staged_entries ORDER BY relative_path")
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        Ok(rows)
    }
}

/// 差分適用の前提と結果。
///
/// `expected` は信頼評価の時点で保存されていたcheckpointそのもの。確定transaction内で
/// 読み直して一致を確かめることで、評価から確定までの間に別のスキャンが進めた
/// checkpointを、古い評価結果で上書きしないようにする。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointTransition {
    /// 評価時点で保存されていたcheckpoint。
    pub expected: IndexCheckpoint,
    /// 確定に成功したときだけ保存する次のhistory token。
    pub next_history_token: String,
}

/// stagingに退避した置換結果で新しいsnapshotを確定する。
///
/// baselineの複製・target範囲の削除・置換の挿入・ハードリンク再集計・session確定・
/// checkpoint更新を1つのtransactionで行う。途中で失敗した場合は何も残らない。
pub fn apply_staged_snapshot(
    database_path: &Path,
    staging: &mut IncrementalStaging,
    baseline_scan_id: i64,
    transition: &CheckpointTransition,
) -> Result<i64, String> {
    if transition.expected.root_path != staging.root_path.to_string_lossy() {
        return Err("部分更新対象とcheckpointのrootが一致しません".to_owned());
    }
    if transition.expected.baseline_scan_id != Some(baseline_scan_id) {
        return Err("checkpointが指す基準スキャンと更新元が一致しません".to_owned());
    }
    staging.flush()?;

    let mut connection = Connection::open(database_path)
        .map_err(|error| format!("スキャン履歴を開けません: {error}"))?;
    connection
        .execute_batch(APPLY_CONNECTION_PRAGMAS)
        .map_err(|error| error.to_string())?;
    connection
        .execute(
            "ATTACH DATABASE ?1 AS staging",
            [staging.path.to_string_lossy().as_ref()],
        )
        .map_err(|error| format!("部分更新の一時DBを接続できません: {error}"))?;
    let result = apply_within_transaction(&mut connection, staging, baseline_scan_id, transition);
    let _ = connection.execute_batch("DETACH DATABASE staging;");
    result
}

fn apply_within_transaction(
    connection: &mut Connection,
    staging: &IncrementalStaging,
    baseline_scan_id: i64,
    transition: &CheckpointTransition,
) -> Result<i64, String> {
    let root_path = staging.root_path.to_string_lossy().into_owned();
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    // 評価から確定までの間に別のスキャンがcheckpointを進めていれば、この結果はもう古い。
    // 古いtokenで上書きすると、その間の変更を二度と拾えなくなるため確定しない。
    if load_checkpoint(&transaction, &root_path)?.as_ref() != Some(&transition.expected) {
        return Err("差分更新の前提となるcheckpointが更新されています".to_owned());
    }
    let baseline_root: Option<String> = transaction
        .query_row(
            "SELECT root_path FROM scan_sessions WHERE id=?1 AND status='complete'",
            [baseline_scan_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    if baseline_root.as_deref() != Some(root_path.as_str()) {
        return Err("一致する完了済み基準スキャンがありません".to_owned());
    }
    transaction
        .execute(
            "INSERT INTO scan_sessions (root_path,status,started_at) VALUES (?1,'in_progress',?2)",
            params![root_path, unix_time()?],
        )
        .map_err(|error| format!("部分更新sessionを開始できません: {error}"))?;
    let scan_id = transaction.last_insert_rowid();
    transaction.execute(&format!("INSERT INTO scan_entries (scan_id,{STAGED_COLUMNS}) SELECT ?1,{STAGED_COLUMNS} FROM scan_entries WHERE scan_id=?2"),params![scan_id,baseline_scan_id]).map_err(|error|format!("基準スキャンを複製できません: {error}"))?;
    for target in &staging.targets {
        if target.relative_path == Path::new(".") {
            transaction
                .execute("DELETE FROM scan_entries WHERE scan_id=?1", [scan_id])
                .map_err(|error| error.to_string())?;
        } else if target.recursive {
            let relative = target.relative_path.to_string_lossy();
            let prefix = format!("{}{sep}", relative, sep = std::path::MAIN_SEPARATOR);
            transaction.execute("DELETE FROM scan_entries WHERE scan_id=?1 AND (relative_path=?2 OR instr(relative_path,?3)=1)",params![scan_id,relative.as_ref(),prefix]).map_err(|error|format!("部分再走査範囲を置換できません: {error}"))?;
        } else {
            transaction
                .execute(
                    "DELETE FROM scan_entries WHERE scan_id=?1 AND relative_path=?2",
                    params![scan_id, target.relative_path.to_string_lossy().as_ref()],
                )
                .map_err(|error| format!("部分再走査項目を置換できません: {error}"))?;
        }
    }
    transaction.execute(&format!("INSERT INTO scan_entries (scan_id,{STAGED_COLUMNS}) SELECT ?1,{STAGED_COLUMNS} FROM staging.staged_entries"),[scan_id]).map_err(|error|format!("部分再走査結果を保存できません: {error}"))?;
    refresh_linked_metadata(&transaction, scan_id)?;
    reaggregate_hard_links(&transaction, scan_id)?;
    let (size, files, directories, skipped): (i64, i64, i64, i64) = transaction.query_row("SELECT COALESCE(SUM(size_bytes),0),COALESCE(SUM(file_count),0),COALESCE(SUM(directory_count),0),COALESCE(SUM(skipped_count),0) FROM scan_entries WHERE scan_id=?1",[scan_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).map_err(|error|format!("部分更新の集計値を計算できません: {error}"))?;
    let changed = transaction.execute("UPDATE scan_sessions SET status='complete',total_size_bytes=?2,file_count=?3,directory_count=?4,skipped_count=?5,elapsed_milliseconds=0,completed_at=?6 WHERE id=?1 AND status='in_progress'",params![scan_id,size,files,directories,skipped,unix_time()?]).map_err(|error|format!("部分更新sessionを確定できません: {error}"))?;
    if changed != 1 {
        return Err("部分更新sessionを確定できません".to_owned());
    }
    upsert_checkpoint(
        &transaction,
        &IndexCheckpoint {
            history_token: transition.next_history_token.clone(),
            baseline_scan_id: Some(scan_id),
            updated_at: unix_time()?,
            ..transition.expected.clone()
        },
    )?;
    transaction
        .commit()
        .map_err(|error| format!("部分更新結果とcheckpointを確定できません: {error}"))?;
    Ok(scan_id)
}

/// 再走査で見たfileのmetadataを、同じidentityを指す未再走査の行にも反映する。
///
/// ハードリンクはどのリンク経由で書き換えても実体が変わる。再走査targetに含まれない
/// 側の行はbaselineから複製されたままなので、論理サイズ・割り当てサイズ・更新時刻が
/// 古いまま残る。identityが一致する行は同じ実体なので、今回観測した値へ揃える。
fn refresh_linked_metadata(connection: &Connection, scan_id: i64) -> Result<(), String> {
    connection.execute("UPDATE scan_entries SET logical_size=(SELECT MAX(s.logical_size) FROM staging.staged_entries s WHERE s.volume_identity=scan_entries.volume_identity AND s.file_identity=scan_entries.file_identity),allocated_size=(SELECT MAX(s.allocated_size) FROM staging.staged_entries s WHERE s.volume_identity=scan_entries.volume_identity AND s.file_identity=scan_entries.file_identity),modified_at=(SELECT MAX(s.modified_at) FROM staging.staged_entries s WHERE s.volume_identity=scan_entries.volume_identity AND s.file_identity=scan_entries.file_identity) WHERE scan_id=?1 AND file_identity IS NOT NULL AND volume_identity IS NOT NULL AND EXISTS (SELECT 1 FROM staging.staged_entries s WHERE s.volume_identity=scan_entries.volume_identity AND s.file_identity=scan_entries.file_identity)",[scan_id]).map_err(|error|format!("ハードリンク先のmetadataを更新できません: {error}"))?;
    Ok(())
}

/// 未変更領域も含めてハードリンクを再集計する。
///
/// 計上元のリンクが削除・追加・変更されると、baselineから引き継いだ計上が
/// 過少にも過大にもなる。snapshot全体で一度計上を落とし、identityごとに
/// 1件だけ論理サイズを持たせ直すことで、集計元の入れ替わりを吸収する。
fn reaggregate_hard_links(connection: &Connection, scan_id: i64) -> Result<(), String> {
    connection.execute("UPDATE scan_entries SET size_bytes=0 WHERE scan_id=?1 AND file_identity IS NOT NULL AND volume_identity IS NOT NULL",[scan_id]).map_err(|error|format!("ハードリンクの計上を初期化できません: {error}"))?;
    connection.execute("UPDATE scan_entries SET size_bytes=logical_size WHERE id IN (SELECT MIN(id) FROM scan_entries WHERE scan_id=?1 AND file_identity IS NOT NULL AND volume_identity IS NOT NULL GROUP BY volume_identity,file_identity)",[scan_id]).map_err(|error|format!("ハードリンクを再集計できません: {error}"))?;
    Ok(())
}

#[cfg(test)]
pub fn apply_incremental_snapshot(
    database_path: &Path,
    baseline_scan_id: i64,
    root_path: &Path,
    targets: &[IncrementalRescanTarget],
    replacements: &[IncrementalEntry],
    transition: &CheckpointTransition,
) -> Result<i64, String> {
    let mut staging = IncrementalStaging::new(root_path, targets)?;
    for entry in replacements {
        staging.record(entry)?;
    }
    apply_staged_snapshot(database_path, &mut staging, baseline_scan_id, transition)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("disk-visualizer-root-{name}"))
    }

    fn database(name: &str, root: &Path) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "disk-visualizer-incremental-{name}-{}-{unique}.sqlite3",
            std::process::id()
        ));
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE scan_sessions (id INTEGER PRIMARY KEY,root_path TEXT NOT NULL,status TEXT NOT NULL,total_size_bytes INTEGER NOT NULL DEFAULT 0,file_count INTEGER NOT NULL DEFAULT 0,directory_count INTEGER NOT NULL DEFAULT 0,skipped_count INTEGER NOT NULL DEFAULT 0,elapsed_milliseconds INTEGER NOT NULL DEFAULT 0,started_at INTEGER NOT NULL,completed_at INTEGER); CREATE TABLE scan_entries (id INTEGER PRIMARY KEY,scan_id INTEGER NOT NULL REFERENCES scan_sessions(id) ON DELETE CASCADE,name TEXT NOT NULL,path TEXT NOT NULL,parent_path TEXT,relative_path TEXT NOT NULL,entry_type TEXT NOT NULL,size_bytes INTEGER NOT NULL,logical_size INTEGER NOT NULL,allocated_size INTEGER,file_count INTEGER NOT NULL,directory_count INTEGER NOT NULL,skipped_count INTEGER NOT NULL DEFAULT 0,is_directory INTEGER NOT NULL,file_identity TEXT,volume_identity TEXT,modified_at INTEGER,skip_reason TEXT,cache_catalog_version TEXT,cache_definition_id TEXT,cache_definition_version INTEGER,cache_runtime_state TEXT); CREATE TABLE index_checkpoints (root_path TEXT PRIMARY KEY,platform TEXT NOT NULL,volume_identity TEXT NOT NULL,root_identity TEXT NOT NULL,history_source TEXT NOT NULL,history_token TEXT NOT NULL,baseline_scan_id INTEGER,updated_at INTEGER NOT NULL);").unwrap();
        connection.execute("INSERT INTO scan_sessions (id,root_path,status,total_size_bytes,file_count,directory_count,skipped_count,started_at,completed_at) VALUES (1,?1,'complete',6,3,1,0,1,1)",[root.to_string_lossy().as_ref()]).unwrap();
        let rows = [
            ("keep", "keep", 1, 1, 0),
            ("old", "old", 2, 1, 0),
            ("dir", "dir", 0, 0, 1),
            ("nested", "dir/nested", 3, 1, 0),
        ];
        connection.execute("INSERT INTO index_checkpoints (root_path,platform,volume_identity,root_identity,history_source,history_token,baseline_scan_id,updated_at) VALUES (?1,'macos','volume-1','root-1','fsevents',?2,1,2)",params![root.to_string_lossy().as_ref(),STORED_TOKEN]).unwrap();
        for (name, relative, size, files, directories) in rows {
            let relative = relative.replace('/', std::path::MAIN_SEPARATOR_STR);
            let absolute = root.join(&relative);
            connection.execute("INSERT INTO scan_entries (scan_id,name,path,parent_path,relative_path,entry_type,size_bytes,logical_size,file_count,directory_count,skipped_count,is_directory) VALUES (1,?1,?2,?3,?4,?5,?6,?6,?7,?8,0,?9)", params![name,absolute.to_string_lossy().as_ref(),absolute.parent().map(|value|value.to_string_lossy().into_owned()),absolute.strip_prefix(root).unwrap().to_string_lossy().as_ref(),if directories > 0 { "directory" } else { "file" },size,files,directories,if directories > 0 { 1 } else { 0 }]).unwrap();
        }
        path
    }

    fn target(path: &str, recursive: bool) -> IncrementalRescanTarget {
        IncrementalRescanTarget {
            relative_path: path.into(),
            recursive,
        }
    }

    fn entry(path: PathBuf, size: u64) -> IncrementalEntry {
        IncrementalEntry {
            path,
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

    /// fixtureのdatabaseに保存済みのcheckpoint token。
    const STORED_TOKEN: &str = "fsevents:v1:10";

    fn checkpoint(root: &Path) -> IndexCheckpoint {
        IndexCheckpoint {
            root_path: root.to_string_lossy().into_owned(),
            platform: "macos".to_owned(),
            volume_identity: "volume-1".to_owned(),
            root_identity: "root-1".to_owned(),
            history_source: "fsevents".to_owned(),
            history_token: STORED_TOKEN.to_owned(),
            baseline_scan_id: Some(1),
            updated_at: 2,
        }
    }

    /// 保存済みcheckpointを前提に、確定できたら進めるtokenを組み合わせる。
    fn transition(root: &Path, next_history_token: &str) -> CheckpointTransition {
        CheckpointTransition {
            expected: checkpoint(root),
            next_history_token: next_history_token.to_owned(),
        }
    }

    /// 確定用connectionのメモリ上限を、SQLiteの既定値に頼らず固定していることの確認。
    #[test]
    fn bounds_the_page_cache_and_spills_temporary_results_to_disk() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(APPLY_CONNECTION_PRAGMAS).unwrap();
        let cache_size: i64 = connection
            .query_row("PRAGMA cache_size", [], |row| row.get(0))
            .unwrap();
        // 負値はKiB指定。page_sizeに依らず2MiBで頭打ちになる。
        assert_eq!(cache_size, -2048);
        let temp_store: i64 = connection
            .query_row("PRAGMA temp_store", [], |row| row.get(0))
            .unwrap();
        // 1 = FILE。GROUP BYなどの中間結果をheapへ積まない。
        assert_eq!(temp_store, 1);
    }

    #[test]
    fn stops_staging_before_exceeding_the_replacement_limit() {
        let root = root("limit");
        let mut staging = IncrementalStaging::new(&root, &[target("dir", true)])
            .unwrap()
            .with_max_entries(2);
        staging.record(&entry(root.join("dir/a"), 1)).unwrap();
        staging.record(&entry(root.join("dir/b"), 2)).unwrap();
        assert_eq!(
            staging.record(&entry(root.join("dir/c"), 3)),
            Err("部分再走査の置換件数が上限を超えました".to_owned())
        );
        // 上限で止めたあとも、受理済みの分は失われない。
        assert_eq!(
            staging.staged_rows().unwrap(),
            vec![
                (
                    format!("dir{}a", std::path::MAIN_SEPARATOR),
                    "file".to_owned()
                ),
                (
                    format!("dir{}b", std::path::MAIN_SEPARATOR),
                    "file".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn creates_new_snapshot_and_preserves_baseline() {
        let root = root("success");
        let path = database("success", &root);
        let scan_id = apply_incremental_snapshot(
            &path,
            1,
            &root,
            &[target("old", false), target("dir", true)],
            &[entry(root.join("old"), 5)],
            &transition(&root, "fsevents:v1:20"),
        )
        .unwrap();
        let connection = Connection::open(&path).unwrap();
        let baseline_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM scan_entries WHERE scan_id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let updated: (String, i64, i64) = connection
            .query_row(
                "SELECT status,total_size_bytes,file_count FROM scan_sessions WHERE id=?1",
                [scan_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let updated_paths: String = connection.query_row("SELECT group_concat(relative_path,',') FROM (SELECT relative_path FROM scan_entries WHERE scan_id=?1 ORDER BY relative_path)", [scan_id], |row| row.get(0)).unwrap();
        let token: String = connection
            .query_row(
                "SELECT history_token FROM index_checkpoints WHERE root_path=?1",
                [root.to_string_lossy().as_ref()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(baseline_count, 4);
        assert_eq!(updated, ("complete".to_owned(), 6, 2));
        assert_eq!(updated_paths, "keep,old");
        assert_eq!(token, "fsevents:v1:20");
        let _ = std::fs::remove_file(path);
    }

    /// ハードリンク検証用に、同一identityの2件を持つbaselineを作る。
    fn hard_link_database(name: &str, root: &Path) -> PathBuf {
        let path = database(name, root);
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch("DELETE FROM scan_entries WHERE scan_id=1")
            .unwrap();
        for (relative, counted) in [("a.bin", 10), ("b.bin", 0)] {
            let absolute = root.join(relative);
            connection.execute("INSERT INTO scan_entries (scan_id,name,path,parent_path,relative_path,entry_type,size_bytes,logical_size,file_count,directory_count,skipped_count,is_directory,file_identity,volume_identity) VALUES (1,?1,?2,?3,?1,'file',?4,10,1,0,0,0,'file-9','volume-1')", params![relative,absolute.to_string_lossy().as_ref(),root.to_string_lossy().as_ref(),counted]).unwrap();
        }
        connection
            .execute_batch("UPDATE scan_sessions SET total_size_bytes=10,file_count=2 WHERE id=1")
            .unwrap();
        path
    }

    fn totals(path: &Path, scan_id: i64) -> (i64, i64, i64) {
        Connection::open(path)
            .unwrap()
            .query_row(
                "SELECT (SELECT total_size_bytes FROM scan_sessions WHERE id=?1),COALESCE(SUM(size_bytes),0),COALESCE(SUM(logical_size),0) FROM scan_entries WHERE scan_id=?1",
                [scan_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
    }

    #[test]
    fn reaggregates_hard_links_when_the_counted_link_disappears() {
        let root = root("hard-link-removed");
        let path = hard_link_database("hard-link-removed", &root);
        let scan_id = apply_incremental_snapshot(
            &path,
            1,
            &root,
            &[target("a.bin", true)],
            &[],
            &transition(&root, "fsevents:v1:20"),
        )
        .unwrap();
        // 計上元が消えても、残ったリンクへ計上が移る。
        assert_eq!(totals(&path, scan_id), (10, 10, 10));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn reaggregates_hard_links_when_a_link_is_rescanned() {
        let root = root("hard-link-rescanned");
        let path = hard_link_database("hard-link-rescanned", &root);
        let mut replacement = entry(root.join("b.bin"), 10);
        replacement.file_identity = Some("file-9".to_owned());
        let scan_id = apply_incremental_snapshot(
            &path,
            1,
            &root,
            &[target("b.bin", true)],
            &[replacement],
            &transition(&root, "fsevents:v1:20"),
        )
        .unwrap();
        // 再走査した側が満額で戻っても、二重計上しない。
        assert_eq!(totals(&path, scan_id), (10, 10, 20));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn refuses_to_apply_when_the_stored_checkpoint_moved_since_the_assessment() {
        let root = root("stale-checkpoint");
        let path = database("stale-checkpoint", &root);
        // 評価から確定までの間に、別のスキャンがcheckpointを進めた状態を作る。
        let newer = IndexCheckpoint {
            history_token: "fsevents:v1:90".to_owned(),
            updated_at: 9,
            ..checkpoint(&root)
        };
        upsert_checkpoint(&Connection::open(&path).unwrap(), &newer).unwrap();

        let error = apply_incremental_snapshot(
            &path,
            1,
            &root,
            &[target("old", false)],
            &[],
            &transition(&root, "fsevents:v1:20"),
        )
        .unwrap_err();

        assert_eq!(error, "差分更新の前提となるcheckpointが更新されています");
        let connection = Connection::open(&path).unwrap();
        // 進んだcheckpointを古いtokenで巻き戻さない。
        assert_eq!(
            load_checkpoint(&connection, root.to_string_lossy().as_ref()).unwrap(),
            Some(newer)
        );
        let sessions: i64 = connection
            .query_row("SELECT COUNT(*) FROM scan_sessions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(sessions, 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn refreshes_metadata_of_hard_links_that_were_not_rescanned() {
        let root = root("hard-link-metadata");
        let path = hard_link_database("hard-link-metadata", &root);
        // 再走査したのはb.binだけ。a.binは同じ実体を指す未再走査のリンク。
        let mut replacement = entry(root.join("b.bin"), 20);
        replacement.file_identity = Some("file-9".to_owned());
        replacement.modified_at = Some(99);

        let scan_id = apply_incremental_snapshot(
            &path,
            1,
            &root,
            &[target("b.bin", true)],
            &[replacement],
            &transition(&root, "fsevents:v1:20"),
        )
        .unwrap();

        let connection = Connection::open(&path).unwrap();
        let mut statement = connection.prepare("SELECT relative_path,logical_size,allocated_size,modified_at FROM scan_entries WHERE scan_id=?1 ORDER BY relative_path").unwrap();
        let rows: Vec<(String, i64, Option<i64>, Option<i64>)> = statement
            .query_map([scan_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        // 未再走査のa.bin側も、リンク経由の書き換えを反映した値へ揃う。
        assert_eq!(
            rows,
            vec![
                ("a.bin".to_owned(), 20, Some(20), Some(99)),
                ("b.bin".to_owned(), 20, Some(20), Some(99)),
            ]
        );
        // 論理サイズは両方に載るが、計上は1件だけ。
        assert_eq!(totals(&path, scan_id), (20, 20, 40));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rejects_a_checkpoint_that_points_at_another_baseline() {
        let root = root("baseline-mismatch");
        let path = database("baseline-mismatch", &root);
        let mut mismatched = transition(&root, "fsevents:v1:20");
        mismatched.expected.baseline_scan_id = Some(2);
        assert!(apply_incremental_snapshot(
            &path,
            1,
            &root,
            &[target("old", false)],
            &[],
            &mismatched
        )
        .is_err());
        mismatched.expected.baseline_scan_id = None;
        assert!(apply_incremental_snapshot(
            &path,
            1,
            &root,
            &[target("old", false)],
            &[],
            &mismatched
        )
        .is_err());
        let connection = Connection::open(&path).unwrap();
        let sessions: i64 = connection
            .query_row("SELECT COUNT(*) FROM scan_sessions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(sessions, 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn advances_the_baseline_link_to_the_new_snapshot() {
        let root = root("baseline-advance");
        let path = database("baseline-advance", &root);
        let scan_id = apply_incremental_snapshot(
            &path,
            1,
            &root,
            &[target("old", false)],
            &[entry(root.join("old"), 5)],
            &transition(&root, "fsevents:v1:20"),
        )
        .unwrap();
        let baseline: i64 = Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT baseline_scan_id FROM index_checkpoints WHERE root_path=?1",
                [root.to_string_lossy().as_ref()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(baseline, scan_id);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rejects_duplicate_replacement_paths() {
        let root = root("duplicate");
        let path = database("duplicate", &root);
        assert!(apply_incremental_snapshot(
            &path,
            1,
            &root,
            &[target("old", false)],
            &[entry(root.join("old"), 5), entry(root.join("old"), 6)],
            &transition(&root, "fsevents:v1:20")
        )
        .is_err());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn staging_never_retains_more_than_one_batch_in_memory() {
        let root = root("bounded-staging");
        let mut staging = IncrementalStaging::new(&root, &[target(".", true)]).unwrap();
        for index in 0..(STAGING_BATCH_SIZE * 3 + 7) {
            staging
                .record(&entry(root.join(format!("{index}.bin")), 1))
                .unwrap();
            assert!(staging.pending.len() < STAGING_BATCH_SIZE);
        }
        staging.flush().unwrap();
        let staged: i64 = staging
            .connection
            .query_row("SELECT COUNT(*) FROM staged_entries", [], |row| row.get(0))
            .unwrap();
        assert_eq!(staged, (STAGING_BATCH_SIZE * 3 + 7) as i64);
    }

    #[test]
    fn removes_the_staging_database_when_dropped() {
        let root = root("staging-cleanup");
        let staging_path = {
            let staging = IncrementalStaging::new(&root, &[target(".", true)]).unwrap();
            staging.path.clone()
        };
        assert!(!staging_path.exists());
    }

    #[test]
    fn rolls_back_when_replacement_is_outside_target() {
        let root = root("outside");
        let path = database("outside", &root);
        assert!(apply_incremental_snapshot(
            &path,
            1,
            &root,
            &[target("old", false)],
            &[entry(root.join("other"), 5)],
            &transition(&root, "fsevents:v1:20")
        )
        .is_err());
        let connection = Connection::open(&path).unwrap();
        let sessions: i64 = connection
            .query_row("SELECT COUNT(*) FROM scan_sessions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(sessions, 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rolls_back_snapshot_when_checkpoint_is_invalid() {
        let root = root("checkpoint");
        let path = database("checkpoint", &root);
        assert!(apply_incremental_snapshot(
            &path,
            1,
            &root,
            &[target("old", false)],
            &[],
            &transition(&root, "")
        )
        .is_err());
        let connection = Connection::open(&path).unwrap();
        let sessions: i64 = connection
            .query_row("SELECT COUNT(*) FROM scan_sessions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(sessions, 1);
        let _ = std::fs::remove_file(path);
    }
}
