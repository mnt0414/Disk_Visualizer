# SQLite schema v4

Phase 4の精度向上に伴い、スキャン項目の階層、物理サイズ計測、除外理由を保持できる形式へ移行する。

## 保存フィールド

- `parent_path`: 直接の親パス
- `relative_path`: スキャンルートからの相対パス
- `entry_type`: `file` / `directory` / `other`
- `size_bytes`: ハードリンク重複排除後に合計へ加算したサイズ
- `logical_size`: 各パスが参照するファイルの論理サイズ
- `allocated_size`: ファイルシステム上の割り当て済みサイズ
- `file_identity`: ハードリンク判定用のファイルID（inode／Windows file index）
- `volume_identity`: ファイルIDの名前空間を定めるボリュームID（device ID／volume serial）
- `modified_at`: Unix秒単位の更新日時。取得不能または表現不能な場合は`NULL`
- `skipped_count`: その行で読み飛ばした項目数
- `skip_reason`: 読み飛ばし理由。通常項目または既存データでは`NULL`

## 読み飛ばし理由

- `metadata_unavailable`: メタデータを取得できない
- `link_not_followed`: シンボリックリンク／ジャンクション／リパースポイントを非追跡
- `different_volume`: スキャン開始時と異なるボリューム
- `directory_unreadable`: ディレクトリを読み取れない
- `directory_entry_unreadable`: ディレクトリエントリを読み取れない
- `unsupported_entry_type`: ファイル／ディレクトリ以外の項目

## 保存方針

- ファイル行には集計サイズ、論理サイズ、割り当て済みサイズ、ファイルID、ボリュームID、更新日時を保存する。
- ディレクトリ行は論理サイズを`0`、割り当て済みサイズとファイルIDを`NULL`として保存する。
- 合計値ではハードリンクを重複除外するが、各パスの行は維持してファイルIDから同一実体を判定できるようにする。
- 読み飛ばした項目も`entry_type = 'other'`として保存し、`skipped_count`と`skip_reason`を記録する。
- 対応APIがない環境やメタデータ取得に失敗した値は推測せず`NULL`にする。

## 移行と保持

- v1〜v3からの移行前に`VACUUM INTO`で一貫したバックアップを作る。
- v3からは`skip_reason`列を追加し、既存行は`NULL`のまま保持する。
- バックアップは別接続の`PRAGMA quick_check`で検証してから移行する。
- `interrupted`と`failed`のセッションは調査用に7日間保持し、その後起動時に削除する。
- 完了セッションはユーザーが履歴から明示的に削除するまで保持する。
