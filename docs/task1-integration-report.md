# タスク1：共通差分更新・macOS統合

## 状態

2026-09-08。個別に実装済みだった構成要素を、FSEvents履歴の取得からSQLiteスナップショット確定までの
一本の差分更新経路として接続し、Macローカルでの検証を完了した。

Windows実機、二重OS CI、アプリ実機受け入れ検証は未実施。
タスク2（Windows USN変更record読取）以降には着手していない。

## コード基準

- 元のmain：218824046a3df909467df387b90fe29b049004e4
- 依存修復のローカルコミット：ec6323a
- plannerの境界修正：02ae78d（`docs/task1-planner-report.md`）
- 作業ブランチ：feat/task1-incremental-integration-20260907（02ae78dから作成）

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
| `other/x`、`.`（root外の祖先） | `Outside` |
| `/abs/path`、`..`を含むpath、UTF-8化不能 | `Invalid`（fail closed） |

走査rootがvolume rootの場合、device相対pathはそのままscan-root相対pathになる。
`Invalid`は1件でも混じれば差分更新を中止し、フルスキャンへ倒す。`Outside`は無視する。

## メモリ上限設計の根拠

- **計測済み**：`staging_never_retains_more_than_one_batch_in_memory`と
  `buffer_never_retains_a_full_batch`は、バッチ書き出し後にin-memoryバッファ長が0へ戻ることを
  実際に確認している。全replacementをVecに保持していないことは、この2本で示している。
- **計測済み**：置換件数の上限で停止する分岐は、テスト用の縮小seam（`with_max_entries(2)`）で
  実際に到達させて確認している。
- **未計測**：`MAX_REPLACEMENT_ENTRIES = 1_000_000`という実値、および
  `MAX_HISTORY_CHANGES = 65_536`／`MAX_RESCAN_TARGETS = 4_096`という実値での
  ピークメモリ・所要時間は測っていない。上限値そのものの妥当性は未検証。
- **未計測**：大規模実データ（数百万entry）での差分適用のI/O量・所要時間。
  検証は専用の小規模fixtureのみで行い、ホームディレクトリや実プロジェクトは走査していない。

## 意図的なトレードオフ

1. **`scan_entries`へのscan識別index（`scan_entries_scan_identity`）を追加していない。**
   このindexはv7→v8移行を持つcheckpoint repositoryが、`ScanRepository`所有のテーブルに対して
   作ることになり、所有権が分かれる。checkpoint repositoryのテストは`scan_entries`を持たない
   DBを使うため、移行が壊れる。加えて、1回の差分更新で1度だけ実行するGROUP BYのために
   フルスキャンの全INSERTへ書き込み増幅を課すことになる。SQLiteはこのGROUP BYをディスクへ
   spillできるため、現時点では追加しない判断とした。
2. **フルスキャンと差分更新の一致比較で、行ごとの`size_bytes`は比較していない。**
   ハードリンクの計上元として選ばれる行は、走査順によって同じ`(device, inode)`集合の中で
   入れ替わりうる。行単位で固定すると実装依存の期待値になるため、
   pathの集合と合計値（サイズ・file数・directory数・skip数）で一致を確認している
   （`matches_a_fresh_full_scan_after_changes_stop`）。

## 実行したコマンド

```
cargo fmt --manifest-path src-tauri/Cargo.toml
cargo fmt --check --manifest-path src-tauri/Cargo.toml
cargo clippy --locked --all-targets --all-features --manifest-path src-tauri/Cargo.toml -- -D warnings
cargo test --locked --manifest-path src-tauri/Cargo.toml
```

環境：macOS 15.4／arm64、rustc 1.97.1、cargo 1.97.1、clippy 0.1.97。

テスト・警告の無効化、`#[allow]`の追加、lint設定の緩和は行っていない。

## テスト件数と結果

```
cargo fmt --check                         終了コード0（差分なし）
cargo clippy ... -- -D warnings           Finished dev profile（警告0）
cargo test --locked                       129 passed; 0 failed; 1 ignored
                                          main.rs 0件、doc-tests 0件
```

作業前は107 passed／0 failed／1 ignored。今回+22本。

| モジュール | 件数 | 備考 |
|---|---|---|
| `incremental_rescan` | 13 | 02ae78dから変更なし（`normalize_relative`の移動のみ） |
| `incremental_trust` | 11 | +5 |
| `incremental_storage` | 11 | +1（上限停止） |
| `incremental_scan` | 11 | 新規 |
| `incremental_paths` | 7 | 新規 |
| `scan_jobs` | 8 | +5（差分更新統合） |
| `index_checkpoint` | 5 | +2（v7→v8移行、基準scan ID検証） |
| `storage` | 10 | |
| その他（既存） | 53 | |

ignored 1本は既存の実機依存テストで、今回追加したものではない。

### 必須回帰ケースの対応

| 要求ケース | テスト |
|---|---|
| 走査root≠volume rootのpath変換 | `converts_device_relative_changes_for_a_nested_scan_root`、`passes_changes_through_when_the_scan_root_is_the_volume_root` |
| root自身・root外・親参照・不正path | `treats_ancestors_siblings_and_text_prefixes_as_outside`、`rejects_unsafe_change_paths_without_guessing_scope`、`normalizes_relative_paths_and_rejects_escapes`、`represents_the_volume_root_as_a_single_dot` |
| fileの追加・変更・削除 | `replaces_added_changed_and_deleted_files` |
| directoryのrename・削除・型変更 | `collects_a_renamed_directory_under_its_new_name`、`leaves_no_replacement_when_a_directory_is_deleted`、`drops_old_descendants_when_a_directory_becomes_a_file` |
| 読取失敗と消失の区別 | `fails_closed_when_a_target_cannot_be_read`、`leaves_no_replacement_when_a_directory_is_deleted` |
| リンク差し替え | `does_not_follow_a_directory_replaced_by_a_symlink`、`fails_closed_when_the_target_spelling_does_not_match_the_entry` |
| target上限・保持件数上限 | `fails_closed_for_unsafe_paths_and_target_overflow`、`exact_descendants_count_toward_the_target_limit`、`zero_budget_allows_only_an_empty_plan`、`stops_staging_before_exceeding_the_replacement_limit`、`staging_never_retains_more_than_one_batch_in_memory` |
| ハードリンク計上元の削除と再集計 | `reaggregates_hard_links_when_the_counted_link_disappears`、`reaggregates_hard_links_when_a_link_is_rescanned` |
| baseline／checkpoint不一致 | `refuses_to_apply_when_the_checkpoint_does_not_name_the_update_baseline`、`rejects_a_checkpoint_that_points_at_another_baseline`、`requires_a_full_scan_until_the_checkpoint_names_a_baseline`、`rejects_invalid_baseline_scan_id` |
| キャンセル・保存失敗・rollback | `keeps_the_baseline_and_checkpoint_when_cancelled`、`rolls_back_the_new_snapshot_when_the_checkpoint_cannot_be_saved`、`stops_without_a_partial_replacement_when_cancelled`、`rolls_back_snapshot_when_checkpoint_is_invalid`、`rolls_back_when_replacement_is_outside_target` |
| 変更停止後の差分結果とフルスキャンの一致 | `matches_a_fresh_full_scan_after_changes_stop` |

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

これらはすべて、確定処理を自由関数`run_incremental`として切り出し、
テストからスレッドを介さず同期的に駆動して確認している（競合による不定性がない）。

## 検証ログ

以下に保存した。リポジトリには含めない。

```
~/Library/Logs/Disk_Visualizer/task1-integration-fmt.log
~/Library/Logs/Disk_Visualizer/task1-integration-clippy.log
~/Library/Logs/Disk_Visualizer/task1-integration-test.log
```

## 未完了項目・未検証範囲

1. **volume境界越えの実走査**：2つ目のvolumeをmountしないと再現できないため、fixtureを作れていない。
   `device_relative_root`のmount point越え拒否と`crosses_volume`は実装済みだが、
   実際に別volumeを跨ぐ変更を流した検証はしていない。
2. **上限値の実値検証**：`MAX_REPLACEMENT_ENTRIES = 1_000_000`、`MAX_HISTORY_CHANGES = 65_536`、
   `MAX_RESCAN_TARGETS = 4_096`は、分岐の到達のみ確認しており、実値でのメモリ・時間は未計測。
3. **UI接続**：`start_incremental_scan`はバックエンドのみ。React側からの呼び出し、
   信頼状態とフルスキャン理由の表示は未実装。
4. **npmによる検証**：`npm run check`／`npm test`／`npm run build`は未実行。
   ローカルのnodeがdyldエラー（`libsimdutf.34.dylib`不在）で起動しないため。
   今回TypeScript側の契約は変更していない（`savedScanId`は追加のみ、
   `start_incremental_scan`は新規コマンド）ため、既存フロントエンドのビルドには影響しない見込みだが、
   実行して確かめてはいない。
5. **アプリ実機での受け入れ検証**：Tauriアプリを起動しての差分更新の動作確認は未実施。
6. **pause／resumeと確定の競合**：`can_continue`によるキャンセルとの競合はテスト済みだが、
   差分更新中のpause／resumeを実際に往復させる検証はしていない。
7. **外付け媒体の切断・再接続、スリープ復帰でのidentity再評価**：未着手（タスク3以降）。

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

## 再開手順

```
git checkout feat/task1-incremental-integration-20260907
cargo fmt --check --manifest-path src-tauri/Cargo.toml
cargo clippy --locked --all-targets --all-features --manifest-path src-tauri/Cargo.toml -- -D warnings
cargo test --locked --manifest-path src-tauri/Cargo.toml
```

129 passed／0 failed／1 ignored、clippy警告0が再現すれば同じ地点にいる。

次に着手する順序：

1. nodeを復旧し（`libsimdutf`の再インストール）、`npm run check`／`npm test`／`npm run build`を実行する。
2. `start_incremental_scan`をUIへ接続し、信頼状態とフルスキャン理由を表示する。
3. Windowsで回帰確認する。
4. 上限値の実値でメモリ・時間を計測し、必要なら調整する。
5. タスク2（Windows USN変更record読取）へ進む。

## 影響と制約

- ファイル内容の読み取り、削除、外部送信は追加していない。
- 既存のフルスキャン経路（`start_scan`）とWindows向け共通コードの動作は変更していない。
- 公開Tauriコマンドは`start_incremental_scan`の1件追加のみ。既存コマンドの署名は変更なし。
- `ScanJobSnapshot`への`savedScanId`追加はオプショナルな追加フィールドで、既存の利用側は影響を受けない。
- schemaはv7からv8へ上がる。ダウングレード経路は用意していない。
- tsconfig.app.tsbuildinfo、tsconfig.node.tsbuildinfo、.serena/はコミット対象外。
- 初期リリースの機能範囲は削減していない。
