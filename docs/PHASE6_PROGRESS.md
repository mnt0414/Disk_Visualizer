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
- SQLite差分適用とjob統合は次の実装境界とする

## 次の実装

1. 部分再走査計画をscanner／SQLite差分適用へ接続する
2. Windows USN変更record読取adapterを追加する
3. 信頼状態をUIへ表示し、差分不可時は理由付きでフルスキャンを提案する
4. 外付け媒体の切断・再接続、スリープ復帰でidentityを再評価する
