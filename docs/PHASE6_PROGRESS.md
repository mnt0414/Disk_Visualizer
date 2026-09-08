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

## 変更pathの走査root相対化

FSEventsのdevice相対pathを、走査root相対の安全なpathへ変換する経路を`incremental_paths`へ分離した。

- 走査rootのdevice相対pathを、componentごとにdirectory handleを開き`dev`／`ino`で確認しながら復元する
- 名前一致だけで同一性を判断せず、途中のmount point越えは拒否する
- 変更pathを`Inside`／`Outside`／`Invalid`の3値へ落とし、root自身・root外・親参照・不正表現を区別する
- 文字列prefixの一致を親子関係として扱わない
- `Invalid`が1件でもあれば差分更新を中止し、フルスキャンへ倒す

## capability-based部分再走査

計画された再走査targetを、cap-stdのdirectory handle経由でmetadataだけ収集する。

- 親directoryを1 componentずつ開き、`dev`／`ino`が期待と一致しなければ中止する
- symlink・junction・reparse pointを辿らず、別volumeへ出ない
- ファイル内容は読まず、metadataだけを取得する
- 消失は親entryの不在確認を経て空replacementとし、metadata失敗・アクセス拒否は失敗として中止する
- 読取失敗を削除として適用しない
- 置換単位を部分木へ拡大し、directoryのrename・削除・fileへの型変更で古い子孫を残さない
- キャンセル時は部分的な置換をstagingへ残さない

## baselineとcheckpointの対応（SQLite v8）

「同じrootの最新スキャン」という推測を使わず、checkpointが名指しした基準scanだけを差分の適用先とする。

- `index_checkpoints`に`baseline_scan_id`列を追加した（v7→v8）
- 0以下の基準scan IDを不正として拒否する
- checkpointの基準scanと更新元が一致しなければ差分を適用しない
- 移行はVACUUM INTOと`PRAGMA quick_check`によるバックアップを経る。v6→v8とv7→v8の両経路を持つ
- 新スナップショットの確定、token更新、`baseline_scan_id`の付け替えを同一transactionで行う
- キャンセル・走査失敗・適用失敗・identity不一致ではcheckpointを進めず、過去の履歴を残す

## ScanManager統合

`start_incremental_scan`コマンドを追加し、差分更新とフルスキャンの切り替えを一本の経路にまとめた。

- 信頼評価はactive lockの外で行い、進捗照会・pause・resume・cancelを止めない
- 信頼できない履歴、成立しない計画は、理由を`FullScanReason`として返したうえでフルスキャンへ倒す
- 差分更新jobは結果をメモリへ載せず、確定したscan session IDを`savedScanId`として返す
- 確定は取り消せないため、直前にもう一度中断要求を確認する
- 変更が止まったあとの差分結果が、新規フルスキャンとpath集合・合計値で一致することを確認した

詳細と検証結果は`docs/task1-integration-report.md`にある。

## 次の実装

1. 信頼状態とフルスキャン理由をUIへ表示し、`start_incremental_scan`を接続する
2. Windows USN変更record読取adapterを追加する
3. 上限値（履歴件数・target数・置換件数）を実値で計測し、必要なら調整する
4. 外付け媒体の切断・再接続、スリープ復帰でidentityを再評価する
