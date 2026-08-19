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

## 次の実装

1. SQLite v7へ基準スキャンのvolume identity、root identity、OS履歴checkpointを保存する
2. macOS FSEventsの履歴連続性adapterを追加する
3. Windows USN Journal ID／USN範囲の連続性adapterを追加する
4. 信頼状態をUIへ表示し、差分不可時は理由付きでフルスキャンを提案する
5. 外付け媒体の切断・再接続、スリープ復帰でidentityを再評価する
