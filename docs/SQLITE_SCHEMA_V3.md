# SQLite schema v3

Phase 4の精度向上に先立ち、スキャン項目の階層と将来の物理サイズ計測を保持できる形式へ移行する。

## 追加フィールド

- `parent_path`: 直接の親パス
- `relative_path`: スキャンルートからの相対パス
- `entry_type`: `file` / `directory` / `other`
- `logical_size`: 現在の論理ファイルサイズ
- `allocated_size`: Phase 4で実装する割り当て済みサイズ
- `file_identity`: Phase 4で実装するハードリンク判定用ID
- `volume_identity`: Phase 4で実装するボリューム境界判定用ID
- `modified_at`: 将来の差分スキャン用更新時刻

## 移行と保持

- v1/v2からの移行前に`VACUUM INTO`で一貫したバックアップを作る。
- バックアップは別接続の`PRAGMA quick_check`で検証してから移行する。
- `interrupted`と`failed`のセッションは調査用に7日間保持し、その後起動時に削除する。
- 完了セッションはユーザーが履歴から明示的に削除するまで保持する。
