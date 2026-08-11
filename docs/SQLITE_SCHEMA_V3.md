# SQLite schema v3

Phase 4の精度向上に先立ち、スキャン項目の階層と物理サイズ計測を保持できる形式へ移行する。

## 追加フィールド

- `parent_path`: 直接の親パス
- `relative_path`: スキャンルートからの相対パス
- `entry_type`: `file` / `directory` / `other`
- `logical_size`: 論理ファイルサイズ
- `allocated_size`: ファイルシステム上の割り当て済みサイズ
- `file_identity`: ハードリンク判定用のファイルID（inode／Windows file index）
- `volume_identity`: ファイルIDの名前空間を定めるボリュームID（device ID／volume serial）
- `modified_at`: Unix秒単位の更新日時。取得不能または表現不能な場合は`NULL`

## 保存方針

- ファイル行には論理サイズ、割り当て済みサイズ、ファイルID、ボリュームID、更新日時を保存する。
- ディレクトリ行は論理サイズを`0`、割り当て済みサイズとファイルIDを`NULL`として保存する。
- 合計値ではハードリンクを重複除外するが、各パスの行は維持してファイルIDから同一実体を判定できるようにする。
- 対応APIがない環境やメタデータ取得に失敗した値は推測せず`NULL`にする。

## 移行と保持

- v1/v2からの移行前に`VACUUM INTO`で一貫したバックアップを作る。
- バックアップは別接続の`PRAGMA quick_check`で検証してから移行する。
- `interrupted`と`failed`のセッションは調査用に7日間保持し、その後起動時に削除する。
- 完了セッションはユーザーが履歴から明示的に削除するまで保持する。
