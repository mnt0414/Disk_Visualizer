# タスク1：共通差分更新・macOS統合

## 状態

2026-09-08。個別に実装済みだった構成要素を、FSEvents履歴の取得からSQLiteスナップショット確定までの
一本の差分更新経路として接続し、続けて統合コードの安全性を再レビューして不足を修正した。

実施した検証は次の範囲に限る。「Mac検証完了」とは扱わない。

- macOSでの`cargo fmt --check`／`cargo clippy -- -D warnings`／`cargo test`（専用の小規模fixtureに
  対する自動テスト。ホームディレクトリや実プロジェクトは走査していない）
- node@22復旧後のfrontend検証（`npm ci`／`npm run check`／`npm test`／`npm run build`）

未実施：実volume境界越えの走査、実FSEvents eventを流す経路、アプリUIでの受け入れ確認、
Windows実機、二重OS CI、上限値の実値でのメモリ・時間計測。

タスク1の完了条件（Windows回帰確認、二重OS CI、UI接続）を満たしていないため、
タスク2（Windows USN変更record読取）には着手していない。

## コード基準

- 元のmain：218824046a3df909467df387b90fe29b049004e4
- 依存修復のローカルコミット：ec6323a
- plannerの境界修正：02ae78d（`docs/task1-planner-report.md`）
- 作業ブランチ：feat/task1-incremental-integration-20260907（02ae78dから作成）
- 統合コミット：43d8d63（実装）、3bba18c（本報告書の初版）
- 安全性再レビューによる修正：3bba18c以降の作業ツリー変更（未コミット）

## 変更内容と根拠

### 1. device-relativeからscan-root-relativeへの変換（`incremental_paths.rs`、新規）

FSEventsが返す変更pathはvolume（device）相対で、走査rootがvolume rootでない場合は
そのままでは走査範囲の判定に使えない。以下を分離した純モジュールとして追加した。

- `device_relative_root`：走査rootのdevice相対pathを、componentごとにdirectory handleを開き
  `dev`／`ino`で同一性を確認しながら復元する。名前一致だけで確定せず、途中でvolumeが変わる
  経路（mount point越え）は拒否する。volume rootは`.`で表す。
- `to_scan_root_relative`：device相対の変更pathを`Inside`／`Outside`／`Invalid`の3値へ落とす。
  root自身、root外、親参照、絶対path、UTF-8化できない表現をfail closedで区別する。
- `normalize_relative`：`incremental_rescan.rs`にあった同等関数をここへ集約し、
  plannerと変換経路で同じ正規化規則を使う。

`fsevents_history.rs`側のmount point算出・相対化・そのテストは、この実装へ置き換えて削除した。
文字列prefixの一致だけで親子・同一性を判断する経路は残していない。

### 2. capability-based部分再走査（`incremental_scan.rs`、新規）

計画されたtargetを、cap-stdのdirectory handle経由で走査する。

- targetの親directoryを1 componentずつ`open_dir`で辿り、`dev`／`ino`で期待と一致するか確認する。
  一致しなければ再走査を中止する（`fails_closed_when_the_target_spelling_does_not_match_the_entry`）。
- symlink・junction・reparse pointは辿らない。directoryがsymlinkへ差し替えられた場合は
  directoryとして開かない（`does_not_follow_a_directory_replaced_by_a_symlink`）。
- 別volumeへ出る経路は走査しない。
- metadataのみ取得し、ファイル内容は一切読まない。
- 「消失」と「読取失敗」を区別する。消失は親directoryのentry不在を確認したうえで空replacement
  （＝削除）として扱い、metadata取得失敗・アクセス拒否は失敗として全体を中止する。
  読取失敗を削除として適用する経路はない（`fails_closed_when_a_target_cannot_be_read`）。
- 置換単位は`escalate_to_subtrees`で部分木へ拡大する。directoryのrename・削除・fileへの型変更で
  古い子孫が残らず、新しい子孫を取りこぼさない。plannerのexact／recursive契約（02ae78d）は
  変更していない。plannerは計画の粒度、escalateは置換の粒度という分離になっている。
- キャンセル要求時は途中結果を確定せず中止する（`stops_without_a_partial_replacement_when_cancelled`）。

### 3. 有界なSQLite staging（`incremental_storage.rs`）

置換entryとfile identityをメモリのVec／HashSetへ溜めず、一時SQLite DBへストリーミングする。

- `IncrementalStaging`は一時DBファイルへ`STAGING_BATCH_SIZE = 500`件単位で書き出す。
  in-memoryに残るのは常に1バッチ分だけ（`staging_never_retains_more_than_one_batch_in_memory`）。
- `MAX_REPLACEMENT_ENTRIES = 1_000_000`を超える置換は受け付けず、上限で停止する。
  停止しても受理済みの分は破棄しない（`stops_staging_before_exceeding_the_replacement_limit`）。
- 本体DBへは`ATTACH DATABASE` + `INSERT ... SELECT`で移し、行をアプリ側へ載せ替えない。
- 一時DBはdrop時に削除する（`removes_the_staging_database_when_dropped`）。

### 4. baselineとcheckpointの対応（`index_checkpoint.rs`、schema v8）

「同じrootの最新スキャン」を基準として推測しない。checkpointが名指しした基準scanだけを使う。

- `index_checkpoints`に`baseline_scan_id`列を追加した（v7→v8）。
- `validate_checkpoint`は0以下の基準scan IDを拒否する。
- 差分適用時、checkpointの`baseline_scan_id`と更新元が一致しなければ適用しない。
  不一致は`checkpointが指す基準スキャンと更新元が一致しません`で拒否する。
- 移行は既存のVACUUM INTO＋`PRAGMA quick_check`によるバックアップ方式を使う。
  v6→v8とv7→v8の両方の経路を持ち、それぞれテストしている。
  新規DBは初期化時点でv8になる。

### 5. ハードリンク再集計（`incremental_storage.rs`）

置換後のスナップショット全体について、`(device, inode)`単位で計上元リンクを選び直す。
未変更領域も含めて再集計するため、計上元リンクの削除・追加・サイズ変更による
過少／過大計上が起きない（`reaggregates_hard_links_when_the_counted_link_disappears`、
`reaggregates_hard_links_when_a_link_is_rescanned`）。

### 6. ScanManager統合（`scan_jobs.rs`、`lib.rs`）

`start_incremental_scan`コマンドを追加し、`ScanManager::start_incremental`へ接続した。

- 信頼評価（最大5秒のFSEvents履歴読取）は`active`ロックの外で行う。
  評価中も`get_scan_status`／`pause_scan`／`resume_scan`／`cancel_scan`は応答する。
- 信頼できない場合、計画が成立しない場合は、そのままフルスキャンを開始し、
  切り替えた理由を`FullScanReason`として返す。無言でフルスキャンへ倒さない。
- 差分更新jobは結果をメモリへ載せない。確定したscan session IDを`savedScanId`として返し、
  呼び出し側はSQLiteから読む。
- 確定（`apply_staged_snapshot`）の直前にもう一度キャンセル要求を確認する。確定は取り消せないため、
  成功後にキャンセル扱いへ落とすことはしない。

## 安全性再レビューで修正した点

統合後にレビュー指摘を受けて再確認した項目。既存実装で満たしていた点は根拠のテスト名を挙げ、
不足していた点は修正と、その修正が効いていることの確認（修正を外すとテストが落ちること）を示す。

### A. 信頼評価と確定の競合

信頼評価は`active`ロックの外で行うため、評価から確定までの間に別のstartが入る余地がある。
評価時点のcheckpointを`CheckpointTransition`として持ち回り、確定transactionの中で
保存済みcheckpointを読み直して一致を確認するようにした。一致しなければ
「差分更新の前提となるcheckpointが更新されています」で拒否し、新しいcheckpointを上書きしない。

- `refuses_to_apply_when_the_stored_checkpoint_moved_since_the_assessment`（storage層。
  transaction内の検査だけで拒否されること、新しいcheckpointが残ること、sessionが増えないこと）
- `refuses_to_commit_when_another_scan_advanced_the_checkpoint`（実スレッドで確定境界に止め、
  その間に別経路でcheckpointを進める）

同時実行制約そのものは、`active`のMutexGuardを保持したまま`Barrier::new(3)`で2本の`start`を
同時に解放し、両方が「別のスキャンが実行中です」になり、scan sessionも作られないことで確認した
（`refuses_concurrent_starts_while_a_scan_is_active`）。スキャン所要時間に結果が依存しない。

### B. 走査rootの祖先に対する再帰変更通知

走査rootの祖先pathに`MustScanSubDirs`付きのeventが来た場合、pathとしては`Outside`だが、
その部分木にはroot内も含まれる。これを捨てるとroot内の変更を取りこぼす。

`ChangeScope`に`Ancestor`を追加し、祖先pathを`Outside`と区別した。`MustScanSubDirs`を伴う
祖先変更（device root `.` を含む）は`ancestor_rescan_required`として上げ、
`FullScanReason::AncestorSubtreeChanged`を理由にフルスキャンへ倒す。
`MustScanSubDirs`を伴わない祖先変更は、その部分木の中身に触れていないので従来どおり無視する。

root全体を1つのrecursive targetとして差分更新する案は採らなかった。基準スナップショットの複製→
root配下の全削除→再挿入となり、フルスキャンより書き込み量が増えるため。範囲を保証できない場合は
理由付きでフルスキャンへ戻す方針に統一している。

- `distinguishes_strict_ancestors_from_unrelated_changes`、`treats_siblings_and_text_prefixes_as_outside`
- `requires_a_full_scan_when_an_ancestor_demands_a_subtree_rescan`、`ignores_ancestor_changes_without_a_subtree_rescan`

### C. 確定直前のroot／volume identity再検証

走査開始時の信頼評価だけでは、走査中のunmount・rename・差し替えを見逃す。確定直前に
`verify_root_identity`で走査rootの`dev`／`ino`を取り直し、checkpointの
`volume_identity`／`root_identity`と比較する。identityを取得できないplatformでは
検証できないためfail closedで失敗させる（差分更新自体がmacOS専用のため実害はない）。

- `refuses_to_commit_when_the_scan_root_was_replaced`（走査完了後・確定前にrootをrenameし、
  同名のdirectoryを作り直す）

### D. 再走査していないハードリンクのmetadata（実際の不具合）

レビューで見つかった実際の不具合。ハードリンクはどのリンク経由で書き換えても実体が変わるが、
再走査targetに含まれない側の行は基準スナップショットから複製されたままで、論理サイズ・
割り当てサイズ・更新時刻が古い値で残っていた。計上元の選び直し（`reaggregate_hard_links`）は
「どの行を計上するか」しか直さないため、この差は残る。

staged挿入の直後・再集計の前に`refresh_linked_metadata`を挟み、同じ
`(volume_identity, file_identity)`を指す未再走査の行へ、今回観測した値を反映するようにした。

この修正が効いていることの確認として、`refresh_linked_metadata`の呼び出しを一時的に外すと
次の2本が落ちる。

```
refreshes_metadata_of_hard_links_that_were_not_rescanned
  left:  [("a.bin", 10, None, None), ("b.bin", 20, Some(20), Some(99))]
  right: [("a.bin", 20, Some(20), Some(99)), ("b.bin", 20, Some(20), Some(99))]

matches_a_fresh_full_scan_after_changes_stop
  left:  LinkGroup { ... logical_size: (6, 64), ... counted_size: 6 }
  right: LinkGroup { ... logical_size: (64, 64), ... counted_size: 64 }
```

### E. キャンセルを受け付ける境界

確定（`apply_staged_snapshot`）は取り消せない。境界を次のように定めた。

1. 走査中：`can_continue`が偽になった時点で中止し、部分的な置換をstagingへ残さない。
2. 走査完了〜確定直前：ここが最後の受付地点。中断要求を再確認し、次に
   `verify_root_identity`を行い、そのうえで確定transactionへ入る。
3. 確定transaction開始後：キャンセルを受け付けない。成功した確定を後からキャンセル扱いへ
   落とすことはしない。

この境界にテスト専用のフック（`run_incremental_with_hook`）を置き、
`std::sync::mpsc::sync_channel(0)`のrendezvousで実スレッドを確実に止めてから操作する。
同期関数の直接呼出しではなく、実際のworker threadの順序を制御した決定論的なテストになっている。

- `cancels_at_the_last_moment_before_the_commit`（境界で止めてからcancel。sessionもcheckpointも動かない）
- `defers_the_commit_while_paused_and_finishes_after_resume`（境界で止めてpause→resume→確定）
- `refuses_to_commit_when_another_scan_advanced_the_checkpoint`、`refuses_to_commit_when_the_scan_root_was_replaced`

差分更新の確定経路はunix専用（`verify_root_identity`がfail closed）なので、
これらのテストは`#[cfg(unix)] mod baseline_updates`に置いた。windows-latestの`cargo test`では
このモジュールごとコンパイルされない。platform非依存のテスト
（`falls_back_to_a_full_scan_when_the_index_is_not_trusted`、
`refuses_concurrent_starts_while_a_scan_is_active`）は両OSで実行される。

### F. フルスキャンとの一致比較

path集合と全体合計だけの比較をやめ、行ごとのmetadataとidentityごとの集計を比較するようにした
（`matches_a_fresh_full_scan_after_changes_stop`）。

- **行ごと**：`relative_path`・`entry_type`・`is_directory`・`logical_size`・`allocated_size`・
  `file_count`・`directory_count`・`skipped_count`・`skip_reason`・`modified_at`・
  `file_identity`・`volume_identity`。
- **identityごと**：`(volume_identity, file_identity)`単位で、path集合・論理サイズ・
  割り当てサイズ・更新時刻・計上された`size_bytes`を比較する。ハードリンクについて
  正規化するのは「どのpathを計上元に選んだか」だけで、集合と各サイズは正規化しない。
- **比較から除外した列と理由**：`id`・`scan_id`（snapshotごとに必ず変わる代理キー）、
  `name`・`path`・`parent_path`（`relative_path`から一意に決まる冗長表現）、
  `cache_*`（bundled catalogがpathから決める分類で、走査経路に依存しない）、
  行ごとの`size_bytes`（ハードリンクの計上元は走査順で入れ替わりうるため、identityごとに比較する）。

fixtureに含めた変化：file変更、file削除、計上元リンクの削除（`source.bin`削除、`copy.bin`残存）、
**再走査していないリンク経由でのサイズ変更**（`linked.bin`を書き換え、`linked-copy.bin`は
targetに含めない）、directoryからfileへの型変更、変更のないdirectory。

### G. schema v8の移行

- 実際のv7のDDL（`git show 89d529b`で取得。`CHECK(platform IN ('macos','windows'))`と
  `CHECK(history_source IN ('fsevents','usn'))`を含み、`baseline_scan_id`を持たない）から移行する
  （`migrates_v7_by_adding_baseline_link_with_backup`）。移行後、2件の履歴がどちらも
  tokenを保ったまま残り、`baseline_scan_id`はNULLのまま。対応が不明な基準scanを推測しない。
- 新規v8 DBと移行済みv8 DBのschema一致（`new_and_migrated_databases_agree_on_the_v8_schema`）。
  `ALTER TABLE`は列を末尾に足すため物理的な列順は揃わない。列名で参照する限り差はないので、
  名前順に並べた`pragma_table_info`（name／type／notnull／dflt_value／pk）で比較し、
  加えてCHECK制約が両方で同じ入力を拒否することを確認する。
  移行の列型を`INTEGER`から`TEXT`へ変えると、このテストは
  `("baseline_scan_id", "INTEGER", ...)` と `("baseline_scan_id", "TEXT", ...)` の差で落ちる。
- 移行失敗からの復旧（`keeps_the_v7_database_and_its_backup_when_migration_fails`）。
  `ALTER TABLE`が失敗する壊れたv7を与えると
  「スキャン履歴をv8へ移行できません: duplicate column name: baseline_scan_id」で失敗し、
  `user_version`は7のまま（次回起動で同じ移行を再試行できる）、既存の履歴も残り、
  移行前バックアップは`PRAGMA quick_check`が`ok`を返す健全な状態で残る。

## baseline・checkpointの契約

- checkpointは走査root（canonical path）ごとに1件。`root_path`が主キー。
- checkpointの`baseline_scan_id`は、そのcheckpointのtokenが指す時点のスナップショットを持つ
  完了済みscan sessionを指す。この対応が取れないcheckpointでは差分更新を開始しない
  （`requires_a_full_scan_until_the_checkpoint_names_a_baseline`）。
- フルスキャンでは、走査開始「前」のFSEvents位置をcheckpointとして採る。
  走査中に発生した変更は次回の履歴読取で再取得され、取りこぼしにならない。
- 差分更新の確定は、次の3つを同一transactionで行う。
  1. 新しいscan sessionの`complete`化（基準スナップショットの複製＋target範囲の置換＋再集計を含む）
  2. `index_checkpoints`のtoken更新
  3. `baseline_scan_id`の新スナップショットへの付け替え
  途中で失敗すればrusqliteの`Transaction`がdropでrollbackし、新sessionもentryも残らない。
- キャンセル、走査失敗、適用失敗、identity不一致ではcheckpointを進めない。
  基準スナップショットと過去の履歴（既存scan session）はそのまま残る。

## path変換の契約

| 入力（device相対） | 走査root = volume相対`work` の場合 |
|---|---|
| `work/a/b` | `Inside("a/b")` |
| `work` | `Inside(".")`（root自身の変更） |
| `workspace/x` | `Outside`（文字列prefixは親子ではない） |
| `other/x` | `Outside`（無関係な兄弟） |
| `.`、`work`の祖先 | `Ancestor`。`MustScanSubDirs`付きならフルスキャンへ、無ければ無視 |
| `/abs/path`、`..`を含むpath、UTF-8化不能 | `Invalid`（fail closed） |

走査rootがvolume rootの場合、device相対pathはそのままscan-root相対pathになる。
`Invalid`は1件でも混じれば差分更新を中止し、フルスキャンへ倒す。`Outside`は無視する。
`Ancestor`は、その部分木全体の再走査を求めるevent（`MustScanSubDirs`）を伴う場合だけ
`FullScanReason::AncestorSubtreeChanged`としてフルスキャンへ倒す。

## メモリ上限設計の根拠

- **計測済み**：`staging_never_retains_more_than_one_batch_in_memory`と
  `buffer_never_retains_a_full_batch`は、バッチ書き出し後にin-memoryバッファ長が0へ戻ることを
  実際に確認している。全replacementをVecに保持していないことは、この2本で示している。
- **計測済み**：置換件数の上限で停止する分岐は、テスト用の縮小seam（`with_max_entries(2)`）で
  実際に到達させて確認している。
- **未計測**：`MAX_REPLACEMENT_ENTRIES = 1_000_000`という実値、および
  `MAX_HISTORY_CHANGES = 65_536`／`MAX_RESCAN_TARGETS = 4_096`という実値での
  ピークメモリ・所要時間は測っていない。上限値そのものの妥当性は未検証。
- **計測済み（SQLite側の上限）**：差分を確定するconnectionは
  `PRAGMA cache_size=-2048`（page cache 2MiB）と`PRAGMA temp_store=FILE`を明示的に設定する
  （`APPLY_CONNECTION_PRAGMAS`、`bounds_the_page_cache_and_spills_temporary_results_to_disk`）。
  基準スナップショットの複製もハードリンク再集計のGROUP BYもSQLite内で完結し、
  中間結果はheapではなくディスクへ出る。行をアプリ側のVecへ載せ替える経路はない。
  設定前の実測値は`cache_size=-2000`／`temp_store=0`（compile-timeの`TEMP_STORE=1`＝FILEに委譲）／
  `page_size=4096`／`soft_heap_limit=0`で、bundled SQLiteは3.46.0。
  既定値のままでもディスクへ出るが、ビルド構成に依存させないため明示した。
  一時staging DB側も同じく`cache_size=-2048`／`temp_store=FILE`を設定している。
- **未計測**：大規模実データ（数百万entry）での差分適用のI/O量・所要時間・RSS。
  検証は専用の小規模fixtureのみで行い、ホームディレクトリや実プロジェクトは走査していない。
  再現手順：使い捨てのvolume上に数百万件のfixtureを生成し、`/usr/bin/time -l`で
  最大RSSと経過時間を採る。必要環境：数十GBの空き領域と、実行中にSpotlight索引や
  Time Machineが動かない状態。

## 意図的なトレードオフ

1. **`scan_entries`へのscan識別index（`scan_entries_scan_identity`）を追加していない。**
   このindexはv7→v8移行を持つcheckpoint repositoryが、`ScanRepository`所有のテーブルに対して
   作ることになり、所有権が分かれる。checkpoint repositoryのテストは`scan_entries`を持たない
   DBを使うため、移行が壊れる。加えて、1回の差分更新で1度だけ実行するGROUP BYのために
   フルスキャンの全INSERTへ書き込み増幅を課すことになる。SQLiteはこのGROUP BYをディスクへ
   spillできるため、現時点では追加しない判断とした。
2. **フルスキャンと差分更新の一致比較で、行ごとの`size_bytes`だけは比較していない。**
   ハードリンクの計上元として選ばれる行は、走査順によって同じ`(device, inode)`集合の中で
   入れ替わりうる。行単位で固定すると実装依存の期待値になるため、この列だけは
   identityごとの集計（`LinkGroup.counted_size`）として比較する。
   他の列は行ごとに比較する（「安全性再レビューで修正した点」のF）。
3. **確定直前のroot identity検証はunix専用で、それ以外のplatformではfail closedで失敗する。**
   `cap_std::fs::MetadataExt`が`#[cfg(unix)]`のため。差分更新自体がmacOS専用なので
   実害はないが、Windowsで差分更新を有効にする際（タスク2）には同等の検証が必要になる。

## 実行したコマンド

```
cargo fmt --manifest-path src-tauri/Cargo.toml
cargo fmt --check --manifest-path src-tauri/Cargo.toml
cargo clippy --locked --all-targets --all-features --manifest-path src-tauri/Cargo.toml -- -D warnings
cargo test --locked --manifest-path src-tauri/Cargo.toml
```

frontend側は、node@22を復旧したうえで次を実行した。

```
npm ci
npm run check
npm test
npm run build
```

環境：macOS 15.4／arm64、rustc 1.97.1、cargo 1.97.1、clippy 0.1.97、
node v22.23.2（`/opt/homebrew/opt/node@22/bin/node`）、npm 10.9.8。

テスト・警告の無効化、`#[allow]`の追加、lint設定の緩和は行っていない。

## テスト件数と結果

```
cargo fmt --check                         終了コード0（差分なし）
cargo clippy ... -- -D warnings           終了コード0（Finished dev profile、警告0）
cargo test --locked                       終了コード0
                                          142 passed; 0 failed; 1 ignored
                                          main.rs 0件、doc-tests 0件
npm ci                                    終了コード0（0 vulnerabilities）
npm run check                             終了コード0（tsc -b）
npm test                                  終了コード0（3 files / 7 tests passed）
npm run build                             終了コード0
```

統合作業前は107 passed／0 failed／1 ignored。統合で129、安全性再レビューで142になった。
再レビューでは18本追加し、5本を`#[cfg(unix)] mod baseline_updates`へ移した（差し引き+13）。

| モジュール | 件数 | 再レビューでの増減 |
|---|---|---|
| `scan_jobs` | 13 | +5（確定境界のcancel／pause、checkpoint追い越し、root差し替え、同時start） |
| `incremental_trust` | 13 | +2（祖先のsubtree再走査要求） |
| `incremental_storage` | 14 | +3（評価後のcheckpoint移動、未再走査リンクのmetadata、確定connectionのメモリ上限） |
| `incremental_rescan` | 13 | 変更なし |
| `incremental_scan` | 11 | 変更なし（一致比較テストの内容を強化） |
| `incremental_paths` | 8 | +1（祖先と無関係な変更の区別） |
| `index_checkpoint` | 7 | +2（新規v8と移行v8のschema一致、移行失敗からの復旧） |
| `storage` | 10 | |
| `cache_catalog` | 10 | |
| その他（既存） | 44 | |

ignored 1本は既存の実機依存テストで、今回追加したものではない。

### 必須回帰ケースの対応

| 要求ケース | テスト |
|---|---|
| 走査root≠volume rootのpath変換 | `converts_device_relative_changes_for_a_nested_scan_root`、`passes_changes_through_when_the_scan_root_is_the_volume_root` |
| root自身・root外・親参照・不正path | `treats_siblings_and_text_prefixes_as_outside`、`distinguishes_strict_ancestors_from_unrelated_changes`、`rejects_unsafe_change_paths_without_guessing_scope`、`normalizes_relative_paths_and_rejects_escapes`、`represents_the_volume_root_as_a_single_dot` |
| fileの追加・変更・削除 | `replaces_added_changed_and_deleted_files` |
| directoryのrename・削除・型変更 | `collects_a_renamed_directory_under_its_new_name`、`leaves_no_replacement_when_a_directory_is_deleted`、`drops_old_descendants_when_a_directory_becomes_a_file` |
| 読取失敗と消失の区別 | `fails_closed_when_a_target_cannot_be_read`、`leaves_no_replacement_when_a_directory_is_deleted` |
| リンク差し替え | `does_not_follow_a_directory_replaced_by_a_symlink`、`fails_closed_when_the_target_spelling_does_not_match_the_entry` |
| target上限・保持件数上限 | `fails_closed_for_unsafe_paths_and_target_overflow`、`exact_descendants_count_toward_the_target_limit`、`zero_budget_allows_only_an_empty_plan`、`stops_staging_before_exceeding_the_replacement_limit`、`staging_never_retains_more_than_one_batch_in_memory` |
| ハードリンク計上元の削除と再集計 | `reaggregates_hard_links_when_the_counted_link_disappears`、`reaggregates_hard_links_when_a_link_is_rescanned` |
| baseline／checkpoint不一致 | `refuses_to_apply_when_the_checkpoint_does_not_name_the_update_baseline`、`rejects_a_checkpoint_that_points_at_another_baseline`、`requires_a_full_scan_until_the_checkpoint_names_a_baseline`、`rejects_invalid_baseline_scan_id` |
| キャンセル・保存失敗・rollback | `keeps_the_baseline_and_checkpoint_when_cancelled`、`rolls_back_the_new_snapshot_when_the_checkpoint_cannot_be_saved`、`stops_without_a_partial_replacement_when_cancelled`、`rolls_back_snapshot_when_checkpoint_is_invalid`、`rolls_back_when_replacement_is_outside_target` |
| 変更停止後の差分結果とフルスキャンの一致 | `matches_a_fresh_full_scan_after_changes_stop`（行ごとのmetadataとidentityごとの集計を比較） |
| 祖先への再帰変更通知の取りこぼし | `requires_a_full_scan_when_an_ancestor_demands_a_subtree_rescan`、`ignores_ancestor_changes_without_a_subtree_rescan` |
| 評価後にcheckpointが動いた場合 | `refuses_to_apply_when_the_stored_checkpoint_moved_since_the_assessment`、`refuses_to_commit_when_another_scan_advanced_the_checkpoint` |
| 走査中のroot／volume差し替え | `refuses_to_commit_when_the_scan_root_was_replaced` |
| 確定境界でのcancel・pause・resume | `cancels_at_the_last_moment_before_the_commit`、`defers_the_commit_while_paused_and_finishes_after_resume` |
| 同時実行制約（2本のstartの競合） | `refuses_concurrent_starts_while_a_scan_is_active` |
| 未再走査リンク経由のサイズ変更 | `refreshes_metadata_of_hard_links_that_were_not_rescanned`、`matches_a_fresh_full_scan_after_changes_stop` |
| schema v8（実v7形式からの移行・schema一致・移行失敗） | `migrates_v7_by_adding_baseline_link_with_backup`、`new_and_migrated_databases_agree_on_the_v8_schema`、`keeps_the_v7_database_and_its_backup_when_migration_fails` |

## 異常系・ロールバック・キャンセルの検証結果

- **キャンセル**：`keeps_the_baseline_and_checkpoint_when_cancelled`。確定前にキャンセルした場合、
  jobは`Cancelled`、`savedScanId`は`None`、scan sessionは増えず、保存済みcheckpointは変化しない。
- **確定途中の失敗からのrollback**：`rolls_back_the_new_snapshot_when_the_checkpoint_cannot_be_saved`。
  空tokenは`apply_staged_snapshot`の事前検査を通り、transaction末尾の`validate_checkpoint`で失敗する。
  つまり新sessionのINSERT・基準の複製・target削除・staging挿入・ハードリンク再集計をすべて
  行ったあとで失敗させている。結果、scan sessionは増えず、checkpointも進まない。
  jobは`Failed`になる。
- **identity不一致**：`refuses_to_apply_when_the_checkpoint_does_not_name_the_update_baseline`。
  checkpointが指す基準scanと更新元が違えば、走査結果があってもエラーで拒否する。
- **走査中の中止**：`stops_without_a_partial_replacement_when_cancelled`。
  部分的な置換をstagingへ残さない。
- **不正な置換範囲**：`rolls_back_when_replacement_is_outside_target`、
  `rejects_duplicate_replacement_paths`。target外を指す置換と重複pathをtransactionごと拒否する。
- **信頼できない履歴**：`falls_back_to_a_full_scan_when_the_index_is_not_trusted`。
  checkpointが無い状態では`ScanRecommendation::Full`と`FullScanReason::IndexUntrusted`を返し、
  フルスキャンが完走して`savedScanId`が入る。

これらのうち単独の失敗経路は、確定処理を自由関数`run_incremental`として切り出し、
テストから同期的に駆動して確認している。順序が問題になるケース（確定境界でのcancel／pause、
checkpointの追い越し、rootの差し替え、2本のstartの競合）は、実際のworker threadを
`sync_channel(0)`のrendezvousと`Barrier`で止めて決定論的に再現している。

## 検証ログ

以下に保存した。リポジトリには含めない。

```
~/Library/Logs/Disk_Visualizer/task1-integration-fmt.log      （統合時）
~/Library/Logs/Disk_Visualizer/task1-integration-clippy.log   （統合時）
~/Library/Logs/Disk_Visualizer/task1-integration-test.log     （統合時）
~/Library/Logs/Disk_Visualizer/task1-safety-20260908/cargo-fmt.log
~/Library/Logs/Disk_Visualizer/task1-safety-20260908/cargo-clippy.log
~/Library/Logs/Disk_Visualizer/task1-safety-20260908/cargo-test.log
~/Library/Logs/Disk_Visualizer/task1-safety-20260908/node-env.log
~/Library/Logs/Disk_Visualizer/task1-safety-20260908/npm-ci.log
~/Library/Logs/Disk_Visualizer/task1-safety-20260908/npm-check.log
~/Library/Logs/Disk_Visualizer/task1-safety-20260908/npm-test.log
~/Library/Logs/Disk_Visualizer/task1-safety-20260908/npm-build.log
```

各ログの末尾に実行コマンドと終了コードを記録している。

## 未完了項目・未検証範囲

1. **volume境界越えの実走査**：`device_relative_root`のmount point越え拒否と`crosses_volume`は
   実装済みだが、実際に別volumeを跨ぐ変更を流した検証はしていない。
   再現手順：`hdiutil attach -nomount ram://…`でRAM diskを作り`diskutil eraseVolume`で
   APFS volumeとして初期化、走査root配下へmountして、その中と外にfileを作る。
   必要環境：追加のmount権限（`diskutil`実行）。CIのmacos-14 runnerでも実行可能だが、
   後始末（`hdiutil detach`）の失敗がrunnerに残るため、まず手元で確立してから入れる。
2. **上限値の実値検証**：`MAX_REPLACEMENT_ENTRIES = 1_000_000`、`MAX_HISTORY_CHANGES = 65_536`、
   `MAX_RESCAN_TARGETS = 4_096`は、分岐の到達のみ確認しており、実値でのメモリ・時間は未計測。
3. **UI接続**：`start_incremental_scan`はバックエンドのみ。React側からの呼び出し、
   信頼状態とフルスキャン理由の表示は未実装。
4. **アプリ実機での受け入れ検証**：Tauriアプリを起動しての差分更新の動作確認は未実施。
   `npm run tauri dev`でのGUI操作が必要で、自動テストでは代替していない。
5. **実FSEvents eventを流す経路**：`fsevents_history`のstream取得は、テストでは
   callback層（`fsevents_callback`）とdecision層に分けて検証している。実際にファイルを
   変更してeventが届き、それが差分更新の完走まで通ることは確認していない。
   再現手順：走査rootでフルスキャン→checkpoint保存後にfileを変更→`start_incremental_scan`。
   必要環境：FSEventsのlatencyを待つ実時間の待機とGUIまたはコマンド経路。
6. **外付け媒体の切断・再接続、スリープ復帰でのidentity再評価**：未着手（タスク3以降）。
   確定直前の`verify_root_identity`はunmount中の差し替えを捉えるが、
   再接続後の再評価フローは実装していない。

## Windows検証が必要な範囲

MacでのRustテスト成功を、Windows実機成功・二重OS CI成功とは扱わない。
以下はWindowsでの確認が必要。

- `cargo test`／`cargo clippy`がWindowsでも通ること（既存フルスキャンの回帰確認を含む）。
- `is_same_directory`はunix以外で常に失敗を返す（`cap_std::fs::MetadataExt`が`#[cfg(unix)]`のため）。
  この経路がWindowsのフルスキャンに影響していないこと。
- `incremental_paths`のmacOS専用部分（`device_relative_root`）が
  Windowsビルドから正しく除外されていること。
- schema v8移行がWindows上の既存DBに対しても成功すること（v6→v8、v7→v8の両経路）。
- `start_incremental_scan`がWindowsでは`Unsupported`としてフルスキャンへ倒れること。
- Windows固有のpath表現（`\\?\`、ドライブレター、代替データストリーム、大文字小文字）に対する
  変換経路の扱い。現時点でWindows向けのdevice相対path変換は実装していない（タスク2の範囲）。
- `verify_root_identity`はunix以外でfail closedになるため、差分更新の確定経路は
  Windowsでは使えない。`#[cfg(unix)] mod baseline_updates`のテストはWindowsでコンパイルされない。
  タスク2でWindowsの差分更新を有効にする際は、同等のidentity検証（volume serial number＋
  file reference number）と、対応するテストが必要になる。

## 再開手順

```
git checkout feat/task1-incremental-integration-20260907
cargo fmt --check --manifest-path src-tauri/Cargo.toml
cargo clippy --locked --all-targets --all-features --manifest-path src-tauri/Cargo.toml -- -D warnings
cargo test --locked --manifest-path src-tauri/Cargo.toml
```

142 passed／0 failed／1 ignored、clippy警告0が再現すれば同じ地点にいる。

frontend検証にはnode@22を使う（既定のnodeは`libsimdutf.34.dylib`不在でdyldエラーになる）。
コマンド単位でPATHの先頭に`/opt/homebrew/opt/node@22/bin`を置く。symlinkでの取り繕いや
`brew upgrade`／`brew cleanup`は行っていない。

タスク1の残作業（この順序で進める）：

1. `start_incremental_scan`をUIへ接続し、信頼状態とフルスキャン理由を表示する。
2. Windowsで回帰確認する（`cargo test`／`cargo clippy`、schema v6→v8・v7→v8移行、
   `start_incremental_scan`が`Unsupported`でフルスキャンへ倒れること）。
3. 二重OS CI（macos-14／windows-latest）を通す。
4. 上限値の実値でメモリ・時間を計測し、必要なら調整する。

タスク1がここまで完了してから、タスク2（Windows USN変更record読取）へ進む。

## 影響と制約

- ファイル内容の読み取り、削除、外部送信は追加していない。
- 既存のフルスキャン経路（`start_scan`）とWindows向け共通コードの動作は変更していない。
- 公開Tauriコマンドは`start_incremental_scan`の1件追加のみ。既存コマンドの署名は変更なし。
- `ScanJobSnapshot`への`savedScanId`追加はオプショナルな追加フィールドで、既存の利用側は影響を受けない。
- schemaはv7からv8へ上がる。ダウングレード経路は用意していない。
- 差分更新の確定はunix専用（確定直前のroot identity検証が`#[cfg(unix)]`）。
  他のplatformでは差分更新へ入らずフルスキャンへ倒れるため、既存動作は変わらない。
- tsconfig.app.tsbuildinfo、tsconfig.node.tsbuildinfo、.serena/はコミット対象外。
- 初期リリースの機能範囲は削減していない。
