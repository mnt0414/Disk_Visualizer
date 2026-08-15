# SQLite schema v5

Phase 5のアプリキャッシュ分類を履歴として再現できるよう、`scan_entries`へ以下を追加する。

- `cache_catalog_version`: 分類に使った組み込みカタログのバージョン
- `cache_definition_id`: 一致したキャッシュ定義ID
- `cache_definition_version`: 一致した定義のバージョン

未分類項目は3列すべて`NULL`とする。v4からの移行前には整合したバックアップを作成し、分類は新規スキャンから記録する。既存履歴を現在の定義で自動再分類しない。
