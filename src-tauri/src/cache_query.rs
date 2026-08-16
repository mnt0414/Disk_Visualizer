use crate::cache_activity::CacheRuntimeState;
use crate::cache_catalog::{self, CacheDefinition};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::PathBuf;

const DEFAULT_QUERY_LIMIT: u32 = 100;
const MAX_QUERY_LIMIT: u32 = 500;

#[derive(Clone)]
pub struct CacheQueryRepository {
    database_path: PathBuf,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CacheEntryDetail {
    pub id: i64,
    pub scan_id: i64,
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub logical_size: u64,
    pub allocated_size: Option<u64>,
    pub modified_at: Option<i64>,
    pub cache_catalog_version: String,
    pub cache_definition_id: String,
    pub cache_definition_version: u32,
    pub runtime_state: Option<CacheRuntimeState>,
    pub definition: Option<CacheDefinition>,
}

struct StoredCacheEntry {
    id: i64,
    scan_id: i64,
    name: String,
    path: String,
    size_bytes: i64,
    logical_size: i64,
    allocated_size: Option<i64>,
    modified_at: Option<i64>,
    cache_catalog_version: String,
    cache_definition_id: String,
    cache_definition_version: i64,
    runtime_state: Option<String>,
}

fn to_u64(value: i64, label: &str) -> Result<u64, String> {
    u64::try_from(value).map_err(|_| format!("{label}に不正な負数が保存されています"))
}

fn optional_to_u64(value: Option<i64>, label: &str) -> Result<Option<u64>, String> {
    value.map(|value| to_u64(value, label)).transpose()
}

impl CacheQueryRepository {
    pub fn new(database_path: PathBuf) -> Self {
        Self { database_path }
    }

    fn connection(&self) -> Result<Connection, String> {
        let connection = Connection::open(&self.database_path)
            .map_err(|error| format!("スキャン履歴を開けません: {error}"))?;
        connection
            .execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")
            .map_err(|error| error.to_string())?;
        Ok(connection)
    }

    pub fn list(
        &self,
        scan_id: i64,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<Vec<CacheEntryDetail>, String> {
        let limit = limit.unwrap_or(DEFAULT_QUERY_LIMIT);
        let offset = offset.unwrap_or(0);
        if limit == 0 || limit > MAX_QUERY_LIMIT {
            return Err(format!(
                "取得件数は1件以上{MAX_QUERY_LIMIT}件以下で指定してください"
            ));
        }

        let connection = self.connection()?;
        let status = connection
            .query_row(
                "SELECT status FROM scan_sessions WHERE id=?1",
                [scan_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        match status.as_deref() {
            Some("complete") => {}
            Some(_) => return Err("未完了のスキャン結果は表示できません".to_owned()),
            None => return Err("指定されたスキャン履歴が見つかりません".to_owned()),
        }

        let mut statement = connection
            .prepare(
                "SELECT id,scan_id,name,path,size_bytes,logical_size,allocated_size,modified_at,cache_catalog_version,cache_definition_id,cache_definition_version,cache_runtime_state FROM scan_entries WHERE scan_id=?1 AND cache_catalog_version IS NOT NULL AND cache_definition_id IS NOT NULL AND cache_definition_version IS NOT NULL ORDER BY size_bytes DESC,id ASC LIMIT ?2 OFFSET ?3",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(
                params![scan_id, i64::from(limit), i64::from(offset)],
                |row| {
                    Ok(StoredCacheEntry {
                        id: row.get(0)?,
                        scan_id: row.get(1)?,
                        name: row.get(2)?,
                        path: row.get(3)?,
                        size_bytes: row.get(4)?,
                        logical_size: row.get(5)?,
                        allocated_size: row.get(6)?,
                        modified_at: row.get(7)?,
                        cache_catalog_version: row.get(8)?,
                        cache_definition_id: row.get(9)?,
                        cache_definition_version: row.get(10)?,
                        runtime_state: row.get(11)?,
                    })
                },
            )
            .map_err(|error| error.to_string())?;
        let stored = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let catalog = cache_catalog::bundled_catalog()?;

        stored
            .into_iter()
            .map(|entry| {
                let definition_version = u32::try_from(entry.cache_definition_version)
                    .map_err(|_| "キャッシュ定義バージョンが不正です".to_owned())?;
                let runtime_state = entry
                    .runtime_state
                    .as_deref()
                    .map(|value| {
                        CacheRuntimeState::from_str(value)
                            .ok_or_else(|| "キャッシュ実行時状態が不正です".to_owned())
                    })
                    .transpose()?;
                let definition = if entry.cache_catalog_version == catalog.catalog_version {
                    catalog
                        .definitions
                        .iter()
                        .find(|definition| {
                            definition.id == entry.cache_definition_id
                                && definition.definition_version == definition_version
                        })
                        .cloned()
                } else {
                    None
                };
                Ok(CacheEntryDetail {
                    id: entry.id,
                    scan_id: entry.scan_id,
                    name: entry.name,
                    path: entry.path,
                    size_bytes: to_u64(entry.size_bytes, "集計サイズ")?,
                    logical_size: to_u64(entry.logical_size, "論理サイズ")?,
                    allocated_size: optional_to_u64(entry.allocated_size, "割り当て済みサイズ")?,
                    modified_at: entry.modified_at,
                    cache_catalog_version: entry.cache_catalog_version,
                    cache_definition_id: entry.cache_definition_id,
                    cache_definition_version: definition_version,
                    runtime_state,
                    definition,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn repository(name: &str, status: &str) -> CacheQueryRepository {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "disk-visualizer-cache-query-{name}-{}-{unique}.sqlite3",
            std::process::id()
        ));
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE scan_sessions (id INTEGER PRIMARY KEY,status TEXT NOT NULL); CREATE TABLE scan_entries (id INTEGER PRIMARY KEY,scan_id INTEGER NOT NULL,name TEXT NOT NULL,path TEXT NOT NULL,size_bytes INTEGER NOT NULL,logical_size INTEGER NOT NULL,allocated_size INTEGER,modified_at INTEGER,cache_catalog_version TEXT,cache_definition_id TEXT,cache_definition_version INTEGER,cache_runtime_state TEXT CHECK(cache_runtime_state IN ('stable','changing','unknown')));",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO scan_sessions (id,status) VALUES (1,?1)",
                [status],
            )
            .unwrap();
        let catalog = cache_catalog::bundled_catalog().unwrap();
        let definition = &catalog.definitions[0];
        connection
            .execute(
                "INSERT INTO scan_entries (id,scan_id,name,path,size_bytes,logical_size,allocated_size,modified_at,cache_catalog_version,cache_definition_id,cache_definition_version,cache_runtime_state) VALUES (1,1,'cache.bin','/tmp/cache.bin',1024,2048,4096,1723700000,?1,?2,?3,'stable')",
                params![
                    catalog.catalog_version,
                    definition.id,
                    i64::from(definition.definition_version)
                ],
            )
            .unwrap();
        drop(connection);
        CacheQueryRepository::new(path)
    }

    #[test]
    fn returns_saved_classification_with_matching_definition() {
        let repository = repository("complete", "complete");
        let entries = repository.list(1, None, None).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].size_bytes, 1024);
        assert_eq!(entries[0].logical_size, 2048);
        assert_eq!(entries[0].allocated_size, Some(4096));
        assert_eq!(entries[0].runtime_state, Some(CacheRuntimeState::Stable));
        assert!(entries[0].definition.is_some());
        std::fs::remove_file(repository.database_path).unwrap();
    }

    #[test]
    fn rejects_incomplete_scan_results() {
        let repository = repository("incomplete", "interrupted");
        assert!(repository.list(1, None, None).is_err());
        std::fs::remove_file(repository.database_path).unwrap();
    }

    #[test]
    fn validates_query_limit() {
        let repository = repository("limit", "complete");
        assert!(repository.list(1, Some(0), None).is_err());
        assert!(repository.list(1, Some(MAX_QUERY_LIMIT + 1), None).is_err());
        std::fs::remove_file(repository.database_path).unwrap();
    }
}
