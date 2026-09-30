# Phase 6 進捗

## 目的

差分更新・逐次キュー・電源状態対応を、安全性を崩さず段階的に導入する。

## インデックス信頼状態

Phase 6の最初の縦切りとして、OS固有のFSEvents／USN Change Journal実装より先に、差分更新を許可できる条件を純粋なRustモデルとして固定した。

差分更新を許可するのは、次の条件をすべて満たす場合だけとする。

- 対象プラットフォームで変更履歴を利用できる
- 完了済みの基準スキャンがある
- 変更履歴を取得できる
- 基準位置から現在位置まで履歴が連続している
- volume identityが基準スキャンと一致する
- root identityが基準スキャンと一致する

一つでも満たさない場合はフルスキャンを提案する。履歴欠落やidentity不明を同一対象・連続履歴として推測しない。

## 状態

- `trusted`: 差分更新を許可
- `initial_scan_required`: 基準スキャンがなくフルスキャンが必要
- `history_unavailable`: 変更履歴を取得できずフルスキャンが必要
- `history_discontinuous`: 履歴欠落・journal再作成等によりフルスキャンが必要
- `volume_changed`: volume identity不一致のためフルスキャンが必要
- `root_changed`: root identity不一致のためフルスキャンが必要
- `unsupported`: 変更履歴非対応のためフルスキャンが必要

## macOS FSEvents統合

SQLite v7の保存済みcheckpoint、現在のdirectory handleから取得したvolume／root identity、per-device FSEvents履歴streamを`incremental_trust`で統合した。

- canonical rootでcheckpointを検索し、platform・history source・version付きtokenを検証する
- identity不一致時は履歴streamを開始せず、volume／root変更としてフルスキャンへ戻す
- `HistoryDone`まで安全に取得できた`Incremental`／`RescanSubtrees`だけを`trusted`とする
- dropped／wrapped／不正event IDは`history_discontinuous`、native root changeは`root_changed`とする
- timeout、callback失敗、stream作成／開始失敗は`history_unavailable`とする
- 信頼できる結果だけが変更pathと次のFSEvents tokenを返す
- volume root `.` が通常変更eventとして届く場合を安全な相対pathとして受け入れる

## フルスキャンcheckpointの原子的確定

macOSではフルスキャン開始前にFSEvents checkpointとdirectory handle由来のvolume／root identityを取得する。開始前の位置を使うことで、走査中に発生した変更は次回の履歴読取で再取得され、取りこぼしを避ける。

- scan sessionの`complete`更新と`index_checkpoints`のupsertを同じSQLite transactionで確定する
- checkpoint検証・保存に失敗した場合はtransactionをrollbackし、sessionを`failed`へ遷移する
- キャンセル、走査失敗、永続化失敗ではcheckpointを保存しない
- FSEvents checkpointを取得できない環境ではフルスキャン自体を妨げず、推測したtokenは保存しない

## 部分再走査計画

信頼済みFSEvents変更を、そのままファイルシステム操作へ渡さず、決定論的で有界な再走査targetへ変換する純粋ロジックを追加した。

- device-relative pathを再検証し、絶対path・親参照・root逸脱表現をfail closedで拒否する
- 重複targetと祖先配下のtargetを統合し、同じsubtreeを重複走査しない
- `RescanSubtrees`ではtargetを再帰走査対象として保持する
- volume root `.` は一つの再帰targetとして表現する
- target数が設定上限を超えた場合は部分更新を許可せずフルスキャンへ戻す

## SQLiteへの原子的な差分適用

直前の完了済みスキャンを新しいsessionへ複製し、部分再走査targetに含まれる既存entryだけを置換するtransaction primitiveを追加した。

- 基準スキャンを直接更新せず、新しいスナップショットとして履歴を保持する
- exact targetは一致pathだけ、recursive targetは対象と子孫を削除してreplacementへ置換する
- replacementがroot外またはtarget外を指す場合、重複pathを含む場合はfail closedで拒否する
- 空replacementで削除済みpathを表現する
- 更新後のサイズ・file・directory・skip集計をSQLite内容から再計算する
- 新sessionのcomplete化と次の履歴checkpoint更新を同じtransactionで確定する
- checkpoint不正を含む途中失敗では新sessionとentryをすべてrollbackする
- 差分適用の結果は`Applied`（新session ID・集計値）または`HardLinkCrossesTarget`で返し、後者ではtransactionをrollbackしてsessionもcheckpointも書き込まない

## capability-based部分再走査

再走査targetだけをcapability handle経由で走査し、フルスキャンと同じ規則の差分entryを生成する（`scanner::scan_targets_controlled`）。

- rootを`cap_std`で開き、フルスキャンと同じ方法でroot volume identityを取得する
- rootからtargetの親まで一要素ずつ降り、各要素を`symlink_metadata`で確認する。symlinkは辿らず、実directoryかつroot volumeと同一のものだけを許可する
- 途中要素が存在しなければtargetは削除済みとして扱い、空のreplacementで表現する
- 途中要素がsymlink・非directory・別volume・volume不明・読取不能ならfail closedとし、差分更新全体をフルスキャン要求にする（DBは変更しない）
- target自体の記録はフルスキャンと同一で、`scan_entry`を共通化している。fileはmetrics付きentry、directoryはentryと子孫、symlinkは`link_not_followed`、別volumeは`different_volume`／`volume_identity_unavailable`の読み飛ばしになる
- target `.` はroot直下の全項目を走査する
- 走査全体で一つの重複判定store（`SeenFileStore`）を共有し、hard link重複の集計サイズは0とする
- directory entryを列挙できない場合（`directory_entry_unreadable`）は、フルスキャンの重複path記録を差分適用で再現できないためフルスキャンへ戻す
- 差分entryは最大50万件までメモリに保持し、超える場合はフルスキャンへ戻す

### 全targetを再帰走査する

plannerの`recursive`に関わらず、部分再走査は全targetを再帰的に走査し、`apply_incremental_snapshot`にも`recursive: true`で渡す。exact（非再帰）eventが付いたdirectoryには、移動で持ち込まれた内容が含まれ得る。またdirectoryがfileへ置換された場合、非再帰置換では旧子孫のentryが孤立して残る。target配下を丸ごと置換すればどちらも安全に扱える。

### hard linkと集計値

`size_bytes`は初出だけが集計サイズを持ち、重複は0である。target内だけを再走査すると、hard link groupがtargetの内外に跨がる場合に合計が狂う。次のいずれかを検出した場合は、`apply_incremental_snapshot`のtransaction内（commit前）で`HardLinkCrossesTarget`としてrollbackし、フルスキャンへ戻す。

- 置換されるtarget内の基準entryと、target外に残る基準entryが同じ`(volume_identity, file_identity)`を共有する
- replacementが、target外に残る基準entryと同じ`(volume_identity, file_identity)`を持つ

groupの全員がtarget内、または全員が置換対象に含まれる場合は、一つの重複判定storeにより一度だけ集計される。

## 差分更新の実行（`incremental_update`）

評価済みの信頼状態を受け取り、計画・部分再走査・原子的適用までを行う`run_incremental_update`を追加した。OS固有の履歴取得（`assess_macos_index_trust`）とは分離しており、Linuxでもテストできる。

1. 信頼状態が`trusted`（かつ次の履歴tokenあり）であることを確認する
2. 対象rootの最新の`complete` session（`completed_at DESC, id DESC`）を基準にする。なければフルスキャン要求
3. `plan_incremental_rescan`でtargetを作る（上限256）
4. 部分再走査し、新しい完全なscan sessionとcheckpointを一つのtransactionで確定する。新checkpointは旧checkpointのroot・platform・volume／root identity・sourceを保ち、`history_token`と`updated_at`だけを更新する

結果は`IncrementalUpdateOutcome`で表す。

- `Applied`: 新session IDとサイズ・file・directory・skip集計
- `FullScanRequired { reason }`: 理由は`kind`付きのsnake_case
  - `trust_not_trusted`（`state`付き）、`history_token_missing`
  - `invalid_change_path`、`too_many_targets`（planner由来）
  - `unsafe_target_path`、`unreadable_directory_entry`、`too_many_entries`（部分再走査由来）
  - `hard_link_crosses_target`、`baseline_missing`

`FullScanRequired`ではsessionもcheckpointも書き込まない。I/OやSQLiteの失敗、キャンセルは`Err`とし、この場合もDBは変更されない。

## ScanManager統合

`ScanManager::start_incremental`とTauri command `start_incremental_scan`を追加した。

- フルスキャンと同じ単一のactive slotを使い、同時に二つのスキャンは実行しない
- pause／resume／cancel／statusは既存の`pause_scan`／`resume_scan`／`cancel_scan`／`get_scan_status`を共通で使う
- キャンセル・失敗・フルスキャン要求ではsessionもcheckpointも変更しない
- `ScanJobSnapshot`に`mode`（`full`／`incremental`）、`savedScanId`、`fullScanRequired`を追加した。既存フィールドとJSON名は変更していない
- フルスキャンが必要な場合、jobは`failed`になり、`error`に理由文を、`fullScanRequired`に機械可読な理由を入れる。新しいstatusは追加していない
- 差分更新が完了した場合、`result`（`ScanSummary`）は`None`とし、集計値だけをsnapshotへ反映する。上位項目を含む要約の構築にはSQLite全体の再集計が必要なためで、保存済みsessionは`savedScanId`から取得する
- macOS以外では`assess_macos_index_trust`が`Unsupported`を返すため、常にフルスキャン要求になる

## checkpointと基準sessionの世代整合

不変条件: checkpointは、それを適用する基準sessionより新しくてはならない。checkpointが古い分には、履歴の再読取で対象が増えるだけで安全だが、新しいcheckpointを古い基準へ適用すると、その間の変更を黙って取りこぼす。この条件を次の二つの規則で保つ。

- 完了済みsessionを削除する場合（`ScanRepository::delete`）、同じtransactionで同じroot pathのcheckpointも削除する。次回の差分更新は`initial_scan_required`としてフルスキャンへ戻る。完了以外（`in_progress`／`interrupted`／`failed`）のsession削除ではcheckpointを変更しない。`index_checkpoints`テーブルがないDBでも削除できる
- フルスキャン（`ScanManager::start`）は、開始時に指定pathをcanonical化し、session、走査、checkpoint取得のすべてにcanonical pathを使う。これによりsessionの`root_path`とcheckpointの`root_path`が常に一致する。canonical化できないpathは開始時にエラーとする。`ScanJobSnapshot.path`はUI互換のため利用者が指定した文字列のまま返す

以前の非canonical pathで保存された基準sessionは、root pathがcheckpointと一致しないため基準なし（`baseline_missing`）となり、フルスキャンを要求する。

## 次の実装

1. Windows USN変更record読取adapterを追加する
2. 信頼状態と差分更新の結果をUIへ表示し、差分不可時は理由付きでフルスキャンを提案する
3. 外付け媒体の切断・再接続、スリープ復帰でidentityを再評価する
