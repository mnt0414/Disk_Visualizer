# Disk Visualizer 技術引き継ぎ

最終更新: 2026-08-16  
基準: `main` / `e904be1e570c62c8f95e1de42d0c7042d220bad0`（PR #24マージ後）

## 1. プロダクト方針

Disk VisualizerはmacOSとWindows向けの、軽量・高速・完全オフラインなディスク使用量可視化ツールである。

- Rust stable + Tauri 2 + React + TypeScript + Vite
- ローカル保存はSQLite
- macOS Apple Silicon、Windows 11 x64を初期ターゲットとする
- APFS、HFS+、NTFS、exFATを正式検証対象とする
- ファイル内容を読まず、メタデータだけを走査する
- アプリ内で削除・移動を行わず、Finder／Explorerで対象を開く
- テレメトリと外部送信を行わない
- Apache License 2.0

## 2. 実装済み範囲

### Phase 0: 基盤

- Tauri 2 + React + TypeScript + Viteのアプリ基盤
- macOS ARM64／Windows x64のCIとデスクトップビルド
- ESLint、Prettier、TypeScript、Rustfmt、Clippy、Rustテスト
- LICENSE、NOTICE、SECURITY、CONTRIBUTING
- Tauriが要求するPNG／ICOをバージョン管理されたソースから生成

### Phase 1: UIシェル

- Studio Indigoのデザイントークン
- ダッシュボード、ストレージマップ、大容量項目、アプリキャッシュ、比較、スキャン、設定の画面遷移
- ライト／ダークテーマ、高コントラスト、Reduced Motionの基盤
- モックデータを用いた操作フロー

### Phase 2: 最小スキャナー

- ネイティブフォルダーピッカー
- 読み取り専用のRustスキャナー
- 非同期スキャンライフサイクル
- 進捗取得、一時停止、再開、キャンセル
- エラーとアクセス拒否を結果へ集約

### Phase 3: SQLiteと有界メモリ

- スキャン履歴のSQLite永続化
- `WRITE_BATCH_SIZE = 500`のストリーミング書き込み
- 全件をメモリへ保持しない走査・保存フロー
- 不完全スキャンの識別と7日後の整理
- 状態遷移のenum化と不正遷移拒否
- `VACUUM INTO` + `quick_check`による移行前バックアップ
- SQLite整数範囲の検査、IPC一時エラーからの復旧

### Phase 4: ファイルシステム精度

- 論理サイズと割り当て済みサイズの分離
- macOS/Unix: `st_blocks * 512`
- Windows: ハンドルから`FILE_STANDARD_INFO.AllocationSize`を取得
- ボリュームID + ファイルIDによるハードリンク重複排除
- スパース／圧縮ファイルの識別基盤
- シンボリックリンク、Windowsジャンクション／リパースポイントを非追跡
- 別ボリュームを自動横断しない境界ポリシー
- 読み飛ばした項目と理由をSQLiteへ保存
- SQLite schema v4

### Phase 5: アプリキャッシュ認識（進行中）

実装済み:

- バージョン付きJSONカタログ
- OS、アプリ、バージョン制約、パス、根拠、信頼度、再生成可否、整理時影響のモデル
- 固定、設定検出、ユーザー指定パスの区別
- Safari、Chrome、Edge、Firefoxの初期定義
- パス要素単位の境界一致
- Windowsのcase-insensitive照合
- 環境変数由来ルートからの絶対パス分類
- 最も具体的な一致定義の優先
- 分類時のcatalog version、definition ID、definition versionをSQLite schema v5へ保存
- 既存履歴を現在の定義で暗黙に再分類しない

未実装:

- 使用中／変動中の状態判定
- UIで定義バージョン、根拠、信頼度、再生成可否、整理時影響を表示
- Adobe、DaVinci Resolve、Autodesk Flame、Blenderの定義

## 3. 主要データフロー

```text
React UI
  -> Tauri command
  -> scan_jobs.rs（ライフサイクル／同期）
  -> scanner.rs（列挙、境界判定、集計）
  -> file_metrics.rs（OS別メタデータ）
  -> cache_catalog.rs（アプリキャッシュ分類）
  -> storage.rs（SQLiteバッチ書き込み、移行、復旧）
  -> UIが進捗と保存済み結果を取得
```

設計上の重要点:

1. スキャン結果を全件メモリへためない。
2. リンクと別ボリュームを既定で追跡しない。
3. 容量値に誤差要因があるため「実使用量推定値」と表現する。
4. 分類結果には使用した定義の版を保存し、再現性を保つ。
5. 不完全なセッションを正常結果として再利用しない。

## 4. SQLite

- DB: `scan-index.sqlite3`
- 現行schema: v5
- 現行の重要列: logical size、allocated size、file ID、volume ID、modified time、skip reason、cache catalog version、cache definition ID/version
- v4 -> v5では分類参照列を追加し、`scan_entries(scan_id, cache_definition_id)`にインデックスを作る
- 新規v5 DBと移行DBのschemaが同じになることを必ずテストする
- 移行前に`sqlite3.v4-backup`を作る
- 旧履歴の分類列はNULLのままにし、表示時の最新版カタログで意味を上書きしない

参照:

- `docs/SQLITE_SCHEMA_V4.md`
- `docs/SQLITE_SCHEMA_V5.md`
- `docs/SQLITE_BENCHMARK.md`

## 5. CIとローカル確認

```bash
npm install
npm run format:check
npm run check
npm test
npm run icons
cargo fmt --check --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
```

通常CIは次の6系統を確認する。

- frontend
- rust-quality
- platform-tests (macOS)
- platform-tests (Windows)
- desktop-build (macOS ARM64)
- desktop-build (Windows x64)

GitHub Actions botのpushでは通常CIが起動しない場合がある。自動整形用の一時Workflowを使った場合は、必ず削除をユーザーコミットとして行い、最終headで通常CIを再実行する。

## 6. 次の推奨作業

1. Phase 5の使用中／変動中判定を設計する。
2. SQLiteの分類参照をUIへ返すquery/commandを追加する。
3. アプリキャッシュ詳細に根拠・信頼度・定義版・整理時影響を表示する。
4. クリエイティブアプリの定義を、公式資料と実機検証付きで追加する。
5. Phase 5完了レビューを実施する。
6. Phase 6でFSEvents／USN Change Journalと信頼状態を実装する。

## 7. 関連資料

- `docs/IMPLEMENTATION_PLAN.md`
- `docs/FILESYSTEM_ACCURACY.md`
- `docs/PLATFORM_NOTES.md`
- `docs/DEBUGGING_AND_REVIEW.md`
- `docs/PHASE4_PROGRESS.md`
- `docs/PHASE5_PROGRESS.md`
