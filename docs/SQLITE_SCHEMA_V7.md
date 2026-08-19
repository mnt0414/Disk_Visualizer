# SQLite schema v7

Phase 6の差分更新を安全に判断するため、`index_checkpoints`テーブルを追加する。

- `root_path`: スキャン対象。主キー
- `platform`: `macos`／`windows`
- `volume_identity`: 基準スキャン時のvolume identity
- `root_identity`: 基準スキャン時のroot identity
- `history_source`: `fsevents`／`usn`
- `history_token`: OS固有の履歴位置を表す不透明な文字列
- `updated_at`: checkpoint更新時刻

共通層は`history_token`の内部形式を解釈しない。macOS adapterはFSEventsの履歴位置、Windows adapterはUSN Journal IDとUSN位置を、後続実装で検証可能な形式へ符号化する。

identityまたはtokenが不足するcheckpointは保存を拒否し、差分更新可能とは判定しない。root path単位のupsertとし、同じ対象の基準を更新する。

v6からの移行前にはSQLiteの`VACUUM INTO`で整合したバックアップを作成し、`quick_check`成功後にv7へ進む。
