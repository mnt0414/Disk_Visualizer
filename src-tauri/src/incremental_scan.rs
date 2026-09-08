use crate::file_metrics;
use crate::incremental_rescan::{escalate_to_subtrees, IncrementalRescanTarget};
use crate::incremental_storage::{IncrementalEntry, IncrementalStaging};
use crate::scanner::{crosses_volume, scan_entry, ScanProgress, SeenFileStore};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, Metadata};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

/// 部分再走査中に変わらない前提をまとめる。
struct RescanContext<'a> {
    root: &'a Path,
    root_volume_identity: Option<&'a str>,
    seen_files: &'a SeenFileStore,
}

/// 置換対象の親directoryを解決した結果。
enum ResolvedParent {
    Directory(Dir),
    /// 走査rootの走査結果に行が現れない状態。置換は空になる。
    Absent,
}

fn entry_from_progress(progress: &ScanProgress) -> IncrementalEntry {
    IncrementalEntry {
        path: progress.path.clone(),
        file_count: progress.file_count,
        directory_count: progress.directory_count,
        skipped_count: progress.skipped_count,
        skip_reason: progress.skip_reason,
        counted_size_bytes: progress.counted_size_bytes,
        logical_size_bytes: progress.logical_size_bytes,
        allocated_size_bytes: progress.allocated_size_bytes,
        file_identity: progress.file_identity.clone(),
        volume_identity: progress.volume_identity.clone(),
        modified_at: progress.modified_at,
    }
}

/// 直前に確認したentryと、開いたdirectoryが同一であることを検証する。
///
/// 確認と開くの間にsymlinkへ差し替えられても追跡しないための境界。identityを
/// 取得できないプラットフォームでは検証できないため、fail closedで失敗させる。
fn is_same_directory(link: &Metadata, opened: &Metadata) -> Result<bool, String> {
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt;
        Ok((link.dev(), link.ino()) == (opened.dev(), opened.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = (link, opened);
        Err("部分再走査の同一性検証はこのプラットフォームでは未対応です".to_owned())
    }
}

/// 走査rootからの相対pathでdirectoryを開く。
///
/// symlink・junction・fileへの差し替えは追跡しない。フルスキャンも配下へ降りない
/// ため、そうした場合は「行が存在しない」を意味する `Absent` を返す。読み取り失敗と
/// 消失は区別し、消失以外はfail closedでエラーにする。
fn open_relative_directory(
    root: &Dir,
    root_volume_identity: Option<&str>,
    relative: &Path,
) -> Result<ResolvedParent, String> {
    let mut current = root
        .try_clone()
        .map_err(|error| format!("走査rootを複製できません: {error}"))?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err("部分再走査targetに解決できない構成要素が含まれています".to_owned());
        };
        let link = match current.symlink_metadata(name) {
            Ok(value) => value,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(ResolvedParent::Absent),
            Err(error) => {
                return Err(format!("部分再走査targetの親を確認できません: {error}"));
            }
        };
        if !link.is_dir() {
            return Ok(ResolvedParent::Absent);
        }
        let child = match current.open_dir(name) {
            Ok(value) => value,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(ResolvedParent::Absent),
            Err(error) => return Err(format!("部分再走査targetの親を開けません: {error}")),
        };
        let std_child = child.into_std_file();
        let volume_identity = file_metrics::volume_identity_from_open_file(&std_child);
        let child = Dir::from_std_file(std_child);
        let opened = child
            .dir_metadata()
            .map_err(|error| format!("部分再走査targetの親のidentityを取得できません: {error}"))?;
        if !is_same_directory(&link, &opened)? {
            return Err("部分再走査targetの親が確認した対象と一致しません".to_owned());
        }
        if crosses_volume(root_volume_identity, volume_identity.as_deref()) {
            return Ok(ResolvedParent::Absent);
        }
        current = child;
    }
    Ok(ResolvedParent::Directory(current))
}

/// directory直下を1回だけ走査し、要求された名前にbyte一致したentryを再走査する。
///
/// `wanted` が `None` のときはroot全体の置換なので全entryを走査する。戻り値は
/// 実際に一致した名前で、残りは消失かfail closedかを呼び出し側が判別する。
fn scan_directory<C, P>(
    context: &RescanContext<'_>,
    directory: &Dir,
    parent_relative: &Path,
    wanted: Option<&[OsString]>,
    handlers: (&mut C, &mut P),
) -> Result<Vec<OsString>, String>
where
    C: FnMut() -> bool,
    P: FnMut(&ScanProgress),
{
    let (control, progress) = handlers;
    let entries = directory
        .entries()
        .map_err(|error| format!("部分再走査targetの親を読み取れません: {error}"))?;
    let mut matched = Vec::new();
    for entry in entries {
        if !control() {
            return Err("スキャンはキャンセルされました".to_owned());
        }
        // 読めなかったentryは名前が分からない。要求名の生死は後段の再確認で判定する。
        let Ok(entry) = entry else {
            continue;
        };
        let name = entry.file_name();
        if let Some(wanted) = wanted {
            if !wanted.contains(&name) {
                continue;
            }
            matched.push(name);
        }
        scan_entry(
            entry,
            parent_relative.to_path_buf(),
            context.root,
            context.root_volume_identity,
            control,
            progress,
            context.seen_files,
        )?;
    }
    Ok(matched)
}

/// 一致しなかった要求名が本当に消失したのかを確認する。
///
/// 大文字小文字やUnicode正規化を区別しないvolumeでは、byte一致しないだけで
/// 実体が残っていることがある。消失と断定できるのはNotFoundのときだけで、
/// それ以外は読取失敗として扱い、空replacementへ倒さない。
fn confirm_absence(directory: &Dir, name: &OsString) -> Result<(), String> {
    match directory.symlink_metadata(name) {
        Ok(_) => Err("部分再走査targetの表記が実体と一致しません".to_owned()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("部分再走査targetの消失を確認できません: {error}")),
    }
}

fn group_by_parent(
    targets: &[IncrementalRescanTarget],
) -> Result<BTreeMap<PathBuf, Vec<OsString>>, String> {
    let mut groups: BTreeMap<PathBuf, Vec<OsString>> = BTreeMap::new();
    for target in targets {
        let parent = target
            .relative_path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf();
        let name = target
            .relative_path
            .file_name()
            .ok_or_else(|| "部分再走査targetの名前を解決できません".to_owned())?;
        groups.entry(parent).or_default().push(name.to_os_string());
    }
    Ok(groups)
}

/// 計画されたtargetをcapability baseで部分再走査し、置換結果をstagingへ書き出す。
///
/// 置換範囲は必ず部分木へ拡大するため、directoryのrename・削除・型変更で古い子孫が
/// 残らない。resolveできない状態はすべてエラーにして、呼び出し側がcheckpointを
/// 進めずフルスキャンへ戻せるようにする。
pub(crate) fn rescan_targets<C, P>(
    root: &Path,
    planned: &[IncrementalRescanTarget],
    control: C,
    observe: P,
) -> Result<IncrementalStaging, String>
where
    C: Fn() -> bool,
    P: Fn(&ScanProgress),
{
    if !root.is_absolute() {
        return Err("部分再走査には絶対pathが必要です".to_owned());
    }
    let targets = escalate_to_subtrees(planned)?;
    let root_directory = Dir::open_ambient_dir(root, ambient_authority())
        .map_err(|error| format!("走査rootを開けません: {error}"))?;
    let std_root = root_directory.into_std_file();
    let root_volume_identity = file_metrics::volume_identity_from_open_file(&std_root);
    let root_directory = Dir::from_std_file(std_root);
    let seen_files = SeenFileStore::new()?;
    let context = RescanContext {
        root,
        root_volume_identity: root_volume_identity.as_deref(),
        seen_files: &seen_files,
    };
    let mut staging = IncrementalStaging::new(root, &targets)?;
    let replaces_root = targets
        .iter()
        .any(|target| target.relative_path == Path::new("."));
    let failure = RefCell::new(None::<String>);
    let outcome = {
        let mut keep_going = || failure.borrow().is_none() && control();
        let mut record = |progress: &ScanProgress| {
            if failure.borrow().is_some() {
                return;
            }
            observe(progress);
            if let Err(error) = staging.record(&entry_from_progress(progress)) {
                *failure.borrow_mut() = Some(error);
            }
        };
        if replaces_root {
            scan_directory(
                &context,
                &root_directory,
                Path::new(""),
                None,
                (&mut keep_going, &mut record),
            )
            .map(|_| ())
        } else {
            rescan_groups(
                &context,
                &root_directory,
                &targets,
                (&mut keep_going, &mut record),
            )
        }
    };
    if let Some(error) = failure.into_inner() {
        return Err(error);
    }
    outcome?;
    Ok(staging)
}

fn rescan_groups<C, P>(
    context: &RescanContext<'_>,
    root_directory: &Dir,
    targets: &[IncrementalRescanTarget],
    handlers: (&mut C, &mut P),
) -> Result<(), String>
where
    C: FnMut() -> bool,
    P: FnMut(&ScanProgress),
{
    let (control, progress) = handlers;
    for (parent, names) in group_by_parent(targets)? {
        let ResolvedParent::Directory(directory) =
            open_relative_directory(root_directory, context.root_volume_identity, &parent)?
        else {
            // 親ごと消えているので、target部分木の行が消えるのが正しい状態。
            continue;
        };
        let matched = scan_directory(
            context,
            &directory,
            &parent,
            Some(&names),
            (control, progress),
        )?;
        for name in names.iter().filter(|name| !matched.contains(name)) {
            confirm_absence(&directory, name)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incremental_storage::{apply_staged_snapshot, CheckpointTransition};
    use crate::index_checkpoint::{IndexCheckpoint, IndexCheckpointRepository};
    use crate::storage::ScanRepository;
    use rusqlite::Connection;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_root(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "disk-visualizer-rescan-{name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path.canonicalize().unwrap()
    }

    fn target(path: &str) -> IncrementalRescanTarget {
        IncrementalRescanTarget {
            relative_path: PathBuf::from(path),
            recursive: false,
        }
    }

    fn staged(root: &Path, targets: &[&str]) -> Result<Vec<(String, String)>, String> {
        let targets: Vec<_> = targets.iter().map(|path| target(path)).collect();
        rescan_targets(root, &targets, || true, |_| {})?.staged_rows()
    }

    fn paths(rows: &[(String, String)]) -> Vec<&str> {
        rows.iter().map(|(path, _)| path.as_str()).collect()
    }

    #[test]
    fn replaces_added_changed_and_deleted_files() {
        let root = temporary_root("files");
        fs::write(root.join("changed.txt"), b"1234").unwrap();
        fs::write(root.join("removed.txt"), b"1234").unwrap();
        fs::remove_file(root.join("removed.txt")).unwrap();
        fs::write(root.join("added.txt"), b"12").unwrap();
        let rows = staged(&root, &["changed.txt", "removed.txt", "added.txt"]).unwrap();
        assert_eq!(paths(&rows), ["added.txt", "changed.txt"]);
        assert!(rows.iter().all(|(_, kind)| kind == "file"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn drops_old_descendants_when_a_directory_becomes_a_file() {
        let root = temporary_root("type-change");
        fs::create_dir_all(root.join("dir")).unwrap();
        fs::write(root.join("dir/child.txt"), b"1234").unwrap();
        fs::remove_dir_all(root.join("dir")).unwrap();
        fs::write(root.join("dir"), b"12").unwrap();
        let rows = staged(&root, &["dir", "dir/child.txt"]).unwrap();
        assert_eq!(rows, [("dir".to_owned(), "file".to_owned())]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn leaves_no_replacement_when_a_directory_is_deleted() {
        let root = temporary_root("deleted");
        fs::create_dir_all(root.join("dir/inner")).unwrap();
        fs::write(root.join("dir/inner/child.txt"), b"1234").unwrap();
        fs::remove_dir_all(root.join("dir")).unwrap();
        assert!(staged(&root, &["dir", "dir/inner/child.txt"])
            .unwrap()
            .is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn collects_a_renamed_directory_under_its_new_name() {
        let root = temporary_root("renamed");
        fs::create_dir_all(root.join("before")).unwrap();
        fs::write(root.join("before/child.txt"), b"1234").unwrap();
        fs::rename(root.join("before"), root.join("after")).unwrap();
        let rows = staged(&root, &["before", "after"]).unwrap();
        assert_eq!(paths(&rows), ["after", "after/child.txt"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn does_not_follow_a_directory_replaced_by_a_symlink() {
        let root = temporary_root("link-replacement");
        let outside = temporary_root("link-target");
        fs::write(outside.join("secret.bin"), b"12345678").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("dir")).unwrap();
        let rows = staged(&root, &["dir"]).unwrap();
        assert_eq!(rows, [("dir".to_owned(), "other".to_owned())]);
        assert!(staged(&root, &["dir/secret.bin"]).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn scans_every_child_when_the_root_itself_is_the_target() {
        let root = temporary_root("root-target");
        fs::create_dir_all(root.join("dir")).unwrap();
        fs::write(root.join("dir/child.txt"), b"1234").unwrap();
        fs::write(root.join("top.txt"), b"12").unwrap();
        let rows = staged(&root, &[".", "dir"]).unwrap();
        assert_eq!(paths(&rows), ["dir", "dir/child.txt", "top.txt"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fails_closed_when_a_target_cannot_be_read() {
        let root = temporary_root("unreadable");
        fs::create_dir_all(root.join("dir")).unwrap();
        fs::write(root.join("dir/child.txt"), b"1234").unwrap();
        fs::set_permissions(root.join("dir"), fs::Permissions::from_mode(0o000)).unwrap();
        let failure = match staged(&root, &["dir/child.txt"]) {
            Err(error) => error,
            Ok(_) => String::new(),
        };
        fs::set_permissions(root.join("dir"), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(failure.contains("部分再走査target"), "{failure}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fails_closed_when_the_target_spelling_does_not_match_the_entry() {
        let root = temporary_root("spelling");
        fs::write(root.join("MixedCase.txt"), b"1234").unwrap();
        if fs::symlink_metadata(root.join("mixedcase.txt")).is_ok() {
            let Err(failure) = staged(&root, &["mixedcase.txt"]) else {
                panic!("表記不一致は消失として扱わない");
            };
            assert!(failure.contains("表記が実体と一致しません"), "{failure}");
        }
        assert_eq!(
            paths(&staged(&root, &["MixedCase.txt"]).unwrap()),
            ["MixedCase.txt"]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stops_without_a_partial_replacement_when_cancelled() {
        let root = temporary_root("cancelled");
        fs::write(root.join("file.txt"), b"1234").unwrap();
        let Err(failure) = rescan_targets(&root, &[target("file.txt")], || false, |_| {}) else {
            panic!("キャンセル時は置換結果を確定しない");
        };
        assert_eq!(failure, "スキャンはキャンセルされました");
        fs::remove_dir_all(root).unwrap();
    }

    fn temporary_database(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "disk-visualizer-rescan-{name}-{}-{unique}.sqlite3",
            std::process::id()
        ))
    }

    fn full_scan(repository: &ScanRepository, root: &Path, checkpoint: Option<&IndexCheckpoint>) {
        let stream = repository
            .begin_stream(root.to_string_lossy().as_ref())
            .unwrap();
        let recorder = stream.clone();
        let summary = crate::scanner::scan_folder_path_controlled(
            root,
            || true,
            |progress| recorder.record(progress),
        )
        .unwrap();
        stream
            .complete_with_checkpoint(&summary, checkpoint)
            .unwrap();
    }

    fn latest_scan_id(database: &Path) -> i64 {
        Connection::open(database)
            .unwrap()
            .query_row(
                "SELECT id FROM scan_sessions ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// 走査結果1行分のうち、走査経路によらず一致すべき列。
    ///
    /// 比較から外した列と理由:
    /// - `id`・`scan_id`: snapshotごとに必ず変わる代理キー。
    /// - `name`・`path`・`parent_path`: `relative_path` から一意に決まる冗長表現。
    /// - `cache_*`: bundled catalogがpathから決める分類で、走査経路に依存しない。
    #[derive(Debug, PartialEq, Eq)]
    struct EntryRow {
        relative_path: String,
        entry_type: String,
        is_directory: i64,
        size_bytes: i64,
        logical_size: i64,
        allocated_size: Option<i64>,
        file_count: i64,
        directory_count: i64,
        skipped_count: i64,
        skip_reason: Option<String>,
        modified_at: Option<i64>,
        file_identity: Option<String>,
        volume_identity: Option<String>,
    }

    /// 同じ実体を指すpathが複数ある場合の、identity単位の集計。
    ///
    /// どのpathを計上元にするかはsnapshotの行順で決まり、走査経路で変わりうる。
    /// そこで行ごとの `size_bytes` は比較せず、identityごとの合計だけを比較する。
    /// 論理サイズと割り当てサイズはリンク間で一致するはずなので、最小と最大の
    /// 両方を取り、snapshot内での食い違いも検出する。
    #[derive(Debug, PartialEq, Eq)]
    struct LinkGroup {
        volume_identity: String,
        file_identity: String,
        paths: String,
        logical_size: (i64, i64),
        allocated_size: (Option<i64>, Option<i64>),
        modified_at: (Option<i64>, Option<i64>),
        counted_size: i64,
    }

    /// 計上元pathが1つしかない行。ハードリンクの正規化を挟まず、そのまま比較できる。
    fn unique_rows(database: &Path, scan_id: i64) -> Vec<EntryRow> {
        let connection = Connection::open(database).unwrap();
        let mut statement = connection.prepare("SELECT relative_path,entry_type,is_directory,size_bytes,logical_size,allocated_size,file_count,directory_count,skipped_count,skip_reason,modified_at,file_identity,volume_identity FROM scan_entries e WHERE e.scan_id=?1 AND (e.file_identity IS NULL OR e.volume_identity IS NULL OR (SELECT COUNT(*) FROM scan_entries s WHERE s.scan_id=e.scan_id AND s.volume_identity=e.volume_identity AND s.file_identity=e.file_identity)=1) ORDER BY relative_path").unwrap();
        let rows = statement
            .query_map([scan_id], |row| {
                Ok(EntryRow {
                    relative_path: row.get(0)?,
                    entry_type: row.get(1)?,
                    is_directory: row.get(2)?,
                    size_bytes: row.get(3)?,
                    logical_size: row.get(4)?,
                    allocated_size: row.get(5)?,
                    file_count: row.get(6)?,
                    directory_count: row.get(7)?,
                    skipped_count: row.get(8)?,
                    skip_reason: row.get(9)?,
                    modified_at: row.get(10)?,
                    file_identity: row.get(11)?,
                    volume_identity: row.get(12)?,
                })
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    }

    fn link_groups(database: &Path, scan_id: i64) -> Vec<LinkGroup> {
        let connection = Connection::open(database).unwrap();
        let mut statement = connection.prepare("SELECT volume_identity,file_identity,group_concat(relative_path,'|'),MIN(logical_size),MAX(logical_size),MIN(allocated_size),MAX(allocated_size),MIN(modified_at),MAX(modified_at),SUM(size_bytes) FROM (SELECT * FROM scan_entries WHERE scan_id=?1 AND file_identity IS NOT NULL AND volume_identity IS NOT NULL ORDER BY relative_path) GROUP BY volume_identity,file_identity HAVING COUNT(*)>1 ORDER BY volume_identity,file_identity").unwrap();
        let rows = statement
            .query_map([scan_id], |row| {
                Ok(LinkGroup {
                    volume_identity: row.get(0)?,
                    file_identity: row.get(1)?,
                    paths: row.get(2)?,
                    logical_size: (row.get(3)?, row.get(4)?),
                    allocated_size: (row.get(5)?, row.get(6)?),
                    modified_at: (row.get(7)?, row.get(8)?),
                    counted_size: row.get(9)?,
                })
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    }

    fn totals(database: &Path, scan_id: i64) -> (i64, i64, i64, i64) {
        Connection::open(database)
            .unwrap()
            .query_row(
                "SELECT COALESCE(SUM(size_bytes),0),COALESCE(SUM(file_count),0),COALESCE(SUM(directory_count),0),COALESCE(SUM(skipped_count),0) FROM scan_entries WHERE scan_id=?1",
                [scan_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap()
    }

    fn recursive_target(path: &str) -> IncrementalRescanTarget {
        IncrementalRescanTarget {
            relative_path: PathBuf::from(path),
            recursive: true,
        }
    }

    /// 差分更新の結果が、同じ状態を新規にフルスキャンした結果と一致することを確かめる。
    ///
    /// 含める変更: 内容変更・削除・追加・ハードリンクの計上元削除・未変更リンク経由の
    /// サイズ変更・directoryからfileへの型変更。
    #[test]
    fn matches_a_fresh_full_scan_after_changes_stop() {
        let root = temporary_root("equivalence");
        fs::create_dir_all(root.join("dir")).unwrap();
        fs::create_dir_all(root.join("typechanged")).unwrap();
        fs::write(root.join("typechanged/inner.bin"), [0_u8; 3]).unwrap();
        fs::write(root.join("dir/kept.bin"), [0_u8; 8]).unwrap();
        fs::write(root.join("changed.bin"), [0_u8; 4]).unwrap();
        fs::write(root.join("removed.bin"), [0_u8; 4]).unwrap();
        fs::write(root.join("source.bin"), [0_u8; 16]).unwrap();
        fs::hard_link(root.join("source.bin"), root.join("copy.bin")).unwrap();
        fs::write(root.join("linked.bin"), [0_u8; 6]).unwrap();
        fs::hard_link(root.join("linked.bin"), root.join("linked-copy.bin")).unwrap();

        let database = temporary_database("equivalence");
        let repository = ScanRepository::new(database.clone());
        repository.initialize().unwrap();
        let checkpoints = IndexCheckpointRepository::new(database.clone());
        checkpoints.initialize().unwrap();
        full_scan(
            &repository,
            &root,
            Some(&IndexCheckpoint {
                root_path: root.to_string_lossy().into_owned(),
                platform: "macos".to_owned(),
                volume_identity: "volume".to_owned(),
                root_identity: "root".to_owned(),
                history_source: "fsevents".to_owned(),
                history_token: "fsevents:v1:12".to_owned(),
                baseline_scan_id: None,
                updated_at: 1,
            }),
        );
        let baseline = latest_scan_id(&database);

        fs::write(root.join("changed.bin"), [0_u8; 32]).unwrap();
        fs::remove_file(root.join("removed.bin")).unwrap();
        fs::write(root.join("dir/added.bin"), [0_u8; 2]).unwrap();
        // 計上元を消しても、残ったリンクへ計上が移る。
        fs::remove_file(root.join("source.bin")).unwrap();
        // 同じinodeを書き換える。再走査しないlinked-copy.bin側にも反映されるはず。
        fs::write(root.join("linked.bin"), [0_u8; 64]).unwrap();
        // directoryをfileへ差し替える。古い子孫が残ってはいけない。
        fs::remove_dir_all(root.join("typechanged")).unwrap();
        fs::write(root.join("typechanged"), [0_u8; 5]).unwrap();

        let saved = checkpoints
            .load(root.to_string_lossy().as_ref())
            .unwrap()
            .unwrap();
        assert_eq!(saved.baseline_scan_id, Some(baseline));
        let mut targets = ["changed.bin", "removed.bin", "source.bin", "linked.bin"]
            .map(target)
            .to_vec();
        // FSEventsは変更のあったdirectoryも報告する。dir自身のmetadataもここで揃う。
        targets.push(recursive_target("dir"));
        targets.push(recursive_target("typechanged"));
        let mut staging = rescan_targets(&root, &targets, || true, |_| {}).unwrap();
        let transition = CheckpointTransition {
            expected: saved.clone(),
            next_history_token: "fsevents:v1:20".to_owned(),
        };
        let updated =
            apply_staged_snapshot(&database, &mut staging, baseline, &transition).unwrap();

        full_scan(&repository, &root, None);
        let fresh = latest_scan_id(&database);
        assert_ne!(updated, fresh);
        assert_eq!(
            unique_rows(&database, updated),
            unique_rows(&database, fresh)
        );
        assert_eq!(
            link_groups(&database, updated),
            link_groups(&database, fresh)
        );
        assert_eq!(totals(&database, updated), totals(&database, fresh));
        // 未変更リンク経由のサイズ変更が両方の行へ届いている。
        let links = link_groups(&database, updated);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].paths, "linked-copy.bin|linked.bin");
        assert_eq!(links[0].logical_size, (64, 64));
        assert_eq!(links[0].counted_size, 64);
        // 8(kept)+2(added)+32(changed)+16(copy)+64(linked)+5(typechanged)
        assert_eq!(totals(&database, updated).0, 127);

        fs::remove_dir_all(root).unwrap();
        fs::remove_file(database).unwrap();
    }

    #[test]
    fn rejects_relative_and_invalid_targets() {
        let root = temporary_root("invalid");
        assert!(rescan_targets(Path::new("relative"), &[target("a")], || true, |_| {}).is_err());
        assert!(rescan_targets(&root, &[target("../outside")], || true, |_| {}).is_err());
        assert!(rescan_targets(&root, &[target("")], || true, |_| {}).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
